//! Exact conversions between binary64 and decimal (or any radix) text,
//! built on [`BigInt`]:
//!
//! - decimal → Number correctly rounded (ties to even): Clinger's fast path
//!   when the digits and the power of ten are exact doubles, and an exact
//!   big-integer quotient otherwise;
//! - Number → shortest decimal digits that round-trip (Steele & White /
//!   Burger & Dybvig free-format algorithm), picking the closest candidate
//!   and, between two equally close, the even one (Number::toString);
//! - the fixed digit counts of `toFixed`, `toExponential` and
//!   `toPrecision`, with ties rounded up as ECMA-262 asks;
//! - `toString(radix)` for radix ≠ 10, exact in both the integer part and
//!   the fraction digits.

use std::cmp::Ordering;

use crate::bigint::BigInt;

/// `2^k` for `k` in the normal exponent range.
fn pow2(k: i64) -> f64 {
    debug_assert!((-1022..=1023).contains(&k));
    f64::from_bits(((k + 1023) as u64) << 52)
}

/// `m · 2^e` for `m ≤ 2^54`, exact whenever the result is representable
/// (overflow gives Infinity).
fn scale_pow2(m: u64, e: i64) -> f64 {
    let x = m as f64;
    if e > 1000 {
        x * pow2(e - 600) * pow2(600)
    } else if e < -1000 {
        x * pow2(e + 600) * pow2(-600)
    } else {
        x * pow2(e)
    }
}

/// Mantissa and exponent of a finite non-negative `x`: `x = f · 2^e`.
pub(crate) fn decompose(x: f64) -> (u64, i64) {
    let bits = x.to_bits();
    let biased = ((bits >> 52) & 0x7ff) as i64;
    let frac = bits & ((1 << 52) - 1);
    if biased == 0 {
        (frac, -1074)
    } else {
        (frac | (1 << 52), biased - 1075)
    }
}

/// The binary64 nearest to `(q + ε) · 2^e2` (ties to even), where `ε` is
/// some value strictly between 0 and 1 when `sticky` and 0 otherwise. When
/// `sticky`, `q` must have at least 54 bits so that `ε` lies below the
/// rounding position.
pub(crate) fn round_to_f64(q: &BigInt, e2: i64, sticky: bool) -> f64 {
    let n = q.bit_length() as i64;
    if n == 0 {
        return 0.0;
    }
    let top = n - 1 + e2;
    if top > 1023 {
        return f64::INFINITY;
    }
    // Significant bits available at this magnitude (fewer for subnormals).
    let p = if top >= -1022 { 53 } else { 53 - (-1022 - top) };
    if p < 0 {
        return 0.0;
    }
    let drop = n - p;
    if drop <= 0 {
        return scale_pow2(q.low_u64() << (-drop), e2 + drop);
    }
    let m = q.shr_magnitude(drop as u64).low_u64();
    let half = q.bit((drop - 1) as u64);
    let lower = sticky || q.has_low_bits((drop - 1) as u64);
    let up = half && (lower || m & 1 == 1);
    scale_pow2(m + u64::from(up), e2 + drop)
}

const EXACT_POW10: [f64; 23] = [
    1e0, 1e1, 1e2, 1e3, 1e4, 1e5, 1e6, 1e7, 1e8, 1e9, 1e10, 1e11, 1e12, 1e13, 1e14, 1e15, 1e16,
    1e17, 1e18, 1e19, 1e20, 1e21, 1e22,
];

/// Significant digits kept by the exact path. Every halfway point between
/// two doubles has at most 767 significant digits, so a longer input is cut
/// here and a non-zero digit appended in place of the rest, which keeps it
/// on the same side of every boundary.
const MAX_EXACT_DIGITS: usize = 780;

