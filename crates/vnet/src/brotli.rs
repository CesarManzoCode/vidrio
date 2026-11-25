//! A Brotli decoder (RFC 7932), one-shot: the whole compressed stream in,
//! the whole output out.
//!
//! The static dictionary (Appendix A) is `brotli_dictionary.bin`, taken
//! from the official repository; its source and sha256 are recorded in
//! `vault/RESEARCH.md`. The transforms (Appendix B) and the literal
//! context lookup tables (§7.1) are copied from the same source.

use std::fmt;

static DICTIONARY: &[u8; 122784] = include_bytes!("brotli_dictionary.bin");

/// log2 of the number of words of each length (§8, NDBITS).
const NDBITS: [u8; 25] = [
    0, 0, 0, 0, 10, 10, 11, 11, 10, 10, 10, 10, 10, 9, 9, 8, 7, 7, 8, 7, 7, 6, 6, 5, 5,
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BrotliError {
    /// The input ended before the stream was complete.
    Truncated,
    /// The stream breaks a rule of RFC 7932 (the message says which).
    Invalid(&'static str),
    /// The output would exceed the caller's limit.
    TooLarge,
}

impl fmt::Display for BrotliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BrotliError::Truncated => f.write_str("brotli: truncated stream"),
            BrotliError::Invalid(m) => write!(f, "brotli: {m}"),
            BrotliError::TooLarge => f.write_str("brotli: output too large"),
        }
    }
}

impl std::error::Error for BrotliError {}

type Res<T> = Result<T, BrotliError>;

fn bad<T>(m: &'static str) -> Res<T> {
    Err(BrotliError::Invalid(m))
}

/// Prefix, transform type and suffix of the 121 word transforms
/// (Appendix B). Types: 0 identity, 1–9 omit last N, 10 uppercase first,
/// 11 uppercase all, 12–20 omit first N−11.
const TRANSFORMS: [(&[u8], u8, &[u8]); 121] = [
    (b"", 0, b""),
    (b"", 0, b" "),
    (b" ", 0, b" "),
    (b"", 12, b""),
    (b"", 10, b" "),
    (b"", 0, b" the "),
    (b" ", 0, b""),
    (b"s ", 0, b" "),
    (b"", 0, b" of "),
    (b"", 10, b""),
    (b"", 0, b" and "),
    (b"", 13, b""),
    (b"", 1, b""),
    (b", ", 0, b" "),
    (b"", 0, b", "),
    (b" ", 10, b" "),
    (b"", 0, b" in "),
    (b"", 0, b" to "),
    (b"e ", 0, b" "),
    (b"", 0, b"\x22"),
    (b"", 0, b"."),
    (b"", 0, b"\x22>"),
    (b"", 0, b"\x0a"),
    (b"", 3, b""),
    (b"", 0, b"]"),
    (b"", 0, b" for "),
    (b"", 14, b""),
    (b"", 2, b""),
    (b"", 0, b" a "),
    (b"", 0, b" that "),
    (b" ", 10, b""),
    (b"", 0, b". "),
    (b".", 0, b""),
    (b" ", 0, b", "),
    (b"", 15, b""),
    (b"", 0, b" with "),
    (b"", 0, b"'"),
    (b"", 0, b" from "),
    (b"", 0, b" by "),
    (b"", 16, b""),
    (b"", 17, b""),
    (b" the ", 0, b""),
    (b"", 4, b""),
    (b"", 0, b". The "),
    (b"", 11, b""),
    (b"", 0, b" on "),
    (b"", 0, b" as "),
    (b"", 0, b" is "),
    (b"", 7, b""),
    (b"", 1, b"ing "),
    (b"", 0, b"\x0a\x09"),
    (b"", 0, b":"),
    (b" ", 0, b". "),
    (b"", 0, b"ed "),
    (b"", 20, b""),
    (b"", 18, b""),
    (b"", 6, b""),
    (b"", 0, b"("),
    (b"", 10, b", "),
    (b"", 8, b""),
    (b"", 0, b" at "),
    (b"", 0, b"ly "),
    (b" the ", 0, b" of "),
    (b"", 5, b""),
    (b"", 9, b""),
    (b" ", 10, b", "),
    (b"", 10, b"\x22"),
    (b".", 0, b"("),
    (b"", 11, b" "),
    (b"", 10, b"\x22>"),
    (b"", 0, b"=\x22"),
    (b" ", 0, b"."),
    (b".com/", 0, b""),
    (b" the ", 0, b" of the "),
    (b"", 10, b"'"),
    (b"", 0, b". This "),
    (b"", 0, b","),
    (b".", 0, b" "),
    (b"", 10, b"("),
    (b"", 10, b"."),
    (b"", 0, b" not "),
    (b" ", 0, b"=\x22"),
    (b"", 0, b"er "),
    (b" ", 11, b" "),
    (b"", 0, b"al "),
    (b" ", 11, b""),
    (b"", 0, b"='"),
    (b"", 11, b"\x22"),
    (b"", 10, b". "),
    (b" ", 0, b"("),
    (b"", 0, b"ful "),
    (b" ", 10, b". "),
    (b"", 0, b"ive "),
    (b"", 0, b"less "),
    (b"", 11, b"'"),
    (b"", 0, b"est "),
    (b" ", 10, b"."),
    (b"", 11, b"\x22>"),
    (b" ", 0, b"='"),
    (b"", 10, b","),
    (b"", 0, b"ize "),
    (b"", 11, b"."),
    (b"\xc2\xa0", 0, b""),
    (b" ", 0, b","),
    (b"", 10, b"=\x22"),
    (b"", 11, b"=\x22"),
    (b"", 0, b"ous "),
    (b"", 11, b", "),
    (b"", 10, b"='"),
    (b" ", 10, b","),
    (b" ", 11, b"=\x22"),
    (b" ", 11, b", "),
    (b"", 11, b","),
    (b"", 11, b"("),
    (b"", 11, b". "),
    (b" ", 11, b"."),
    (b"", 11, b"='"),
    (b" ", 11, b". "),
    (b" ", 10, b"=\x22"),
    (b" ", 11, b"='"),
    (b" ", 10, b"='"),
];

