//! The TLS 1.3 key schedule (RFC 8446 §7).

use crate::crypto::hmac::{hkdf_expand, hkdf_extract, hmac};
use crate::crypto::{Aead, HashAlg};

/// HKDF-Expand-Label(secret, label, context, length) with the `tls13 ` prefix.
pub fn hkdf_expand_label(
    alg: HashAlg,
    secret: &[u8],
    label: &[u8],
    context: &[u8],
    len: usize,
) -> Vec<u8> {
    let mut info = Vec::with_capacity(4 + 6 + label.len() + context.len());
    info.extend_from_slice(&(len as u16).to_be_bytes());
    info.push((6 + label.len()) as u8);
    info.extend_from_slice(b"tls13 ");
    info.extend_from_slice(label);
    info.push(context.len() as u8);
    info.extend_from_slice(context);
    hkdf_expand(alg, secret, &info, len)
}

/// Derive-Secret(secret, label, messages) given the transcript hash.
pub fn derive_secret(alg: HashAlg, secret: &[u8], label: &[u8], transcript_hash: &[u8]) -> Vec<u8> {
    hkdf_expand_label(alg, secret, label, transcript_hash, alg.output_len())
}

/// Early Secret without a PSK.
pub fn early_secret(alg: HashAlg) -> Vec<u8> {
    hkdf_extract(alg, &[], &vec![0u8; alg.output_len()])
}

/// Handshake Secret from the early secret and the (EC)DHE shared secret.
pub fn handshake_secret(alg: HashAlg, early: &[u8], shared: &[u8]) -> Vec<u8> {
    let derived = derive_secret(alg, early, b"derived", &alg.digest(&[]));
    hkdf_extract(alg, &derived, shared)
}

/// Master Secret from the handshake secret.
pub fn master_secret(alg: HashAlg, handshake: &[u8]) -> Vec<u8> {
    let derived = derive_secret(alg, handshake, b"derived", &alg.digest(&[]));
    hkdf_extract(alg, &derived, &vec![0u8; alg.output_len()])
}

/// verify_data of a Finished message sent with traffic secret `base_key`.
pub fn finished_verify_data(alg: HashAlg, base_key: &[u8], transcript_hash: &[u8]) -> Vec<u8> {
    let key = hkdf_expand_label(alg, base_key, b"finished", &[], alg.output_len());
    hmac(alg, &key, transcript_hash)
}

/// application_traffic_secret_N+1 (KeyUpdate).
pub fn next_traffic_secret(alg: HashAlg, secret: &[u8]) -> Vec<u8> {
    hkdf_expand_label(alg, secret, b"traffic upd", &[], alg.output_len())
}

/// The TLS 1.3 cipher suites.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CipherSuite {
    Aes128GcmSha256,
    Aes256GcmSha384,
    ChaCha20Poly1305Sha256,
}

impl CipherSuite {
    pub const ALL: [CipherSuite; 3] = [
        CipherSuite::Aes128GcmSha256,
        CipherSuite::Aes256GcmSha384,
        CipherSuite::ChaCha20Poly1305Sha256,
    ];

    pub fn id(self) -> u16 {
        match self {
            CipherSuite::Aes128GcmSha256 => 0x1301,
            CipherSuite::Aes256GcmSha384 => 0x1302,
            CipherSuite::ChaCha20Poly1305Sha256 => 0x1303,
        }
    }

    pub fn from_id(id: u16) -> Option<CipherSuite> {
        CipherSuite::ALL.into_iter().find(|s| s.id() == id)
    }

    pub fn hash(self) -> HashAlg {
        match self {
            CipherSuite::Aes256GcmSha384 => HashAlg::Sha384,
            _ => HashAlg::Sha256,
        }
    }

    pub fn key_len(self) -> usize {
        match self {
            CipherSuite::Aes128GcmSha256 => 16,
            _ => 32,
        }
    }

    pub fn aead(self, key: &[u8]) -> Aead {
        match self {
            CipherSuite::ChaCha20Poly1305Sha256 => {
                Aead::chacha20_poly1305(key.try_into().expect("32-byte key"))
            }
            _ => Aead::aes_gcm(key),
        }
    }
}

/// Write key and IV of one direction.
pub fn traffic_keys(suite: CipherSuite, secret: &[u8]) -> (Vec<u8>, [u8; 12]) {
    let alg = suite.hash();
    let key = hkdf_expand_label(alg, secret, b"key", &[], suite.key_len());
    let iv = hkdf_expand_label(alg, secret, b"iv", &[], 12);
    (key, iv.try_into().unwrap())
}

/// Record protection for one direction: AEAD, IV and sequence number.
pub struct RecordCipher {
    aead: Aead,
    iv: [u8; 12],
    seq: u64,
}

impl RecordCipher {
    pub fn new(suite: CipherSuite, secret: &[u8]) -> RecordCipher {
        let (key, iv) = traffic_keys(suite, secret);
        RecordCipher {
            aead: suite.aead(&key),
            iv,
            seq: 0,
        }
    }

    fn nonce(&self) -> [u8; 12] {
        let mut nonce = self.iv;
        for (n, s) in nonce[4..].iter_mut().zip(self.seq.to_be_bytes()) {
            *n ^= s;
        }
        nonce
    }

    /// A whole TLSCiphertext record carrying `content` of type `kind`.
    pub fn seal(&mut self, kind: u8, content: &[u8]) -> Vec<u8> {
        let mut inner = Vec::with_capacity(content.len() + 1);
        inner.extend_from_slice(content);
        inner.push(kind);
        let len = inner.len() + Aead::TAG_LEN;
        let header = [23, 3, 3, (len >> 8) as u8, len as u8];
        let sealed = self.aead.seal(&self.nonce(), &header, &inner);
        self.seq += 1;
        let mut record = header.to_vec();
        record.extend(sealed);
        record
    }

    /// Decrypt a record body; returns the inner content type and content.
    pub fn open(&mut self, header: &[u8; 5], body: &[u8]) -> Option<(u8, Vec<u8>)> {
        let mut inner = self.aead.open(&self.nonce(), header, body)?;
        self.seq += 1;
        while let Some(&last) = inner.last() {
            inner.pop();
            if last != 0 {
                return Some((last, inner));
            }
        }
        // All padding: no content type.
        Some((0, inner))
    }
}
