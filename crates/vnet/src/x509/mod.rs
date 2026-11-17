//! X.509 certificates (RFC 5280) in DER: parsing, chain validation against
//! trust anchors, validity dates, `basicConstraints`, key usage, and server
//! name matching with `subjectAltName` (RFC 6125, wildcards in the leftmost
//! label only).

pub mod der;

use crate::crypto::HashAlg;
use crate::crypto::p256;
use crate::crypto::rsa::RsaPublicKey;
use der::{DerError, Reader, tag};
use std::fmt;
use std::io;
use std::net::IpAddr;
use std::path::Path;

/// The system bundle of trusted roots on Arch Linux.
pub const SYSTEM_BUNDLE: &str = "/etc/ssl/certs/ca-certificates.crt";

/// Longest chain accepted, counting the leaf and the anchor.
const MAX_CHAIN: usize = 10;

mod oid {
    pub const RSA_ENCRYPTION: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x01];
    pub const SHA256_WITH_RSA: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x0b];
    pub const SHA384_WITH_RSA: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x0c];
    pub const SHA512_WITH_RSA: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x0d];
    pub const EC_PUBLIC_KEY: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01];
    pub const PRIME256V1: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07];
    pub const ECDSA_SHA256: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x04, 0x03, 0x02];
    pub const ECDSA_SHA384: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x04, 0x03, 0x03];
    pub const ECDSA_SHA512: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x04, 0x03, 0x04];
    pub const COMMON_NAME: &[u8] = &[0x55, 0x04, 0x03];
    pub const KEY_USAGE: &[u8] = &[0x55, 0x1d, 0x0f];
    pub const SUBJECT_ALT_NAME: &[u8] = &[0x55, 0x1d, 0x11];
    pub const BASIC_CONSTRAINTS: &[u8] = &[0x55, 0x1d, 0x13];
    pub const NAME_CONSTRAINTS: &[u8] = &[0x55, 0x1d, 0x1e];
    pub const CERTIFICATE_POLICIES: &[u8] = &[0x55, 0x1d, 0x20];
    pub const POLICY_CONSTRAINTS: &[u8] = &[0x55, 0x1d, 0x24];
    pub const EXT_KEY_USAGE: &[u8] = &[0x55, 0x1d, 0x25];
    pub const INHIBIT_ANY_POLICY: &[u8] = &[0x55, 0x1d, 0x36];
    pub const ANY_EXTENDED_KEY_USAGE: &[u8] = &[0x55, 0x1d, 0x25, 0x00];
    pub const SERVER_AUTH: &[u8] = &[0x2b, 0x06, 0x01, 0x05, 0x05, 0x07, 0x03, 0x01];
}

/// `keyCertSign` in the KeyUsage bit string (bit 5).
pub const KEY_USAGE_CERT_SIGN: u16 = 1 << 5;
/// `digitalSignature` (bit 0).
pub const KEY_USAGE_DIGITAL_SIGNATURE: u16 = 1 << 0;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CertError {
    /// The server sent no certificate.
    Empty,
    Malformed,
    Expired,
    NotYetValid,
    /// No trusted anchor issues the chain.
    UnknownIssuer,
    BadSignature,
    /// An issuer is not a CA, or a path length limit is exceeded.
    NotCa,
    /// An issuer may not sign certificates, or the leaf may not serve TLS.
    BadKeyUsage,
    UnsupportedAlgorithm,
    UnknownCriticalExtension,
    /// The certificate does not name the server.
    NameMismatch(String),
    ChainTooLong,
}

