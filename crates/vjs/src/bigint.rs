//! Arbitrary-precision integers for the BigInt type (ECMA-262 §6.1.6.2):
//! sign and magnitude in little-endian 32-bit words.

use std::cmp::Ordering;

#[derive(Clone, Debug, PartialEq, Eq, Hash, Default)]
pub struct BigInt {
    negative: bool,
    /// Little-endian words without trailing zeros; zero is empty.
    mag: Vec<u32>,
}

fn trim(mut mag: Vec<u32>) -> Vec<u32> {
    while mag.last() == Some(&0) {
        mag.pop();
    }
    mag
}

fn mag_cmp(a: &[u32], b: &[u32]) -> Ordering {
    if a.len() != b.len() {
        return a.len().cmp(&b.len());
    }
    for i in (0..a.len()).rev() {
        if a[i] != b[i] {
            return a[i].cmp(&b[i]);
        }
    }
    Ordering::Equal
}

fn mag_add(a: &[u32], b: &[u32]) -> Vec<u32> {
    let (long, short) = if a.len() >= b.len() { (a, b) } else { (b, a) };
    let mut out = Vec::with_capacity(long.len() + 1);
    let mut carry = 0u64;
    for i in 0..long.len() {
        let s = u64::from(long[i]) + u64::from(*short.get(i).unwrap_or(&0)) + carry;
        out.push(s as u32);
        carry = s >> 32;
    }
    if carry != 0 {
        out.push(carry as u32);
    }
    out
}

/// `a - b` for `a >= b`.
fn mag_sub(a: &[u32], b: &[u32]) -> Vec<u32> {
    let mut out = Vec::with_capacity(a.len());
    let mut borrow = 0i64;
    for i in 0..a.len() {
        let mut d = i64::from(a[i]) - i64::from(*b.get(i).unwrap_or(&0)) - borrow;
        if d < 0 {
            d += 1 << 32;
            borrow = 1;
        } else {
            borrow = 0;
        }
        out.push(d as u32);
    }
    trim(out)
}

fn mag_mul(a: &[u32], b: &[u32]) -> Vec<u32> {
    if a.is_empty() || b.is_empty() {
        return Vec::new();
    }
    let mut out = vec![0u32; a.len() + b.len()];
    for (i, &x) in a.iter().enumerate() {
        let mut carry = 0u64;
        for (j, &y) in b.iter().enumerate() {
            let t = u64::from(x) * u64::from(y) + u64::from(out[i + j]) + carry;
            out[i + j] = t as u32;
            carry = t >> 32;
        }
        let mut k = i + b.len();
        while carry != 0 {
            let t = u64::from(out[k]) + carry;
            out[k] = t as u32;
            carry = t >> 32;
            k += 1;
        }
    }
    trim(out)
}

/// `a * m + add` in place.
fn mag_mul_small_add(a: &mut Vec<u32>, m: u32, add: u32) {
    let mut carry = u64::from(add);
    for w in a.iter_mut() {
        let t = u64::from(*w) * u64::from(m) + carry;
        *w = t as u32;
        carry = t >> 32;
    }
    if carry != 0 {
        a.push(carry as u32);
    }
}

/// Quotient and remainder of a division by one word.
fn mag_divrem_small(a: &[u32], d: u32) -> (Vec<u32>, u32) {
    let mut q = vec![0u32; a.len()];
    let mut r = 0u64;
    for i in (0..a.len()).rev() {
        let cur = (r << 32) | u64::from(a[i]);
        q[i] = (cur / u64::from(d)) as u32;
        r = cur % u64::from(d);
    }
    (trim(q), r as u32)
}

fn shl_words(a: &[u32], bits: u32) -> Vec<u32> {
    if bits == 0 {
        return a.to_vec();
    }
    let mut out = Vec::with_capacity(a.len() + 1);
    let mut carry = 0u32;
    for &w in a {
        out.push((w << bits) | carry);
        carry = w >> (32 - bits);
    }
    out.push(carry);
    out
}