/// The Number nearest to `digits × 10^exp10` (ties to even), where
/// `digits` are ASCII decimal digits read as an integer.
pub fn decimal_to_f64(digits: &[u8], exp10: i64) -> f64 {
    let Some(start) = digits.iter().position(|&d| d != b'0') else {
        return 0.0;
    };
    let d = &digits[start..];
    let end = d.iter().rposition(|&c| c != b'0').map_or(0, |i| i + 1);
    let mut exp = exp10.saturating_add((d.len() - end) as i64);
    let mut d = &d[..end];
    // The value lies in [10^(lead-1), 10^lead).
    let lead = (d.len() as i64).saturating_add(exp);
    if lead > 310 {
        return f64::INFINITY;
    }
    if lead < -324 {
        return 0.0;
    }
    if d.len() <= 15 {
        let m = d
            .iter()
            .fold(0u64, |acc, &c| acc * 10 + u64::from(c - b'0')) as f64;
        if (0..=22).contains(&exp) {
            return m * EXACT_POW10[exp as usize];
        }
        if (-22..0).contains(&exp) {
            return m / EXACT_POW10[(-exp) as usize];
        }
        if exp > 22 && exp - 22 <= 15 - d.len() as i64 {
            // Still an exact integer below 10^15 after the first factor.
            return (m * EXACT_POW10[(exp - 22) as usize]) * 1e22;
        }
    }
    let mut truncated = false;
    if d.len() > MAX_EXACT_DIGITS {
        exp += (d.len() - MAX_EXACT_DIGITS) as i64;
        d = &d[..MAX_EXACT_DIGITS];
        truncated = true;
    }
    let mut big = BigInt::parse_radix(d, 10).expect("ASCII decimal digits");
    if truncated {
        big = big.mul_small_add(10, 1);
        exp -= 1;
    }
    if exp >= 0 {
        let n = big.mul(&BigInt::pow10(exp as u32));
        return round_to_f64(&n, 0, false);
    }
    let s = BigInt::pow10((-exp) as u32);
    // Scale so that the quotient has at least 55 bits.
    let k = 55 + s.bit_length() as i64 - big.bit_length() as i64;
    let (num, den) = if k >= 0 {
        (big.shl(k as u64), s)
    } else {
        (big, s.shl((-k) as u64))
    };
    let (q, r) = num.div_rem(&den).expect("non-zero power of ten");
    round_to_f64(&q, -k, !r.is_zero())
}

/// Parses `[+-] digits [. digits] [(e|E) [+-] digits]` (at least one
/// digit before or after the point) into the correctly rounded Number.
/// `None` if `text` does not match exactly.
pub fn parse_decimal(text: &[u8]) -> Option<f64> {
    let mut i = 0;
    let negative = match text.first() {
        Some(b'-') => {
            i = 1;
            true
        }
        Some(b'+') => {
            i = 1;
            false
        }
        _ => false,
    };
    let mut digits = Vec::with_capacity(text.len());
    let int_start = i;
    while i < text.len() && text[i].is_ascii_digit() {
        digits.push(text[i]);
        i += 1;
    }
    let mut any = i > int_start;
    let mut exp: i64 = 0;
    if i < text.len() && text[i] == b'.' {
        i += 1;
        let frac_start = i;
        while i < text.len() && text[i].is_ascii_digit() {
            digits.push(text[i]);
            i += 1;
        }
        exp -= (i - frac_start) as i64;
        any |= i > frac_start;
    }
    if !any {
        return None;
    }
    if i < text.len() && (text[i] == b'e' || text[i] == b'E') {
        i += 1;
        let mut exp_negative = false;
        if i < text.len() && (text[i] == b'+' || text[i] == b'-') {
            exp_negative = text[i] == b'-';
            i += 1;
        }
        let exp_start = i;
        let mut e: i64 = 0;
        while i < text.len() && text[i].is_ascii_digit() {
            // Saturate: anything this large is already 0 or Infinity.
            e = (e * 10 + i64::from(text[i] - b'0')).min(1 << 40);
            i += 1;
        }
        if i == exp_start {
            return None;
        }
        exp += if exp_negative { -e } else { e };
    }
    if i != text.len() {
        return None;
    }
    let v = decimal_to_f64(&digits, exp);
    Some(if negative { -v } else { v })
}