impl fmt::Display for CertError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CertError::Empty => write!(f, "no certificate"),
            CertError::Malformed => write!(f, "malformed certificate"),
            CertError::Expired => write!(f, "certificate expired"),
            CertError::NotYetValid => write!(f, "certificate not yet valid"),
            CertError::UnknownIssuer => write!(f, "unknown certificate issuer"),
            CertError::BadSignature => write!(f, "bad certificate signature"),
            CertError::NotCa => write!(f, "issuer is not a certificate authority"),
            CertError::BadKeyUsage => write!(f, "key usage does not allow this use"),
            CertError::UnsupportedAlgorithm => write!(f, "unsupported certificate algorithm"),
            CertError::UnknownCriticalExtension => write!(f, "unknown critical extension"),
            CertError::NameMismatch(n) => write!(f, "certificate is not valid for {n}"),
            CertError::ChainTooLong => write!(f, "certificate chain too long"),
        }
    }
}

impl std::error::Error for CertError {}

impl From<DerError> for CertError {
    fn from(_: DerError) -> Self {
        CertError::Malformed
    }
}

/// A subject public key.
#[derive(Clone, Debug)]
pub enum PublicKey {
    Rsa(RsaPublicKey),
    /// Uncompressed P-256 point.
    EcP256(Vec<u8>),
    /// Any other algorithm or curve; kept so that such roots still load.
    Unsupported,
}

/// How a certificate is signed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignatureAlgorithm {
    RsaPkcs1(HashAlg),
    Ecdsa(HashAlg),
    Unsupported,
}

impl PublicKey {
    /// Verify `sig` over `msg` with a certificate signature algorithm.
    pub fn verify(&self, alg: SignatureAlgorithm, msg: &[u8], sig: &[u8]) -> Result<(), CertError> {
        let ok = match (self, alg) {
            (PublicKey::Rsa(k), SignatureAlgorithm::RsaPkcs1(h)) => k.verify_pkcs1(h, msg, sig),
            (PublicKey::EcP256(q), SignatureAlgorithm::Ecdsa(h)) => {
                p256::verify_der(q, &h.digest(msg), sig)
            }
            (PublicKey::Unsupported, _) | (_, SignatureAlgorithm::Unsupported) => {
                return Err(CertError::UnsupportedAlgorithm);
            }
            _ => false,
        };
        if ok {
            Ok(())
        } else {
            Err(CertError::BadSignature)
        }
    }
}

#[derive(Clone, Debug)]
pub struct Certificate {
    pub der: Vec<u8>,
    tbs: (usize, usize),
    pub version: u8,
    pub serial: Vec<u8>,
    /// Raw DER of the issuer and subject `Name`s, compared byte for byte.
    pub issuer: Vec<u8>,
    pub subject: Vec<u8>,
    /// Seconds since the Unix epoch.
    pub not_before: i64,
    pub not_after: i64,
    pub public_key: PublicKey,
    pub signature_algorithm: SignatureAlgorithm,
    pub signature: Vec<u8>,
    /// `basicConstraints`: `Some((ca, path_len))` when present.
    pub basic_constraints: Option<(bool, Option<u32>)>,
    pub key_usage: Option<u16>,
    /// `extKeyUsage` OIDs when present.
    pub ext_key_usage: Option<Vec<Vec<u8>>>,
    pub has_subject_alt_name: bool,
    pub dns_names: Vec<String>,
    pub ip_addresses: Vec<IpAddr>,
    pub common_names: Vec<String>,
}

