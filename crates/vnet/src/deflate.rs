//! Deflate encoder (RFC 1951) with zlib and gzip wrappers.
//!
//! Greedy LZ77 over hash chains; each block is emitted stored, with the fixed
//! code or with a dynamic code, whichever is smallest.

use crate::checksum::{adler32, crc32};
use crate::inflate::{CLEN_ORDER, DIST_BASE, DIST_EXTRA, LEN_BASE, LEN_EXTRA, reverse_bits};
use std::cmp::Reverse;
use std::collections::BinaryHeap;

/// Compression level 0 (stored only) to 9 (longest match search).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Level(pub u8);

impl Default for Level {
    fn default() -> Self {
        Level(6)
    }
}

const WINDOW: usize = 32 * 1024;
const MIN_MATCH: usize = 3;
const MAX_MATCH: usize = 258;
const HASH_BITS: u32 = 15;
/// Tokens per block.
const BLOCK_TOKENS: usize = 1 << 14;
const MAX_STORED: usize = 65535;

#[derive(Clone, Copy)]
enum Token {
    Literal(u8),
    Match { len: u16, dist: u16 },
}

struct BitWriter {
    out: Vec<u8>,
    acc: u64,
    n: u32,
}

impl BitWriter {
    fn new() -> BitWriter {
        BitWriter {
            out: Vec::new(),
            acc: 0,
            n: 0,
        }
    }

    fn put(&mut self, bits: u32, n: u32) {
        debug_assert!(n <= 32);
        self.acc |= (bits as u64 & ((1u64 << n) - 1)) << self.n;
        self.n += n;
        while self.n >= 8 {
            self.out.push(self.acc as u8);
            self.acc >>= 8;
            self.n -= 8;
        }
    }

    /// Write a Huffman code (stored most-significant bit first).
    fn code(&mut self, code: u16, len: u8) {
        self.put(reverse_bits(code as u32, len as u32), len as u32);
    }

    fn align(&mut self) {
        if self.n > 0 {
            self.put(0, 8 - self.n);
        }
    }

    fn finish(mut self) -> Vec<u8> {
        self.align();
        self.out
    }
}

fn len_code(len: usize) -> usize {
    LEN_BASE.iter().rposition(|&b| b as usize <= len).unwrap()
}

fn dist_code(dist: usize) -> usize {
    DIST_BASE.iter().rposition(|&b| b as usize <= dist).unwrap()
}

fn max_chain(level: u8) -> usize {
    match level {
        0 => 0,
        1 => 4,
        2 => 8,
        3 => 16,
        4 => 32,
        5 => 64,
        6 => 128,
        7 => 256,
        8 => 1024,
        _ => 4096,
    }
}

fn hash(data: &[u8], i: usize) -> usize {
    let v = (data[i] as u32) | (data[i + 1] as u32) << 8 | (data[i + 2] as u32) << 16;
    (v.wrapping_mul(0x9E37_79B1) >> (32 - HASH_BITS)) as usize
}

/// Greedy LZ77 parse.
fn tokenize(data: &[u8], level: u8) -> Vec<Token> {
    let chain_limit = max_chain(level);
    let mut head = vec![usize::MAX; 1 << HASH_BITS];
    let mut prev = vec![usize::MAX; data.len()];
    let mut tokens = Vec::with_capacity(data.len() / 2 + 1);
    let insert = |head: &mut Vec<usize>, prev: &mut Vec<usize>, i: usize| {
        if i + MIN_MATCH <= data.len() {
            let h = hash(data, i);
            prev[i] = head[h];
            head[h] = i;
        }
    };
    let mut i = 0;
    while i < data.len() {
        let mut best_len = 0;
        let mut best_dist = 0;
        if i + MIN_MATCH <= data.len() {
            let max_len = MAX_MATCH.min(data.len() - i);
            let mut cand = head[hash(data, i)];
            let mut chain = chain_limit;
            while cand != usize::MAX && i - cand <= WINDOW && chain > 0 {
                let mut l = 0;
                while l < max_len && data[cand + l] == data[i + l] {
                    l += 1;
                }
                if l > best_len {
                    best_len = l;
                    best_dist = i - cand;
                    if l == max_len {
                        break;
                    }
                }
                cand = prev[cand];
                chain -= 1;
            }
        }
        if best_len >= MIN_MATCH {
            tokens.push(Token::Match {
                len: best_len as u16,
                dist: best_dist as u16,
            });
            for k in i..i + best_len {
                insert(&mut head, &mut prev, k);
            }
            i += best_len;
        } else {
            tokens.push(Token::Literal(data[i]));
            insert(&mut head, &mut prev, i);
            i += 1;
        }
    }
    tokens
}

