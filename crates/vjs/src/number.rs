//! Number ↔ string conversions (Number::toString with radix 10 and
//! StringToNumber, ECMA-262 §6.1.6.1.20 and §7.1.4.1.1), integer
//! conversions and binary16 rounding.

use crate::bigint::BigInt;
use crate::unicode::{is_line_terminator, is_whitespace};

/// Number::toString(x, 10).
pub fn number_to_string(x: f64) -> String {
    if x.is_nan() {
        return "NaN".to_string();
    }
    if x == 0.0 {
        return "0".to_string();
    }
    if x.is_infinite() {
        return if x < 0.0 { "-Infinity" } else { "Infinity" }.to_string();
    }
    if x < 0.0 {
        return format!("-{}", number_to_string(-x));
    }
    // `{:e}` prints the shortest digits that round-trip: "d.ddde±x".
    let sci = format!("{x:e}");
    let (mantissa, exponent) = sci.split_once('e').expect("LowerExp has an exponent");
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let e: i32 = exponent.parse().expect("LowerExp exponent is an integer");
    let k = digits.len() as i32;
    let n = e + 1;
    if k <= n && n <= 21 {
        let mut s = digits;
        s.extend(std::iter::repeat_n('0', (n - k) as usize));
        s
    } else if 0 < n && n <= 21 {
        format!("{}.{}", &digits[..n as usize], &digits[n as usize..])
    } else if -6 < n && n <= 0 {
        format!("0.{}{}", "0".repeat((-n) as usize), digits)
    } else {
        let sign = if n - 1 < 0 { '-' } else { '+' };
        let exp = (n - 1).abs();
        if k == 1 {
            format!("{digits}e{sign}{exp}")
        } else {
            format!("{}.{}e{sign}{exp}", &digits[..1], &digits[1..])
        }
    }
}

fn is_str_whitespace(u: u16) -> bool {
    match char::from_u32(u32::from(u)) {
        Some(c) => is_whitespace(c) || is_line_terminator(c),
        None => false,
    }
}

/// Removes leading and trailing WhiteSpace and LineTerminator code units.
pub fn trim_str_whitespace(units: &[u16]) -> &[u16] {
    let start = units
        .iter()
        .position(|u| !is_str_whitespace(*u))
        .unwrap_or(units.len());
    let end = units
        .iter()
        .rposition(|u| !is_str_whitespace(*u))
        .map_or(start, |i| i + 1);
    &units[start..end]
}

/// Digits of a non-decimal integer literal (`0x…`, `0o…`, `0b…`), if the
/// text is one.
pub(crate) fn non_decimal_digits(text: &[u8]) -> Option<(&[u8], u32)> {
    if text.len() < 3 || text[0] != b'0' {
        return None;
    }
    let radix = match text[1] {
        b'x' | b'X' => 16,
        b'o' | b'O' => 8,
        b'b' | b'B' => 2,
        _ => return None,
    };
    Some((&text[2..], radix))
}

/// StringToNumber.
pub fn string_to_number(units: &[u16]) -> f64 {
    let trimmed = trim_str_whitespace(units);
    if trimmed.is_empty() {
        return 0.0;
    }
    let mut text = Vec::with_capacity(trimmed.len());
    for &u in trimmed {
        if u > 0x7f {
            return f64::NAN;
        }
        text.push(u as u8);
    }
    if let Some((digits, radix)) = non_decimal_digits(&text) {
        return match BigInt::parse_radix(digits, radix) {
            Some(b) => b.to_f64(),
            None => f64::NAN,
        };
    }
    let (negative, body) = match text[0] {
        b'+' => (false, &text[1..]),
        b'-' => (true, &text[1..]),
        _ => (false, &text[..]),
    };
    let magnitude = if body == b"Infinity" {
        f64::INFINITY
    } else {
        match parse_unsigned_decimal(body) {
            Some(v) => v,
            None => return f64::NAN,
        }
    };
    if negative { -magnitude } else { magnitude }
}