/// Shortest decimal digits of a finite `x > 0` that read back as `x`:
/// `(digits, n)` with `x ≈ 0.d₁d₂…d_k × 10^n`, `d₁ ≠ 0` (the `s`, `k` and
/// `n` of Number::toString). Among the shortest candidates it picks the
/// closest to `x`, and between two equally close the even one.
pub fn shortest_digits(x: f64) -> (Vec<u8>, i32) {
    debug_assert!(x.is_finite() && x > 0.0);
    let (f, e) = decompose(x);
    // Round-to-nearest-even reading accepts the interval bounds when the
    // mantissa is even.
    let even = f & 1 == 0;
    let biased = (x.to_bits() >> 52) & 0x7ff;
    // At a power of two the gap below is half the gap above.
    let lower_closer = biased > 1 && f == 1 << 52;
    let fb = BigInt::from_u64(f);
    // x = r / s; the bounds are (r - mm) / s and (r + mp) / s.
    let (mut r, mut s, mut mp, mut mm) = if e >= 0 {
        let unit = BigInt::one().shl(e as u64);
        if lower_closer {
            (fb.shl(e as u64 + 2), BigInt::from_u64(4), unit.shl(1), unit)
        } else {
            (
                fb.shl(e as u64 + 1),
                BigInt::from_u64(2),
                unit.clone(),
                unit,
            )
        }
    } else if lower_closer {
        (
            fb.shl(2),
            BigInt::one().shl((2 - e) as u64),
            BigInt::from_u64(2),
            BigInt::one(),
        )
    } else {
        (
            fb.shl(1),
            BigInt::one().shl((1 - e) as u64),
            BigInt::one(),
            BigInt::one(),
        )
    };
    let mut k = x.log10().ceil() as i32;
    if k >= 0 {
        s = s.mul(&BigInt::pow10(k as u32));
    } else {
        let p = BigInt::pow10((-k) as u32);
        r = r.mul(&p);
        mp = mp.mul(&p);
        mm = mm.mul(&p);
    }
    // The smallest k with the upper bound below 10^k (or at it, when the
    // bound itself is not admissible).
    let high_ok = |r: &BigInt, mp: &BigInt, s: &BigInt| {
        let c = r.add(mp).cmp_bigint(s);
        if even {
            c == Ordering::Less
        } else {
            c != Ordering::Greater
        }
    };
    while !high_ok(&r, &mp, &s) {
        s = s.mul_small_add(10, 0);
        k += 1;
    }
    loop {
        let (r10, mp10) = (r.mul_small_add(10, 0), mp.mul_small_add(10, 0));
        if !high_ok(&r10, &mp10, &s) {
            break;
        }
        r = r10;
        mp = mp10;
        mm = mm.mul_small_add(10, 0);
        k -= 1;
    }
    let mut digits = Vec::new();
    loop {
        r = r.mul_small_add(10, 0);
        mp = mp.mul_small_add(10, 0);
        mm = mm.mul_small_add(10, 0);
        let (q, rem) = r.div_rem(&s).expect("non-zero scale");
        r = rem;
        let d = q.low_u64() as u8;
        let low = {
            let c = r.cmp_bigint(&mm);
            if even {
                c != Ordering::Greater
            } else {
                c == Ordering::Less
            }
        };
        let high = {
            let c = r.add(&mp).cmp_bigint(&s);
            if even {
                c != Ordering::Less
            } else {
                c == Ordering::Greater
            }
        };
        let digit = match (low, high) {
            (false, false) => {
                digits.push(b'0' + d);
                continue;
            }
            (true, false) => d,
            (false, true) => d + 1,
            (true, true) => match r.shl(1).cmp_bigint(&s) {
                Ordering::Less => d,
                Ordering::Greater => d + 1,
                Ordering::Equal => d + (d & 1),
            },
        };
        digits.push(b'0' + digit);
        return (digits, k);
    }
}

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
    if x.trunc() == x && x < 9007199254740992.0 {
        return (x as u64).to_string();
    }
    let (digits, n) = shortest_digits(x);
    let digits = String::from_utf8(digits).expect("ASCII digits");
    let k = digits.len() as i32;
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

/// `x · 10^scale` for a finite `x ≥ 0` as an exact fraction `(num, den)`.
fn scaled_fraction(x: f64, scale: i32) -> (BigInt, BigInt) {
    let (f, e) = decompose(x);
    let mut num = BigInt::from_u64(f);
    let mut den = BigInt::one();
    if e >= 0 {
        num = num.shl(e as u64);
    } else {
        den = den.shl((-e) as u64);
    }
    if scale >= 0 {
        num = num.mul(&BigInt::pow10(scale as u32));
    } else {
        den = den.mul(&BigInt::pow10((-scale) as u32));
    }
    (num, den)
}

/// The integer nearest to `x · 10^scale` for a finite `x ≥ 0`, ties
/// rounded up (the "larger n" of `toFixed`, `toExponential` and
/// `toPrecision`).
pub fn scaled_round(x: f64, scale: i32) -> BigInt {
    let (num, den) = scaled_fraction(x, scale);
    let (q, r) = num.div_rem(&den).expect("non-zero denominator");
    if r.shl(1).cmp_bigint(&den) != Ordering::Less {
        q.add(&BigInt::one())
    } else {
        q
    }
}