/// Huffman code lengths for `freqs`, limited to `limit` bits. At least two
/// symbols always get a code so the result is a complete prefix code.
fn code_lengths(freqs: &[u32], limit: u8) -> Vec<u8> {
    let mut f: Vec<u32> = freqs.to_vec();
    for i in 0..f.len() {
        if f.iter().filter(|&&x| x > 0).count() >= 2 {
            break;
        }
        if f[i] == 0 {
            f[i] = 1;
        }
    }
    loop {
        let lengths = huffman_lengths(&f);
        if lengths.iter().all(|&l| l <= limit) {
            return lengths;
        }
        // Flatten the distribution and retry.
        for x in f.iter_mut() {
            if *x > 0 {
                *x = (*x >> 1).max(1);
            }
        }
    }
}

/// Unlimited Huffman code lengths.
fn huffman_lengths(freqs: &[u32]) -> Vec<u8> {
    let n = freqs.len();
    let mut parent: Vec<usize> = vec![usize::MAX; n];
    let mut heap = BinaryHeap::new();
    for (i, &f) in freqs.iter().enumerate() {
        if f > 0 {
            heap.push(Reverse((f as u64, i)));
        }
    }
    let mut lengths = vec![0u8; n];
    if heap.len() == 1 {
        let Reverse((_, i)) = heap.pop().unwrap();
        lengths[i] = 1;
        return lengths;
    }
    while heap.len() > 1 {
        let Reverse((fa, a)) = heap.pop().unwrap();
        let Reverse((fb, b)) = heap.pop().unwrap();
        let node = parent.len();
        parent.push(usize::MAX);
        parent[a] = node;
        parent[b] = node;
        heap.push(Reverse((fa + fb, node)));
    }
    for (i, &f) in freqs.iter().enumerate() {
        if f > 0 {
            let mut depth = 0u32;
            let mut p = parent[i];
            while p != usize::MAX {
                depth += 1;
                p = parent[p];
            }
            lengths[i] = depth.min(255) as u8;
        }
    }
    lengths
}

/// Canonical codes for `lengths` (RFC 1951 §3.2.2).
fn canonical_codes(lengths: &[u8]) -> Vec<u16> {
    let mut count = [0u16; 16];
    for &l in lengths {
        count[l as usize] += 1;
    }
    count[0] = 0;
    let mut next = [0u16; 16];
    let mut code = 0u16;
    for len in 1..16 {
        code = (code + count[len - 1]) << 1;
        next[len] = code;
    }
    lengths
        .iter()
        .map(|&l| {
            if l == 0 {
                0
            } else {
                let c = next[l as usize];
                next[l as usize] += 1;
                c
            }
        })
        .collect()
}

fn fixed_lengths() -> (Vec<u8>, Vec<u8>) {
    let lit = (0..288)
        .map(|i| match i {
            0..=143 => 8,
            144..=255 => 9,
            256..=279 => 7,
            _ => 8,
        })
        .collect();
    (lit, vec![5u8; 30])
}

/// Run-length encoding of code lengths: (symbol, extra value).
fn rle_lengths(lengths: &[u8]) -> Vec<(u8, u8)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < lengths.len() {
        let l = lengths[i];
        let mut run = 1;
        while i + run < lengths.len() && lengths[i + run] == l {
            run += 1;
        }
        i += run;
        if l == 0 {
            let mut r = run;
            while r >= 11 {
                let n = r.min(138);
                out.push((18, (n - 11) as u8));
                r -= n;
            }
            if r >= 3 {
                out.push((17, (r - 3) as u8));
                r = 0;
            }
            for _ in 0..r {
                out.push((0, 0));
            }
        } else {
            out.push((l, 0));
            let mut r = run - 1;
            while r >= 3 {
                let n = r.min(6);
                out.push((16, (n - 3) as u8));
                r -= n;
            }
            for _ in 0..r {
                out.push((l, 0));
            }
        }
    }
    out
}

struct Dynamic {
    lit: Vec<u8>,
    dist: Vec<u8>,
    hlit: usize,
    hdist: usize,
    rle: Vec<(u8, u8)>,
    clens: Vec<u8>,
    hclen: usize,
}