/// Literal context lookup (§7.1): for mode m, the context id is
/// `CONTEXT_LUT[m * 512 + p1] | CONTEXT_LUT[m * 512 + 256 + p2]`.
const CONTEXT_LUT: [u8; 2048] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25,
    26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48, 49,
    50, 51, 52, 53, 54, 55, 56, 57, 58, 59, 60, 61, 62, 63, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11,
    12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32, 33, 34, 35,
    36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48, 49, 50, 51, 52, 53, 54, 55, 56, 57, 58, 59,
    60, 61, 62, 63, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21,
    22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45,
    46, 47, 48, 49, 50, 51, 52, 53, 54, 55, 56, 57, 58, 59, 60, 61, 62, 63, 0, 1, 2, 3, 4, 5, 6, 7,
    8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31,
    32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48, 49, 50, 51, 52, 53, 54, 55,
    56, 57, 58, 59, 60, 61, 62, 63, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5,
    5, 5, 5, 6, 6, 6, 6, 7, 7, 7, 7, 8, 8, 8, 8, 9, 9, 9, 9, 10, 10, 10, 10, 11, 11, 11, 11, 12,
    12, 12, 12, 13, 13, 13, 13, 14, 14, 14, 14, 15, 15, 15, 15, 16, 16, 16, 16, 17, 17, 17, 17, 18,
    18, 18, 18, 19, 19, 19, 19, 20, 20, 20, 20, 21, 21, 21, 21, 22, 22, 22, 22, 23, 23, 23, 23, 24,
    24, 24, 24, 25, 25, 25, 25, 26, 26, 26, 26, 27, 27, 27, 27, 28, 28, 28, 28, 29, 29, 29, 29, 30,
    30, 30, 30, 31, 31, 31, 31, 32, 32, 32, 32, 33, 33, 33, 33, 34, 34, 34, 34, 35, 35, 35, 35, 36,
    36, 36, 36, 37, 37, 37, 37, 38, 38, 38, 38, 39, 39, 39, 39, 40, 40, 40, 40, 41, 41, 41, 41, 42,
    42, 42, 42, 43, 43, 43, 43, 44, 44, 44, 44, 45, 45, 45, 45, 46, 46, 46, 46, 47, 47, 47, 47, 48,
    48, 48, 48, 49, 49, 49, 49, 50, 50, 50, 50, 51, 51, 51, 51, 52, 52, 52, 52, 53, 53, 53, 53, 54,
    54, 54, 54, 55, 55, 55, 55, 56, 56, 56, 56, 57, 57, 57, 57, 58, 58, 58, 58, 59, 59, 59, 59, 60,
    60, 60, 60, 61, 61, 61, 61, 62, 62, 62, 62, 63, 63, 63, 63, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 4, 4, 0,
    0, 4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 8, 12, 16, 12, 12, 20, 12, 16, 24,
    28, 12, 12, 32, 12, 36, 12, 44, 44, 44, 44, 44, 44, 44, 44, 44, 44, 32, 32, 24, 40, 28, 12, 12,
    48, 52, 52, 52, 48, 52, 52, 52, 48, 52, 52, 52, 52, 52, 48, 52, 52, 52, 52, 52, 48, 52, 52, 52,
    52, 52, 24, 12, 28, 12, 12, 12, 56, 60, 60, 60, 56, 60, 60, 60, 56, 60, 60, 60, 60, 60, 56, 60,
    60, 60, 60, 60, 56, 60, 60, 60, 60, 60, 24, 12, 28, 12, 0, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1,
    0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1,
    0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 2, 3, 2, 3, 2, 3, 2, 3, 2, 3, 2, 3,
    2, 3, 2, 3, 2, 3, 2, 3, 2, 3, 2, 3, 2, 3, 2, 3, 2, 3, 2, 3, 2, 3, 2, 3, 2, 3, 2, 3, 2, 3, 2, 3,
    2, 3, 2, 3, 2, 3, 2, 3, 2, 3, 2, 3, 2, 3, 2, 3, 2, 3, 2, 3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
    1, 1, 1, 1, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 1, 1, 1, 1, 1, 1, 1, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2,
    2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 1, 1, 1, 1, 1, 1, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3,
    3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2,
    2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 0, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8, 8,
    8, 8, 8, 8, 16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 16,
    16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 16,
    16, 16, 16, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24,
    24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24,
    24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 32, 32, 32, 32, 32,
    32, 32, 32, 32, 32, 32, 32, 32, 32, 32, 32, 32, 32, 32, 32, 32, 32, 32, 32, 32, 32, 32, 32, 32,
    32, 32, 32, 32, 32, 32, 32, 32, 32, 32, 32, 32, 32, 32, 32, 32, 32, 32, 32, 32, 32, 32, 32, 32,
    32, 32, 32, 32, 32, 32, 32, 32, 32, 32, 32, 40, 40, 40, 40, 40, 40, 40, 40, 40, 40, 40, 40, 40,
    40, 40, 40, 40, 40, 40, 40, 40, 40, 40, 40, 40, 40, 40, 40, 40, 40, 40, 40, 40, 40, 40, 40, 40,
    40, 40, 40, 40, 40, 40, 40, 40, 40, 40, 40, 48, 48, 48, 48, 48, 48, 48, 48, 48, 48, 48, 48, 48,
    48, 48, 56, 0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2,
    2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2,
    2, 2, 2, 2, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3,
    3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3,
    3, 3, 3, 3, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4,
    4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4,
    4, 4, 4, 4, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5,
    5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6,
    6, 6, 6, 7,
];