/// Knuth's algorithm D: quotient and remainder of magnitudes, `v` non-zero.
fn mag_divrem(u: &[u32], v: &[u32]) -> (Vec<u32>, Vec<u32>) {
    if mag_cmp(u, v) == Ordering::Less {
        return (Vec::new(), u.to_vec());
    }
    if v.len() == 1 {
        let (q, r) = mag_divrem_small(u, v[0]);
        return (q, if r == 0 { Vec::new() } else { vec![r] });
    }
    let s = v[v.len() - 1].leading_zeros();
    let mut vn = shl_words(v, s);
    vn.truncate(v.len());
    let mut un = shl_words(u, s);
    if un.len() == u.len() {
        un.push(0);
    }
    let n = vn.len();
    let m = u.len() - n;
    let mut q = vec![0u32; m + 1];
    let base = 1u64 << 32;
    for j in (0..=m).rev() {
        let num = (u64::from(un[j + n]) << 32) | u64::from(un[j + n - 1]);
        let mut qhat = num / u64::from(vn[n - 1]);
        let mut rhat = num % u64::from(vn[n - 1]);
        while qhat >= base
            || qhat * u64::from(vn[n - 2]) > ((rhat << 32) | u64::from(un[j + n - 2]))
        {
            qhat -= 1;
            rhat += u64::from(vn[n - 1]);
            if rhat >= base {
                break;
            }
        }
        let mut borrow = 0i64;
        let mut carry = 0u64;
        for i in 0..n {
            let p = qhat * u64::from(vn[i]) + carry;
            carry = p >> 32;
            let t = i64::from(un[i + j]) - borrow - (p & 0xffff_ffff) as i64;
            un[i + j] = t as u32;
            borrow = if t < 0 { 1 } else { 0 };
        }
        let t = i64::from(un[j + n]) - borrow - carry as i64;
        un[j + n] = t as u32;
        if t < 0 {
            qhat -= 1;
            let mut c = 0u64;
            for i in 0..n {
                let s2 = u64::from(un[i + j]) + u64::from(vn[i]) + c;
                un[i + j] = s2 as u32;
                c = s2 >> 32;
            }
            un[j + n] = un[j + n].wrapping_add(c as u32);
        }
        q[j] = qhat as u32;
    }
    let mut r = vec![0u32; n];
    for i in 0..n {
        r[i] = if s == 0 {
            un[i]
        } else {
            (un[i] >> s) | (un[i + 1] << (32 - s))
        };
    }
    (trim(q), trim(r))
}

/// Two's complement words of `x` (infinitely sign-extended) cut to `len`.
fn to_twos(x: &BigInt, len: usize) -> Vec<u32> {
    let mut out = x.mag.clone();
    out.resize(len, 0);
    if x.negative {
        for w in out.iter_mut() {
            *w = !*w;
        }
        for w in out.iter_mut() {
            let (s, overflow) = w.overflowing_add(1);
            *w = s;
            if !overflow {
                break;
            }
        }
    }
    out
}

fn from_twos(mut words: Vec<u32>) -> BigInt {
    let negative = words.last().is_some_and(|w| w & 0x8000_0000 != 0);
    if negative {
        for w in words.iter_mut() {
            *w = !*w;
        }
        for w in words.iter_mut() {
            let (s, overflow) = w.overflowing_add(1);
            *w = s;
            if !overflow {
                break;
            }
        }
    }
    BigInt::from_parts(negative, words)
}

impl BigInt {
    fn from_parts(negative: bool, mag: Vec<u32>) -> BigInt {
        let mag = trim(mag);
        let negative = negative && !mag.is_empty();
        BigInt { negative, mag }
    }

    pub fn zero() -> BigInt {
        BigInt::default()
    }

    pub fn from_i64(n: i64) -> BigInt {
        let m = n.unsigned_abs();
        BigInt::from_parts(n < 0, vec![m as u32, (m >> 32) as u32])
    }

    pub fn from_u64(n: u64) -> BigInt {
        BigInt::from_parts(false, vec![n as u32, (n >> 32) as u32])
    }

    pub fn is_zero(&self) -> bool {
        self.mag.is_empty()
    }

    pub fn is_negative(&self) -> bool {
        self.negative
    }

    pub fn word_count(&self) -> usize {
        self.mag.len()
    }

    /// Number of bits of the magnitude.
    pub fn bit_length(&self) -> u64 {
        match self.mag.last() {
            None => 0,
            Some(top) => (self.mag.len() as u64) * 32 - u64::from(top.leading_zeros()),
        }
    }

    /// The BigInt of an integral finite number (NumberToBigInt).
    pub fn from_f64(x: f64) -> Option<BigInt> {
        if !x.is_finite() || x.trunc() != x {
            return None;
        }
        if x == 0.0 {
            return Some(BigInt::zero());
        }
        let bits = x.to_bits();
        let exp = ((bits >> 52) & 0x7ff) as i64;
        let frac = bits & ((1 << 52) - 1);
        let (mantissa, shift) = if exp == 0 {
            (frac, -1074)
        } else {
            (frac | (1 << 52), exp - 1075)
        };
        let m = BigInt::from_u64(mantissa);
        let r = if shift >= 0 {
            m.shl(shift as u64)
        } else {
            m.shr_magnitude((-shift) as u64)
        };
        Some(if x < 0.0 { r.neg() } else { r })
    }