impl Certificate {
    pub fn parse(der: &[u8]) -> Result<Certificate, CertError> {
        let mut outer = Reader::new(der);
        let cert_tlv = outer.read()?;
        if cert_tlv.tag != tag::SEQUENCE || !outer.is_empty() {
            return Err(CertError::Malformed);
        }
        let mut cert = Reader::new(cert_tlv.value);
        let tbs_tlv = cert.read()?;
        if tbs_tlv.tag != tag::SEQUENCE {
            return Err(CertError::Malformed);
        }
        let tbs_start = tbs_tlv.raw.as_ptr() as usize - der.as_ptr() as usize;
        let tbs_range = (tbs_start, tbs_start + tbs_tlv.raw.len());
        let outer_alg = cert.expect(tag::SEQUENCE)?;
        let signature = cert.bit_string_bytes()?.to_vec();
        if !cert.is_empty() {
            return Err(CertError::Malformed);
        }

        let mut tbs = Reader::new(tbs_tlv.value);
        let version = match tbs.optional(tag::context(0))? {
            Some(v) => {
                let mut r = Reader::new(v);
                let n = r.small_integer()?;
                if n > 2 || !r.is_empty() {
                    return Err(CertError::Malformed);
                }
                n as u8 + 1
            }
            None => 1,
        };
        let serial = tbs.expect(tag::INTEGER)?.to_vec();
        let inner_alg = tbs.expect(tag::SEQUENCE)?;
        if inner_alg != outer_alg {
            return Err(CertError::Malformed);
        }
        let issuer = tbs.read()?;
        if issuer.tag != tag::SEQUENCE {
            return Err(CertError::Malformed);
        }
        let mut validity = Reader::new(tbs.expect(tag::SEQUENCE)?);
        let not_before = parse_time(&mut validity)?;
        let not_after = parse_time(&mut validity)?;
        let subject = tbs.read()?;
        if subject.tag != tag::SEQUENCE {
            return Err(CertError::Malformed);
        }
        let spki = tbs.expect(tag::SEQUENCE)?;
        let public_key = parse_spki(spki)?;
        tbs.optional(tag::context_primitive(1))?;
        tbs.optional(tag::context_primitive(2))?;
        let mut c = Certificate {
            der: der.to_vec(),
            tbs: tbs_range,
            version,
            serial,
            issuer: issuer.raw.to_vec(),
            subject: subject.raw.to_vec(),
            not_before,
            not_after,
            public_key,
            signature_algorithm: parse_signature_algorithm(outer_alg)?,
            signature,
            basic_constraints: None,
            key_usage: None,
            ext_key_usage: None,
            has_subject_alt_name: false,
            dns_names: Vec::new(),
            ip_addresses: Vec::new(),
            common_names: common_names(subject.value)?,
        };
        if let Some(exts) = tbs.optional(tag::context(3))? {
            if version != 3 {
                return Err(CertError::Malformed);
            }
            let mut r = Reader::new(exts);
            let list = r.expect(tag::SEQUENCE)?;
            if !r.is_empty() {
                return Err(CertError::Malformed);
            }
            c.parse_extensions(list)?;
        }
        if !tbs.is_empty() {
            return Err(CertError::Malformed);
        }
        Ok(c)
    }

    /// The signed part (`tbsCertificate`, with its header).
    pub fn tbs(&self) -> &[u8] {
        &self.der[self.tbs.0..self.tbs.1]
    }

    pub fn is_self_issued(&self) -> bool {
        self.issuer == self.subject
    }

