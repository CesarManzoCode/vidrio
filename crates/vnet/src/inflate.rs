//! Streaming inflate (RFC 1951) with zlib (RFC 1950) and gzip (RFC 1952) wrappers.
//!
//! The decoder accepts input in arbitrary pieces. Before each symbol it records
//! the bit position; when the input runs out mid-symbol it rewinds to that
//! position and waits for more data, so no partial state is kept below the
//! symbol level.

use crate::checksum::{Adler32, Crc32};
use std::fmt;

/// Back-reference window of deflate.
const WINDOW: usize = 32 * 1024;

pub(crate) const LEN_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
pub(crate) const LEN_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
pub(crate) const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
pub(crate) const DIST_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];
/// Order in which code length code lengths are stored (RFC 1951 §3.2.7).
pub(crate) const CLEN_ORDER: [usize; 19] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InflateError {
    InvalidBlockType,
    StoredLengthMismatch,
    InvalidCode,
    OversubscribedCode,
    InvalidCodeLengths,
    DistanceTooFar,
    BadHeader(&'static str),
    ChecksumMismatch,
    LengthMismatch,
    PresetDictionary,
    TrailingData,
    Truncated,
}

impl fmt::Display for InflateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InflateError::InvalidBlockType => write!(f, "invalid deflate block type"),
            InflateError::StoredLengthMismatch => write!(f, "stored block length mismatch"),
            InflateError::InvalidCode => write!(f, "invalid Huffman code"),
            InflateError::OversubscribedCode => write!(f, "over-subscribed Huffman code"),
            InflateError::InvalidCodeLengths => write!(f, "invalid code lengths"),
            InflateError::DistanceTooFar => write!(f, "distance too far back"),
            InflateError::BadHeader(what) => write!(f, "bad header: {what}"),
            InflateError::ChecksumMismatch => write!(f, "checksum mismatch"),
            InflateError::LengthMismatch => write!(f, "length mismatch"),
            InflateError::PresetDictionary => write!(f, "preset dictionary not supported"),
            InflateError::TrailingData => write!(f, "trailing data after stream"),
            InflateError::Truncated => write!(f, "truncated stream"),
        }
    }
}

impl std::error::Error for InflateError {}

/// Bits of the fast lookup table.
const FAST_BITS: u32 = 9;

/// Canonical Huffman decoding table.
pub(crate) struct Huffman {
    counts: [u16; 16],
    symbols: Vec<u16>,
    /// Indexed by the next `FAST_BITS` input bits: `(symbol << 4) | length`, 0 = slow path.
    fast: Vec<u32>,
}

impl Huffman {
    pub(crate) fn new(lengths: &[u8]) -> Result<Huffman, InflateError> {
        let mut counts = [0u16; 16];
        for &l in lengths {
            counts[l as usize] += 1;
        }
        counts[0] = 0;
        let mut left: i32 = 1;
        for len in 1..16 {
            left <<= 1;
            left -= counts[len] as i32;
            if left < 0 {
                return Err(InflateError::OversubscribedCode);
            }
        }
        let mut offs = [0u16; 16];
        for len in 1..15 {
            offs[len + 1] = offs[len] + counts[len];
        }
        let mut symbols = vec![0u16; lengths.len()];
        for (sym, &l) in lengths.iter().enumerate() {
            if l != 0 {
                symbols[offs[l as usize] as usize] = sym as u16;
                offs[l as usize] += 1;
            }
        }
        // Canonical codes for the fast table (RFC 1951 §3.2.2; counts[0] is zero).
        let mut next = [0u32; 16];
        let mut code = 0u32;
        for len in 1..16 {
            code = (code + counts[len - 1] as u32) << 1;
            next[len] = code;
        }
        let mut fast = vec![0u32; 1 << FAST_BITS];
        for (sym, &l) in lengths.iter().enumerate() {
            let l = l as u32;
            if l == 0 {
                continue;
            }
            let c = next[l as usize];
            next[l as usize] += 1;
            if l <= FAST_BITS {
                let rev = reverse_bits(c, l);
                let entry = ((sym as u32) << 4) | l;
                let mut i = rev as usize;
                while i < fast.len() {
                    fast[i] = entry;
                    i += 1 << l;
                }
            }
        }
        Ok(Huffman {
            counts,
            symbols,
            fast,
        })
    }

    fn fixed_literal() -> Huffman {
        let mut lengths = [0u8; 288];
        for (i, l) in lengths.iter_mut().enumerate() {
            *l = match i {
                0..=143 => 8,
                144..=255 => 9,
                256..=279 => 7,
                _ => 8,
            };
        }
        Huffman::new(&lengths).expect("fixed literal code")
    }