    /// The Number closest to this integer (ties to even).
    pub fn to_f64(&self) -> f64 {
        let bits = self.bit_length();
        let magnitude = if bits <= 64 {
            self.low_u64() as f64
        } else {
            let shift = bits - 64;
            let top = self.shr_magnitude(shift).low_u64();
            let sticky = self.has_low_bits(shift);
            let top = if sticky { top | 1 } else { top };
            if shift > 2000 {
                f64::INFINITY
            } else {
                (top as f64) * 2f64.powi(shift as i32)
            }
        };
        if self.negative { -magnitude } else { magnitude }
    }

    fn low_u64(&self) -> u64 {
        let lo = u64::from(*self.mag.first().unwrap_or(&0));
        let hi = u64::from(*self.mag.get(1).unwrap_or(&0));
        lo | (hi << 32)
    }

    /// Whether any of the lowest `bits` bits of the magnitude is set.
    fn has_low_bits(&self, bits: u64) -> bool {
        let words = (bits / 32) as usize;
        let rest = (bits % 32) as u32;
        if self.mag.iter().take(words).any(|w| *w != 0) {
            return true;
        }
        rest != 0
            && self
                .mag
                .get(words)
                .is_some_and(|w| w & ((1u32 << rest) - 1) != 0)
    }

    /// Magnitude shifted right (sign kept, truncating).
    fn shr_magnitude(&self, bits: u64) -> BigInt {
        let words = (bits / 32) as usize;
        if words >= self.mag.len() {
            return BigInt::zero();
        }
        let rest = (bits % 32) as u32;
        let src = &self.mag[words..];
        let mut out = Vec::with_capacity(src.len());
        for i in 0..src.len() {
            let lo = src[i] >> rest;
            let hi = if rest == 0 {
                0
            } else {
                src.get(i + 1).map_or(0, |w| w << (32 - rest))
            };
            out.push(lo | hi);
        }
        BigInt::from_parts(self.negative, out)
    }

    /// Parses unsigned digits in `radix` (2..=36); `None` if empty or a
    /// digit is invalid.
    pub fn parse_radix(digits: &[u8], radix: u32) -> Option<BigInt> {
        if digits.is_empty() {
            return None;
        }
        let mut mag = Vec::new();
        for &d in digits {
            let v = (d as char).to_digit(radix)?;
            mag_mul_small_add(&mut mag, radix, v);
        }
        Some(BigInt::from_parts(false, mag))
    }

    pub fn to_string_radix(&self, radix: u32) -> String {
        if self.is_zero() {
            return "0".to_string();
        }
        let mut digits = Vec::new();
        let mut cur = self.mag.clone();
        while !cur.is_empty() {
            let (q, r) = mag_divrem_small(&cur, radix);
            digits.push(char::from_digit(r, radix).expect("digit below radix"));
            cur = q;
        }
        if self.negative {
            digits.push('-');
        }
        digits.iter().rev().collect()
    }

    pub fn neg(&self) -> BigInt {
        BigInt::from_parts(!self.negative, self.mag.clone())
    }

    pub fn add(&self, other: &BigInt) -> BigInt {
        if self.negative == other.negative {
            return BigInt::from_parts(self.negative, mag_add(&self.mag, &other.mag));
        }
        match mag_cmp(&self.mag, &other.mag) {
            Ordering::Equal => BigInt::zero(),
            Ordering::Greater => BigInt::from_parts(self.negative, mag_sub(&self.mag, &other.mag)),
            Ordering::Less => BigInt::from_parts(other.negative, mag_sub(&other.mag, &self.mag)),
        }
    }

    pub fn sub(&self, other: &BigInt) -> BigInt {
        self.add(&other.neg())
    }

    pub fn mul(&self, other: &BigInt) -> BigInt {
        BigInt::from_parts(
            self.negative != other.negative,
            mag_mul(&self.mag, &other.mag),
        )
    }

    /// Truncating division and remainder; `None` when dividing by zero.
    pub fn div_rem(&self, other: &BigInt) -> Option<(BigInt, BigInt)> {
        if other.is_zero() {
            return None;
        }
        let (q, r) = mag_divrem(&self.mag, &other.mag);
        Some((
            BigInt::from_parts(self.negative != other.negative, q),
            BigInt::from_parts(self.negative, r),
        ))
    }

