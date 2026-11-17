//! ChaCha20, Poly1305 and the ChaCha20-Poly1305 AEAD (RFC 8439).

use super::hmac::ct_eq;

fn quarter_round(s: &mut [u32; 16], a: usize, b: usize, c: usize, d: usize) {
    s[a] = s[a].wrapping_add(s[b]);
    s[d] = (s[d] ^ s[a]).rotate_left(16);
    s[c] = s[c].wrapping_add(s[d]);
    s[b] = (s[b] ^ s[c]).rotate_left(12);
    s[a] = s[a].wrapping_add(s[b]);
    s[d] = (s[d] ^ s[a]).rotate_left(8);
    s[c] = s[c].wrapping_add(s[d]);
    s[b] = (s[b] ^ s[c]).rotate_left(7);
}

/// One 64-byte keystream block.
pub fn chacha20_block(key: &[u8; 32], counter: u32, nonce: &[u8; 12]) -> [u8; 64] {
    let mut init = [0u32; 16];
    init[0] = 0x6170_7865;
    init[1] = 0x3320_646e;
    init[2] = 0x7962_2d32;
    init[3] = 0x6b20_6574;
    for i in 0..8 {
        init[4 + i] = u32::from_le_bytes(key[4 * i..4 * i + 4].try_into().unwrap());
    }
    init[12] = counter;
    for i in 0..3 {
        init[13 + i] = u32::from_le_bytes(nonce[4 * i..4 * i + 4].try_into().unwrap());
    }
    let mut s = init;
    for _ in 0..10 {
        quarter_round(&mut s, 0, 4, 8, 12);
        quarter_round(&mut s, 1, 5, 9, 13);
        quarter_round(&mut s, 2, 6, 10, 14);
        quarter_round(&mut s, 3, 7, 11, 15);
        quarter_round(&mut s, 0, 5, 10, 15);
        quarter_round(&mut s, 1, 6, 11, 12);
        quarter_round(&mut s, 2, 7, 8, 13);
        quarter_round(&mut s, 3, 4, 9, 14);
    }
    let mut out = [0u8; 64];
    for i in 0..16 {
        out[4 * i..4 * i + 4].copy_from_slice(&s[i].wrapping_add(init[i]).to_le_bytes());
    }
    out
}

/// XOR `data` with the keystream starting at block `counter`.
pub fn chacha20_xor(key: &[u8; 32], counter: u32, nonce: &[u8; 12], data: &mut [u8]) {
    let mut counter = counter;
    for chunk in data.chunks_mut(64) {
        let ks = chacha20_block(key, counter, nonce);
        for (d, k) in chunk.iter_mut().zip(ks) {
            *d ^= k;
        }
        counter = counter.wrapping_add(1);
    }
}

fn le32(b: &[u8]) -> u32 {
    u32::from_le_bytes(b[..4].try_into().unwrap())
}

/// Poly1305 with 26-bit limbs.
pub struct Poly1305 {
    r: [u32; 5],
    h: [u32; 5],
    pad: [u32; 4],
    buf: [u8; 16],
    buf_len: usize,
}

impl Poly1305 {
    pub fn new(key: &[u8; 32]) -> Poly1305 {
        let r = [
            le32(&key[0..]) & 0x03ff_ffff,
            (le32(&key[3..]) >> 2) & 0x03ff_ff03,
            (le32(&key[6..]) >> 4) & 0x03ff_c0ff,
            (le32(&key[9..]) >> 6) & 0x03f0_3fff,
            (le32(&key[12..]) >> 8) & 0x000f_ffff,
        ];
        let pad = [
            le32(&key[16..]),
            le32(&key[20..]),
            le32(&key[24..]),
            le32(&key[28..]),
        ];
        Poly1305 {
            r,
            h: [0; 5],
            pad,
            buf: [0; 16],
            buf_len: 0,
        }
    }

    fn block(&mut self, m: &[u8; 16], hibit: u32) {
        let [r0, r1, r2, r3, r4] = self.r.map(u64::from);
        let (s1, s2, s3, s4) = (r1 * 5, r2 * 5, r3 * 5, r4 * 5);
        let mut h = self.h;
        h[0] += le32(&m[0..]) & 0x03ff_ffff;
        h[1] += (le32(&m[3..]) >> 2) & 0x03ff_ffff;
        h[2] += (le32(&m[6..]) >> 4) & 0x03ff_ffff;
        h[3] += (le32(&m[9..]) >> 6) & 0x03ff_ffff;
        h[4] += (le32(&m[12..]) >> 8) | hibit;
        let [h0, h1, h2, h3, h4] = h.map(u64::from);
        let d0 = h0 * r0 + h1 * s4 + h2 * s3 + h3 * s2 + h4 * s1;
        let mut d1 = h0 * r1 + h1 * r0 + h2 * s4 + h3 * s3 + h4 * s2;
        let mut d2 = h0 * r2 + h1 * r1 + h2 * r0 + h3 * s4 + h4 * s3;
        let mut d3 = h0 * r3 + h1 * r2 + h2 * r1 + h3 * r0 + h4 * s4;
        let mut d4 = h0 * r4 + h1 * r3 + h2 * r2 + h3 * r1 + h4 * r0;
        let mut c = d0 >> 26;
        let mut n0 = d0 & 0x03ff_ffff;
        d1 += c;
        c = d1 >> 26;
        let mut n1 = d1 & 0x03ff_ffff;
        d2 += c;
        c = d2 >> 26;
        let n2 = d2 & 0x03ff_ffff;
        d3 += c;
        c = d3 >> 26;
        let n3 = d3 & 0x03ff_ffff;
        d4 += c;
        c = d4 >> 26;
        let n4 = d4 & 0x03ff_ffff;
        n0 += c * 5;
        c = n0 >> 26;
        n0 &= 0x03ff_ffff;
        n1 += c;
        self.h = [n0 as u32, n1 as u32, n2 as u32, n3 as u32, n4 as u32];
    }

