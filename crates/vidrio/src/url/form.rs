//! `application/x-www-form-urlencoded` (URL §5): parser and serializer.

use super::percent::{PercentEncodeSet, percent_decode, percent_encode_after_encoding};
use crate::encoding::{Encoding, utf8_decode_without_bom};

/// The urlencoded parser (URL §5.1): name-value pairs of `input`.
pub fn parse(input: &[u8]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for sequence in input.split(|&b| b == b'&') {
        if sequence.is_empty() {
            continue;
        }
        let (name, value) = match sequence.iter().position(|&b| b == b'=') {
            Some(i) => (&sequence[..i], &sequence[i + 1..]),
            None => (sequence, &[][..]),
        };
        out.push((decode_component(name), decode_component(value)));
    }
    out
}

fn decode_component(bytes: &[u8]) -> String {
    let plus_as_space: Vec<u8> = bytes
        .iter()
        .map(|&b| if b == b'+' { b' ' } else { b })
        .collect();
    utf8_decode_without_bom(&percent_decode(&plus_as_space))
}

/// The urlencoded serializer (URL §5.2) with UTF-8.
pub fn serialize<'a>(pairs: impl IntoIterator<Item = (&'a str, &'a str)>) -> String {
    serialize_with_encoding(pairs, Encoding::Utf8)
}

/// The urlencoded serializer (URL §5.2) with `encoding` (through get an
/// output encoding).
pub fn serialize_with_encoding<'a>(
    pairs: impl IntoIterator<Item = (&'a str, &'a str)>,
    encoding: Encoding,
) -> String {
    let mut output = String::new();
    for (name, value) in pairs {
        if !output.is_empty() {
            output.push('&');
        }
        output.push_str(&percent_encode_after_encoding(
            encoding,
            name,
            PercentEncodeSet::FormUrlencoded,
            true,
        ));
        output.push('=');
        output.push_str(&percent_encode_after_encoding(
            encoding,
            value,
            PercentEncodeSet::FormUrlencoded,
            true,
        ));
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_splits_and_decodes() {
        assert_eq!(
            parse(b"a=1&&b&c=x+y%20z&=e&%FF=%"),
            vec![
                ("a".into(), "1".into()),
                ("b".into(), "".into()),
                ("c".into(), "x y z".into()),
                ("".into(), "e".into()),
                ("\u{FFFD}".into(), "%".into()),
            ]
        );
    }

    #[test]
    fn serialize_escapes_with_plus_for_space() {
        assert_eq!(
            serialize([("a b", "c&d"), ("\u{E9}", "*-._~")]),
            "a+b=c%26d&%C3%A9=*-._%7E"
        );
    }
}