    /// `self ** exponent`; `None` if the result would be absurdly large.
    pub fn pow(&self, exponent: u64) -> Option<BigInt> {
        if exponent == 0 {
            return Some(BigInt::from_i64(1));
        }
        if self.is_zero() || self.mag == [1] {
            let negative = self.negative && exponent % 2 == 1;
            return Some(BigInt::from_parts(negative, self.mag.clone()));
        }
        if self.bit_length().saturating_mul(exponent) > MAX_BIGINT_BITS {
            return None;
        }
        let mut result = BigInt::from_i64(1);
        let mut base = self.clone();
        let mut e = exponent;
        while e > 0 {
            if e & 1 == 1 {
                result = result.mul(&base);
            }
            e >>= 1;
            if e > 0 {
                base = base.mul(&base);
            }
        }
        Some(result)
    }

    /// `self * 2^bits`.
    pub fn shl(&self, bits: u64) -> BigInt {
        if self.is_zero() {
            return BigInt::zero();
        }
        let words = (bits / 32) as usize;
        let mut mag = vec![0u32; words];
        mag.extend(shl_words(&self.mag, (bits % 32) as u32));
        BigInt::from_parts(self.negative, mag)
    }

    /// `floor(self / 2^bits)`.
    pub fn shr(&self, bits: u64) -> BigInt {
        if !self.negative {
            return self.shr_magnitude(bits);
        }
        // floor(-m / 2^n) = -((m - 1) >> n) - 1
        let m1 = BigInt::from_parts(false, mag_sub(&self.mag, &[1]));
        let shifted = m1.shr_magnitude(bits);
        shifted.add(&BigInt::from_i64(1)).neg()
    }

    /// Left shift by a signed amount (BigInt::leftShift); `None` if the
    /// result is too large.
    pub fn shift_left(&self, amount: &BigInt) -> Option<BigInt> {
        let words = amount.mag.len();
        if amount.negative {
            if words > 2 {
                return Some(if self.negative {
                    BigInt::from_i64(-1)
                } else {
                    BigInt::zero()
                });
            }
            return Some(self.shr(amount.low_u64()));
        }
        if self.is_zero() {
            return Some(BigInt::zero());
        }
        if words > 2 || self.bit_length().saturating_add(amount.low_u64()) > MAX_BIGINT_BITS {
            return None;
        }
        Some(self.shl(amount.low_u64()))
    }

    fn bitwise(&self, other: &BigInt, op: impl Fn(u32, u32) -> u32) -> BigInt {
        let len = self.mag.len().max(other.mag.len()) + 1;
        let a = to_twos(self, len);
        let b = to_twos(other, len);
        from_twos(a.iter().zip(&b).map(|(x, y)| op(*x, *y)).collect())
    }

    pub fn bitand(&self, other: &BigInt) -> BigInt {
        self.bitwise(other, |x, y| x & y)
    }

    pub fn bitor(&self, other: &BigInt) -> BigInt {
        self.bitwise(other, |x, y| x | y)
    }

    pub fn bitxor(&self, other: &BigInt) -> BigInt {
        self.bitwise(other, |x, y| x ^ y)
    }

    /// `~self` = `-self - 1`.
    pub fn bitnot(&self) -> BigInt {
        self.neg().sub(&BigInt::from_i64(1))
    }

    /// BigInt.asUintN: `self` modulo 2^bits. `None` if the result would
    /// need more than [`MAX_BIGINT_BITS`].
    pub fn as_uint_n(&self, bits: u64) -> Option<BigInt> {
        if bits == 0 {
            return Some(BigInt::zero());
        }
        if !self.negative && self.bit_length() <= bits {
            return Some(self.clone());
        }
        if bits > MAX_BIGINT_BITS {
            return None;
        }
        let words = bits.div_ceil(32) as usize;
        let mut tw = to_twos(self, words);
        let rest = (bits % 32) as u32;
        if rest != 0 {
            tw[words - 1] &= (1u32 << rest) - 1;
        }
        Some(BigInt::from_parts(false, tw))
    }

    /// BigInt.asIntN: `self` modulo 2^bits as a signed integer.
    pub fn as_int_n(&self, bits: u64) -> Option<BigInt> {
        if bits == 0 {
            return Some(BigInt::zero());
        }
        if self.bit_length() < bits {
            return Some(self.clone());
        }
        let u = self.as_uint_n(bits)?;
        if u.bit_length() == bits {
            Some(u.sub(&BigInt::from_i64(1).shl(bits)))
        } else {
            Some(u)
        }
    }

