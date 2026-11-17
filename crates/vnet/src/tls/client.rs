//! The TLS 1.3 client state machine (RFC 8446), without I/O: received bytes
//! go in through [`ClientConnection::feed`] and bytes to send come out of
//! [`ClientConnection::take_outgoing`].

use super::codec::{
    Cursor, Truncated, handshake_message, put_extension, put_u16, put_vec8, put_vec16, put_vec24,
};
use super::key_schedule::{
    CipherSuite, RecordCipher, derive_secret, early_secret, finished_verify_data, handshake_secret,
    master_secret, next_traffic_secret,
};
use crate::crypto::hmac::ct_eq;
use crate::crypto::p256::{self, EcdhPrivate};
use crate::crypto::{HashAlg, Hasher, random, x25519};
use crate::x509::{self, CertError, Certificate, PublicKey, TrustStore};
use std::fmt;
use std::net::IpAddr;
use std::sync::Arc;

/// Record content types.
pub mod content {
    pub const CHANGE_CIPHER_SPEC: u8 = 20;
    pub const ALERT: u8 = 21;
    pub const HANDSHAKE: u8 = 22;
    pub const APPLICATION_DATA: u8 = 23;
}

/// Handshake message types.
mod hs {
    pub const CLIENT_HELLO: u8 = 1;
    pub const SERVER_HELLO: u8 = 2;
    pub const NEW_SESSION_TICKET: u8 = 4;
    pub const ENCRYPTED_EXTENSIONS: u8 = 8;
    pub const CERTIFICATE: u8 = 11;
    pub const CERTIFICATE_REQUEST: u8 = 13;
    pub const CERTIFICATE_VERIFY: u8 = 15;
    pub const FINISHED: u8 = 20;
    pub const KEY_UPDATE: u8 = 24;
    pub const MESSAGE_HASH: u8 = 254;
}

/// Extension types.
mod ext {
    pub const SERVER_NAME: u16 = 0;
    pub const SUPPORTED_GROUPS: u16 = 10;
    pub const SIGNATURE_ALGORITHMS: u16 = 13;
    pub const ALPN: u16 = 16;
    pub const PRE_SHARED_KEY: u16 = 41;
    pub const SUPPORTED_VERSIONS: u16 = 43;
    pub const COOKIE: u16 = 44;
    pub const KEY_SHARE: u16 = 51;
}

/// Alert descriptions.
pub mod alert {
    pub const CLOSE_NOTIFY: u8 = 0;
    pub const UNEXPECTED_MESSAGE: u8 = 10;
    pub const BAD_RECORD_MAC: u8 = 20;
    pub const RECORD_OVERFLOW: u8 = 22;
    pub const HANDSHAKE_FAILURE: u8 = 40;
    pub const BAD_CERTIFICATE: u8 = 42;
    pub const UNSUPPORTED_CERTIFICATE: u8 = 43;
    pub const CERTIFICATE_EXPIRED: u8 = 45;
    pub const ILLEGAL_PARAMETER: u8 = 47;
    pub const UNKNOWN_CA: u8 = 48;
    pub const DECODE_ERROR: u8 = 50;
    pub const DECRYPT_ERROR: u8 = 51;
    pub const PROTOCOL_VERSION: u8 = 70;
    pub const INTERNAL_ERROR: u8 = 80;
    pub const USER_CANCELED: u8 = 90;
    pub const MISSING_EXTENSION: u8 = 109;
    pub const UNSUPPORTED_EXTENSION: u8 = 110;
    pub const NO_APPLICATION_PROTOCOL: u8 = 120;
}

/// Signature schemes (RFC 8446 §4.2.3).
pub mod scheme {
    pub const RSA_PKCS1_SHA256: u16 = 0x0401;
    pub const ECDSA_SECP256R1_SHA256: u16 = 0x0403;
    pub const RSA_PSS_RSAE_SHA256: u16 = 0x0804;
    pub const RSA_PSS_RSAE_SHA384: u16 = 0x0805;
    pub const RSA_PSS_RSAE_SHA512: u16 = 0x0806;
}

/// Offered in `signature_algorithms`; `rsa_pkcs1_sha256` only applies to
/// certificates.
const SIGNATURE_SCHEMES: [u16; 5] = [
    scheme::ECDSA_SECP256R1_SHA256,
    scheme::RSA_PSS_RSAE_SHA256,
    scheme::RSA_PSS_RSAE_SHA384,
    scheme::RSA_PSS_RSAE_SHA512,
    scheme::RSA_PKCS1_SHA256,
];

/// ServerHello.random of a HelloRetryRequest: SHA-256("HelloRetryRequest").
pub const HRR_RANDOM: [u8; 32] = [
    0xcf, 0x21, 0xad, 0x74, 0xe5, 0x9a, 0x61, 0x11, 0xbe, 0x1d, 0x8c, 0x02, 0x1e, 0x65, 0xb8, 0x91,
    0xc2, 0xa2, 0x11, 0x16, 0x7a, 0xbb, 0x8c, 0x5e, 0x07, 0x9e, 0x09, 0xe2, 0xc8, 0xa8, 0x33, 0x9c,
];

