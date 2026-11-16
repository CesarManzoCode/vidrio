//! The base64 and hex algorithms of `Uint8Array` (feature
//! `uint8array-base64`): FromBase64, FromHex and the encoders, on the code
//! units of a string. They never run user code.

/// What FromBase64 does with a final chunk of fewer than four characters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LastChunk {
    Loose,
    Strict,
    StopBeforePartial,
}

/// The result of FromBase64 or FromHex: the code units read, the decoded
/// bytes, and whether decoding stopped at an error (a SyntaxError).
#[derive(Debug, PartialEq, Eq)]
pub struct Decoded {
    pub read: usize,
    pub bytes: Vec<u8>,
    pub error: bool,
}

const STANDARD: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const URL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// SkipAsciiWhitespace(string, index).
fn skip_ascii_whitespace(units: &[u16], mut index: usize) -> usize {
    while index < units.len() && matches!(units[index], 0x09 | 0x0a | 0x0c | 0x0d | 0x20) {
        index += 1;
    }
    index
}

/// The 6-bit value of a character of the standard alphabet.
fn sextet(c: u16) -> Option<u32> {
    let c = u8::try_from(c).ok()?;
    STANDARD.iter().position(|&a| a == c).map(|p| p as u32)
}

/// DecodeBase64Chunk(chunk, throwOnExtraBits): `None` if the padding bits
/// must be zero and are not.
fn decode_chunk(chunk: &[u32], throw_on_extra_bits: bool) -> Option<Vec<u8>> {
    let mut n = 0u32;
    for i in 0..4 {
        n = (n << 6) | chunk.get(i).copied().unwrap_or(0);
    }
    let bytes = [(n >> 16) as u8, (n >> 8) as u8, n as u8];
    match chunk.len() {
        2 => {
            if throw_on_extra_bits && bytes[1] != 0 {
                return None;
            }
            Some(vec![bytes[0]])
        }
        3 => {
            if throw_on_extra_bits && bytes[2] != 0 {
                return None;
            }
            Some(vec![bytes[0], bytes[1]])
        }
        _ => Some(bytes.to_vec()),
    }
}

/// FromBase64(string, alphabet, lastChunkHandling, maxLength).
pub fn from_base64(units: &[u16], url: bool, last: LastChunk, max_length: usize) -> Decoded {
    let mut out = Decoded {
        read: 0,
        bytes: Vec::new(),
        error: false,
    };
    if max_length == 0 {
        return out;
    }
    let length = units.len();
    let mut chunk: Vec<u32> = Vec::with_capacity(4);
    let mut index = 0;
    loop {
        index = skip_ascii_whitespace(units, index);
        if index == length {
            if !chunk.is_empty() {
                match last {
                    LastChunk::StopBeforePartial => return out,
                    LastChunk::Loose => {
                        if chunk.len() == 1 {
                            out.error = true;
                            return out;
                        }
                        out.bytes
                            .extend(decode_chunk(&chunk, false).unwrap_or_default());
                    }
                    LastChunk::Strict => {
                        out.error = true;
                        return out;
                    }
                }
            }
            out.read = length;
            return out;
        }
        let mut c = units[index];
        index += 1;
        if c == u16::from(b'=') {
            if chunk.len() < 2 {
                out.error = true;
                return out;
            }
            index = skip_ascii_whitespace(units, index);
            if chunk.len() == 2 {
                if index == length {
                    if last != LastChunk::StopBeforePartial {
                        out.error = true;
                    }
                    return out;
                }
                if units[index] == u16::from(b'=') {
                    index = skip_ascii_whitespace(units, index + 1);
                }
            }
            if index < length {
                out.error = true;
                return out;
            }
            let Some(decoded) = decode_chunk(&chunk, last == LastChunk::Strict) else {
                out.error = true;
                return out;
            };
            out.bytes.extend(decoded);
            out.read = length;
            return out;
        }
        if url {
            if c == u16::from(b'+') || c == u16::from(b'/') {
                out.error = true;
                return out;
            }
            if c == u16::from(b'-') {
                c = u16::from(b'+');
            } else if c == u16::from(b'_') {
                c = u16::from(b'/');
            }
        }
        let Some(v) = sextet(c) else {
            out.error = true;
            return out;
        };
        let remaining = max_length - out.bytes.len();
        if (remaining == 1 && chunk.len() == 2) || (remaining == 2 && chunk.len() == 3) {
            return out;
        }
        chunk.push(v);
        if chunk.len() == 4 {
            out.bytes
                .extend(decode_chunk(&chunk, false).unwrap_or_default());
            chunk.clear();
            out.read = index;
            if out.bytes.len() == max_length {
                return out;
            }
        }
    }
}