/// Insert length codes: (extra bits, base) (§5).
const INSERT_CODES: [(u8, u32); 24] = [
    (0, 0),
    (0, 1),
    (0, 2),
    (0, 3),
    (0, 4),
    (0, 5),
    (1, 6),
    (1, 8),
    (2, 10),
    (2, 14),
    (3, 18),
    (3, 26),
    (4, 34),
    (4, 50),
    (5, 66),
    (5, 98),
    (6, 130),
    (7, 194),
    (8, 322),
    (9, 578),
    (10, 1090),
    (12, 2114),
    (14, 6210),
    (24, 22594),
];

/// Copy length codes: (extra bits, base) (§5).
const COPY_CODES: [(u8, u32); 24] = [
    (0, 2),
    (0, 3),
    (0, 4),
    (0, 5),
    (0, 6),
    (0, 7),
    (0, 8),
    (0, 9),
    (1, 10),
    (1, 12),
    (2, 14),
    (2, 18),
    (3, 22),
    (3, 30),
    (4, 38),
    (4, 54),
    (5, 70),
    (5, 102),
    (6, 134),
    (7, 198),
    (8, 326),
    (9, 582),
    (10, 1094),
    (24, 2118),
];

/// Block count codes: (base, extra bits) (§6).
const BLOCK_COUNT_CODES: [(u32, u8); 26] = [
    (1, 2),
    (5, 2),
    (9, 2),
    (13, 2),
    (17, 3),
    (25, 3),
    (33, 3),
    (41, 3),
    (49, 4),
    (65, 4),
    (81, 4),
    (97, 4),
    (113, 5),
    (145, 5),
    (177, 5),
    (209, 5),
    (241, 6),
    (305, 6),
    (369, 7),
    (497, 8),
    (753, 9),
    (1265, 10),
    (2289, 11),
    (4337, 12),
    (8433, 13),
    (16625, 24),
];

/// Order of the code length code lengths (§3.5).
const CODE_LENGTH_ORDER: [usize; 18] =
    [1, 2, 3, 4, 0, 5, 17, 6, 16, 7, 8, 9, 10, 11, 12, 13, 14, 15];

/// LSB-first bit reader.
struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
    bit: u32,
}

impl<'a> Bits<'a> {
    fn bit1(&mut self) -> Res<u32> {
        let byte = *self.data.get(self.pos).ok_or(BrotliError::Truncated)?;
        let v = (byte >> self.bit) & 1;
        self.bit += 1;
        if self.bit == 8 {
            self.bit = 0;
            self.pos += 1;
        }
        Ok(v as u32)
    }

    fn read(&mut self, n: u32) -> Res<u32> {
        let mut v = 0u32;
        for i in 0..n {
            v |= self.bit1()? << i;
        }
        Ok(v)
    }