    fn parse_extensions(&mut self, list: &[u8]) -> Result<(), CertError> {
        let mut r = Reader::new(list);
        let mut seen: Vec<&[u8]> = Vec::new();
        while !r.is_empty() {
            let mut ext = Reader::new(r.expect(tag::SEQUENCE)?);
            let id = ext.expect(tag::OID)?;
            let critical = if ext.peek_tag() == Some(tag::BOOLEAN) {
                ext.boolean()?
            } else {
                false
            };
            let value = ext.expect(tag::OCTET_STRING)?;
            if !ext.is_empty() || seen.contains(&id) {
                return Err(CertError::Malformed);
            }
            seen.push(id);
            match id {
                oid::BASIC_CONSTRAINTS => {
                    let mut v = Reader::new(value);
                    let mut s = Reader::new(v.expect(tag::SEQUENCE)?);
                    let ca = if s.peek_tag() == Some(tag::BOOLEAN) {
                        s.boolean()?
                    } else {
                        false
                    };
                    let path_len = if s.peek_tag() == Some(tag::INTEGER) {
                        Some(s.small_integer()?.min(u32::MAX as u64) as u32)
                    } else {
                        None
                    };
                    if !s.is_empty() || !v.is_empty() {
                        return Err(CertError::Malformed);
                    }
                    self.basic_constraints = Some((ca, path_len));
                }
                oid::KEY_USAGE => {
                    let mut v = Reader::new(value);
                    let bits = v.expect(tag::BIT_STRING)?;
                    let (unused, bytes) = bits.split_first().ok_or(CertError::Malformed)?;
                    if *unused > 7 || bytes.len() > 2 {
                        return Err(CertError::Malformed);
                    }
                    // Bit 0 is the most significant bit of the first byte.
                    let mut usage = 0u16;
                    for (i, &b) in bytes.iter().enumerate() {
                        for j in 0..8 {
                            if b & (0x80 >> j) != 0 {
                                usage |= 1 << (8 * i + j);
                            }
                        }
                    }
                    self.key_usage = Some(usage);
                }
                oid::EXT_KEY_USAGE => {
                    let mut v = Reader::new(value);
                    let mut s = Reader::new(v.expect(tag::SEQUENCE)?);
                    let mut list = Vec::new();
                    while !s.is_empty() {
                        list.push(s.expect(tag::OID)?.to_vec());
                    }
                    self.ext_key_usage = Some(list);
                }
                oid::SUBJECT_ALT_NAME => {
                    self.has_subject_alt_name = true;
                    let mut v = Reader::new(value);
                    let mut s = Reader::new(v.expect(tag::SEQUENCE)?);
                    while !s.is_empty() {
                        let name = s.read()?;
                        match name.tag {
                            // dNSName [2] IA5String
                            0x82 => {
                                let text = std::str::from_utf8(name.value)
                                    .map_err(|_| CertError::Malformed)?;
                                self.dns_names.push(text.to_ascii_lowercase());
                            }
                            // iPAddress [7] OCTET STRING
                            0x87 => match name.value.len() {
                                4 => {
                                    let o: [u8; 4] = name.value.try_into().unwrap();
                                    self.ip_addresses.push(IpAddr::from(o));
                                }
                                16 => {
                                    let o: [u8; 16] = name.value.try_into().unwrap();
                                    self.ip_addresses.push(IpAddr::from(o));
                                }
                                _ => return Err(CertError::Malformed),
                            },
                            _ => {}
                        }
                    }
                }
                oid::NAME_CONSTRAINTS
                | oid::CERTIFICATE_POLICIES
                | oid::POLICY_CONSTRAINTS
                | oid::INHIBIT_ANY_POLICY => {}
                _ if critical => return Err(CertError::UnknownCriticalExtension),
                _ => {}
            }
        }
        Ok(())
    }

    pub fn is_valid_at(&self, now: i64) -> Result<(), CertError> {
        if now < self.not_before {
            Err(CertError::NotYetValid)
        } else if now > self.not_after {
            Err(CertError::Expired)
        } else {
            Ok(())
        }
    }

    /// Does this certificate name `host` (a DNS name or an IP literal)?
    pub fn matches_name(&self, host: &str) -> bool {
        let host = host.trim_start_matches('[').trim_end_matches(']');
        if let Ok(ip) = host.parse::<IpAddr>() {
            return self.ip_addresses.contains(&ip);
        }
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        if self.has_subject_alt_name {
            return self.dns_names.iter().any(|p| dns_name_matches(p, &host));
        }
        // No subjectAltName: fall back to the common name (RFC 6125 §6.4.4).
        self.common_names.iter().any(|p| dns_name_matches(p, &host))
    }

    /// Check the signature of `child` with this certificate's key.
    pub fn verify_issued(&self, child: &Certificate) -> Result<(), CertError> {
        self.public_key
            .verify(child.signature_algorithm, child.tbs(), &child.signature)
    }
}