/// StrUnsignedDecimalLiteral without `Infinity`: digits with an optional
/// fraction and exponent, no separators.
fn parse_unsigned_decimal(body: &[u8]) -> Option<f64> {
    let mut i = 0;
    let int_start = i;
    while i < body.len() && body[i].is_ascii_digit() {
        i += 1;
    }
    let int_part = &body[int_start..i];
    let mut frac_part: &[u8] = &[];
    if i < body.len() && body[i] == b'.' {
        i += 1;
        let frac_start = i;
        while i < body.len() && body[i].is_ascii_digit() {
            i += 1;
        }
        frac_part = &body[frac_start..i];
    }
    if int_part.is_empty() && frac_part.is_empty() {
        return None;
    }
    let mut exp_part: &[u8] = &[];
    if i < body.len() && (body[i] == b'e' || body[i] == b'E') {
        let exp_start = i + 1;
        i += 1;
        if i < body.len() && (body[i] == b'+' || body[i] == b'-') {
            i += 1;
        }
        let digits_start = i;
        while i < body.len() && body[i].is_ascii_digit() {
            i += 1;
        }
        if i == digits_start {
            return None;
        }
        exp_part = &body[exp_start..i];
    }
    if i != body.len() {
        return None;
    }
    let mut normalized = String::with_capacity(body.len() + 3);
    if int_part.is_empty() {
        normalized.push('0');
    } else {
        normalized.push_str(std::str::from_utf8(int_part).ok()?);
    }
    if !frac_part.is_empty() {
        normalized.push('.');
        normalized.push_str(std::str::from_utf8(frac_part).ok()?);
    }
    if !exp_part.is_empty() {
        normalized.push('e');
        normalized.push_str(std::str::from_utf8(exp_part).ok()?);
    }
    normalized.parse::<f64>().ok()
}

/// The integer of `x` modulo 2^bits as used by ToInt32 and friends
/// (`x` already finite, non-zero).
fn modulo_pow2(x: f64, bits: i32) -> f64 {
    x.trunc().rem_euclid(2f64.powi(bits))
}

pub fn f64_to_int32(x: f64) -> i32 {
    if !x.is_finite() || x == 0.0 {
        return 0;
    }
    modulo_pow2(x, 32) as u32 as i32
}

pub fn f64_to_uint32(x: f64) -> u32 {
    if !x.is_finite() || x == 0.0 {
        return 0;
    }
    modulo_pow2(x, 32) as u32
}

pub fn f64_to_int16(x: f64) -> i16 {
    f64_to_uint16(x) as i16
}

pub fn f64_to_uint16(x: f64) -> u16 {
    if !x.is_finite() || x == 0.0 {
        return 0;
    }
    modulo_pow2(x, 16) as u16
}

pub fn f64_to_int8(x: f64) -> i8 {
    f64_to_uint8(x) as i8
}

pub fn f64_to_uint8(x: f64) -> u8 {
    if !x.is_finite() || x == 0.0 {
        return 0;
    }
    modulo_pow2(x, 8) as u8
}

/// ToUint8Clamp on a number.
pub fn f64_to_uint8_clamp(x: f64) -> u8 {
    if x.is_nan() || x <= 0.0 {
        return 0;
    }
    if x >= 255.0 {
        return 255;
    }
    round_half_even(x) as u8
}

/// Rounds to the nearest integer, ties to even.
pub fn round_half_even(x: f64) -> f64 {
    let f = x.floor();
    let d = x - f;
    if d > 0.5 {
        f + 1.0
    } else if d < 0.5 {
        f
    } else if f % 2.0 == 0.0 {
        f
    } else {
        f + 1.0
    }
}

/// Rounds `x` to IEEE 754 binary16 (ties to even) and returns its bits.
pub fn f64_to_f16_bits(x: f64) -> u16 {
    let sign: u16 = if x.is_sign_negative() { 0x8000 } else { 0 };
    if x.is_nan() {
        return 0x7e00;
    }
    let a = x.abs();
    if a.is_infinite() || a >= 65520.0 {
        return sign | 0x7c00;
    }
    if a < 2f64.powi(-14) {
        // Subnormal (or zero): units of 2^-24; 1024 becomes the smallest
        // normal, whose bits follow on.
        let m = round_half_even(a * 2f64.powi(24)) as u16;
        return sign | m;
    }
    let mut exp = ((a.to_bits() >> 52) & 0x7ff) as i32 - 1023;
    let mantissa = a / 2f64.powi(exp);
    let mut m = round_half_even((mantissa - 1.0) * 1024.0) as u32;
    if m == 1024 {
        exp += 1;
        m = 0;
    }
    if exp > 15 {
        return sign | 0x7c00;
    }
    sign | (((exp + 15) as u16) << 10) | m as u16
}