    /// Skips to the next byte boundary; the skipped bits must be zero.
    fn align(&mut self) -> Res<()> {
        if self.bit != 0 {
            let n = 8 - self.bit;
            if self.read(n)? != 0 {
                return bad("nonzero padding bits");
            }
        }
        Ok(())
    }
}

/// A canonical prefix code, decoded bit by bit (counts per length plus
/// symbols sorted by code).
#[derive(Clone, Default)]
struct Prefix {
    counts: [u16; 16],
    symbols: Vec<u16>,
    /// Single-symbol codes take zero bits.
    single: Option<u16>,
}

impl Prefix {
    fn from_lengths(lengths: &[u8]) -> Res<Prefix> {
        let mut counts = [0u16; 16];
        let mut nonzero = 0;
        let mut last = 0u16;
        for (s, &l) in lengths.iter().enumerate() {
            if l > 0 {
                counts[l as usize] += 1;
                nonzero += 1;
                last = s as u16;
            }
        }
        if nonzero == 0 {
            return bad("empty prefix code");
        }
        if nonzero == 1 {
            return Ok(Prefix {
                counts,
                symbols: vec![last],
                single: Some(last),
            });
        }
        let mut left: i32 = 1;
        for len in 1..16 {
            left <<= 1;
            left -= counts[len] as i32;
            if left < 0 {
                return bad("oversubscribed prefix code");
            }
        }
        if left != 0 {
            return bad("incomplete prefix code");
        }
        let mut offs = [0u16; 16];
        for len in 1..15 {
            offs[len + 1] = offs[len] + counts[len];
        }
        let mut symbols = vec![0u16; nonzero];
        for (s, &l) in lengths.iter().enumerate() {
            if l > 0 {
                symbols[offs[l as usize] as usize] = s as u16;
                offs[l as usize] += 1;
            }
        }
        Ok(Prefix {
            counts,
            symbols,
            single: None,
        })
    }

    fn decode(&self, br: &mut Bits) -> Res<u32> {
        if let Some(s) = self.single {
            return Ok(s as u32);
        }
        let mut code: i32 = 0;
        let mut first: i32 = 0;
        let mut index: i32 = 0;
        for len in 1..16 {
            code |= br.bit1()? as i32;
            let count = self.counts[len] as i32;
            if code - count < first {
                return Ok(self.symbols[(index + (code - first)) as usize] as u32);
            }
            index += count;
            first += count;
            first <<= 1;
            code <<= 1;
        }
        bad("invalid prefix code")
    }
}

/// Reads a prefix code over `alphabet` symbols (§3.4, §3.5); `max_symbol`
/// bounds the symbols a simple code may name.
fn read_prefix(br: &mut Bits, alphabet: usize, max_symbol: usize) -> Res<Prefix> {
    let hskip = br.read(2)?;
    if hskip == 1 {
        let nsym = br.read(2)? as usize + 1;
        let mut bits = 0;
        while (1usize << bits) < alphabet {
            bits += 1;
        }
        let mut syms = [0usize; 4];
        for s in syms.iter_mut().take(nsym) {
            *s = br.read(bits)? as usize;
            if *s >= max_symbol {
                return bad("simple code symbol out of range");
            }
        }
        for i in 0..nsym {
            for j in i + 1..nsym {
                if syms[i] == syms[j] {
                    return bad("duplicate symbol in simple code");
                }
            }
        }
        let lens: &[u8] = match nsym {
            1 => &[0],
            2 => &[1, 1],
            3 => &[1, 2, 2],
            _ => {
                if br.read(1)? == 1 {
                    &[1, 2, 3, 3]
                } else {
                    &[2, 2, 2, 2]
                }
            }
        };
        if nsym == 1 {
            return Ok(Prefix {
                counts: [0; 16],
                symbols: vec![syms[0] as u16],
                single: Some(syms[0] as u16),
            });
        }
        let mut lengths = vec![0u8; alphabet];
        for i in 0..nsym {
            lengths[syms[i]] = lens[i];
        }
        return Prefix::from_lengths(&lengths);
    }
    // Complex prefix code: first the code length code lengths.
    const PREFIX_LEN: [u8; 16] = [2, 2, 2, 3, 2, 2, 2, 4, 2, 2, 2, 3, 2, 2, 2, 4];
    const PREFIX_VAL: [u8; 16] = [0, 4, 3, 2, 0, 4, 3, 1, 0, 4, 3, 2, 0, 4, 3, 5];
    let mut cl_lengths = [0u8; 18];
    let mut space: i32 = 32;
    let mut num_codes = 0;
    for &idx in CODE_LENGTH_ORDER.iter().skip(hskip as usize) {
        // Peek up to four bits without consuming more than the code needs.
        let save = (br.pos, br.bit);
        let mut peek = 0u32;
        let mut got = 0;
        for i in 0..4 {
            match br.bit1() {
                Ok(b) => {
                    peek |= b << i;
                    got += 1;
                }
                Err(_) => break,
            }
        }
        let len = PREFIX_LEN[peek as usize] as u32;
        if got < len {
            return Err(BrotliError::Truncated);
        }
        br.pos = save.0;
        br.bit = save.1;
        br.read(len)?;
        let v = PREFIX_VAL[peek as usize];
        cl_lengths[idx] = v;
        if v != 0 {
            space -= 32 >> v;
            num_codes += 1;
            if space <= 0 {
                break;
            }
        }
    }
    if !(num_codes == 1 || space == 0) {
        return bad("invalid code length code");
    }
    let cl = Prefix::from_lengths(&cl_lengths)?;
    let mut lengths = vec![0u8; alphabet];
    let mut symbol = 0usize;
    let mut prev_len = 8u8;
    let mut repeat = 0usize;
    let mut repeat_len = 0u8;
    let mut space: i32 = 32768;
    while symbol < alphabet && space > 0 {
        let code = cl.decode(br)? as u8;
        if code < 16 {
            repeat = 0;
            lengths[symbol] = code;
            symbol += 1;
            if code != 0 {
                prev_len = code;
                space -= 32768 >> code;
            }
        } else {
            let (extra, new_len) = if code == 16 { (2, prev_len) } else { (3, 0) };
            if repeat_len != new_len {
                repeat = 0;
                repeat_len = new_len;
            }
            let old = repeat;
            if repeat > 0 {
                repeat -= 2;
                repeat <<= extra;
            }
            repeat += br.read(extra)? as usize + 3;
            let delta = repeat - old;
            if symbol + delta > alphabet {
                return bad("code length repeat past the alphabet");
            }
            for _ in 0..delta {
                lengths[symbol] = repeat_len;
                symbol += 1;
            }
            if repeat_len != 0 {
                space -= (delta as i32) << (15 - repeat_len as i32);
            }
        }
    }
    if space != 0 {
        return bad("incomplete complex prefix code");
    }
    Prefix::from_lengths(&lengths)
}

