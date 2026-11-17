//! Cryptography for TLS 1.3 and X.509, written from the specifications.
//!
//! Nothing here claims to run in constant time (see `LIMITATIONS.md`).

pub mod aes;
pub mod bigint;
pub mod chacha;
pub mod hmac;
pub mod p256;
pub mod random;
pub mod rsa;
pub mod sha2;
pub mod x25519;

pub use sha2::{HashAlg, Hasher};

use aes::AesGcm;
use chacha::ChaCha20Poly1305;

/// The AEADs of the TLS 1.3 cipher suites, all with 12-byte nonces and
/// 16-byte tags.
#[derive(Clone)]
pub enum Aead {
    AesGcm(AesGcm),
    ChaCha20Poly1305(ChaCha20Poly1305),
}

impl Aead {
    pub const NONCE_LEN: usize = 12;
    pub const TAG_LEN: usize = 16;

    pub fn aes_gcm(key: &[u8]) -> Aead {
        Aead::AesGcm(AesGcm::new(key))
    }

    pub fn chacha20_poly1305(key: &[u8; 32]) -> Aead {
        Aead::ChaCha20Poly1305(ChaCha20Poly1305::new(key))
    }

    pub fn seal(&self, nonce: &[u8; 12], aad: &[u8], plaintext: &[u8]) -> Vec<u8> {
        match self {
            Aead::AesGcm(a) => a.seal(nonce, aad, plaintext),
            Aead::ChaCha20Poly1305(a) => a.seal(nonce, aad, plaintext),
        }
    }

    pub fn open(&self, nonce: &[u8; 12], aad: &[u8], sealed: &[u8]) -> Option<Vec<u8>> {
        match self {
            Aead::AesGcm(a) => a.open(nonce, aad, sealed),
            Aead::ChaCha20Poly1305(a) => a.open(nonce, aad, sealed),
        }
    }
}
