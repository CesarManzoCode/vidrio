//! WHATWG Encoding: every encoding of the standard with streaming decoders
//! and encoders, label lookup, BOM sniffing, both error modes of each side,
//! and the hooks other standards call (decode, UTF-8 decode, encode, encode
//! or fail).
//!
//! The indexes come from [`crate::generated`], made by
//! `crates/vidrio/gen/encoding.py` from the pinned whatwg/encoding commit.

mod decoder;
mod encoder;
mod index;

pub use decoder::{BomHandling, DecodeError, Decoder, DecoderErrorMode};
pub use encoder::{EncodeError, Encoder, EncoderErrorMode};

use crate::generated::encoding_labels::{ENCODINGS, LABELS};

/// An encoding of the Encoding Standard (§4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Encoding {
    Utf8,
    Ibm866,
    Iso8859_2,
    Iso8859_3,
    Iso8859_4,
    Iso8859_5,
    Iso8859_6,
    Iso8859_7,
    Iso8859_8,
    Iso8859_8I,
    Iso8859_10,
    Iso8859_13,
    Iso8859_14,
    Iso8859_15,
    Iso8859_16,
    Koi8R,
    Koi8U,
    Macintosh,
    Windows874,
    Windows1250,
    Windows1251,
    Windows1252,
    Windows1253,
    Windows1254,
    Windows1255,
    Windows1256,
    Windows1257,
    Windows1258,
    XMacCyrillic,
    Gbk,
    Gb18030,
    Big5,
    EucJp,
    Iso2022Jp,
    ShiftJis,
    EucKr,
    Replacement,
    Utf16Be,
    Utf16Le,
    XUserDefined,
}

impl Encoding {
    /// Every encoding, in the order of the specification.
    pub fn all() -> &'static [Encoding] {
        ENCODINGS
    }

    /// The encoding's name, as the standard writes it.
    pub fn name(self) -> &'static str {
        match self {
            Encoding::Utf8 => "UTF-8",
            Encoding::Ibm866 => "IBM866",
            Encoding::Iso8859_2 => "ISO-8859-2",
            Encoding::Iso8859_3 => "ISO-8859-3",
            Encoding::Iso8859_4 => "ISO-8859-4",
            Encoding::Iso8859_5 => "ISO-8859-5",
            Encoding::Iso8859_6 => "ISO-8859-6",
            Encoding::Iso8859_7 => "ISO-8859-7",
            Encoding::Iso8859_8 => "ISO-8859-8",
            Encoding::Iso8859_8I => "ISO-8859-8-I",
            Encoding::Iso8859_10 => "ISO-8859-10",
            Encoding::Iso8859_13 => "ISO-8859-13",
            Encoding::Iso8859_14 => "ISO-8859-14",
            Encoding::Iso8859_15 => "ISO-8859-15",
            Encoding::Iso8859_16 => "ISO-8859-16",
            Encoding::Koi8R => "KOI8-R",
            Encoding::Koi8U => "KOI8-U",
            Encoding::Macintosh => "macintosh",
            Encoding::Windows874 => "windows-874",
            Encoding::Windows1250 => "windows-1250",
            Encoding::Windows1251 => "windows-1251",
            Encoding::Windows1252 => "windows-1252",
            Encoding::Windows1253 => "windows-1253",
            Encoding::Windows1254 => "windows-1254",
            Encoding::Windows1255 => "windows-1255",
            Encoding::Windows1256 => "windows-1256",
            Encoding::Windows1257 => "windows-1257",
            Encoding::Windows1258 => "windows-1258",
            Encoding::XMacCyrillic => "x-mac-cyrillic",
            Encoding::Gbk => "GBK",
            Encoding::Gb18030 => "gb18030",
            Encoding::Big5 => "Big5",
            Encoding::EucJp => "EUC-JP",
            Encoding::Iso2022Jp => "ISO-2022-JP",
            Encoding::ShiftJis => "Shift_JIS",
            Encoding::EucKr => "EUC-KR",
            Encoding::Replacement => "replacement",
            Encoding::Utf16Be => "UTF-16BE",
            Encoding::Utf16Le => "UTF-16LE",
            Encoding::XUserDefined => "x-user-defined",
        }
    }

    /// Get an encoding (§4.2): the encoding of `label` after trimming ASCII
    /// whitespace, matched ASCII case-insensitively; `None` for an unknown
    /// label.
    pub fn for_label(label: &str) -> Option<Encoding> {
        let label = label.trim_matches(|c| matches!(c, '\t' | '\n' | '\x0C' | '\r' | ' '));
        if label.is_empty() || label.len() > 32 || !label.is_ascii() {
            return None;
        }
        let lower = label.to_ascii_lowercase();
        LABELS
            .binary_search_by(|&(l, _)| l.cmp(lower.as_str()))
            .ok()
            .map(|i| LABELS[i].1)
    }

    /// [`Encoding::for_label`] on bytes (a label read from a document).
    pub fn for_label_bytes(label: &[u8]) -> Option<Encoding> {
        std::str::from_utf8(label)
            .ok()
            .and_then(Encoding::for_label)
    }

    /// Get an output encoding (§4.3): replacement and UTF-16BE/LE become
    /// UTF-8.
    pub fn output_encoding(self) -> Encoding {
        match self {
            Encoding::Replacement | Encoding::Utf16Be | Encoding::Utf16Le => Encoding::Utf8,
            other => other,
        }
    }

    /// Whether the encoding has an encoder of its own.
    pub fn has_encoder(self) -> bool {
        self.output_encoding() == self
    }

    /// Whether the encoding is one of the legacy single-byte encodings.
    pub fn is_single_byte(self) -> bool {
        crate::generated::encoding_single_byte::index(self).is_some()
    }

    /// Whether bytes 0x00 to 0x7F decode to the ASCII code points in every
    /// position; false for UTF-16BE/LE, ISO-2022-JP and replacement.
    pub fn is_ascii_compatible(self) -> bool {
        !matches!(
            self,
            Encoding::Utf16Be | Encoding::Utf16Le | Encoding::Iso2022Jp | Encoding::Replacement
        )
    }
}