/// Variable-length count 1..=256 (NBLTYPES, NTREES).
fn read_var_256(br: &mut Bits) -> Res<usize> {
    if br.read(1)? == 0 {
        return Ok(1);
    }
    let n = br.read(3)?;
    if n == 0 {
        return Ok(2);
    }
    Ok((1usize << n) + br.read(n)? as usize + 1)
}

fn read_block_count(br: &mut Bits, code: &Prefix) -> Res<u32> {
    let sym = code.decode(br)? as usize;
    let (base, extra) = BLOCK_COUNT_CODES[sym];
    Ok(base + br.read(extra as u32)?)
}

/// Block switching state of one category (literals, commands, distances).
struct BlockCat {
    ntypes: usize,
    type_code: Prefix,
    count_code: Prefix,
    cur: usize,
    prev: usize,
    left: u32,
}

impl BlockCat {
    fn read(br: &mut Bits) -> Res<BlockCat> {
        let ntypes = read_var_256(br)?;
        if ntypes < 2 {
            return Ok(BlockCat {
                ntypes,
                type_code: Prefix::default(),
                count_code: Prefix::default(),
                cur: 0,
                prev: 1,
                left: 1 << 24,
            });
        }
        let type_code = read_prefix(br, ntypes + 2, ntypes + 2)?;
        let count_code = read_prefix(br, 26, 26)?;
        let left = read_block_count(br, &count_code)?;
        Ok(BlockCat {
            ntypes,
            type_code,
            count_code,
            cur: 0,
            prev: 1,
            left,
        })
    }

    /// Accounts for one symbol of this category, switching block first if
    /// the current block is exhausted.
    fn step(&mut self, br: &mut Bits) -> Res<()> {
        if self.left == 0 {
            let code = self.type_code.decode(br)? as usize;
            let next = match code {
                0 => self.prev,
                1 => (self.cur + 1) % self.ntypes,
                c => c - 2,
            };
            if next >= self.ntypes {
                return bad("block type out of range");
            }
            self.prev = self.cur;
            self.cur = next;
            self.left = read_block_count(br, &self.count_code)?;
        }
        if self.ntypes >= 2 {
            self.left -= 1;
        }
        Ok(())
    }
}