/// RFC 6125 matching: exact (case-insensitive), or `*.` followed by at least
/// two labels matching all of `host` but its single leftmost label.
pub fn dns_name_matches(pattern: &str, host: &str) -> bool {
    let pattern = pattern.trim_end_matches('.').to_ascii_lowercase();
    if pattern.is_empty() || host.is_empty() {
        return false;
    }
    match pattern.strip_prefix("*.") {
        Some(suffix) => {
            if !suffix.contains('.') || suffix.contains('*') {
                return false;
            }
            match host.split_once('.') {
                Some((label, rest)) => !label.is_empty() && rest == suffix,
                None => false,
            }
        }
        None => !pattern.contains('*') && pattern == host,
    }
}

fn parse_signature_algorithm(alg: &[u8]) -> Result<SignatureAlgorithm, CertError> {
    let mut r = Reader::new(alg);
    let id = r.expect(tag::OID)?;
    let algorithm = match id {
        oid::SHA256_WITH_RSA => SignatureAlgorithm::RsaPkcs1(HashAlg::Sha256),
        oid::SHA384_WITH_RSA => SignatureAlgorithm::RsaPkcs1(HashAlg::Sha384),
        oid::SHA512_WITH_RSA => SignatureAlgorithm::RsaPkcs1(HashAlg::Sha512),
        oid::ECDSA_SHA256 => SignatureAlgorithm::Ecdsa(HashAlg::Sha256),
        oid::ECDSA_SHA384 => SignatureAlgorithm::Ecdsa(HashAlg::Sha384),
        oid::ECDSA_SHA512 => SignatureAlgorithm::Ecdsa(HashAlg::Sha512),
        _ => return Ok(SignatureAlgorithm::Unsupported),
    };
    // RSA parameters are NULL or absent; ECDSA ones are absent.
    if let SignatureAlgorithm::RsaPkcs1(_) = algorithm {
        r.optional(tag::NULL)?;
    }
    if !r.is_empty() {
        return Err(CertError::Malformed);
    }
    Ok(algorithm)
}

fn parse_spki(spki: &[u8]) -> Result<PublicKey, CertError> {
    let mut r = Reader::new(spki);
    let mut alg = Reader::new(r.expect(tag::SEQUENCE)?);
    let key = r.bit_string_bytes()?;
    if !r.is_empty() {
        return Err(CertError::Malformed);
    }
    let id = alg.expect(tag::OID)?;
    match id {
        oid::RSA_ENCRYPTION => {
            let mut k = Reader::new(key);
            let mut seq = Reader::new(k.expect(tag::SEQUENCE)?);
            let n = seq.unsigned_integer()?;
            let e = seq.unsigned_integer()?;
            if !seq.is_empty() || !k.is_empty() {
                return Err(CertError::Malformed);
            }
            Ok(match RsaPublicKey::new(n, e) {
                Some(k) => PublicKey::Rsa(k),
                None => PublicKey::Unsupported,
            })
        }
        oid::EC_PUBLIC_KEY => {
            if alg.optional(tag::OID)? == Some(oid::PRIME256V1) && key.len() == 65 && key[0] == 4 {
                Ok(PublicKey::EcP256(key.to_vec()))
            } else {
                Ok(PublicKey::Unsupported)
            }
        }
        _ => Ok(PublicKey::Unsupported),
    }
}

fn common_names(name: &[u8]) -> Result<Vec<String>, CertError> {
    let mut out = Vec::new();
    let mut rdns = Reader::new(name);
    while !rdns.is_empty() {
        let mut set = Reader::new(rdns.expect(tag::SET)?);
        while !set.is_empty() {
            let mut atv = Reader::new(set.expect(tag::SEQUENCE)?);
            let id = atv.expect(tag::OID)?;
            let value = atv.read()?;
            if id == oid::COMMON_NAME
                && matches!(
                    value.tag,
                    tag::UTF8_STRING | tag::PRINTABLE_STRING | tag::IA5_STRING | tag::T61_STRING
                )
                && let Ok(s) = std::str::from_utf8(value.value)
            {
                out.push(s.to_ascii_lowercase());
            }
        }
    }
    Ok(out)
}