/// BOM sniff (§6): the encoding of a byte order mark at the start of
/// `bytes` and the mark's length.
pub fn bom_sniff(bytes: &[u8]) -> Option<(Encoding, usize)> {
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        Some((Encoding::Utf8, 3))
    } else if bytes.starts_with(&[0xFE, 0xFF]) {
        Some((Encoding::Utf16Be, 2))
    } else if bytes.starts_with(&[0xFF, 0xFE]) {
        Some((Encoding::Utf16Le, 2))
    } else {
        None
    }
}

/// Decode (§6): a byte order mark overrides `fallback`; errors become
/// U+FFFD. Returns the text and the encoding used.
pub fn decode(bytes: &[u8], fallback: Encoding) -> (String, Encoding) {
    let mut decoder = Decoder::new(fallback, DecoderErrorMode::Replacement, BomHandling::Sniff);
    let mut out = String::with_capacity(bytes.len());
    let _ = decoder.decode(bytes, true, &mut out);
    (out, decoder.encoding())
}

/// Decode `bytes` as `encoding` without looking for a byte order mark.
pub fn decode_without_bom(bytes: &[u8], encoding: Encoding) -> String {
    let mut decoder = Decoder::new(encoding, DecoderErrorMode::Replacement, BomHandling::Keep);
    let mut out = String::with_capacity(bytes.len());
    let _ = decoder.decode(bytes, true, &mut out);
    out
}

/// UTF-8 decode (§6): drops a UTF-8 byte order mark, errors become U+FFFD.
pub fn utf8_decode(bytes: &[u8]) -> String {
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    utf8_decode_without_bom(bytes)
}

/// UTF-8 decode without BOM (§6).
pub fn utf8_decode_without_bom(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_string(),
        Err(_) => decode_without_bom(bytes, Encoding::Utf8),
    }
}

/// UTF-8 decode without BOM or fail (§6): `None` at the first error.
pub fn utf8_decode_without_bom_or_fail(bytes: &[u8]) -> Option<String> {
    std::str::from_utf8(bytes).ok().map(str::to_string)
}

/// UTF-8 encode (§6).
pub fn utf8_encode(text: &str) -> Vec<u8> {
    text.as_bytes().to_vec()
}

/// Encode (§6, legacy hook for HTML forms): unmappable code points become
/// `&#N;`. `encoding` goes through get an output encoding first.
pub fn encode(text: &str, encoding: Encoding) -> Vec<u8> {
    let mut encoder = Encoder::new(encoding);
    let mut out = Vec::with_capacity(text.len());
    let _ = encoder.encode(text, true, &mut out, EncoderErrorMode::Html);
    out
}

/// The isomorphic decoding of `bytes` (Infra): each byte becomes the code
/// point of the same value.
pub fn isomorphic_decode(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| b as char).collect()
}

/// The isomorphic encoding of `text` (Infra); `None` if a code point is
/// above U+00FF.
pub fn isomorphic_encode(text: &str) -> Option<Vec<u8>> {
    text.chars().map(|c| u8::try_from(c as u32).ok()).collect()
}