pub fn f16_bits_to_f64(h: u16) -> f64 {
    let sign = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exp = i32::from((h >> 10) & 0x1f);
    let m = f64::from(h & 0x3ff);
    let magnitude = match exp {
        0 => m * 2f64.powi(-24),
        31 => {
            if m == 0.0 {
                f64::INFINITY
            } else {
                f64::NAN
            }
        }
        _ => (1.0 + m / 1024.0) * 2f64.powi(exp - 15),
    };
    sign * magnitude
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::string::utf16;

    #[test]
    fn number_to_string_follows_the_spec_forms() {
        assert_eq!(number_to_string(1.0), "1");
        assert_eq!(number_to_string(-1.5), "-1.5");
        assert_eq!(number_to_string(123.456), "123.456");
        assert_eq!(number_to_string(1e21), "1e+21");
        assert_eq!(number_to_string(1e20), "100000000000000000000");
        assert_eq!(number_to_string(0.000001), "0.000001");
        assert_eq!(number_to_string(1e-7), "1e-7");
        assert_eq!(number_to_string(1.25e-7), "1.25e-7");
        assert_eq!(number_to_string(-0.0), "0");
        assert_eq!(number_to_string(0.1 + 0.2), "0.30000000000000004");
        assert_eq!(number_to_string(f64::MAX), "1.7976931348623157e+308");
        assert_eq!(number_to_string(5e-324), "5e-324");
    }

    #[test]
    fn string_to_number_grammar() {
        let n = |s: &str| string_to_number(&utf16(s));
        assert_eq!(n(""), 0.0);
        assert_eq!(n(" \n\t "), 0.0);
        assert_eq!(n(" 12 "), 12.0);
        assert_eq!(n("0x1F"), 31.0);
        assert_eq!(n("0b101"), 5.0);
        assert_eq!(n("0o17"), 15.0);
        assert!(n("-0x1").is_nan());
        assert!(n("1_0").is_nan());
        assert!(n("0x").is_nan());
        assert_eq!(n(".5"), 0.5);
        assert_eq!(n("5."), 5.0);
        assert_eq!(n("1e3"), 1000.0);
        assert!(n("1e").is_nan());
        assert!(n(".").is_nan());
        assert_eq!(n("-Infinity"), f64::NEG_INFINITY);
        assert!(n("infinity").is_nan());
        assert!(n("inf").is_nan());
        assert!(n("NaN").is_nan());
        assert!(n("-0").is_sign_negative());
        assert_eq!(n("0x20000000000001"), 9007199254740992.0);
    }

    #[test]
    fn integer_conversions_wrap() {
        assert_eq!(f64_to_int32(4294967296.0 + 5.0), 5);
        assert_eq!(f64_to_int32(-1.0), -1);
        assert_eq!(f64_to_int32(2147483648.0), -2147483648);
        assert_eq!(f64_to_uint32(-1.0), 4294967295);
        assert_eq!(f64_to_int8(200.0), -56);
        assert_eq!(f64_to_uint8_clamp(2.5), 2);
        assert_eq!(f64_to_uint8_clamp(3.5), 4);
        assert_eq!(f64_to_uint8_clamp(-3.0), 0);
        assert_eq!(f64_to_uint8_clamp(300.0), 255);
    }

    #[test]
    fn binary16_rounds_to_even() {
        assert_eq!(f64_to_f16_bits(1.0), 0x3c00);
        assert_eq!(f64_to_f16_bits(65504.0), 0x7bff);
        assert_eq!(f64_to_f16_bits(65520.0), 0x7c00);
        assert_eq!(f64_to_f16_bits(2f64.powi(-24)), 0x0001);
        assert_eq!(f64_to_f16_bits(2f64.powi(-25)), 0x0000);
        assert_eq!(f64_to_f16_bits(1.0 + 2f64.powi(-11)), 0x3c00);
        assert_eq!(f16_bits_to_f64(0x3c00), 1.0);
        assert_eq!(f16_bits_to_f64(0x0001), 2f64.powi(-24));
    }
}