fn digits(b: &[u8]) -> Result<i64, CertError> {
    let mut v = 0i64;
    for &d in b {
        if !d.is_ascii_digit() {
            return Err(CertError::Malformed);
        }
        v = v * 10 + (d - b'0') as i64;
    }
    Ok(v)
}

/// UTCTime (`YYMMDDHHMMSSZ`) or GeneralizedTime (`YYYYMMDDHHMMSSZ`) to Unix seconds.
fn parse_time(r: &mut Reader<'_>) -> Result<i64, CertError> {
    let t = r.read()?;
    let (year, rest) = match (t.tag, t.value.len()) {
        (tag::UTC_TIME, 13) => {
            let yy = digits(&t.value[..2])?;
            (if yy < 50 { 2000 + yy } else { 1900 + yy }, &t.value[2..])
        }
        (tag::GENERALIZED_TIME, 15) => (digits(&t.value[..4])?, &t.value[4..]),
        _ => return Err(CertError::Malformed),
    };
    if rest[10] != b'Z' {
        return Err(CertError::Malformed);
    }
    let month = digits(&rest[0..2])?;
    let day = digits(&rest[2..4])?;
    let hour = digits(&rest[4..6])?;
    let min = digits(&rest[6..8])?;
    let sec = digits(&rest[8..10])?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || min > 59 || sec > 60 {
        return Err(CertError::Malformed);
    }
    Ok(days_from_civil(year, month, day) * 86400 + hour * 3600 + min * 60 + sec)
}

/// Days since 1970-01-01 of a proleptic Gregorian date.
pub fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// Decode standard Base64, skipping whitespace; `None` on any other error.
pub fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let mut acc = 0u32;
    let mut bits = 0;
    let mut padding = 0;
    for c in text.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => {
                padding += 1;
                continue;
            }
            b' ' | b'\t' | b'\r' | b'\n' => continue,
            _ => return None,
        };
        if padding > 0 {
            return None;
        }
        acc = (acc << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    if padding > 2 {
        return None;
    }
    Some(out)
}

/// DER bodies of every `CERTIFICATE` block in PEM text.
pub fn pem_certificates(text: &str) -> Vec<Vec<u8>> {
    const BEGIN: &str = "-----BEGIN CERTIFICATE-----";
    const END: &str = "-----END CERTIFICATE-----";
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find(BEGIN) {
        let body = &rest[start + BEGIN.len()..];
        let Some(end) = body.find(END) else {
            break;
        };
        if let Some(der) = base64_decode(&body[..end]) {
            out.push(der);
        }
        rest = &body[end + END.len()..];
    }
    out
}

/// Current time in Unix seconds.
pub fn now() -> i64 {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(_) => 0,
    }
}

/// Trusted root certificates.
#[derive(Clone, Debug, Default)]
pub struct TrustStore {
    roots: Vec<Certificate>,
}

impl TrustStore {
    pub fn new() -> TrustStore {
        TrustStore::default()
    }

    pub fn len(&self) -> usize {
        self.roots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.roots.is_empty()
    }

    pub fn add_der(&mut self, der: &[u8]) -> Result<(), CertError> {
        let cert = Certificate::parse(der)?;
        if !self.roots.iter().any(|r| r.der == cert.der) {
            self.roots.push(cert);
        }
        Ok(())
    }

    /// Add every certificate in PEM text; returns how many parsed.
    pub fn add_pem(&mut self, text: &str) -> usize {
        pem_certificates(text)
            .iter()
            .filter(|der| self.add_der(der).is_ok())
            .count()
    }

