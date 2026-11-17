//! AES-128 and AES-256 encryption (FIPS 197) and GCM (NIST SP 800-38D)
//! with 96-bit nonces.

use super::hmac::ct_eq;

const fn gmul(mut a: u8, mut b: u8) -> u8 {
    let mut p = 0u8;
    while b != 0 {
        if b & 1 != 0 {
            p ^= a;
        }
        let hi = a & 0x80;
        a <<= 1;
        if hi != 0 {
            a ^= 0x1b;
        }
        b >>= 1;
    }
    p
}

/// The S-box: multiplicative inverse in GF(2^8) followed by the affine map.
const fn make_sbox() -> [u8; 256] {
    let mut sbox = [0u8; 256];
    let mut i = 0;
    while i < 256 {
        let mut inv = 1u8;
        let mut base = i as u8;
        let mut e = 254u32;
        while e > 0 {
            if e & 1 == 1 {
                inv = gmul(inv, base);
            }
            base = gmul(base, base);
            e >>= 1;
        }
        let b = inv;
        sbox[i] =
            b ^ b.rotate_left(1) ^ b.rotate_left(2) ^ b.rotate_left(3) ^ b.rotate_left(4) ^ 0x63;
        i += 1;
    }
    sbox
}

const SBOX: [u8; 256] = make_sbox();

#[derive(Clone)]
pub struct Aes {
    round_keys: Vec<[u8; 16]>,
}

impl Aes {
    /// `key` is 16 or 32 bytes.
    pub fn new(key: &[u8]) -> Aes {
        let nk = key.len() / 4;
        assert!(
            key.len() == 16 || key.len() == 32,
            "AES key must be 16 or 32 bytes"
        );
        let nr = nk + 6;
        let total = 4 * (nr + 1);
        let mut w: Vec<[u8; 4]> = Vec::with_capacity(total);
        for chunk in key.chunks_exact(4) {
            w.push(chunk.try_into().unwrap());
        }
        let mut rcon = 1u8;
        for i in nk..total {
            let mut t = w[i - 1];
            if i % nk == 0 {
                t = [
                    SBOX[t[1] as usize],
                    SBOX[t[2] as usize],
                    SBOX[t[3] as usize],
                    SBOX[t[0] as usize],
                ];
                t[0] ^= rcon;
                rcon = gmul(rcon, 2);
            } else if nk > 6 && i % nk == 4 {
                t = t.map(|b| SBOX[b as usize]);
            }
            let prev = w[i - nk];
            w.push([
                prev[0] ^ t[0],
                prev[1] ^ t[1],
                prev[2] ^ t[2],
                prev[3] ^ t[3],
            ]);
        }
        let round_keys = w
            .chunks_exact(4)
            .map(|c| {
                let mut k = [0u8; 16];
                for (j, word) in c.iter().enumerate() {
                    k[4 * j..4 * j + 4].copy_from_slice(word);
                }
                k
            })
            .collect();
        Aes { round_keys }
    }

    pub fn encrypt_block(&self, block: &mut [u8; 16]) {
        let nr = self.round_keys.len() - 1;
        xor16(block, &self.round_keys[0]);
        for round in 1..=nr {
            for b in block.iter_mut() {
                *b = SBOX[*b as usize];
            }
            let s = *block;
            for c in 0..4 {
                for r in 0..4 {
                    block[r + 4 * c] = s[r + 4 * ((c + r) % 4)];
                }
            }
            if round != nr {
                for c in 0..4 {
                    let a = [
                        block[4 * c],
                        block[4 * c + 1],
                        block[4 * c + 2],
                        block[4 * c + 3],
                    ];
                    let x = a.map(|v| gmul(v, 2));
                    block[4 * c] = x[0] ^ (x[1] ^ a[1]) ^ a[2] ^ a[3];
                    block[4 * c + 1] = a[0] ^ x[1] ^ (x[2] ^ a[2]) ^ a[3];
                    block[4 * c + 2] = a[0] ^ a[1] ^ x[2] ^ (x[3] ^ a[3]);
                    block[4 * c + 3] = (x[0] ^ a[0]) ^ a[1] ^ a[2] ^ x[3];
                }
            }
            xor16(block, &self.round_keys[round]);
        }
    }
}