    fn fixed_distance() -> Huffman {
        Huffman::new(&[5u8; 30]).expect("fixed distance code")
    }
}

pub(crate) fn reverse_bits(code: u32, len: u32) -> u32 {
    let mut r = 0;
    let mut c = code;
    for _ in 0..len {
        r = (r << 1) | (c & 1);
        c >>= 1;
    }
    r
}

/// Input buffer read least-significant bit first.
struct Bits {
    buf: Vec<u8>,
    /// Position in bits from the start of `buf`.
    pos: usize,
}

impl Bits {
    fn available(&self) -> usize {
        self.buf.len() * 8 - self.pos
    }

    /// Next `n` (≤ 25) bits without consuming them; missing bits read as zero.
    fn peek(&self, n: u32) -> u32 {
        let byte = self.pos / 8;
        let off = self.pos % 8;
        let mut v: u64 = 0;
        for i in 0..5 {
            if let Some(&b) = self.buf.get(byte + i) {
                v |= (b as u64) << (8 * i);
            }
        }
        ((v >> off) & ((1u64 << n) - 1)) as u32
    }

    fn take(&mut self, n: u32) -> Option<u32> {
        if self.available() < n as usize {
            return None;
        }
        let v = self.peek(n);
        self.pos += n as usize;
        Some(v)
    }

    fn align(&mut self) {
        self.pos = self.pos.div_ceil(8) * 8;
    }

    /// Drop fully consumed bytes.
    fn compact(&mut self) {
        let bytes = self.pos / 8;
        if bytes > 0 {
            self.buf.drain(..bytes);
            self.pos -= bytes * 8;
        }
    }

    /// Decode one symbol; `Ok(None)` when more input is needed.
    fn decode(&mut self, h: &Huffman) -> Result<Option<u16>, InflateError> {
        let avail = self.available();
        let entry = h.fast[self.peek(FAST_BITS) as usize];
        if entry != 0 {
            let len = (entry & 15) as usize;
            if len > avail {
                return Ok(None);
            }
            self.pos += len;
            return Ok(Some((entry >> 4) as u16));
        }
        // Slow path, one bit at a time (as in zlib's puff).
        let mut p = self.pos;
        let mut code: i32 = 0;
        let mut first: i32 = 0;
        let mut index: i32 = 0;
        for len in 1..16 {
            if p >= self.buf.len() * 8 {
                return Ok(None);
            }
            code |= ((self.buf[p / 8] >> (p % 8)) & 1) as i32;
            p += 1;
            let count = h.counts[len] as i32;
            if code - count < first {
                self.pos = p;
                return Ok(Some(h.symbols[(index + (code - first)) as usize]));
            }
            index += count;
            first += count;
            first <<= 1;
            code <<= 1;
        }
        Err(InflateError::InvalidCode)
    }
}

enum State {
    BlockHeader,
    Stored {
        remaining: usize,
    },
    Codes {
        lit: Box<Huffman>,
        dist: Box<Huffman>,
    },
    Done,
}

/// Raw deflate decoder (RFC 1951) fed in pieces.
pub struct Inflater {
    bits: Bits,
    state: State,
    last_block: bool,
    /// Output history; holds at least the last `WINDOW` bytes.
    window: Vec<u8>,
}

impl Default for Inflater {
    fn default() -> Self {
        Self::new()
    }
}

impl Inflater {
    pub fn new() -> Inflater {
        Inflater {
            bits: Bits {
                buf: Vec::new(),
                pos: 0,
            },
            state: State::BlockHeader,
            last_block: false,
            window: Vec::new(),
        }
    }

    /// True once the final block has been decoded.
    pub fn is_done(&self) -> bool {
        matches!(self.state, State::Done)
    }

    /// Bytes after the end of the deflate stream (from the next byte boundary).
    /// Only meaningful once `is_done()`.
    pub fn take_remaining(&mut self) -> Vec<u8> {
        self.bits.align();
        self.bits.compact();
        std::mem::take(&mut self.bits.buf)
    }

    /// Feed `input` and append everything that can be decoded to `out`.
    pub fn push(&mut self, input: &[u8], out: &mut Vec<u8>) -> Result<(), InflateError> {
        self.bits.buf.extend_from_slice(input);
        let start = self.window.len();
        let result = self.run();
        out.extend_from_slice(&self.window[start..]);
        if self.window.len() > 4 * WINDOW {
            let cut = self.window.len() - WINDOW;
            self.window.drain(..cut);
        }
        if !self.is_done() {
            self.bits.compact();
        }
        result
    }