impl Dynamic {
    fn build(lit_freq: &[u32], dist_freq: &[u32]) -> Dynamic {
        let lit = code_lengths(&lit_freq[..286], 15);
        let dist = code_lengths(&dist_freq[..30], 15);
        let hlit = 257.max(lit.iter().rposition(|&l| l != 0).map_or(0, |p| p + 1));
        let hdist = 1.max(dist.iter().rposition(|&l| l != 0).map_or(0, |p| p + 1));
        let mut all = lit[..hlit].to_vec();
        all.extend_from_slice(&dist[..hdist]);
        let rle = rle_lengths(&all);
        let mut cfreq = [0u32; 19];
        for &(s, _) in &rle {
            cfreq[s as usize] += 1;
        }
        let clens = code_lengths(&cfreq, 7);
        let hclen = 4.max(
            CLEN_ORDER
                .iter()
                .rposition(|&i| clens[i] != 0)
                .map_or(0, |p| p + 1),
        );
        Dynamic {
            lit,
            dist,
            hlit,
            hdist,
            rle,
            clens,
            hclen,
        }
    }

    fn header_bits(&self) -> u64 {
        let mut bits = 5 + 5 + 4 + 3 * self.hclen as u64;
        for &(s, _) in &self.rle {
            bits += self.clens[s as usize] as u64 + extra_rle_bits(s) as u64;
        }
        bits
    }

    fn write_header(&self, w: &mut BitWriter) {
        w.put((self.hlit - 257) as u32, 5);
        w.put((self.hdist - 1) as u32, 5);
        w.put((self.hclen - 4) as u32, 4);
        for &i in CLEN_ORDER.iter().take(self.hclen) {
            w.put(self.clens[i] as u32, 3);
        }
        let codes = canonical_codes(&self.clens);
        for &(s, extra) in &self.rle {
            w.code(codes[s as usize], self.clens[s as usize]);
            let n = extra_rle_bits(s);
            if n > 0 {
                w.put(extra as u32, n);
            }
        }
    }
}

fn extra_rle_bits(sym: u8) -> u32 {
    match sym {
        16 => 2,
        17 => 3,
        18 => 7,
        _ => 0,
    }
}

/// Bits for the symbols of a block under the given code lengths.
fn body_bits(tokens: &[Token], lit: &[u8], dist: &[u8]) -> u64 {
    let mut bits = lit[256] as u64;
    for t in tokens {
        match *t {
            Token::Literal(b) => bits += lit[b as usize] as u64,
            Token::Match { len, dist: d } => {
                let lc = len_code(len as usize);
                let dc = dist_code(d as usize);
                bits += lit[257 + lc] as u64 + LEN_EXTRA[lc] as u64;
                bits += dist[dc] as u64 + DIST_EXTRA[dc] as u64;
            }
        }
    }
    bits
}

fn write_symbols(w: &mut BitWriter, tokens: &[Token], lit: &[u8], dist: &[u8]) {
    let lcodes = canonical_codes(lit);
    let dcodes = canonical_codes(dist);
    for t in tokens {
        match *t {
            Token::Literal(b) => w.code(lcodes[b as usize], lit[b as usize]),
            Token::Match { len, dist: d } => {
                let lc = len_code(len as usize);
                w.code(lcodes[257 + lc], lit[257 + lc]);
                if LEN_EXTRA[lc] > 0 {
                    w.put((len - LEN_BASE[lc]) as u32, LEN_EXTRA[lc] as u32);
                }
                let dc = dist_code(d as usize);
                w.code(dcodes[dc], dist[dc]);
                if DIST_EXTRA[dc] > 0 {
                    w.put((d - DIST_BASE[dc]) as u32, DIST_EXTRA[dc] as u32);
                }
            }
        }
    }
    w.code(lcodes[256], lit[256]);
}

fn write_stored(w: &mut BitWriter, bytes: &[u8], last: bool) {
    let mut chunks: Vec<&[u8]> = bytes.chunks(MAX_STORED).collect();
    if chunks.is_empty() {
        chunks.push(&[]);
    }
    let n = chunks.len();
    for (k, chunk) in chunks.into_iter().enumerate() {
        w.put((last && k + 1 == n) as u32, 1);
        w.put(0, 2);
        w.align();
        let len = chunk.len() as u16;
        w.put(len as u32, 16);
        w.put(!len as u32, 16);
        w.out.extend_from_slice(chunk);
    }
}