/// Largest TLSPlaintext fragment.
pub const MAX_FRAGMENT: usize = 1 << 14;
/// Largest handshake message accepted (certificate chains included).
const MAX_HANDSHAKE_MESSAGE: usize = 1 << 18;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TlsError {
    /// The peer sent this fatal alert.
    PeerAlert(u8),
    /// The connection failed here; `alert` was sent to the peer.
    Protocol { alert: u8, reason: &'static str },
    /// The server's certificate was rejected.
    Certificate(CertError),
}

impl TlsError {
    fn protocol(alert: u8, reason: &'static str) -> TlsError {
        TlsError::Protocol { alert, reason }
    }

    /// The alert describing this error.
    pub fn alert(&self) -> u8 {
        match self {
            TlsError::PeerAlert(a) => *a,
            TlsError::Protocol { alert, .. } => *alert,
            TlsError::Certificate(c) => match c {
                CertError::Expired | CertError::NotYetValid => alert::CERTIFICATE_EXPIRED,
                CertError::UnknownIssuer => alert::UNKNOWN_CA,
                CertError::UnsupportedAlgorithm => alert::UNSUPPORTED_CERTIFICATE,
                CertError::Empty | CertError::Malformed => alert::DECODE_ERROR,
                _ => alert::BAD_CERTIFICATE,
            },
        }
    }
}

impl fmt::Display for TlsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TlsError::PeerAlert(a) => write!(f, "peer sent fatal alert {a}"),
            TlsError::Protocol { alert, reason } => write!(f, "{reason} (alert {alert})"),
            TlsError::Certificate(c) => write!(f, "server certificate rejected: {c}"),
        }
    }
}

impl std::error::Error for TlsError {}

impl From<Truncated> for TlsError {
    fn from(_: Truncated) -> Self {
        TlsError::protocol(alert::DECODE_ERROR, "truncated message")
    }
}

/// Decides whether the server's certificate chain is acceptable.
pub trait ServerVerifier: Send + Sync {
    /// `chain` is the server's list, leaf first, as DER.
    fn verify(&self, chain: &[Vec<u8>], server_name: &str) -> Result<(), CertError>;
}