/// The `n` and `e` of `toExponential`/`toPrecision` for a finite `x > 0`:
/// `digits` decimal digits (`10^(digits-1) ≤ n < 10^digits`) with
/// `n × 10^(e-digits+1)` closest to `x`, ties to the larger `n`.
pub fn exponent_digits(x: f64, digits: u32) -> (String, i32) {
    debug_assert!(x.is_finite() && x > 0.0 && digits >= 1);
    let low = BigInt::pow10(digits - 1);
    let high = BigInt::pow10(digits);
    let mut e = x.log10().floor() as i32;
    for _ in 0..4 {
        let n = scaled_round(x, digits as i32 - 1 - e);
        if n.cmp_bigint(&high) != Ordering::Less {
            e += 1;
        } else if n.cmp_bigint(&low) == Ordering::Less {
            e -= 1;
        } else {
            return (n.to_string_radix(10), e);
        }
    }
    unreachable!("the decimal exponent estimate is off by at most one")
}

/// `Number.prototype.toFixed` for a finite `x` and `0 ≤ f ≤ 100`.
pub fn to_fixed(x: f64, f: u32) -> String {
    let (sign, x) = if x < 0.0 { ("-", -x) } else { ("", x) };
    if x >= 1e21 {
        return format!("{sign}{}", number_to_string(x));
    }
    let mut m = scaled_round(x, f as i32).to_string_radix(10);
    if f != 0 {
        let f = f as usize;
        if m.len() <= f {
            m = format!("{}{m}", "0".repeat(f + 1 - m.len()));
        }
        let point = m.len() - f;
        m = format!("{}.{}", &m[..point], &m[point..]);
    }
    format!("{sign}{m}")
}

fn exponent_suffix(e: i32) -> String {
    format!("e{}{}", if e < 0 { '-' } else { '+' }, e.abs())
}

/// `Number.prototype.toExponential` for a finite `x`; `None` for an
/// undefined fraction digits argument (as many digits as needed).
pub fn to_exponential(x: f64, f: Option<u32>) -> String {
    let (sign, x) = if x < 0.0 { ("-", -x) } else { ("", x) };
    let (m, e) = if x == 0.0 {
        ("0".repeat(f.unwrap_or(0) as usize + 1), 0)
    } else {
        match f {
            Some(f) => exponent_digits(x, f + 1),
            None => {
                let (digits, n) = shortest_digits(x);
                (String::from_utf8(digits).expect("ASCII digits"), n - 1)
            }
        }
    };
    let m = if m.len() > 1 {
        format!("{}.{}", &m[..1], &m[1..])
    } else {
        m
    };
    format!("{sign}{m}{}", exponent_suffix(e))
}

/// `Number.prototype.toPrecision` for a finite `x` and `1 ≤ p ≤ 100`.
pub fn to_precision(x: f64, p: u32) -> String {
    let (sign, x) = if x < 0.0 { ("-", -x) } else { ("", x) };
    let (m, e) = if x == 0.0 {
        ("0".repeat(p as usize), 0)
    } else {
        exponent_digits(x, p)
    };
    let p = p as i32;
    let body = if x != 0.0 && (e < -6 || e >= p) {
        let m = if p > 1 {
            format!("{}.{}", &m[..1], &m[1..])
        } else {
            m
        };
        format!("{m}{}", exponent_suffix(e))
    } else if e == p - 1 {
        m
    } else if e >= 0 {
        let point = (e + 1) as usize;
        format!("{}.{}", &m[..point], &m[point..])
    } else {
        format!("0.{}{m}", "0".repeat((-(e + 1)) as usize))
    };
    format!("{sign}{body}")
}

fn radix_digit(d: u32) -> u8 {
    char::from_digit(d, 36).expect("digit below 36") as u8
}