fn xor16(a: &mut [u8; 16], b: &[u8; 16]) {
    for (x, y) in a.iter_mut().zip(b) {
        *x ^= y;
    }
}

/// Multiplication in GF(2^128) with the GCM bit order.
fn gf_mul(x: u128, y: u128) -> u128 {
    const R: u128 = 0xe1 << 120;
    let mut z = 0u128;
    let mut v = y;
    for i in 0..128 {
        if (x >> (127 - i)) & 1 == 1 {
            z ^= v;
        }
        v = if v & 1 == 1 { (v >> 1) ^ R } else { v >> 1 };
    }
    z
}

#[derive(Clone)]
pub struct AesGcm {
    aes: Aes,
    h: u128,
}

impl AesGcm {
    pub const TAG_LEN: usize = 16;

    pub fn new(key: &[u8]) -> AesGcm {
        let aes = Aes::new(key);
        let mut zero = [0u8; 16];
        aes.encrypt_block(&mut zero);
        AesGcm {
            aes,
            h: u128::from_be_bytes(zero),
        }
    }

    fn ghash(&self, aad: &[u8], ct: &[u8]) -> u128 {
        let mut y = 0u128;
        for data in [aad, ct] {
            for chunk in data.chunks(16) {
                let mut block = [0u8; 16];
                block[..chunk.len()].copy_from_slice(chunk);
                y = gf_mul(y ^ u128::from_be_bytes(block), self.h);
            }
        }
        let lens = ((aad.len() as u128 * 8) << 64) | (ct.len() as u128 * 8);
        gf_mul(y ^ lens, self.h)
    }

    /// CTR keystream starting at counter 2 (counter 1 masks the tag).
    fn ctr(&self, nonce: &[u8; 12], data: &mut [u8]) {
        let mut counter = 2u32;
        for chunk in data.chunks_mut(16) {
            let mut block = [0u8; 16];
            block[..12].copy_from_slice(nonce);
            block[12..].copy_from_slice(&counter.to_be_bytes());
            self.aes.encrypt_block(&mut block);
            for (d, k) in chunk.iter_mut().zip(block) {
                *d ^= k;
            }
            counter = counter.wrapping_add(1);
        }
    }

    fn tag(&self, nonce: &[u8; 12], aad: &[u8], ct: &[u8]) -> [u8; 16] {
        let mut j0 = [0u8; 16];
        j0[..12].copy_from_slice(nonce);
        j0[15] = 1;
        self.aes.encrypt_block(&mut j0);
        (self.ghash(aad, ct) ^ u128::from_be_bytes(j0)).to_be_bytes()
    }

    /// Ciphertext followed by the 16-byte tag.
    pub fn seal(&self, nonce: &[u8; 12], aad: &[u8], plaintext: &[u8]) -> Vec<u8> {
        let mut out = plaintext.to_vec();
        self.ctr(nonce, &mut out);
        let tag = self.tag(nonce, aad, &out);
        out.extend_from_slice(&tag);
        out
    }

    /// Plaintext, or `None` if the tag does not match.
    pub fn open(&self, nonce: &[u8; 12], aad: &[u8], sealed: &[u8]) -> Option<Vec<u8>> {
        if sealed.len() < Self::TAG_LEN {
            return None;
        }
        let (ct, tag) = sealed.split_at(sealed.len() - Self::TAG_LEN);
        if !ct_eq(&self.tag(nonce, aad, ct), tag) {
            return None;
        }
        let mut out = ct.to_vec();
        self.ctr(nonce, &mut out);
        Some(out)
    }
}