    fn run(&mut self) -> Result<(), InflateError> {
        loop {
            match &mut self.state {
                State::Done => return Ok(()),
                State::BlockHeader => {
                    if self.last_block {
                        self.state = State::Done;
                        return Ok(());
                    }
                    let save = self.bits.pos;
                    match self.block_header()? {
                        Some(state) => self.state = state,
                        None => {
                            self.bits.pos = save;
                            return Ok(());
                        }
                    }
                }
                State::Stored { remaining } => {
                    let byte = self.bits.pos / 8;
                    let n = (*remaining).min(self.bits.buf.len() - byte);
                    self.window
                        .extend_from_slice(&self.bits.buf[byte..byte + n]);
                    self.bits.pos += n * 8;
                    *remaining -= n;
                    if *remaining > 0 {
                        return Ok(());
                    }
                    self.state = State::BlockHeader;
                }
                State::Codes { lit, dist } => {
                    let end = decode_codes(&mut self.bits, &mut self.window, &**lit, &**dist)?;
                    if !end {
                        return Ok(());
                    }
                    self.state = State::BlockHeader;
                }
            }
        }
    }

    fn block_header(&mut self) -> Result<Option<State>, InflateError> {
        let Some(bfinal) = self.bits.take(1) else {
            return Ok(None);
        };
        let Some(btype) = self.bits.take(2) else {
            return Ok(None);
        };
        let state = match btype {
            0 => {
                self.bits.align();
                let Some(len) = self.bits.take(16) else {
                    return Ok(None);
                };
                let Some(nlen) = self.bits.take(16) else {
                    return Ok(None);
                };
                if len != !nlen & 0xFFFF {
                    return Err(InflateError::StoredLengthMismatch);
                }
                State::Stored {
                    remaining: len as usize,
                }
            }
            1 => State::Codes {
                lit: Box::new(Huffman::fixed_literal()),
                dist: Box::new(Huffman::fixed_distance()),
            },
            2 => {
                let Some((lit, dist)) = read_dynamic(&mut self.bits)? else {
                    return Ok(None);
                };
                State::Codes {
                    lit: Box::new(lit),
                    dist: Box::new(dist),
                }
            }
            _ => return Err(InflateError::InvalidBlockType),
        };
        self.last_block = bfinal == 1;
        Ok(Some(state))
    }
}

fn read_dynamic(bits: &mut Bits) -> Result<Option<(Huffman, Huffman)>, InflateError> {
    macro_rules! take {
        ($n:expr) => {
            match bits.take($n) {
                Some(v) => v,
                None => return Ok(None),
            }
        };
    }
    let hlit = take!(5) as usize + 257;
    let hdist = take!(5) as usize + 1;
    let hclen = take!(4) as usize + 4;
    if hlit > 286 || hdist > 30 {
        return Err(InflateError::InvalidCodeLengths);
    }
    let mut clens = [0u8; 19];
    for &i in CLEN_ORDER.iter().take(hclen) {
        clens[i] = take!(3) as u8;
    }
    let clcode = Huffman::new(&clens)?;
    let mut lengths = vec![0u8; hlit + hdist];
    let mut i = 0;
    while i < hlit + hdist {
        let sym = match bits.decode(&clcode)? {
            Some(s) => s,
            None => return Ok(None),
        };
        let (value, repeat) = match sym {
            0..=15 => (sym as u8, 1),
            16 => {
                if i == 0 {
                    return Err(InflateError::InvalidCodeLengths);
                }
                (lengths[i - 1], 3 + take!(2) as usize)
            }
            17 => (0, 3 + take!(3) as usize),
            18 => (0, 11 + take!(7) as usize),
            _ => return Err(InflateError::InvalidCodeLengths),
        };
        if i + repeat > hlit + hdist {
            return Err(InflateError::InvalidCodeLengths);
        }
        for _ in 0..repeat {
            lengths[i] = value;
            i += 1;
        }
    }
    if lengths[256] == 0 {
        return Err(InflateError::InvalidCodeLengths);
    }
    let lit = Huffman::new(&lengths[..hlit])?;
    let dist = Huffman::new(&lengths[hlit..])?;
    Ok(Some((lit, dist)))
}

