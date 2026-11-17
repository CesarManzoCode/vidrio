//! Percent-encoded bytes (URL §1.3): the percent-encode sets, percent-encode
//! after encoding, and percent-decode.

use crate::encoding::{Encoder, Encoding};

/// The percent-encode sets of the URL Standard, each a superset of the one
/// before it except where noted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PercentEncodeSet {
    /// C0 controls and every code point above U+007E.
    C0Control,
    /// C0 control set and space, `"`, `<`, `>`, `` ` ``.
    Fragment,
    /// C0 control set and space, `"`, `#`, `<`, `>`.
    Query,
    /// Query set and `'`.
    SpecialQuery,
    /// Query set and `?`, `^`, `` ` ``, `{`, `}`.
    Path,
    /// Path set and `/`, `:`, `;`, `=`, `@`, `[` to `^`, `|`.
    Userinfo,
    /// Userinfo set and `$` to `&`, `+`, `,`.
    Component,
    /// Component set and `!`, `'` to `)`, `~`.
    FormUrlencoded,
}

impl PercentEncodeSet {
    /// Whether `c` is in the set.
    pub fn contains(self, c: char) -> bool {
        let c = c as u32;
        if c < 0x20 || c > 0x7E {
            return true;
        }
        let c = c as u8;
        match self {
            PercentEncodeSet::C0Control => false,
            PercentEncodeSet::Fragment => matches!(c, b' ' | b'"' | b'<' | b'>' | b'`'),
            PercentEncodeSet::Query => matches!(c, b' ' | b'"' | b'#' | b'<' | b'>'),
            PercentEncodeSet::SpecialQuery => {
                c == b'\'' || PercentEncodeSet::Query.contains(c as char)
            }
            PercentEncodeSet::Path => {
                matches!(c, b'?' | b'^' | b'`' | b'{' | b'}')
                    || PercentEncodeSet::Query.contains(c as char)
            }
            PercentEncodeSet::Userinfo => {
                matches!(c, b'/' | b':' | b';' | b'=' | b'@' | b'['..=b'^' | b'|')
                    || PercentEncodeSet::Path.contains(c as char)
            }
            PercentEncodeSet::Component => {
                matches!(c, b'$'..=b'&' | b'+' | b',')
                    || PercentEncodeSet::Userinfo.contains(c as char)
            }
            PercentEncodeSet::FormUrlencoded => {
                matches!(c, b'!' | b'\''..=b')' | b'~')
                    || PercentEncodeSet::Component.contains(c as char)
            }
        }
    }
}

/// Append the percent-encoding of `byte` (`%XX`, uppercase hex) to `out`.
pub fn percent_encode_byte(byte: u8, out: &mut String) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    out.push('%');
    out.push(HEX[usize::from(byte >> 4)] as char);
    out.push(HEX[usize::from(byte & 0xF)] as char);
}

/// UTF-8 percent-encode `c` using `set`, appending to `out`.
pub fn utf8_percent_encode_char(c: char, set: PercentEncodeSet, out: &mut String) {
    if !set.contains(c) {
        out.push(c);
        return;
    }
    let mut buf = [0; 4];
    for &b in c.encode_utf8(&mut buf).as_bytes() {
        percent_encode_byte(b, out);
    }
}

/// UTF-8 percent-encode `input` using `set`.
pub fn utf8_percent_encode(input: &str, set: PercentEncodeSet) -> String {
    let mut out = String::with_capacity(input.len());
    for c in input.chars() {
        utf8_percent_encode_char(c, set, &mut out);
    }
    out
}

/// Percent-encode after encoding (URL §1.3): encode `input` with
/// `encoding` (through get an output encoding), percent-encode the bytes
/// whose isomorphic code point is in `set`, write unmappable code points as
/// `%26%23N%3B`, and with `space_as_plus` write 0x20 as `+`.
pub fn percent_encode_after_encoding(
    encoding: Encoding,
    input: &str,
    set: PercentEncodeSet,
    space_as_plus: bool,
) -> String {
    let mut output = String::with_capacity(input.len());
    let mut encoder = Encoder::new(encoding);
    let mut rest = input;
    let mut bytes = Vec::new();
    loop {
        bytes.clear();
        let result = encoder.encode_or_fail(rest, true, &mut bytes);
        for &b in &bytes {
            if space_as_plus && b == b' ' {
                output.push('+');
            } else if set.contains(b as char) {
                percent_encode_byte(b, &mut output);
            } else {
                output.push(b as char);
            }
        }
        match result {
            Ok(()) => return output,
            Err(e) => {
                output.push_str("%26%23");
                output.push_str(&e.code_point.to_string());
                output.push_str("%3B");
                rest = &rest[e.consumed..];
            }
        }
    }
}

/// Percent-decode a byte sequence (URL §1.3).
pub fn percent_decode(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len());
    let mut i = 0;
    while i < input.len() {
        let b = input[i];
        if b == b'%' && i + 2 < input.len() {
            if let (Some(hi), Some(lo)) = (hex_value(input[i + 1]), hex_value(input[i + 2])) {
                out.push((hi << 4) | lo);
                i += 3;
                continue;
            }
        }
        out.push(b);
        i += 1;
    }
    out
}

/// String percent-decode (URL §1.3): UTF-8 encode, then percent-decode.
pub fn percent_decode_str(input: &str) -> Vec<u8> {
    percent_decode(input.as_bytes())
}

fn hex_value(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sets_nest() {
        assert!(!PercentEncodeSet::Query.contains('\''));
        assert!(PercentEncodeSet::SpecialQuery.contains('\''));
        assert!(PercentEncodeSet::Path.contains('^'));
        assert!(!PercentEncodeSet::Path.contains('|'));
        assert!(PercentEncodeSet::Userinfo.contains('|'));
        assert!(PercentEncodeSet::Component.contains('+'));
        assert!(PercentEncodeSet::FormUrlencoded.contains('~'));
        assert!(!PercentEncodeSet::FormUrlencoded.contains('*'));
    }

    #[test]
    fn decode_keeps_malformed_escapes() {
        assert_eq!(percent_decode(b"%41%4g%%2"), b"A%4g%%2");
        assert_eq!(percent_decode(b"%e2%82%ac"), "\u{20AC}".as_bytes());
    }

    #[test]
    fn after_encoding_writes_unmappable_as_numeric_reference() {
        let s = percent_encode_after_encoding(
            Encoding::Windows1252,
            "\u{E9} \u{2603}",
            PercentEncodeSet::FormUrlencoded,
            true,
        );
        assert_eq!(s, "%E9+%26%239731%3B");
    }
}