    /// The value modulo 2^64 as a signed integer (ToBigInt64).
    pub fn to_i64_wrapping(&self) -> i64 {
        self.to_u64_wrapping() as i64
    }

    /// The value modulo 2^64 (ToBigUint64).
    pub fn to_u64_wrapping(&self) -> u64 {
        let tw = to_twos(self, 2.max(self.mag.len()));
        u64::from(tw[0]) | (u64::from(tw[1]) << 32)
    }

    pub fn cmp_bigint(&self, other: &BigInt) -> Ordering {
        match (self.negative, other.negative) {
            (false, true) => Ordering::Greater,
            (true, false) => Ordering::Less,
            (false, false) => mag_cmp(&self.mag, &other.mag),
            (true, true) => mag_cmp(&other.mag, &self.mag),
        }
    }

    /// Order against a Number; `None` if `x` is NaN.
    pub fn cmp_f64(&self, x: f64) -> Option<Ordering> {
        if x.is_nan() {
            return None;
        }
        if x == f64::INFINITY {
            return Some(Ordering::Less);
        }
        if x == f64::NEG_INFINITY {
            return Some(Ordering::Greater);
        }
        let floor = x.floor();
        let fb = BigInt::from_f64(floor).expect("floor of a finite number is integral");
        Some(match self.cmp_bigint(&fb) {
            Ordering::Greater => Ordering::Greater,
            Ordering::Less => Ordering::Less,
            Ordering::Equal => {
                if floor == x {
                    Ordering::Equal
                } else {
                    Ordering::Less
                }
            }
        })
    }
}

/// Largest BigInt the engine builds, in bits (results beyond throw a
/// RangeError).
pub const MAX_BIGINT_BITS: u64 = 1 << 30;

#[cfg(test)]
mod tests {
    use super::*;

    fn big(s: &str) -> BigInt {
        let (neg, digits) = match s.strip_prefix('-') {
            Some(d) => (true, d),
            None => (false, s),
        };
        let b = BigInt::parse_radix(digits.as_bytes(), 10).unwrap();
        if neg { b.neg() } else { b }
    }

    #[test]
    fn arithmetic_round_trips_through_strings() {
        let a = big("123456789012345678901234567890");
        let b = big("-987654321098765432109876543210");
        assert_eq!(
            a.mul(&b).to_string_radix(10),
            "-121932631137021795226185032733622923332237463801111263526900"
        );
        assert_eq!(
            a.add(&b).to_string_radix(10),
            "-864197532086419753208641975320"
        );
        let (q, r) = b.div_rem(&a).unwrap();
        assert_eq!(q.to_string_radix(10), "-8");
        assert_eq!(r.to_string_radix(10), "-9000000000900000000090");
        assert_eq!(big("255").to_string_radix(16), "ff");
    }

    #[test]
    fn shifts_and_bitwise_use_twos_complement() {
        assert_eq!(big("-5").shr(1).to_string_radix(10), "-3");
        assert_eq!(big("-5").bitand(&big("3")).to_string_radix(10), "3");
        assert_eq!(big("-5").bitor(&big("2")).to_string_radix(10), "-5");
        assert_eq!(big("5").bitnot().to_string_radix(10), "-6");
        assert_eq!(big("-1").as_uint_n(8).unwrap().to_string_radix(10), "255");
        assert_eq!(big("255").as_int_n(8).unwrap().to_string_radix(10), "-1");
        assert_eq!(big("-1").to_u64_wrapping(), u64::MAX);
    }

    #[test]
    fn number_conversions_round_to_nearest_even() {
        assert_eq!(
            BigInt::from_f64(1e20).unwrap().to_string_radix(10),
            "100000000000000000000"
        );
        assert_eq!(big("9007199254740993").to_f64(), 9007199254740992.0);
        assert_eq!(big("9007199254740995").to_f64(), 9007199254740996.0);
        assert_eq!(
            big("-36893488147419103231").to_f64(),
            -36893488147419103232.0
        );
        assert_eq!(big("3").cmp_f64(2.5), Some(Ordering::Greater));
        assert_eq!(big("2").cmp_f64(2.5), Some(Ordering::Less));
        assert_eq!(big("-3").cmp_f64(-2.5), Some(Ordering::Less));
        assert_eq!(BigInt::from_f64(0.5), None);
    }
}