/// Decode literal/length symbols until end of block (`Ok(true)`) or end of input (`Ok(false)`).
fn decode_codes(
    bits: &mut Bits,
    window: &mut Vec<u8>,
    lit: &Huffman,
    dist: &Huffman,
) -> Result<bool, InflateError> {
    loop {
        let save = bits.pos;
        let Some(sym) = bits.decode(lit)? else {
            return Ok(false);
        };
        if sym < 256 {
            window.push(sym as u8);
            continue;
        }
        if sym == 256 {
            return Ok(true);
        }
        let li = (sym - 257) as usize;
        if li >= 29 {
            return Err(InflateError::InvalidCode);
        }
        let Some(extra) = bits.take(LEN_EXTRA[li] as u32) else {
            bits.pos = save;
            return Ok(false);
        };
        let len = LEN_BASE[li] as usize + extra as usize;
        let Some(dsym) = bits.decode(dist)? else {
            bits.pos = save;
            return Ok(false);
        };
        let di = dsym as usize;
        if di >= 30 {
            return Err(InflateError::InvalidCode);
        }
        let Some(dextra) = bits.take(DIST_EXTRA[di] as u32) else {
            bits.pos = save;
            return Ok(false);
        };
        let d = DIST_BASE[di] as usize + dextra as usize;
        if d > window.len() {
            return Err(InflateError::DistanceTooFar);
        }
        let from = window.len() - d;
        for k in 0..len {
            let b = window[from + k];
            window.push(b);
        }
    }
}

/// Container around the deflate data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Raw,
    Zlib,
    Gzip,
    /// zlib if the first two bytes form a valid zlib header, raw otherwise
    /// (HTTP `deflate` as sent by servers in practice).
    ZlibOrRaw,
}

enum Phase {
    Header,
    Body,
    Trailer,
    Done,
}

/// Streaming decoder for raw, zlib and gzip data.
pub struct Decoder {
    format: Format,
    phase: Phase,
    pending: Vec<u8>,
    inflater: Inflater,
    crc: Crc32,
    adler: Adler32,
    size: u32,
    members: usize,
}

impl Decoder {
    pub fn new(format: Format) -> Decoder {
        Decoder {
            format,
            phase: Phase::Header,
            pending: Vec::new(),
            inflater: Inflater::new(),
            crc: Crc32::new(),
            adler: Adler32::new(),
            size: 0,
            members: 0,
        }
    }

    pub fn is_done(&self) -> bool {
        matches!(self.phase, Phase::Done)
    }

    /// Feed compressed bytes; decoded bytes are appended to `out`.
    pub fn push(&mut self, input: &[u8], out: &mut Vec<u8>) -> Result<(), InflateError> {
        self.pending.extend_from_slice(input);
        loop {
            match self.phase {
                Phase::Header => {
                    if self.pending.is_empty() {
                        return Ok(());
                    }
                    let Some(used) = self.header()? else {
                        return Ok(());
                    };
                    self.pending.drain(..used);
                    self.phase = Phase::Body;
                }
                Phase::Body => {
                    let data = std::mem::take(&mut self.pending);
                    let start = out.len();
                    self.inflater.push(&data, out)?;
                    self.crc.update(&out[start..]);
                    self.adler.update(&out[start..]);
                    self.size = self.size.wrapping_add((out.len() - start) as u32);
                    if !self.inflater.is_done() {
                        return Ok(());
                    }
                    self.pending = self.inflater.take_remaining();
                    self.phase = Phase::Trailer;
                }
                Phase::Trailer => {
                    let need = match self.format {
                        Format::Raw => 0,
                        Format::Zlib => 4,
                        Format::Gzip => 8,
                        Format::ZlibOrRaw => unreachable!(),
                    };
                    if self.pending.len() < need {
                        return Ok(());
                    }
                    let t = &self.pending[..need];
                    match self.format {
                        Format::Zlib => {
                            let expect = u32::from_be_bytes([t[0], t[1], t[2], t[3]]);
                            if expect != self.adler.value() {
                                return Err(InflateError::ChecksumMismatch);
                            }
                        }
                        Format::Gzip => {
                            let crc = u32::from_le_bytes([t[0], t[1], t[2], t[3]]);
                            let size = u32::from_le_bytes([t[4], t[5], t[6], t[7]]);
                            if crc != self.crc.value() {
                                return Err(InflateError::ChecksumMismatch);
                            }
                            if size != self.size {
                                return Err(InflateError::LengthMismatch);
                            }
                        }
                        _ => {}
                    }
                    self.pending.drain(..need);
                    self.members += 1;
                    self.phase = Phase::Done;
                }
                Phase::Done => {
                    if self.pending.is_empty() {
                        return Ok(());
                    }
                    if self.format != Format::Gzip {
                        return Err(InflateError::TrailingData);
                    }
                    // Another gzip member follows.
                    self.inflater = Inflater::new();
                    self.crc = Crc32::new();
                    self.size = 0;
                    self.phase = Phase::Header;
                }
            }
        }
    }