    pub fn update(&mut self, mut data: &[u8]) {
        if self.buf_len > 0 {
            let take = (16 - self.buf_len).min(data.len());
            self.buf[self.buf_len..self.buf_len + take].copy_from_slice(&data[..take]);
            self.buf_len += take;
            data = &data[take..];
            if self.buf_len < 16 {
                return;
            }
            let block = self.buf;
            self.block(&block, 1 << 24);
            self.buf_len = 0;
        }
        while data.len() >= 16 {
            self.block(data[..16].try_into().unwrap(), 1 << 24);
            data = &data[16..];
        }
        self.buf[..data.len()].copy_from_slice(data);
        self.buf_len = data.len();
    }

    pub fn finish(mut self) -> [u8; 16] {
        if self.buf_len > 0 {
            let mut block = [0u8; 16];
            block[..self.buf_len].copy_from_slice(&self.buf[..self.buf_len]);
            block[self.buf_len] = 1;
            self.block(&block, 0);
        }
        let mask = 0x03ff_ffffu32;
        let [mut h0, mut h1, mut h2, mut h3, mut h4] = self.h;
        let mut c = h1 >> 26;
        h1 &= mask;
        h2 += c;
        c = h2 >> 26;
        h2 &= mask;
        h3 += c;
        c = h3 >> 26;
        h3 &= mask;
        h4 += c;
        c = h4 >> 26;
        h4 &= mask;
        h0 += c * 5;
        c = h0 >> 26;
        h0 &= mask;
        h1 += c;

        // g = h + 5 - 2^130; keep it if it did not go negative.
        let mut g0 = h0.wrapping_add(5);
        c = g0 >> 26;
        g0 &= mask;
        let mut g1 = h1.wrapping_add(c);
        c = g1 >> 26;
        g1 &= mask;
        let mut g2 = h2.wrapping_add(c);
        c = g2 >> 26;
        g2 &= mask;
        let mut g3 = h3.wrapping_add(c);
        c = g3 >> 26;
        g3 &= mask;
        let g4 = h4.wrapping_add(c).wrapping_sub(1 << 26);
        let select = (g4 >> 31).wrapping_sub(1);
        h0 = (h0 & !select) | (g0 & select);
        h1 = (h1 & !select) | (g1 & select);
        h2 = (h2 & !select) | (g2 & select);
        h3 = (h3 & !select) | (g3 & select);
        h4 = (h4 & !select) | (g4 & select);

        let w0 = h0 | (h1 << 26);
        let w1 = (h1 >> 6) | (h2 << 20);
        let w2 = (h2 >> 12) | (h3 << 14);
        let w3 = (h3 >> 18) | (h4 << 8);
        let mut out = [0u8; 16];
        let mut carry = 0u64;
        for (i, (w, p)) in [w0, w1, w2, w3].into_iter().zip(self.pad).enumerate() {
            let f = w as u64 + p as u64 + carry;
            out[4 * i..4 * i + 4].copy_from_slice(&(f as u32).to_le_bytes());
            carry = f >> 32;
        }
        out
    }
}

pub fn poly1305(key: &[u8; 32], msg: &[u8]) -> [u8; 16] {
    let mut p = Poly1305::new(key);
    p.update(msg);
    p.finish()
}

#[derive(Clone)]
pub struct ChaCha20Poly1305 {
    key: [u8; 32],
}

impl ChaCha20Poly1305 {
    pub const TAG_LEN: usize = 16;

    pub fn new(key: &[u8; 32]) -> ChaCha20Poly1305 {
        ChaCha20Poly1305 { key: *key }
    }

    fn tag(&self, nonce: &[u8; 12], aad: &[u8], ct: &[u8]) -> [u8; 16] {
        let block = chacha20_block(&self.key, 0, nonce);
        let otk: [u8; 32] = block[..32].try_into().unwrap();
        let mut p = Poly1305::new(&otk);
        let zeros = [0u8; 16];
        p.update(aad);
        p.update(&zeros[..(16 - aad.len() % 16) % 16]);
        p.update(ct);
        p.update(&zeros[..(16 - ct.len() % 16) % 16]);
        p.update(&(aad.len() as u64).to_le_bytes());
        p.update(&(ct.len() as u64).to_le_bytes());
        p.finish()
    }

    pub fn seal(&self, nonce: &[u8; 12], aad: &[u8], plaintext: &[u8]) -> Vec<u8> {
        let mut out = plaintext.to_vec();
        chacha20_xor(&self.key, 1, nonce, &mut out);
        let tag = self.tag(nonce, aad, &out);
        out.extend_from_slice(&tag);
        out
    }

    pub fn open(&self, nonce: &[u8; 12], aad: &[u8], sealed: &[u8]) -> Option<Vec<u8>> {
        if sealed.len() < Self::TAG_LEN {
            return None;
        }
        let (ct, tag) = sealed.split_at(sealed.len() - Self::TAG_LEN);
        if !ct_eq(&self.tag(nonce, aad, ct), tag) {
            return None;
        }
        let mut out = ct.to_vec();
        chacha20_xor(&self.key, 1, nonce, &mut out);
        Some(out)
    }
}