/// Compress `data` into a raw deflate stream.
pub fn deflate(data: &[u8], level: Level) -> Vec<u8> {
    let mut w = BitWriter::new();
    if level.0 == 0 {
        write_stored(&mut w, data, true);
        return w.finish();
    }
    let tokens = tokenize(data, level.0);
    if tokens.is_empty() {
        // Empty fixed block: BFINAL=1, BTYPE=01, end of block.
        w.put(1, 1);
        w.put(1, 2);
        w.put(0, 7);
        return w.finish();
    }
    let (fixed_lit, fixed_dist) = fixed_lengths();
    let mut start_byte = 0usize;
    let blocks: Vec<&[Token]> = tokens.chunks(BLOCK_TOKENS).collect();
    let nblocks = blocks.len();
    for (bi, block) in blocks.into_iter().enumerate() {
        let last = bi + 1 == nblocks;
        let mut lit_freq = [0u32; 288];
        let mut dist_freq = [0u32; 30];
        let mut nbytes = 0usize;
        for t in block {
            match *t {
                Token::Literal(b) => {
                    lit_freq[b as usize] += 1;
                    nbytes += 1;
                }
                Token::Match { len, dist } => {
                    lit_freq[257 + len_code(len as usize)] += 1;
                    dist_freq[dist_code(dist as usize)] += 1;
                    nbytes += len as usize;
                }
            }
        }
        lit_freq[256] = 1;
        let dynamic = Dynamic::build(&lit_freq, &dist_freq);
        let dyn_bits = 3 + dynamic.header_bits() + body_bits(block, &dynamic.lit, &dynamic.dist);
        let fixed_bits = 3 + body_bits(block, &fixed_lit, &fixed_dist);
        let stored_bits =
            (nbytes.div_ceil(MAX_STORED).max(1) as u64) * (3 + 7 + 32) + 8 * nbytes as u64;
        let raw = &data[start_byte..start_byte + nbytes];
        if stored_bits <= dyn_bits.min(fixed_bits) {
            write_stored(&mut w, raw, last);
        } else if fixed_bits <= dyn_bits {
            w.put(last as u32, 1);
            w.put(1, 2);
            write_symbols(&mut w, block, &fixed_lit, &fixed_dist);
        } else {
            w.put(last as u32, 1);
            w.put(2, 2);
            dynamic.write_header(&mut w);
            write_symbols(&mut w, block, &dynamic.lit, &dynamic.dist);
        }
        start_byte += nbytes;
    }
    w.finish()
}

/// Compress into a zlib stream (RFC 1950).
pub fn zlib_compress(data: &[u8], level: Level) -> Vec<u8> {
    let flevel: u8 = match level.0 {
        0 | 1 => 0,
        2..=5 => 1,
        6 => 2,
        _ => 3,
    };
    let cmf: u8 = 0x78;
    let mut flg: u8 = flevel << 6;
    flg += (31 - ((cmf as u16 * 256 + flg as u16) % 31) as u8) % 31;
    let mut out = vec![cmf, flg];
    out.extend(deflate(data, level));
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

/// Compress into a single-member gzip file (RFC 1952).
pub fn gzip_compress(data: &[u8], level: Level) -> Vec<u8> {
    // No name, no mtime; OS = 3 (Unix).
    let mut out = vec![0x1F, 0x8B, 8, 0, 0, 0, 0, 0, 0, 3];
    out.extend(deflate(data, level));
    out.extend_from_slice(&crc32(data).to_le_bytes());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lengths_respect_limit() {
        // Fibonacci frequencies force deep trees without a limit.
        let mut f = vec![1u32, 1];
        while f.len() < 30 {
            let n = f[f.len() - 1] + f[f.len() - 2];
            f.push(n);
        }
        let l = code_lengths(&f, 7);
        assert!(l.iter().all(|&x| (1..=7).contains(&x)));
        let kraft: f64 = l.iter().map(|&x| 0.5f64.powi(x as i32)).sum();
        assert!(kraft <= 1.0);
    }

    #[test]
    fn single_symbol_gets_complete_code() {
        let mut f = vec![0u32; 30];
        f[5] = 10;
        let l = code_lengths(&f, 15);
        assert_eq!(l.iter().filter(|&&x| x > 0).count(), 2);
        assert_eq!(l[5], 1);
    }
}