impl ServerVerifier for TrustStore {
    fn verify(&self, chain: &[Vec<u8>], server_name: &str) -> Result<(), CertError> {
        self.verify_at(chain, server_name, x509::now())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NamedGroup {
    X25519,
    Secp256r1,
}

impl NamedGroup {
    pub fn id(self) -> u16 {
        match self {
            NamedGroup::X25519 => 0x001d,
            NamedGroup::Secp256r1 => 0x0017,
        }
    }

    pub fn from_id(id: u16) -> Option<NamedGroup> {
        [NamedGroup::X25519, NamedGroup::Secp256r1]
            .into_iter()
            .find(|g| g.id() == id)
    }
}

#[derive(Clone)]
enum ShareSecret {
    X25519([u8; 32]),
    P256(EcdhPrivate),
}

/// An ephemeral key pair offered in `key_share`.
#[derive(Clone)]
pub struct KeyShare {
    secret: ShareSecret,
    public: Vec<u8>,
}

impl KeyShare {
    pub fn generate(group: NamedGroup) -> std::io::Result<KeyShare> {
        match group {
            NamedGroup::X25519 => Ok(KeyShare::x25519(random::bytes::<32>()?)),
            NamedGroup::Secp256r1 => Ok(KeyShare::from_p256(EcdhPrivate::generate()?)),
        }
    }

    /// An X25519 share with a fixed private key.
    pub fn x25519(private: [u8; 32]) -> KeyShare {
        KeyShare {
            public: x25519::x25519_public(&private).to_vec(),
            secret: ShareSecret::X25519(private),
        }
    }

    /// A secp256r1 share with a fixed 32-byte private scalar.
    pub fn p256(private: &[u8]) -> Option<KeyShare> {
        Some(KeyShare::from_p256(EcdhPrivate::from_bytes(private)?))
    }

    fn from_p256(key: EcdhPrivate) -> KeyShare {
        KeyShare {
            public: key.public_key(),
            secret: ShareSecret::P256(key),
        }
    }

    pub fn group(&self) -> NamedGroup {
        match self.secret {
            ShareSecret::X25519(_) => NamedGroup::X25519,
            ShareSecret::P256(_) => NamedGroup::Secp256r1,
        }
    }

    pub fn public(&self) -> &[u8] {
        &self.public
    }

    /// The shared secret with the server's share; `None` if it is invalid.
    pub fn agree(&self, peer: &[u8]) -> Option<Vec<u8>> {
        match &self.secret {
            ShareSecret::X25519(k) => {
                let peer: [u8; 32] = peer.try_into().ok()?;
                let shared = x25519::x25519(k, &peer);
                if shared.iter().all(|&b| b == 0) {
                    return None;
                }
                Some(shared.to_vec())
            }
            ShareSecret::P256(k) => k.agree(peer),
        }
    }
}

#[derive(Clone)]
pub struct ClientConfig {
    pub verifier: Arc<dyn ServerVerifier>,
    pub cipher_suites: Vec<CipherSuite>,
    /// Supported groups in preference order; the first gets a key share in
    /// the first ClientHello, the others are reached through HelloRetryRequest.
    pub groups: Vec<NamedGroup>,
    /// ALPN protocols; empty sends no ALPN extension.
    pub alpn: Vec<Vec<u8>>,
    /// Send a random legacy_session_id and a dummy ChangeCipherSpec
    /// (RFC 8446 Appendix D.4).
    pub middlebox_compat: bool,
}

impl ClientConfig {
    pub fn new(verifier: Arc<dyn ServerVerifier>) -> ClientConfig {
        ClientConfig {
            verifier,
            cipher_suites: CipherSuite::ALL.to_vec(),
            groups: vec![NamedGroup::X25519, NamedGroup::Secp256r1],
            alpn: vec![b"http/1.1".to_vec()],
            middlebox_compat: true,
        }
    }
}

/// Name sent in SNI: none for IP literals (RFC 6066 §3).
fn sni_name(server_name: &str) -> Option<&str> {
    let name = server_name.trim_end_matches('.');
    let bare = name.trim_start_matches('[').trim_end_matches(']');
    if name.is_empty() || bare.parse::<IpAddr>().is_ok() {
        None
    } else {
        Some(name)
    }
}

/// A ClientHello handshake message.
fn build_client_hello(
    config: &ClientConfig,
    server_name: &str,
    random: &[u8; 32],
    session_id: &[u8],
    shares: &[KeyShare],
    cookie: Option<&[u8]>,
) -> Vec<u8> {
    let mut body = Vec::with_capacity(512);
    put_u16(&mut body, 0x0303);
    body.extend_from_slice(random);
    put_vec8(&mut body, session_id);
    let mut suites = Vec::new();
    for s in &config.cipher_suites {
        put_u16(&mut suites, s.id());
    }
    put_vec16(&mut body, &suites);
    put_vec8(&mut body, &[0]);

    let mut exts = Vec::new();
    if let Some(name) = sni_name(server_name) {
        let mut entry = vec![0u8];
        put_vec16(&mut entry, name.as_bytes());
        let mut list = Vec::new();
        put_vec16(&mut list, &entry);
        put_extension(&mut exts, ext::SERVER_NAME, &list);
    }
    let mut versions = Vec::new();
    put_vec8(&mut versions, &[0x03, 0x04]);
    put_extension(&mut exts, ext::SUPPORTED_VERSIONS, &versions);
    let mut groups = Vec::new();
    for g in &config.groups {
        put_u16(&mut groups, g.id());
    }
    let mut data = Vec::new();
    put_vec16(&mut data, &groups);
    put_extension(&mut exts, ext::SUPPORTED_GROUPS, &data);
    let mut schemes = Vec::new();
    for s in SIGNATURE_SCHEMES {
        put_u16(&mut schemes, s);
    }
    let mut data = Vec::new();
    put_vec16(&mut data, &schemes);
    put_extension(&mut exts, ext::SIGNATURE_ALGORITHMS, &data);
    let mut entries = Vec::new();
    for share in shares {
        put_u16(&mut entries, share.group().id());
        put_vec16(&mut entries, share.public());
    }
    let mut data = Vec::new();
    put_vec16(&mut data, &entries);
    put_extension(&mut exts, ext::KEY_SHARE, &data);
    if !config.alpn.is_empty() {
        let mut list = Vec::new();
        for p in &config.alpn {
            put_vec8(&mut list, p);
        }
        let mut data = Vec::new();
        put_vec16(&mut data, &list);
        put_extension(&mut exts, ext::ALPN, &data);
    }
    if let Some(cookie) = cookie {
        let mut data = Vec::new();
        put_vec16(&mut data, cookie);
        put_extension(&mut exts, ext::COOKIE, &data);
    }
    put_vec16(&mut body, &exts);
    handshake_message(hs::CLIENT_HELLO, &body)
}

/// Extensions as (type, data), rejecting duplicates.
fn parse_extensions(data: &[u8]) -> Result<Vec<(u16, &[u8])>, TlsError> {
    let mut c = Cursor::new(data);
    let mut out: Vec<(u16, &[u8])> = Vec::new();
    while !c.is_empty() {
        let kind = c.u16()?;
        let body = c.vec16()?;
        if out.iter().any(|(k, _)| *k == kind) {
            return Err(TlsError::protocol(
                alert::ILLEGAL_PARAMETER,
                "duplicate extension",
            ));
        }
        out.push((kind, body));
    }
    Ok(out)
}

/// Waiting for ServerHello (or HelloRetryRequest).
struct HelloState {
    /// Transcript bytes so far: ClientHello, or message_hash, HelloRetryRequest
    /// and the second ClientHello.
    transcript: Vec<u8>,
    random: [u8; 32],
    session_id: Vec<u8>,
    offered_suites: Vec<CipherSuite>,
    shares: Vec<KeyShare>,
    hrr_suite: Option<CipherSuite>,
    /// The ClientHello came from outside; it cannot be rebuilt for a retry.
    fixed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    EncryptedExtensions,
    CertificateOrRequest,
    Certificate,
    CertificateVerify,
    Finished,
}

/// Between ServerHello and the server Finished.
struct HandshakeState {
    suite: CipherSuite,
    transcript: Hasher,
    handshake_secret: Vec<u8>,
    client_secret: Vec<u8>,
    server_secret: Vec<u8>,
    step: Step,
    cert_request_context: Option<Vec<u8>>,
    leaf_key: Option<PublicKey>,
}

/// After the handshake.
struct Traffic {
    suite: CipherSuite,
    client_secret: Vec<u8>,
    server_secret: Vec<u8>,
}

enum State {
    Hello(Box<HelloState>),
    Handshake(Box<HandshakeState>),
    Connected(Box<Traffic>),
    Failed,
}

pub struct ClientConnection {
    config: Arc<ClientConfig>,
    server_name: String,
    state: State,
    incoming: Vec<u8>,
    hs_buf: Vec<u8>,
    read_cipher: Option<RecordCipher>,
    write_cipher: Option<RecordCipher>,
    outgoing: Vec<u8>,
    plaintext: Vec<u8>,
    plaintext_pos: usize,
    peer_closed: bool,
    sent_close: bool,
    error: Option<TlsError>,
    compat: bool,
    sent_ccs: bool,
    suite: Option<CipherSuite>,
    alpn: Option<Vec<u8>>,
}

fn internal(_: std::io::Error) -> TlsError {
    TlsError::protocol(alert::INTERNAL_ERROR, "random number generator failed")
}

impl ClientConnection {
    /// Start a handshake with `server_name`; the ClientHello is queued.
    pub fn new(config: Arc<ClientConfig>, server_name: &str) -> Result<ClientConnection, TlsError> {
        let random = random::bytes::<32>().map_err(internal)?;
        let session_id = if config.middlebox_compat {
            random::bytes::<32>().map_err(internal)?.to_vec()
        } else {
            Vec::new()
        };
        let group = *config.groups.first().ok_or(TlsError::protocol(
            alert::INTERNAL_ERROR,
            "no groups configured",
        ))?;
        let shares = vec![KeyShare::generate(group).map_err(internal)?];
        let ch = build_client_hello(&config, server_name, &random, &session_id, &shares, None);
        let suites = config.cipher_suites.clone();
        Ok(ClientConnection::start(
            config,
            server_name,
            ch,
            random,
            session_id,
            suites,
            shares,
            false,
        ))
    }

    /// Start with a ClientHello built elsewhere and the private keys of its
    /// shares, to reproduce published traces such as RFC 8448. A
    /// HelloRetryRequest cannot be answered on such a connection.
    pub fn from_client_hello(
        config: Arc<ClientConfig>,
        server_name: &str,
        client_hello: &[u8],
        shares: Vec<KeyShare>,
    ) -> Result<ClientConnection, TlsError> {
        let mut c = Cursor::new(client_hello);
        if c.u8()? != hs::CLIENT_HELLO || c.u24()? != client_hello.len() - 4 {
            return Err(TlsError::protocol(alert::DECODE_ERROR, "not a ClientHello"));
        }
        c.u16()?;
        let random: [u8; 32] = c.take(32)?.try_into().unwrap();
        let session_id = c.vec8()?.to_vec();
        let mut suites_c = Cursor::new(c.vec16()?);
        let mut suites = Vec::new();
        while !suites_c.is_empty() {
            if let Some(s) = CipherSuite::from_id(suites_c.u16()?) {
                suites.push(s);
            }
        }
        Ok(ClientConnection::start(
            config,
            server_name,
            client_hello.to_vec(),
            random,
            session_id,
            suites,
            shares,
            true,
        ))
    }

    #[allow(clippy::too_many_arguments)]
    fn start(
        config: Arc<ClientConfig>,
        server_name: &str,
        client_hello: Vec<u8>,
        random: [u8; 32],
        session_id: Vec<u8>,
        offered_suites: Vec<CipherSuite>,
        shares: Vec<KeyShare>,
        fixed: bool,
    ) -> ClientConnection {
        let mut conn = ClientConnection {
            config,
            server_name: server_name.to_string(),
            state: State::Failed,
            incoming: Vec::new(),
            hs_buf: Vec::new(),
            read_cipher: None,
            write_cipher: None,
            outgoing: Vec::new(),
            plaintext: Vec::new(),
            plaintext_pos: 0,
            peer_closed: false,
            sent_close: false,
            error: None,
            compat: !session_id.is_empty(),
            sent_ccs: false,
            suite: None,
            alpn: None,
        };
        // The first ClientHello may use the legacy record version 0x0301.
        conn.send_plain(content::HANDSHAKE, &client_hello, 0x0301);
        conn.state = State::Hello(Box::new(HelloState {
            transcript: client_hello,
            random,
            session_id,
            offered_suites,
            shares,
            hrr_suite: None,
            fixed,
        }));
        conn
    }

    pub fn is_handshaking(&self) -> bool {
        matches!(self.state, State::Hello(_) | State::Handshake(_))
    }

    pub fn is_connected(&self) -> bool {
        matches!(self.state, State::Connected(_))
    }

    /// The peer sent close_notify.
    pub fn peer_closed(&self) -> bool {
        self.peer_closed
    }

    pub fn error(&self) -> Option<&TlsError> {
        self.error.as_ref()
    }

    pub fn cipher_suite(&self) -> Option<CipherSuite> {
        self.suite
    }

    /// The ALPN protocol the server selected, if any.
    pub fn alpn_protocol(&self) -> Option<&[u8]> {
        self.alpn.as_deref()
    }

    /// Bytes to send to the server.
    pub fn take_outgoing(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.outgoing)
    }

    /// Copy received application data into `buf`.
    pub fn read_plaintext(&mut self, buf: &mut [u8]) -> usize {
        let avail = &self.plaintext[self.plaintext_pos..];
        let n = avail.len().min(buf.len());
        buf[..n].copy_from_slice(&avail[..n]);
        self.plaintext_pos += n;
        if self.plaintext_pos == self.plaintext.len() {
            self.plaintext.clear();
            self.plaintext_pos = 0;
        }
        n
    }

    /// Queue application data.
    pub fn write_plaintext(&mut self, data: &[u8]) -> Result<(), TlsError> {
        if let Some(e) = &self.error {
            return Err(e.clone());
        }
        if !self.is_connected() || self.sent_close {
            return Err(TlsError::protocol(
                alert::INTERNAL_ERROR,
                "connection not open for writing",
            ));
        }
        for chunk in data.chunks(MAX_FRAGMENT) {
            self.send_encrypted(content::APPLICATION_DATA, chunk);
        }
        Ok(())
    }

    /// Queue a close_notify alert.
    pub fn send_close_notify(&mut self) {
        if !self.sent_close && self.error.is_none() {
            self.sent_close = true;
            self.send_alert(1, alert::CLOSE_NOTIFY);
        }
    }

    /// Send a KeyUpdate and switch to the next write key.
    pub fn send_key_update(&mut self, request_peer: bool) -> Result<(), TlsError> {
        if self.sent_close {
            return Err(TlsError::protocol(
                alert::INTERNAL_ERROR,
                "connection closed",
            ));
        }
        let State::Connected(t) = &mut self.state else {
            return Err(TlsError::protocol(
                alert::INTERNAL_ERROR,
                "handshake not complete",
            ));
        };
        let suite = t.suite;
        t.client_secret = next_traffic_secret(suite.hash(), &t.client_secret);
        let secret = t.client_secret.clone();
        let msg = handshake_message(hs::KEY_UPDATE, &[u8::from(request_peer)]);
        self.send_handshake(&msg);
        self.write_cipher = Some(RecordCipher::new(suite, &secret));
        Ok(())
    }

    /// Process bytes received from the server.
    pub fn feed(&mut self, data: &[u8]) -> Result<(), TlsError> {
        if let Some(e) = &self.error {
            return Err(e.clone());
        }
        self.incoming.extend_from_slice(data);
        let result = self.process();
        if let Err(e) = &result {
            self.fail(e.clone());
        }
        result
    }

    fn fail(&mut self, e: TlsError) {
        if !matches!(e, TlsError::PeerAlert(_)) && !self.sent_close {
            self.send_alert(2, e.alert());
        }
        self.sent_close = true;
        self.state = State::Failed;
        self.error = Some(e);
    }

    fn process(&mut self) -> Result<(), TlsError> {
        while self.incoming.len() >= 5 {
            let len = u16::from_be_bytes([self.incoming[3], self.incoming[4]]) as usize;
            if len > MAX_FRAGMENT + 256 {
                return Err(TlsError::protocol(
                    alert::RECORD_OVERFLOW,
                    "record too long",
                ));
            }
            if self.incoming.len() < 5 + len {
                break;
            }
            let record: Vec<u8> = self.incoming.drain(..5 + len).collect();
            let header: [u8; 5] = record[..5].try_into().unwrap();
            self.handle_record(header, &record[5..])?;
        }
        Ok(())
    }

    fn handle_record(&mut self, header: [u8; 5], body: &[u8]) -> Result<(), TlsError> {
        let unexpected = |reason| Err(TlsError::protocol(alert::UNEXPECTED_MESSAGE, reason));
        let (kind, data) = match header[0] {
            content::CHANGE_CIPHER_SPEC => {
                // Middlebox compatibility: ignored during the handshake.
                if self.is_handshaking() && body == [1] {
                    return Ok(());
                }
                return unexpected("unexpected change_cipher_spec");
            }
            content::APPLICATION_DATA if self.read_cipher.is_some() => {
                let cipher = self.read_cipher.as_mut().unwrap();
                let (kind, data) = cipher.open(&header, body).ok_or(TlsError::protocol(
                    alert::BAD_RECORD_MAC,
                    "record authentication failed",
                ))?;
                if data.len() > MAX_FRAGMENT {
                    return Err(TlsError::protocol(
                        alert::RECORD_OVERFLOW,
                        "plaintext too long",
                    ));
                }
                if kind == 0 {
                    return unexpected("record without content type");
                }
                (kind, data)
            }
            content::ALERT | content::HANDSHAKE if self.read_cipher.is_none() => {
                if body.len() > MAX_FRAGMENT {
                    return Err(TlsError::protocol(
                        alert::RECORD_OVERFLOW,
                        "plaintext too long",
                    ));
                }
                (header[0], body.to_vec())
            }
            _ => return unexpected("unexpected record type"),
        };
        if kind != content::HANDSHAKE && !self.hs_buf.is_empty() {
            return unexpected("handshake message interleaved with other records");
        }
        match kind {
            content::HANDSHAKE => {
                if data.is_empty() {
                    return unexpected("empty handshake record");
                }
                self.hs_buf.extend_from_slice(&data);
                self.drain_handshake()
            }
            content::ALERT => self.handle_alert(&data),
            content::APPLICATION_DATA => {
                if !self.is_connected() {
                    return unexpected("application data before the handshake ended");
                }
                if !self.peer_closed {
                    self.plaintext.extend_from_slice(&data);
                }
                Ok(())
            }
            _ => unexpected("unknown content type"),
        }
    }

    fn handle_alert(&mut self, data: &[u8]) -> Result<(), TlsError> {
        if data.len() != 2 {
            return Err(TlsError::protocol(alert::DECODE_ERROR, "malformed alert"));
        }
        match data[1] {
            alert::CLOSE_NOTIFY => {
                self.peer_closed = true;
                Ok(())
            }
            alert::USER_CANCELED => Ok(()),
            other => Err(TlsError::PeerAlert(other)),
        }
    }

    fn drain_handshake(&mut self) -> Result<(), TlsError> {
        while self.hs_buf.len() >= 4 {
            let len = ((self.hs_buf[1] as usize) << 16)
                | ((self.hs_buf[2] as usize) << 8)
                | self.hs_buf[3] as usize;
            if len > MAX_HANDSHAKE_MESSAGE {
                return Err(TlsError::protocol(
                    alert::DECODE_ERROR,
                    "handshake message too long",
                ));
            }
            if self.hs_buf.len() < 4 + len {
                break;
            }
            let msg: Vec<u8> = self.hs_buf.drain(..4 + len).collect();
            let state = std::mem::replace(&mut self.state, State::Failed);
            let (next, key_change) = match state {
                State::Hello(h) => self.on_server_hello(h, &msg)?,
                State::Handshake(h) => self.on_handshake(h, &msg)?,
                State::Connected(t) => self.on_post_handshake(t, &msg)?,
                State::Failed => {
                    return Err(TlsError::protocol(
                        alert::INTERNAL_ERROR,
                        "connection failed",
                    ));
                }
            };
            self.state = next;
            // Handshake messages may not span a key change (RFC 8446 §5.1).
            if key_change && !self.hs_buf.is_empty() {
                return Err(TlsError::protocol(
                    alert::UNEXPECTED_MESSAGE,
                    "handshake data across a key change",
                ));
            }
        }
        Ok(())
    }

    fn on_server_hello(
        &mut self,
        h: Box<HelloState>,
        msg: &[u8],
    ) -> Result<(State, bool), TlsError> {
        if msg[0] != hs::SERVER_HELLO {
            return Err(TlsError::protocol(
                alert::UNEXPECTED_MESSAGE,
                "expected ServerHello",
            ));
        }
        let illegal = |reason| TlsError::protocol(alert::ILLEGAL_PARAMETER, reason);
        let mut c = Cursor::new(&msg[4..]);
        let legacy_version = c.u16()?;
        let random = c.take(32)?;
        let session_id = c.vec8()?;
        let suite_id = c.u16()?;
        let compression = c.u8()?;
        let exts = if c.is_empty() { &[][..] } else { c.vec16()? };
        if !c.is_empty() {
            return Err(TlsError::protocol(
                alert::DECODE_ERROR,
                "trailing data in ServerHello",
            ));
        }
        let exts = parse_extensions(exts)?;
        let version = exts.iter().find(|(k, _)| *k == ext::SUPPORTED_VERSIONS);
        if version.map(|(_, d)| *d) != Some(&[0x03, 0x04][..]) {
            return Err(TlsError::protocol(
                alert::PROTOCOL_VERSION,
                "server does not speak TLS 1.3",
            ));
        }
        if legacy_version != 0x0303 {
            return Err(illegal("bad ServerHello legacy_version"));
        }
        if session_id != h.session_id.as_slice() {
            return Err(illegal("session id not echoed"));
        }
        if compression != 0 {
            return Err(illegal("compression selected"));
        }
        let suite = CipherSuite::from_id(suite_id)
            .filter(|s| h.offered_suites.contains(s))
            .ok_or(illegal("cipher suite not offered"))?;
        if random == HRR_RANDOM {
            return self.on_hello_retry(h, suite, &exts, msg);
        }
        if h.hrr_suite.is_some_and(|s| s != suite) {
            return Err(illegal("cipher suite changed after HelloRetryRequest"));
        }
        let mut shared = None;
        for (kind, data) in &exts {
            match *kind {
                ext::SUPPORTED_VERSIONS => {}
                ext::KEY_SHARE => {
                    let mut k = Cursor::new(data);
                    let group = k.u16()?;
                    let key = k.vec16()?;
                    if !k.is_empty() {
                        return Err(TlsError::protocol(alert::DECODE_ERROR, "bad key_share"));
                    }
                    let share = h
                        .shares
                        .iter()
                        .find(|s| s.group().id() == group)
                        .ok_or(illegal("key share for a group not offered"))?;
                    shared = Some(
                        share
                            .agree(key)
                            .ok_or(illegal("invalid server key share"))?,
                    );
                }
                ext::PRE_SHARED_KEY => return Err(illegal("pre_shared_key not offered")),
                _ => {
                    return Err(TlsError::protocol(
                        alert::UNSUPPORTED_EXTENSION,
                        "unexpected ServerHello extension",
                    ));
                }
            }
        }
        let shared = shared.ok_or(TlsError::protocol(
            alert::MISSING_EXTENSION,
            "no key_share in ServerHello",
        ))?;

        let alg = suite.hash();
        let mut transcript = alg.hasher();
        transcript.update(&h.transcript);
        transcript.update(msg);
        let th = transcript.current();
        let hs_secret = handshake_secret(alg, &early_secret(alg), &shared);
        let client_secret = derive_secret(alg, &hs_secret, b"c hs traffic", &th);
        let server_secret = derive_secret(alg, &hs_secret, b"s hs traffic", &th);
        self.read_cipher = Some(RecordCipher::new(suite, &server_secret));
        // From here on the client's records (Finished, alerts) use its handshake key.
        self.write_cipher = Some(RecordCipher::new(suite, &client_secret));
        self.suite = Some(suite);
        Ok((
            State::Handshake(Box::new(HandshakeState {
                suite,
                transcript,
                handshake_secret: hs_secret,
                client_secret,
                server_secret,
                step: Step::EncryptedExtensions,
                cert_request_context: None,
                leaf_key: None,
            })),
            true,
        ))
    }

    fn on_hello_retry(
        &mut self,
        h: Box<HelloState>,
        suite: CipherSuite,
        exts: &[(u16, &[u8])],
        msg: &[u8],
    ) -> Result<(State, bool), TlsError> {
        let illegal = |reason| TlsError::protocol(alert::ILLEGAL_PARAMETER, reason);
        if h.hrr_suite.is_some() || h.fixed {
            return Err(TlsError::protocol(
                alert::UNEXPECTED_MESSAGE,
                "unexpected HelloRetryRequest",
            ));
        }
        let mut group = None;
        let mut cookie = None;
        for (kind, data) in exts {
            match *kind {
                ext::SUPPORTED_VERSIONS => {}
                ext::KEY_SHARE => {
                    let mut k = Cursor::new(data);
                    let id = k.u16()?;
                    if !k.is_empty() {
                        return Err(TlsError::protocol(alert::DECODE_ERROR, "bad key_share"));
                    }
                    let g = NamedGroup::from_id(id)
                        .filter(|g| self.config.groups.contains(g))
                        .ok_or(illegal("HelloRetryRequest for a group not offered"))?;
                    if h.shares.iter().any(|s| s.group() == g) {
                        return Err(illegal("HelloRetryRequest for a group already shared"));
                    }
                    group = Some(g);
                }
                ext::COOKIE => {
                    let mut k = Cursor::new(data);
                    let value = k.vec16()?;
                    if value.is_empty() || !k.is_empty() {
                        return Err(TlsError::protocol(alert::DECODE_ERROR, "bad cookie"));
                    }
                    cookie = Some(value.to_vec());
                }
                _ => {
                    return Err(TlsError::protocol(
                        alert::UNSUPPORTED_EXTENSION,
                        "unexpected HelloRetryRequest extension",
                    ));
                }
            }
        }
        if group.is_none() && cookie.is_none() {
            return Err(illegal("HelloRetryRequest changes nothing"));
        }
        let shares = match group {
            Some(g) => vec![KeyShare::generate(g).map_err(internal)?],
            None => h.shares.clone(),
        };
        // The first ClientHello is replaced by message_hash (RFC 8446 §4.4.1).
        let alg = suite.hash();
        let mut transcript = vec![hs::MESSAGE_HASH, 0, 0, alg.output_len() as u8];
        transcript.extend(alg.digest(&h.transcript));
        transcript.extend_from_slice(msg);
        let ch2 = build_client_hello(
            &self.config,
            &self.server_name,
            &h.random,
            &h.session_id,
            &shares,
            cookie.as_deref(),
        );
        transcript.extend_from_slice(&ch2);
        self.send_ccs_once();
        self.send_plain(content::HANDSHAKE, &ch2, 0x0303);
        let HelloState {
            random,
            session_id,
            offered_suites,
            ..
        } = *h;
        Ok((
            State::Hello(Box::new(HelloState {
                transcript,
                random,
                session_id,
                offered_suites,
                shares,
                hrr_suite: Some(suite),
                fixed: false,
            })),
            false,
        ))
    }

    fn on_handshake(
        &mut self,
        mut h: Box<HandshakeState>,
        msg: &[u8],
    ) -> Result<(State, bool), TlsError> {
        let kind = msg[0];
        let body = &msg[4..];
        let alg = h.suite.hash();
        match (h.step, kind) {
            (Step::EncryptedExtensions, hs::ENCRYPTED_EXTENSIONS) => {
                let mut c = Cursor::new(body);
                let exts = c.vec16()?;
                if !c.is_empty() {
                    return Err(TlsError::protocol(
                        alert::DECODE_ERROR,
                        "bad EncryptedExtensions",
                    ));
                }
                for (kind, data) in parse_extensions(exts)? {
                    if kind == ext::ALPN {
                        let mut a = Cursor::new(data);
                        let mut list = Cursor::new(a.vec16()?);
                        let proto = list.vec8()?;
                        if !a.is_empty() || !list.is_empty() || proto.is_empty() {
                            return Err(TlsError::protocol(alert::DECODE_ERROR, "bad ALPN"));
                        }
                        if !self.config.alpn.iter().any(|p| p == proto) {
                            return Err(TlsError::protocol(
                                alert::NO_APPLICATION_PROTOCOL,
                                "server chose an ALPN protocol not offered",
                            ));
                        }
                        self.alpn = Some(proto.to_vec());
                    }
                }
                h.transcript.update(msg);
                h.step = Step::CertificateOrRequest;
            }
            (Step::CertificateOrRequest, hs::CERTIFICATE_REQUEST) => {
                let mut c = Cursor::new(body);
                let context = c.vec8()?.to_vec();
                c.vec16()?;
                if !c.is_empty() {
                    return Err(TlsError::protocol(
                        alert::DECODE_ERROR,
                        "bad CertificateRequest",
                    ));
                }
                h.cert_request_context = Some(context);
                h.transcript.update(msg);
                h.step = Step::Certificate;
            }
            (Step::CertificateOrRequest | Step::Certificate, hs::CERTIFICATE) => {
                let mut c = Cursor::new(body);
                let context = c.vec8()?;
                let mut list = Cursor::new(c.vec24()?);
                if !c.is_empty() {
                    return Err(TlsError::protocol(alert::DECODE_ERROR, "bad Certificate"));
                }
                if !context.is_empty() {
                    return Err(TlsError::protocol(
                        alert::ILLEGAL_PARAMETER,
                        "server certificate with a request context",
                    ));
                }
                let mut chain = Vec::new();
                while !list.is_empty() {
                    chain.push(list.vec24()?.to_vec());
                    list.vec16()?;
                }
                if chain.is_empty() {
                    return Err(TlsError::Certificate(CertError::Empty));
                }
                self.config
                    .verifier
                    .verify(&chain, &self.server_name)
                    .map_err(TlsError::Certificate)?;
                let leaf = Certificate::parse(&chain[0]).map_err(TlsError::Certificate)?;
                h.leaf_key = Some(leaf.public_key);
                h.transcript.update(msg);
                h.step = Step::CertificateVerify;
            }
            (Step::CertificateVerify, hs::CERTIFICATE_VERIFY) => {
                let mut c = Cursor::new(body);
                let scheme = c.u16()?;
                let sig = c.vec16()?;
                if !c.is_empty() {
                    return Err(TlsError::protocol(
                        alert::DECODE_ERROR,
                        "bad CertificateVerify",
                    ));
                }
                let mut signed = vec![0x20u8; 64];
                signed.extend_from_slice(b"TLS 1.3, server CertificateVerify");
                signed.push(0);
                signed.extend(h.transcript.current());
                let key = h
                    .leaf_key
                    .as_ref()
                    .expect("certificate before CertificateVerify");
                verify_certificate_verify(scheme, key, &signed, sig)?;
                h.transcript.update(msg);
                h.step = Step::Finished;
            }
            (Step::Finished, hs::FINISHED) => {
                let expected = finished_verify_data(alg, &h.server_secret, &h.transcript.current());
                if !ct_eq(body, &expected) {
                    return Err(TlsError::protocol(
                        alert::DECRYPT_ERROR,
                        "server Finished is wrong",
                    ));
                }
                h.transcript.update(msg);
                let th = h.transcript.current();
                let master = master_secret(alg, &h.handshake_secret);
                let client_app = derive_secret(alg, &master, b"c ap traffic", &th);
                let server_app = derive_secret(alg, &master, b"s ap traffic", &th);

                self.send_ccs_once();
                if let Some(context) = h.cert_request_context.take() {
                    // No client certificate: an empty Certificate message.
                    let mut b = Vec::new();
                    put_vec8(&mut b, &context);
                    put_vec24(&mut b, &[]);
                    let m = handshake_message(hs::CERTIFICATE, &b);
                    h.transcript.update(&m);
                    self.send_handshake(&m);
                }
                let verify = finished_verify_data(alg, &h.client_secret, &h.transcript.current());
                let fin = handshake_message(hs::FINISHED, &verify);
                self.send_handshake(&fin);
                self.read_cipher = Some(RecordCipher::new(h.suite, &server_app));
                self.write_cipher = Some(RecordCipher::new(h.suite, &client_app));
                return Ok((
                    State::Connected(Box::new(Traffic {
                        suite: h.suite,
                        client_secret: client_app,
                        server_secret: server_app,
                    })),
                    true,
                ));
            }
            _ => {
                return Err(TlsError::protocol(
                    alert::UNEXPECTED_MESSAGE,
                    "unexpected handshake message",
                ));
            }
        }
        Ok((State::Handshake(h), false))
    }

    fn on_post_handshake(
        &mut self,
        mut t: Box<Traffic>,
        msg: &[u8],
    ) -> Result<(State, bool), TlsError> {
        let body = &msg[4..];
        match msg[0] {
            // Resumption is not implemented; tickets are dropped.
            hs::NEW_SESSION_TICKET => Ok((State::Connected(t), false)),
            hs::KEY_UPDATE => {
                let request = match body {
                    [0] => false,
                    [1] => true,
                    [_] => {
                        return Err(TlsError::protocol(
                            alert::ILLEGAL_PARAMETER,
                            "bad KeyUpdate",
                        ));
                    }
                    _ => return Err(TlsError::protocol(alert::DECODE_ERROR, "bad KeyUpdate")),
                };
                let alg = t.suite.hash();
                t.server_secret = next_traffic_secret(alg, &t.server_secret);
                self.read_cipher = Some(RecordCipher::new(t.suite, &t.server_secret));
                if request && !self.sent_close {
                    let m = handshake_message(hs::KEY_UPDATE, &[0]);
                    self.send_handshake(&m);
                    t.client_secret = next_traffic_secret(alg, &t.client_secret);
                    self.write_cipher = Some(RecordCipher::new(t.suite, &t.client_secret));
                }
                Ok((State::Connected(t), true))
            }
            _ => Err(TlsError::protocol(
                alert::UNEXPECTED_MESSAGE,
                "unexpected post-handshake message",
            )),
        }
    }

    fn send_ccs_once(&mut self) {
        if self.compat && !self.sent_ccs {
            self.sent_ccs = true;
            self.send_plain(content::CHANGE_CIPHER_SPEC, &[1], 0x0303);
        }
    }

    fn send_plain(&mut self, kind: u8, data: &[u8], version: u16) {
        for chunk in data.chunks(MAX_FRAGMENT) {
            self.outgoing.push(kind);
            put_u16(&mut self.outgoing, version);
            put_vec16(&mut self.outgoing, chunk);
        }
    }

    fn send_encrypted(&mut self, kind: u8, data: &[u8]) {
        let cipher = self.write_cipher.as_mut().expect("write keys");
        if data.is_empty() {
            self.outgoing.extend(cipher.seal(kind, data));
        }
        for chunk in data.chunks(MAX_FRAGMENT) {
            self.outgoing.extend(cipher.seal(kind, chunk));
        }
    }

    fn send_handshake(&mut self, msg: &[u8]) {
        if self.write_cipher.is_some() {
            self.send_encrypted(content::HANDSHAKE, msg);
        } else {
            self.send_plain(content::HANDSHAKE, msg, 0x0303);
        }
    }

    fn send_alert(&mut self, level: u8, description: u8) {
        if self.write_cipher.is_some() {
            self.send_encrypted(content::ALERT, &[level, description]);
        } else {
            self.send_plain(content::ALERT, &[level, description], 0x0303);
        }
    }
}

/// Check the server's CertificateVerify signature over `signed`.
fn verify_certificate_verify(
    id: u16,
    key: &PublicKey,
    signed: &[u8],
    sig: &[u8],
) -> Result<(), TlsError> {
    let ok = match (id, key) {
        (scheme::RSA_PSS_RSAE_SHA256, PublicKey::Rsa(k)) => {
            k.verify_pss(HashAlg::Sha256, signed, sig)
        }
        (scheme::RSA_PSS_RSAE_SHA384, PublicKey::Rsa(k)) => {
            k.verify_pss(HashAlg::Sha384, signed, sig)
        }
        (scheme::RSA_PSS_RSAE_SHA512, PublicKey::Rsa(k)) => {
            k.verify_pss(HashAlg::Sha512, signed, sig)
        }
        (scheme::ECDSA_SECP256R1_SHA256, PublicKey::EcP256(q)) => {
            p256::verify_der(q, &HashAlg::Sha256.digest(signed), sig)
        }
        (
            scheme::RSA_PSS_RSAE_SHA256
            | scheme::RSA_PSS_RSAE_SHA384
            | scheme::RSA_PSS_RSAE_SHA512
            | scheme::ECDSA_SECP256R1_SHA256,
            _,
        ) => {
            return Err(TlsError::protocol(
                alert::ILLEGAL_PARAMETER,
                "signature scheme does not match the certificate key",
            ));
        }
        _ => {
            return Err(TlsError::protocol(
                alert::ILLEGAL_PARAMETER,
                "signature scheme not offered",
            ));
        }
    };
    if ok {
        Ok(())
    } else {
        Err(TlsError::protocol(
            alert::DECRYPT_ERROR,
            "bad CertificateVerify signature",
        ))
    }
}