    /// Add a PEM file (or a single DER certificate).
    pub fn add_file(&mut self, path: &Path) -> io::Result<usize> {
        let bytes = std::fs::read(path)?;
        let added = match std::str::from_utf8(&bytes) {
            Ok(text) if text.contains("-----BEGIN") => self.add_pem(text),
            _ => usize::from(self.add_der(&bytes).is_ok()),
        };
        if added == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("no certificate in {}", path.display()),
            ));
        }
        Ok(added)
    }

    /// The roots in [`SYSTEM_BUNDLE`], or none if it cannot be read.
    pub fn system() -> TrustStore {
        let mut store = TrustStore::new();
        if let Ok(text) = std::fs::read_to_string(SYSTEM_BUNDLE) {
            store.add_pem(&text);
        }
        store
    }

    /// Validate `chain` (leaf first, then intermediates in any order) for
    /// `server_name` at Unix time `now`.
    pub fn verify_at(
        &self,
        chain: &[Vec<u8>],
        server_name: &str,
        now: i64,
    ) -> Result<(), CertError> {
        let leaf_der = chain.first().ok_or(CertError::Empty)?;
        let leaf = Certificate::parse(leaf_der)?;
        leaf.is_valid_at(now)?;
        if !leaf.matches_name(server_name) {
            return Err(CertError::NameMismatch(server_name.to_string()));
        }
        if let Some(eku) = &leaf.ext_key_usage
            && !eku
                .iter()
                .any(|o| o == oid::SERVER_AUTH || o == oid::ANY_EXTENDED_KEY_USAGE)
        {
            return Err(CertError::BadKeyUsage);
        }
        if let Some(ku) = leaf.key_usage
            && ku & KEY_USAGE_DIGITAL_SIGNATURE == 0
        {
            return Err(CertError::BadKeyUsage);
        }
        // A leaf that is itself a trusted certificate.
        if self.roots.iter().any(|r| r.der == leaf.der) {
            return Ok(());
        }
        let intermediates: Vec<Certificate> = chain[1..]
            .iter()
            .filter_map(|d| Certificate::parse(d).ok())
            .collect();
        self.build(&leaf, &intermediates, &mut Vec::new(), 0, now)
    }

    /// Find an issuer for `cert`: a root ends the path, an intermediate
    /// continues it. `below` counts the intermediates between that issuer
    /// and the leaf (`cert` included when it is not the leaf).
    fn build(
        &self,
        cert: &Certificate,
        intermediates: &[Certificate],
        used: &mut Vec<usize>,
        below: usize,
        now: i64,
    ) -> Result<(), CertError> {
        if used.len() + 2 > MAX_CHAIN {
            return Err(CertError::ChainTooLong);
        }
        let mut error = CertError::UnknownIssuer;
        for root in self.roots.iter().filter(|r| r.subject == cert.issuer) {
            match check_issuer(root, cert, below, now, true) {
                Ok(()) => return Ok(()),
                Err(e) => error = e,
            }
        }
        for (i, inter) in intermediates.iter().enumerate() {
            if inter.subject != cert.issuer || used.contains(&i) {
                continue;
            }
            if let Err(e) = check_issuer(inter, cert, below, now, false) {
                error = e;
                continue;
            }
            used.push(i);
            match self.build(inter, intermediates, used, below + 1, now) {
                Ok(()) => return Ok(()),
                Err(e) => error = e,
            }
            used.pop();
        }
        Err(error)
    }
}

/// May `issuer` sign `child`, with `below` intermediates between `issuer`
/// and the leaf?
fn check_issuer(
    issuer: &Certificate,
    child: &Certificate,
    below: usize,
    now: i64,
    anchor: bool,
) -> Result<(), CertError> {
    match issuer.basic_constraints {
        Some((true, Some(max))) if below > max as usize => return Err(CertError::NotCa),
        Some((true, _)) => {}
        Some((false, _)) => return Err(CertError::NotCa),
        // Version 1 roots carry no extensions; such trust anchors count as CAs.
        None if anchor && issuer.version < 3 => {}
        None => return Err(CertError::NotCa),
    }
    if let Some(ku) = issuer.key_usage
        && ku & KEY_USAGE_CERT_SIGN == 0
    {
        return Err(CertError::BadKeyUsage);
    }
    issuer.is_valid_at(now)?;
    issuer.verify_issued(child)
}