    /// Signal end of input; fails if the stream is incomplete.
    pub fn finish(&mut self) -> Result<(), InflateError> {
        match self.phase {
            Phase::Done => Ok(()),
            Phase::Header if self.members > 0 && self.pending.is_empty() => Ok(()),
            _ => Err(InflateError::Truncated),
        }
    }

    /// Parse the wrapper header from `pending`; returns its length, or `None` if incomplete.
    fn header(&mut self) -> Result<Option<usize>, InflateError> {
        let p = &self.pending;
        match self.format {
            Format::Raw => Ok(Some(0)),
            Format::ZlibOrRaw => {
                if p.len() < 2 {
                    return Ok(None);
                }
                let valid = p[0] & 0x0F == 8
                    && p[0] >> 4 <= 7
                    && (p[0] as u16 * 256 + p[1] as u16) % 31 == 0
                    && p[1] & 0x20 == 0;
                self.format = if valid { Format::Zlib } else { Format::Raw };
                self.header()
            }
            Format::Zlib => {
                if p.len() < 2 {
                    return Ok(None);
                }
                if p[0] & 0x0F != 8 || p[0] >> 4 > 7 {
                    return Err(InflateError::BadHeader("zlib method"));
                }
                if (p[0] as u16 * 256 + p[1] as u16) % 31 != 0 {
                    return Err(InflateError::BadHeader("zlib check bits"));
                }
                if p[1] & 0x20 != 0 {
                    return Err(InflateError::PresetDictionary);
                }
                Ok(Some(2))
            }
            Format::Gzip => {
                if p.len() < 10 {
                    return Ok(None);
                }
                if p[0] != 0x1F || p[1] != 0x8B {
                    return Err(InflateError::BadHeader("gzip magic"));
                }
                if p[2] != 8 {
                    return Err(InflateError::BadHeader("gzip method"));
                }
                let flags = p[3];
                if flags & 0xE0 != 0 {
                    return Err(InflateError::BadHeader("gzip reserved flags"));
                }
                let mut i = 10;
                if flags & 0x04 != 0 {
                    if p.len() < i + 2 {
                        return Ok(None);
                    }
                    let xlen = u16::from_le_bytes([p[i], p[i + 1]]) as usize;
                    i += 2 + xlen;
                    if p.len() < i {
                        return Ok(None);
                    }
                }
                for flag in [0x08, 0x10] {
                    if flags & flag != 0 {
                        match p[i.min(p.len())..].iter().position(|&b| b == 0) {
                            Some(z) => i += z + 1,
                            None => return Ok(None),
                        }
                    }
                }
                if flags & 0x02 != 0 {
                    if p.len() < i + 2 {
                        return Ok(None);
                    }
                    let mut c = Crc32::new();
                    c.update(&p[..i]);
                    if u16::from_le_bytes([p[i], p[i + 1]]) != (c.value() & 0xFFFF) as u16 {
                        return Err(InflateError::BadHeader("gzip header CRC"));
                    }
                    i += 2;
                }
                Ok(Some(i))
            }
        }
    }
}

fn decode_all(format: Format, data: &[u8]) -> Result<Vec<u8>, InflateError> {
    let mut d = Decoder::new(format);
    let mut out = Vec::new();
    d.push(data, &mut out)?;
    d.finish()?;
    Ok(out)
}

/// Decode a complete raw deflate stream.
pub fn inflate(data: &[u8]) -> Result<Vec<u8>, InflateError> {
    decode_all(Format::Raw, data)
}

/// Decode a complete zlib stream.
pub fn zlib_decompress(data: &[u8]) -> Result<Vec<u8>, InflateError> {
    decode_all(Format::Zlib, data)
}

/// Decode a complete gzip file (one or more members).
pub fn gzip_decompress(data: &[u8]) -> Result<Vec<u8>, InflateError> {
    decode_all(Format::Gzip, data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn huffman_rejects_oversubscribed() {
        assert_eq!(
            Huffman::new(&[1, 1, 1]).err(),
            Some(InflateError::OversubscribedCode)
        );
    }

    #[test]
    fn empty_fixed_block() {
        assert_eq!(inflate(&[0x03, 0x00]).unwrap(), b"");
    }
}
