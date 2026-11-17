//! Hosts (URL §3): the host parser with its IPv4, IPv6, opaque-host and
//! domain branches, domain to ASCII and to Unicode, and the host
//! serializer.

use std::fmt;

use super::idna;
use super::percent::{PercentEncodeSet, percent_decode_str, utf8_percent_encode};
use crate::encoding::utf8_decode_without_bom;

/// A host (URL §3.1).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Host {
    /// An ASCII domain, already through domain to ASCII.
    Domain(String),
    Ipv4(u32),
    Ipv6([u16; 8]),
    /// A percent-encoded host of a non-special URL.
    Opaque(String),
    /// The empty host.
    Empty,
}

impl fmt::Display for Host {
    /// The host serializer (URL §3.6).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Host::Domain(s) | Host::Opaque(s) => f.write_str(s),
            Host::Empty => Ok(()),
            Host::Ipv4(address) => f.write_str(&serialize_ipv4(*address)),
            Host::Ipv6(address) => write!(f, "[{}]", serialize_ipv6(address)),
        }
    }
}

/// A forbidden host code point (URL §3.1).
pub fn is_forbidden_host_code_point(c: char) -> bool {
    matches!(
        c,
        '\0' | '\t'
            | '\n'
            | '\r'
            | ' '
            | '#'
            | '/'
            | ':'
            | '<'
            | '>'
            | '?'
            | '@'
            | '['
            | '\\'
            | ']'
            | '^'
            | '|'
    )
}

/// A forbidden domain code point (URL §3.1).
pub fn is_forbidden_domain_code_point(c: char) -> bool {
    is_forbidden_host_code_point(c) || (c as u32) <= 0x1F || c == '%' || c == '\u{7F}'
}

/// The host parser (URL §3.5). `is_opaque` is true for non-special URLs.
pub fn parse_host(input: &str, is_opaque: bool) -> Option<Host> {
    if let Some(rest) = input.strip_prefix('[') {
        let inner = rest.strip_suffix(']')?;
        return parse_ipv6(inner).map(Host::Ipv6);
    }
    if is_opaque {
        return parse_opaque_host(input);
    }
    debug_assert!(!input.is_empty());
    let domain = utf8_decode_without_bom(&percent_decode_str(input));
    let ascii_domain = domain_to_ascii(&domain, false)?;
    if ends_in_a_number(&ascii_domain) {
        return parse_ipv4(&ascii_domain).map(Host::Ipv4);
    }
    Some(Host::Domain(ascii_domain))
}

/// Domain to ASCII (URL §3.3).
pub fn domain_to_ascii(domain: &str, be_strict: bool) -> Option<String> {
    let result = idna::to_ascii(domain, be_strict)?;
    if result.is_empty() {
        return None;
    }
    if !be_strict && result.chars().any(is_forbidden_domain_code_point) {
        return None;
    }
    Some(result)
}

/// Domain to Unicode (URL §3.3): the Unicode form; errors are only
/// validation errors there, so the text is returned in every case.
pub fn domain_to_unicode(domain: &str, be_strict: bool) -> String {
    idna::to_unicode(domain, be_strict).0
}

/// The opaque-host parser (URL §3.5).
fn parse_opaque_host(input: &str) -> Option<Host> {
    if input.chars().any(is_forbidden_host_code_point) {
        return None;
    }
    Some(Host::Opaque(utf8_percent_encode(
        input,
        PercentEncodeSet::C0Control,
    )))
}

/// The ends-in-a-number checker (URL §3.5).
pub fn ends_in_a_number(input: &str) -> bool {
    let mut parts: Vec<&str> = input.split('.').collect();
    if parts.last() == Some(&"") {
        if parts.len() == 1 {
            return false;
        }
        parts.pop();
    }
    let last = parts.last().copied().unwrap_or("");
    if !last.is_empty() && last.bytes().all(|b| b.is_ascii_digit()) {
        return true;
    }
    parse_ipv4_number(last).is_some()
}

/// The IPv4 number parser (URL §3.5): the value, saturated at `u64::MAX`.
fn parse_ipv4_number(input: &str) -> Option<u64> {
    if input.is_empty() {
        return None;
    }
    let (digits, radix) = if let Some(rest) = input
        .strip_prefix("0x")
        .or_else(|| input.strip_prefix("0X"))
    {
        (rest, 16)
    } else if input.len() >= 2 && input.starts_with('0') {
        (&input[1..], 8)
    } else {
        (input, 10)
    };
    if digits.is_empty() {
        return Some(0);
    }
    let mut value: u64 = 0;
    for c in digits.chars() {
        let d = c.to_digit(radix)?;
        value = value
            .saturating_mul(u64::from(radix))
            .saturating_add(u64::from(d));
    }
    Some(value)
}

/// The IPv4 parser (URL §3.5).
pub fn parse_ipv4(input: &str) -> Option<u32> {
    let mut parts: Vec<&str> = input.split('.').collect();
    if parts.last() == Some(&"") && parts.len() > 1 {
        parts.pop();
    }
    if parts.len() > 4 {
        return None;
    }
    let mut numbers = Vec::with_capacity(parts.len());
    for part in parts {
        numbers.push(parse_ipv4_number(part)?);
    }
    let (&last, rest) = numbers.split_last()?;
    if rest.iter().any(|&n| n > 255) {
        return None;
    }
    if last >= 256u64.pow(5 - numbers.len() as u32) {
        return None;
    }
    let mut ipv4 = last;
    for (counter, &n) in rest.iter().enumerate() {
        ipv4 += n * 256u64.pow(3 - counter as u32);
    }
    Some(ipv4 as u32)
}