/// Reads a context map of `size` entries (§7.3); returns it and NTREES.
fn read_context_map(br: &mut Bits, size: usize) -> Res<(Vec<u8>, usize)> {
    let ntrees = read_var_256(br)?;
    let mut map = vec![0u8; size];
    if ntrees < 2 {
        return Ok((map, ntrees));
    }
    let rlemax = if br.read(1)? == 1 {
        br.read(4)? as usize + 1
    } else {
        0
    };
    let code = read_prefix(br, ntrees + rlemax, ntrees + rlemax)?;
    let mut i = 0;
    while i < size {
        let c = code.decode(br)? as usize;
        if c == 0 {
            map[i] = 0;
            i += 1;
        } else if c <= rlemax {
            let reps = (1usize << c) + br.read(c as u32)? as usize;
            if i + reps > size {
                return bad("context map run too long");
            }
            for _ in 0..reps {
                map[i] = 0;
                i += 1;
            }
        } else {
            map[i] = (c - rlemax) as u8;
            i += 1;
        }
    }
    if br.read(1)? == 1 {
        // Inverse move-to-front transform.
        let mut mtf: Vec<u8> = (0..=255).collect();
        for v in map.iter_mut() {
            let idx = *v as usize;
            let val = mtf[idx];
            *v = val;
            mtf.remove(idx);
            mtf.insert(0, val);
        }
    }
    Ok((map, ntrees))
}

fn uppercase(word: &mut [u8], at: usize) -> usize {
    let c = word[at];
    if c < 0xc0 {
        if c.is_ascii_lowercase() {
            word[at] ^= 32;
        }
        1
    } else if c < 0xe0 {
        if at + 1 < word.len() {
            word[at + 1] ^= 32;
        }
        2
    } else {
        if at + 2 < word.len() {
            word[at + 2] ^= 5;
        }
        3
    }
}

/// Appends the transformed dictionary word (§8).
fn transform_word(out: &mut Vec<u8>, word: &[u8], transform: usize) {
    let (prefix, kind, suffix) = TRANSFORMS[transform];
    out.extend_from_slice(prefix);
    let mut w: Vec<u8> = match kind {
        1..=9 => word[..word.len().saturating_sub(kind as usize)].to_vec(),
        12..=20 => word[word.len().min((kind - 11) as usize)..].to_vec(),
        _ => word.to_vec(),
    };
    if kind == 10 && !w.is_empty() {
        uppercase(&mut w, 0);
    } else if kind == 11 {
        let mut i = 0;
        while i < w.len() {
            i += uppercase(&mut w, i);
        }
    }
    out.extend_from_slice(&w);
    out.extend_from_slice(suffix);
}

/// Decodes a complete Brotli stream.
pub fn decompress(data: &[u8]) -> Result<Vec<u8>, BrotliError> {
    decompress_limited(data, usize::MAX)
}

/// Decodes a complete Brotli stream whose output must not exceed `limit`
/// bytes.
pub fn decompress_limited(data: &[u8], limit: usize) -> Result<Vec<u8>, BrotliError> {
    let mut br = Bits {
        data,
        pos: 0,
        bit: 0,
    };
    // Stream header: WBITS (§9.1).
    let wbits = if br.read(1)? == 0 {
        16
    } else {
        let n = br.read(3)?;
        if n != 0 {
            17 + n
        } else {
            let m = br.read(3)?;
            if m == 1 {
                return bad("large window brotli is not RFC 7932");
            }
            if m != 0 { 8 + m } else { 17 }
        }
    };
    let max_backward = (1usize << wbits) - 16;
    let mut out: Vec<u8> = Vec::new();
    // Distance ring buffer: last, second last, third, fourth.
    let mut dist_rb = [4usize, 11, 15, 16];
    let mut dist_rb_idx = 0usize;
    loop {
        let is_last = br.read(1)? == 1;
        if is_last && br.read(1)? == 1 {
            break;
        }
        let mnibbles = br.read(2)?;
        if mnibbles == 3 {
            // Metadata block.
            if br.read(1)? != 0 {
                return bad("reserved bit set");
            }
            let skip_bytes = br.read(2)?;
            let mut skip_len = 0usize;
            for i in 0..skip_bytes {
                let b = br.read(8)? as usize;
                if i + 1 == skip_bytes && skip_bytes > 1 && b == 0 {
                    return bad("metadata length not minimal");
                }
                skip_len |= b << (8 * i);
            }
            if skip_bytes > 0 {
                skip_len += 1;
            }
            br.align()?;
            if br.pos + skip_len > data.len() {
                return Err(BrotliError::Truncated);
            }
            br.pos += skip_len;
            if is_last {
                break;
            }
            continue;
        }
        let nibbles = mnibbles + 4;
        let mut mlen = 0usize;
        for i in 0..nibbles {
            let v = br.read(4)? as usize;
            if i + 1 == nibbles && nibbles > 4 && v == 0 {
                return bad("meta-block length not minimal");
            }
            mlen |= v << (4 * i);
        }
        mlen += 1;
        if out.len().saturating_add(mlen) > limit {
            return Err(BrotliError::TooLarge);
        }
        if !is_last && br.read(1)? == 1 {
            br.align()?;
            if br.pos + mlen > data.len() {
                return Err(BrotliError::Truncated);
            }
            out.extend_from_slice(&data[br.pos..br.pos + mlen]);
            br.pos += mlen;
            continue;
        }
        decode_meta_block(
            &mut br,
            &mut out,
            mlen,
            max_backward,
            &mut dist_rb,
            &mut dist_rb_idx,
        )?;
        if is_last {
            break;
        }
    }
    Ok(out)
}

