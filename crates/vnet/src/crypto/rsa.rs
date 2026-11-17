//! RSA signature verification (RFC 8017): RSASSA-PKCS1-v1_5 and RSASSA-PSS
//! with MGF1, on the public key only.

use super::bigint::{BigUint, Modulus};
use super::hmac::ct_eq;
use super::sha2::HashAlg;

/// Smallest modulus accepted, in bits.
pub const MIN_BITS: usize = 1024;
/// Largest modulus accepted, in bits.
pub const MAX_BITS: usize = 16384;

#[derive(Clone, Debug)]
pub struct RsaPublicKey {
    modulus: Modulus,
    e: BigUint,
}

impl RsaPublicKey {
    /// From big-endian modulus and exponent bytes.
    pub fn new(n: &[u8], e: &[u8]) -> Option<RsaPublicKey> {
        let n = BigUint::from_be_bytes(n);
        let e = BigUint::from_be_bytes(e);
        if n.bits() < MIN_BITS || n.bits() > MAX_BITS || e.is_zero() || !e.is_odd() || e >= n {
            return None;
        }
        Some(RsaPublicKey {
            modulus: Modulus::new(&n)?,
            e,
        })
    }

    pub fn bits(&self) -> usize {
        self.modulus.value().bits()
    }

    /// Modulus length in bytes.
    pub fn modulus_len(&self) -> usize {
        self.modulus.value().byte_len()
    }

    /// s^e mod n as `modulus_len()` bytes; `None` if `sig` is not a valid representative.
    fn public_op(&self, sig: &[u8]) -> Option<Vec<u8>> {
        let k = self.modulus_len();
        if sig.len() != k {
            return None;
        }
        let s = BigUint::from_be_bytes(sig);
        let s_m = self.modulus.to_mont(&s)?;
        let m = self.modulus.from_mont(&self.modulus.pow(&s_m, &self.e));
        m.to_be_bytes(k)
    }

    /// RSASSA-PKCS1-v1_5 with `alg` over `msg`.
    pub fn verify_pkcs1(&self, alg: HashAlg, msg: &[u8], sig: &[u8]) -> bool {
        self.verify_pkcs1_digest(alg, &alg.digest(msg), sig)
    }

    /// RSASSA-PKCS1-v1_5 over an already computed digest.
    pub fn verify_pkcs1_digest(&self, alg: HashAlg, digest: &[u8], sig: &[u8]) -> bool {
        let Some(em) = self.public_op(sig) else {
            return false;
        };
        let prefix = digest_info_prefix(alg);
        let t_len = prefix.len() + digest.len();
        let k = em.len();
        if k < t_len + 11 {
            return false;
        }
        let mut expected = vec![0x00, 0x01];
        expected.resize(k - t_len - 1, 0xff);
        expected.push(0x00);
        expected.extend_from_slice(prefix);
        expected.extend_from_slice(digest);
        ct_eq(&em, &expected)
    }

    /// RSASSA-PSS with `alg` for the message hash and MGF1, and a salt as
    /// long as the hash (the only form TLS 1.3 and X.509 use here).
    pub fn verify_pss(&self, alg: HashAlg, msg: &[u8], sig: &[u8]) -> bool {
        self.verify_pss_digest(alg, &alg.digest(msg), sig, alg.output_len())
    }

    pub fn verify_pss_digest(
        &self,
        alg: HashAlg,
        m_hash: &[u8],
        sig: &[u8],
        salt_len: usize,
    ) -> bool {
        let Some(m) = self.public_op(sig) else {
            return false;
        };
        let mod_bits = self.bits();
        let em_bits = mod_bits - 1;
        let em_len = em_bits.div_ceil(8);
        // m has `modulus_len()` bytes; when em_len is one less, its top byte must be zero.
        let em = if em_len < m.len() {
            if m[0] != 0 {
                return false;
            }
            &m[1..]
        } else {
            &m[..]
        };
        let h_len = alg.output_len();
        if m_hash.len() != h_len || em_len < h_len + salt_len + 2 {
            return false;
        }
        if em[em_len - 1] != 0xbc {
            return false;
        }
        let db_len = em_len - h_len - 1;
        let (masked_db, rest) = em.split_at(db_len);
        let h = &rest[..h_len];
        let top_bits = 8 * em_len - em_bits;
        let top_mask = (0xffu16 >> top_bits) as u8;
        if masked_db[0] & !top_mask != 0 {
            return false;
        }
        let mask = mgf1(alg, h, db_len);
        let mut db: Vec<u8> = masked_db.iter().zip(&mask).map(|(a, b)| a ^ b).collect();
        db[0] &= top_mask;
        let ps_len = em_len - h_len - salt_len - 2;
        if db[..ps_len].iter().any(|&b| b != 0) || db[ps_len] != 0x01 {
            return false;
        }
        let salt = &db[db_len - salt_len..];
        let mut hasher = alg.hasher();
        hasher.update(&[0u8; 8]);
        hasher.update(m_hash);
        hasher.update(salt);
        ct_eq(&hasher.finish(), h)
    }
}

/// MGF1 (RFC 8017 B.2.1).
pub fn mgf1(alg: HashAlg, seed: &[u8], len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(len + alg.output_len());
    let mut counter = 0u32;
    while out.len() < len {
        let mut h = alg.hasher();
        h.update(seed);
        h.update(&counter.to_be_bytes());
        out.extend(h.finish());
        counter += 1;
    }
    out.truncate(len);
    out
}

/// DER of `DigestInfo` up to the digest bytes (RFC 8017 §9.2 note 1).
fn digest_info_prefix(alg: HashAlg) -> &'static [u8] {
    match alg {
        HashAlg::Sha256 => &[
            0x30, 0x31, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02,
            0x01, 0x05, 0x00, 0x04, 0x20,
        ],
        HashAlg::Sha384 => &[
            0x30, 0x41, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02,
            0x02, 0x05, 0x00, 0x04, 0x30,
        ],
        HashAlg::Sha512 => &[
            0x30, 0x51, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02,
            0x03, 0x05, 0x00, 0x04, 0x40,
        ],
    }
}