/// The IPv6 parser (URL §3.5).
pub fn parse_ipv6(input: &str) -> Option<[u16; 8]> {
    let input: Vec<char> = input.chars().collect();
    let at = |p: usize| input.get(p).copied();
    let mut address = [0u16; 8];
    let mut piece_index = 0;
    let mut compress: Option<usize> = None;
    let mut pointer = 0;
    if at(pointer) == Some(':') {
        if at(pointer + 1) != Some(':') {
            return None;
        }
        pointer += 2;
        piece_index += 1;
        compress = Some(piece_index);
    }
    while at(pointer).is_some() {
        if piece_index == 8 {
            return None;
        }
        if at(pointer) == Some(':') {
            if compress.is_some() {
                return None;
            }
            pointer += 1;
            piece_index += 1;
            compress = Some(piece_index);
            continue;
        }
        let mut value: u32 = 0;
        let mut length = 0;
        while length < 4 {
            let Some(d) = at(pointer).and_then(|c| c.to_digit(16)) else {
                break;
            };
            value = value * 0x10 + d;
            pointer += 1;
            length += 1;
        }
        if at(pointer) == Some('.') {
            if length == 0 {
                return None;
            }
            pointer -= length;
            if piece_index > 6 {
                return None;
            }
            let mut numbers_seen = 0;
            while at(pointer).is_some() {
                let mut ipv4_piece: Option<u32> = None;
                if numbers_seen > 0 {
                    if at(pointer) == Some('.') && numbers_seen < 4 {
                        pointer += 1;
                    } else {
                        return None;
                    }
                }
                at(pointer).filter(char::is_ascii_digit)?;
                while let Some(d) = at(pointer).and_then(|c| c.to_digit(10)) {
                    ipv4_piece = match ipv4_piece {
                        None => Some(d),
                        Some(0) => return None,
                        Some(p) => Some(p * 10 + d),
                    };
                    if ipv4_piece > Some(255) {
                        return None;
                    }
                    pointer += 1;
                }
                let piece = ipv4_piece?;
                address[piece_index] = address[piece_index] * 0x100 + piece as u16;
                numbers_seen += 1;
                if numbers_seen == 2 || numbers_seen == 4 {
                    piece_index += 1;
                }
            }
            if numbers_seen != 4 {
                return None;
            }
            break;
        } else if at(pointer) == Some(':') {
            pointer += 1;
            at(pointer)?;
        } else if at(pointer).is_some() {
            return None;
        }
        address[piece_index] = value as u16;
        piece_index += 1;
    }
    if let Some(compress) = compress {
        let mut swaps = piece_index - compress;
        piece_index = 7;
        while piece_index != 0 && swaps > 0 {
            address.swap(piece_index, compress + swaps - 1);
            piece_index -= 1;
            swaps -= 1;
        }
    } else if piece_index != 8 {
        return None;
    }
    Some(address)
}

/// The IPv4 serializer (URL §3.6).
pub fn serialize_ipv4(address: u32) -> String {
    let [a, b, c, d] = address.to_be_bytes();
    format!("{a}.{b}.{c}.{d}")
}

/// The IPv6 serializer (URL §3.6), without brackets: the first longest run
/// of two or more zero pieces becomes `::`.
pub fn serialize_ipv6(address: &[u16; 8]) -> String {
    let mut compress = None;
    let mut best = 1;
    let mut i = 0;
    while i < 8 {
        if address[i] == 0 {
            let start = i;
            while i < 8 && address[i] == 0 {
                i += 1;
            }
            if i - start > best {
                best = i - start;
                compress = Some(start);
            }
        } else {
            i += 1;
        }
    }
    let mut output = String::new();
    let mut ignore0 = false;
    for (piece_index, &piece) in address.iter().enumerate() {
        if ignore0 && piece == 0 {
            continue;
        }
        ignore0 = false;
        if compress == Some(piece_index) {
            output.push_str(if piece_index == 0 { "::" } else { ":" });
            ignore0 = true;
            continue;
        }
        output.push_str(&format!("{piece:x}"));
        if piece_index != 7 {
            output.push(':');
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ipv4_forms() {
        assert_eq!(parse_ipv4("0x7f.1"), Some(0x7F00_0001));
        assert_eq!(parse_ipv4("017.0.0.1"), Some(0x0F00_0001));
        assert_eq!(parse_ipv4("4294967295"), Some(u32::MAX));
        assert_eq!(parse_ipv4("4294967296"), None);
        assert_eq!(parse_ipv4("256.0.0.1"), None);
        assert_eq!(parse_ipv4("1.2.3.4."), Some(0x0102_0304));
        assert_eq!(parse_ipv4("1.2.3.4.5"), None);
    }

    #[test]
    fn ipv6_round_trip() {
        let a = parse_ipv6("1:0::").unwrap();
        assert_eq!(serialize_ipv6(&a), "1::");
        let b = parse_ipv6("::ffff:1.2.3.4").unwrap();
        assert_eq!(serialize_ipv6(&b), "::ffff:102:304");
        assert_eq!(parse_ipv6("::1.2.3."), None);
        assert_eq!(parse_ipv6("1:2:3:4:5:6:7:8:9"), None);
        assert_eq!(
            serialize_ipv6(&parse_ipv6("0:0:1:0:0:0:0:1").unwrap()),
            "0:0:1::1"
        );
    }

    #[test]
    fn ends_in_a_number_cases() {
        assert!(ends_in_a_number("foo.0x"));
        assert!(ends_in_a_number("foo.09"));
        assert!(!ends_in_a_number("foo.0x1g"));
        assert!(!ends_in_a_number("example.com."));
    }
}