fn hex_value(c: u16) -> Option<u8> {
    match c {
        0x30..=0x39 => Some((c - 0x30) as u8),
        0x41..=0x46 => Some((c - 0x41 + 10) as u8),
        0x61..=0x66 => Some((c - 0x61 + 10) as u8),
        _ => None,
    }
}

/// FromHex(string, maxLength).
pub fn from_hex(units: &[u16], max_length: usize) -> Decoded {
    let mut out = Decoded {
        read: 0,
        bytes: Vec::new(),
        error: false,
    };
    if units.len() % 2 != 0 {
        out.error = true;
        return out;
    }
    while out.read < units.len() && out.bytes.len() < max_length {
        let (Some(hi), Some(lo)) = (hex_value(units[out.read]), hex_value(units[out.read + 1]))
        else {
            out.error = true;
            return out;
        };
        out.read += 2;
        out.bytes.push((hi << 4) | lo);
    }
    out
}

/// The base64 encoding of `bytes` (`url` selects base64url).
pub fn to_base64(bytes: &[u8], url: bool, omit_padding: bool) -> String {
    let alphabet = if url { URL } else { STANDARD };
    let mut s = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(chunk.get(1).copied().unwrap_or(0)) << 8)
            | u32::from(chunk.get(2).copied().unwrap_or(0));
        let chars = chunk.len() + 1;
        for i in 0..4 {
            if i < chars {
                s.push(alphabet[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else if !omit_padding {
                s.push('=');
            }
        }
    }
    s
}

/// The lowercase hex encoding of `bytes`.
pub fn to_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push(DIGITS[usize::from(b >> 4)] as char);
        s.push(DIGITS[usize::from(b & 15)] as char);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    fn decode(s: &str, url: bool, last: LastChunk) -> Decoded {
        from_base64(&u(s), url, last, usize::MAX)
    }

    #[test]
    fn encodes_with_both_alphabets() {
        assert_eq!(to_base64(b"\xfb\xff", false, false), "+/8=");
        assert_eq!(to_base64(b"\xfb\xff", true, false), "-_8=");
        assert_eq!(to_base64(b"\xfb\xff", true, true), "-_8");
        assert_eq!(to_base64(b"foobar", false, false), "Zm9vYmFy");
        assert_eq!(to_hex(&[0, 0xab, 0x10]), "00ab10");
    }

    #[test]
    fn last_chunk_handling() {
        let d = decode("Zm9vYg", false, LastChunk::Loose);
        assert_eq!(
            (d.bytes.as_slice(), d.read, d.error),
            (&b"foob"[..], 6, false)
        );
        let d = decode("Zm9vYg", false, LastChunk::Strict);
        assert!(d.error);
        assert_eq!(d.bytes, b"foo");
        let d = decode("Zm9vYg", false, LastChunk::StopBeforePartial);
        assert_eq!(
            (d.bytes.as_slice(), d.read, d.error),
            (&b"foo"[..], 4, false)
        );
        // Non-zero padding bits only fail in strict mode.
        assert!(decode("Zm9vYh==", false, LastChunk::Strict).error);
        assert_eq!(decode("Zm9vYh==", false, LastChunk::Loose).bytes, b"foob");
        // A lone character in the last chunk is always an error.
        assert!(decode("Zm9vY", false, LastChunk::Loose).error);
        assert!(decode("Zm9v=", false, LastChunk::Loose).error);
        assert!(decode("Zg=", false, LastChunk::Loose).error);
        assert!(!decode("Zg=", false, LastChunk::StopBeforePartial).error);
        assert!(decode("Zg==x", false, LastChunk::Loose).error);
        assert!(decode("-_8=", false, LastChunk::Loose).error);
        assert_eq!(decode(" -_8 = ", true, LastChunk::Loose).bytes, b"\xfb\xff");
        assert!(decode("+/8=", true, LastChunk::Loose).error);
    }

    #[test]
    fn max_length_stops_on_a_chunk_boundary() {
        let d = from_base64(&u("Zm9vYmFy"), false, LastChunk::Loose, 4);
        assert_eq!(
            (d.bytes.as_slice(), d.read, d.error),
            (&b"foo"[..], 4, false)
        );
        let d = from_hex(&u("aabbcc"), 2);
        assert_eq!((d.bytes, d.read, d.error), (vec![0xaa, 0xbb], 4, false));
        assert!(from_hex(&u("abc"), 9).error);
        let d = from_hex(&u("aaxx"), 9);
        assert_eq!((d.bytes, d.read, d.error), (vec![0xaa], 2, true));
    }
}