fn decode_meta_block(
    br: &mut Bits,
    out: &mut Vec<u8>,
    mlen: usize,
    max_backward: usize,
    dist_rb: &mut [usize; 4],
    dist_rb_idx: &mut usize,
) -> Res<()> {
    let mut lit = BlockCat::read(br)?;
    let mut cmd = BlockCat::read(br)?;
    let mut dst = BlockCat::read(br)?;
    let npostfix = br.read(2)?;
    let ndirect = (br.read(4)? << npostfix) as usize;
    let postfix_mask = (1usize << npostfix) - 1;
    let mut context_modes = vec![0u8; lit.ntypes];
    for m in context_modes.iter_mut() {
        *m = br.read(2)? as u8;
    }
    let (lit_map, ntreesl) = read_context_map(br, 64 * lit.ntypes)?;
    let (dist_map, ntreesd) = read_context_map(br, 4 * dst.ntypes)?;
    let mut lit_codes = Vec::with_capacity(ntreesl);
    for _ in 0..ntreesl {
        lit_codes.push(read_prefix(br, 256, 256)?);
    }
    let mut cmd_codes = Vec::with_capacity(cmd.ntypes);
    for _ in 0..cmd.ntypes {
        cmd_codes.push(read_prefix(br, 704, 704)?);
    }
    let dist_alphabet = 16 + ndirect + (48usize << npostfix);
    let mut dist_codes = Vec::with_capacity(ntreesd);
    for _ in 0..ntreesd {
        dist_codes.push(read_prefix(br, dist_alphabet, dist_alphabet)?);
    }
    let end = out.len() + mlen;
    while out.len() < end {
        cmd.step(br)?;
        let code = cmd_codes[cmd.cur].decode(br)? as usize;
        let (ins_start, copy_start, implicit_zero) = match code >> 6 {
            0 => (0, 0, true),
            1 => (0, 8, true),
            2 => (0, 0, false),
            3 => (0, 8, false),
            4 => (8, 0, false),
            5 => (8, 8, false),
            6 => (0, 16, false),
            7 => (16, 0, false),
            8 => (8, 16, false),
            9 => (16, 8, false),
            _ => (16, 16, false),
        };
        let ins_code = ins_start + ((code >> 3) & 7);
        let copy_code = copy_start + (code & 7);
        let (ie, ib) = INSERT_CODES[ins_code];
        let insert_len = (ib + br.read(ie as u32)?) as usize;
        let (ce, cb) = COPY_CODES[copy_code];
        let copy_len = (cb + br.read(ce as u32)?) as usize;
        for _ in 0..insert_len {
            if out.len() >= end {
                return bad("insert past the meta-block");
            }
            lit.step(br)?;
            let n = out.len();
            let p1 = if n >= 1 { out[n - 1] } else { 0 } as usize;
            let p2 = if n >= 2 { out[n - 2] } else { 0 } as usize;
            let mode = context_modes[lit.cur] as usize;
            let ctx = (CONTEXT_LUT[mode * 512 + p1] | CONTEXT_LUT[mode * 512 + 256 + p2]) as usize;
            let tree = lit_map[lit.cur * 64 + ctx] as usize;
            let b = lit_codes[tree].decode(br)? as u8;
            out.push(b);
        }
        if out.len() >= end {
            break;
        }
        let distance;
        let mut push_distance = true;
        if implicit_zero {
            distance = dist_rb[(*dist_rb_idx + 3) & 3];
            push_distance = false;
        } else {
            dst.step(br)?;
            let dctx = if copy_len > 4 { 3 } else { copy_len - 2 };
            let tree = dist_map[dst.cur * 4 + dctx] as usize;
            let dcode = dist_codes[tree].decode(br)? as usize;
            let last = |k: usize| dist_rb[(*dist_rb_idx + 4 - k) & 3];
            distance = if dcode < 16 {
                if dcode == 0 {
                    push_distance = false;
                }
                let (base, delta): (usize, i64) = match dcode {
                    0 => (last(1), 0),
                    1 => (last(2), 0),
                    2 => (last(3), 0),
                    3 => (last(4), 0),
                    4 => (last(1), -1),
                    5 => (last(1), 1),
                    6 => (last(1), -2),
                    7 => (last(1), 2),
                    8 => (last(1), -3),
                    9 => (last(1), 3),
                    10 => (last(2), -1),
                    11 => (last(2), 1),
                    12 => (last(2), -2),
                    13 => (last(2), 2),
                    14 => (last(2), -3),
                    _ => (last(2), 3),
                };
                let d = base as i64 + delta;
                if d <= 0 {
                    return bad("invalid distance");
                }
                d as usize
            } else if dcode < 16 + ndirect {
                dcode - 15
            } else {
                let x = dcode - ndirect - 16;
                let ndistbits = 1 + (x >> (npostfix + 1));
                let hcode = x >> npostfix;
                let lcode = x & postfix_mask;
                let offset = ((2 + (hcode & 1)) << ndistbits) - 4;
                let dextra = br.read(ndistbits as u32)? as usize;
                ((offset + dextra) << npostfix) + lcode + ndirect + 1
            };
        }
        let max_distance = out.len().min(max_backward);
        if distance > max_distance {
            // Static dictionary reference.
            if !(4..=24).contains(&copy_len) {
                return bad("invalid dictionary word length");
            }
            let word_id = distance - max_distance - 1;
            let bits = NDBITS[copy_len] as usize;
            let index = word_id & ((1 << bits) - 1);
            let transform = word_id >> bits;
            if transform >= TRANSFORMS.len() {
                return bad("invalid dictionary transform");
            }
            let mut offset = 0usize;
            for len in 4..copy_len {
                offset += len << NDBITS[len];
            }
            let start = offset + index * copy_len;
            let word = &DICTIONARY[start..start + copy_len];
            let before = out.len();
            transform_word(out, word, transform);
            if out.len() > end {
                return bad("dictionary word past the meta-block");
            }
            let _ = before;
        } else {
            if push_distance {
                dist_rb[*dist_rb_idx & 3] = distance;
                *dist_rb_idx += 1;
            }
            if out.len() + copy_len > end {
                return bad("copy past the meta-block");
            }
            let from = out.len() - distance;
            for i in 0..copy_len {
                let b = out[from + i];
                out.push(b);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_stream() {
        // WBITS=16 (bit 0), ISLAST=1, ISLASTEMPTY=1.
        assert_eq!(decompress(&[0x06]).unwrap(), b"");
    }

    #[test]
    fn uncompressed_meta_block() {
        // `brotli -q 0` on "hello": WBITS=22, a non-last uncompressed
        // meta-block and an empty last one.
        let data = [0x0f, 0x02, 0x80, b'h', b'e', b'l', b'l', b'o', 0x03];
        assert_eq!(decompress(&data).unwrap(), b"hello");
    }

    #[test]
    fn compressed_with_dictionary_and_copies() {
        // `brotli -q 11` (reference encoder 1.1) on the text below.
        let data = [
            0x1f, 0x67, 0x00, 0x00, 0x14, 0xa1, 0xd2, 0x6d, 0x6b, 0x98, 0xea, 0x85, 0x64, 0xec,
            0xc2, 0x06, 0xc5, 0xe0, 0x87, 0xe1, 0x01, 0x17, 0xfd, 0xc4, 0x19, 0xcf, 0x94, 0x18,
            0xfd, 0x64, 0x1b, 0x2a, 0xb3, 0x4b, 0xec, 0x09, 0x85, 0xc9, 0x2d, 0x8a, 0xcd, 0x45,
            0x87, 0x17, 0xca, 0x16, 0xff, 0x07, 0x19, 0x27, 0x6e, 0x81, 0xb2, 0x17, 0x43, 0x50,
            0x01,
        ];
        let text: &[u8] = b"the quick brown fox jumps over the lazy dog. \
the quick brown fox jumps over the lazy dog. time down life";
        assert_eq!(decompress(&data).unwrap(), text);
    }

    #[test]
    fn small_window_with_back_references() {
        // `brotli -q 5 -w 16` on "Hello, world! Hello, world! ".
        let data = [
            0x62, 0x03, 0x00, 0x80, 0x34, 0x48, 0x08, 0x0b, 0x92, 0x54, 0x64, 0x12, 0x69, 0xf2,
            0x99, 0x3e, 0xcf, 0x5a, 0xe4, 0x08,
        ];
        assert_eq!(decompress(&data).unwrap(), b"Hello, world! Hello, world! ");
    }

    #[test]
    fn dictionary_is_the_official_one() {
        // First and last bytes of the RFC 7932 dictionary.
        assert_eq!(&DICTIONARY[..4], b"time");
        assert_eq!(DICTIONARY.len(), 122784);
    }

    #[test]
    fn transforms_table_matches_appendix_b() {
        assert_eq!(TRANSFORMS[0], (&b""[..], 0, &b""[..]));
        assert_eq!(TRANSFORMS[1], (&b""[..], 0, &b" "[..]));
        assert_eq!(TRANSFORMS[120].1, 11);
    }

    #[test]
    fn truncated_is_an_error() {
        assert_eq!(decompress(&[]), Err(BrotliError::Truncated));
    }
}