/// Number::toString(x, radix) for `2 ≤ radix ≤ 36`. The integer part is
/// exact; the fraction stops at the first digit from which the value
/// already reads back as `x` (half an ulp, as in V8), with the remainder
/// rounded half to even. All arithmetic is exact.
pub fn number_to_string_radix(x: f64, radix: u32) -> String {
    if radix == 10 || !x.is_finite() || x == 0.0 {
        return number_to_string(x);
    }
    let negative = x < 0.0;
    let x = x.abs();
    let mut integer = BigInt::from_f64(x.trunc()).expect("integral");
    let (f, e) = decompose(x);
    let mut fraction: Vec<u8> = Vec::new();
    if e < 0 {
        let bits = (-e) as u64;
        // Units of 2^-(bits+1), so that half an ulp is a whole unit.
        let den = BigInt::one().shl(bits + 1);
        let frac_mantissa = if bits >= 64 {
            f
        } else {
            f & ((1u64 << bits) - 1)
        };
        let mut num = BigInt::from_u64(frac_mantissa).shl(1);
        // The smallest positive Number bounds delta from below.
        let mut delta = BigInt::from_u64(if e == -1074 { 2 } else { 1 });
        if num.cmp_bigint(&delta) != Ordering::Less {
            loop {
                num = num.mul_small_add(radix, 0);
                delta = delta.mul_small_add(radix, 0);
                let (q, r) = num.div_rem(&den).expect("non-zero denominator");
                num = r;
                let digit = q.low_u64() as u32;
                fraction.push(radix_digit(digit));
                let twice = num.shl(1).cmp_bigint(&den);
                let above_half =
                    twice == Ordering::Greater || (twice == Ordering::Equal && digit & 1 == 1);
                if above_half && num.add(&delta).cmp_bigint(&den) == Ordering::Greater {
                    // Round up, carrying through the digits.
                    loop {
                        match fraction.pop() {
                            None => {
                                integer = integer.add(&BigInt::one());
                                break;
                            }
                            Some(c) => {
                                let d = (c as char).to_digit(36).expect("radix digit");
                                if d + 1 < radix {
                                    fraction.push(radix_digit(d + 1));
                                    break;
                                }
                            }
                        }
                    }
                    break;
                }
                if num.cmp_bigint(&delta) == Ordering::Less {
                    break;
                }
            }
        }
    }
    let mut out = String::new();
    if negative {
        out.push('-');
    }
    out.push_str(&integer.to_string_radix(radix));
    if !fraction.is_empty() {
        out.push('.');
        out.push_str(std::str::from_utf8(&fraction).expect("ASCII digits"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> f64 {
        parse_decimal(s.as_bytes()).unwrap()
    }

    #[test]
    fn decimal_parsing_rounds_correctly() {
        assert_eq!(parse("0.1"), 0.1);
        assert_eq!(parse("5e-324"), 5e-324);
        assert_eq!(parse("2.4703282292062327e-324"), 0.0);
        assert_eq!(parse("2.4703282292062328e-324"), 5e-324);
        assert_eq!(parse("1.7976931348623157e308"), f64::MAX);
        assert_eq!(parse("1.7976931348623159e308"), f64::INFINITY);
        assert_eq!(parse("9007199254740993"), 9007199254740992.0);
        assert_eq!(parse("9007199254740993.0000000001"), 9007199254740994.0);
        assert_eq!(parse("2.2250738585072011e-308"), 2.225073858507201e-308);
        assert_eq!(parse("-0"), 0.0);
        assert!(parse("-0").is_sign_negative());
        assert_eq!(parse(".5e1"), 5.0);
        assert_eq!(parse_decimal(b"e5"), None);
        assert_eq!(parse_decimal(b"1e"), None);
        assert_eq!(parse("1e99999999999999999999"), f64::INFINITY);
    }

    #[test]
    fn shortest_digits_round_trip() {
        for x in [
            0.1,
            0.3,
            1.0 / 3.0,
            5e-324,
            f64::MAX,
            2f64.powi(-1022),
            1e23,
            123.456,
        ] {
            let s = number_to_string(x);
            assert_eq!(parse(&s), x, "{s}");
        }
        assert_eq!(number_to_string(1e23), "1e+23");
        assert_eq!(number_to_string(f64::from_bits(1)), "5e-324");
    }

    #[test]
    fn fixed_point_forms() {
        assert_eq!(to_fixed(1.005, 2), "1.00");
        assert_eq!(to_fixed(0.5, 0), "1");
        assert_eq!(to_fixed(-0.0000001, 2), "-0.00");
        assert_eq!(to_exponential(123456.0, Some(2)), "1.23e+5");
        assert_eq!(to_exponential(0.0, Some(2)), "0.00e+0");
        assert_eq!(to_precision(0.000001234, 2), "0.0000012");
        assert_eq!(to_precision(1e21, 3), "1.00e+21");
        assert_eq!(number_to_string_radix(255.5, 16), "ff.8");
        assert_eq!(number_to_string_radix(-0.5, 2), "-0.1");
    }
}
