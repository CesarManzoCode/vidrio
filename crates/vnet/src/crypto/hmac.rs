//! HMAC (RFC 2104) and HKDF (RFC 5869) over the SHA-2 family.

use super::sha2::{HashAlg, Hasher};

/// Incremental HMAC.
#[derive(Clone)]
pub struct Hmac {
    inner: Hasher,
    outer: Hasher,
}

impl Hmac {
    pub fn new(alg: HashAlg, key: &[u8]) -> Hmac {
        let block = alg.block_len();
        let mut k = if key.len() > block {
            alg.digest(key)
        } else {
            key.to_vec()
        };
        k.resize(block, 0);
        let mut inner = alg.hasher();
        let mut outer = alg.hasher();
        let ipad: Vec<u8> = k.iter().map(|b| b ^ 0x36).collect();
        let opad: Vec<u8> = k.iter().map(|b| b ^ 0x5c).collect();
        inner.update(&ipad);
        outer.update(&opad);
        Hmac { inner, outer }
    }

    pub fn update(&mut self, data: &[u8]) {
        self.inner.update(data);
    }

    pub fn finish(self) -> Vec<u8> {
        let mut outer = self.outer;
        outer.update(&self.inner.finish());
        outer.finish()
    }
}

pub fn hmac(alg: HashAlg, key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut h = Hmac::new(alg, key);
    h.update(data);
    h.finish()
}

/// HKDF-Extract; an empty `salt` means a string of zeros of the hash length.
pub fn hkdf_extract(alg: HashAlg, salt: &[u8], ikm: &[u8]) -> Vec<u8> {
    if salt.is_empty() {
        hmac(alg, &vec![0u8; alg.output_len()], ikm)
    } else {
        hmac(alg, salt, ikm)
    }
}

/// HKDF-Expand; `len` is at most 255 times the hash length.
pub fn hkdf_expand(alg: HashAlg, prk: &[u8], info: &[u8], len: usize) -> Vec<u8> {
    assert!(len <= 255 * alg.output_len(), "HKDF output too long");
    let mut out = Vec::with_capacity(len);
    let mut prev: Vec<u8> = Vec::new();
    let mut counter = 1u8;
    while out.len() < len {
        let mut h = Hmac::new(alg, prk);
        h.update(&prev);
        h.update(info);
        h.update(&[counter]);
        prev = h.finish();
        let take = (len - out.len()).min(prev.len());
        out.extend_from_slice(&prev[..take]);
        counter = counter.wrapping_add(1);
    }
    out
}

/// Equality that does not stop at the first difference.
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}
