//! Unsigned big integers with 64-bit limbs and Montgomery arithmetic modulo
//! an odd number (RSA moduli, the P-256 field and group order).

use std::cmp::Ordering;

/// Little-endian limbs; no high zero limbs except for zero itself (empty).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BigUint {
    limbs: Vec<u64>,
}

impl BigUint {
    pub fn zero() -> BigUint {
        BigUint { limbs: Vec::new() }
    }

    pub fn from_u64(v: u64) -> BigUint {
        let mut b = BigUint { limbs: vec![v] };
        b.normalize();
        b
    }

    pub fn from_limbs(limbs: Vec<u64>) -> BigUint {
        let mut b = BigUint { limbs };
        b.normalize();
        b
    }

    pub fn from_be_bytes(bytes: &[u8]) -> BigUint {
        let mut limbs = Vec::with_capacity(bytes.len().div_ceil(8));
        for chunk in bytes.rchunks(8) {
            let mut v = 0u64;
            for &b in chunk {
                v = (v << 8) | b as u64;
            }
            limbs.push(v);
        }
        BigUint::from_limbs(limbs)
    }

    /// Big-endian bytes left-padded to `len`; `None` if the value needs more.
    pub fn to_be_bytes(&self, len: usize) -> Option<Vec<u8>> {
        if self.byte_len() > len {
            return None;
        }
        let mut out = vec![0u8; len];
        for (i, limb) in self.limbs.iter().enumerate() {
            for j in 0..8 {
                let pos = i * 8 + j;
                if pos < len {
                    out[len - 1 - pos] = (limb >> (8 * j)) as u8;
                }
            }
        }
        Some(out)
    }

    fn normalize(&mut self) {
        while self.limbs.last() == Some(&0) {
            self.limbs.pop();
        }
    }

    pub fn limbs(&self) -> &[u64] {
        &self.limbs
    }

    pub fn is_zero(&self) -> bool {
        self.limbs.is_empty()
    }

    pub fn is_odd(&self) -> bool {
        self.limbs.first().is_some_and(|&l| l & 1 == 1)
    }

    pub fn bits(&self) -> usize {
        match self.limbs.last() {
            None => 0,
            Some(top) => self.limbs.len() * 64 - top.leading_zeros() as usize,
        }
    }

    pub fn byte_len(&self) -> usize {
        self.bits().div_ceil(8)
    }

    pub fn bit(&self, i: usize) -> bool {
        self.limbs
            .get(i / 64)
            .is_some_and(|&l| (l >> (i % 64)) & 1 == 1)
    }

    /// Limbs padded with zeros to `k`.
    fn padded(&self, k: usize) -> Vec<u64> {
        let mut v = self.limbs.clone();
        v.resize(k, 0);
        v
    }

    pub fn sub_u64(&self, v: u64) -> BigUint {
        let mut limbs = self.limbs.clone();
        let mut borrow = v;
        for l in limbs.iter_mut() {
            let (d, b) = l.overflowing_sub(borrow);
            *l = d;
            borrow = b as u64;
            if borrow == 0 {
                break;
            }
        }
        assert!(borrow == 0, "BigUint underflow");
        BigUint::from_limbs(limbs)
    }
}

impl PartialOrd for BigUint {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for BigUint {
    fn cmp(&self, other: &Self) -> Ordering {
        self.limbs
            .len()
            .cmp(&other.limbs.len())
            .then_with(|| cmp_limbs(&self.limbs, &other.limbs))
    }
}

/// Compare equal-length limb slices.
fn cmp_limbs(a: &[u64], b: &[u64]) -> Ordering {
    for (x, y) in a.iter().rev().zip(b.iter().rev()) {
        match x.cmp(y) {
            Ordering::Equal => {}
            o => return o,
        }
    }
    Ordering::Equal
}

/// a -= b, returning the borrow.
fn sub_in_place(a: &mut [u64], b: &[u64]) -> bool {
    let mut borrow = false;
    for (x, &y) in a.iter_mut().zip(b) {
        let (d1, b1) = x.overflowing_sub(y);
        let (d2, b2) = d1.overflowing_sub(borrow as u64);
        *x = d2;
        borrow = b1 || b2;
    }
    borrow
}

/// a += b, returning the carry.
fn add_in_place(a: &mut [u64], b: &[u64]) -> bool {
    let mut carry = false;
    for (x, &y) in a.iter_mut().zip(b) {
        let (s1, c1) = x.overflowing_add(y);
        let (s2, c2) = s1.overflowing_add(carry as u64);
        *x = s2;
        carry = c1 || c2;
    }
    carry
}

#[inline]
fn mac(acc: u64, a: u64, b: u64, carry: u64) -> (u64, u64) {
    let t = acc as u128 + (a as u128) * (b as u128) + carry as u128;
    (t as u64, (t >> 64) as u64)
}

/// An element in Montgomery form: exactly `k` limbs, below the modulus.
pub type Elem = Vec<u64>;

/// An odd modulus with its Montgomery constants (R = 2^(64k)).
#[derive(Clone, Debug)]
pub struct Modulus {
    m: Vec<u64>,
    m0inv: u64,
    r2: Vec<u64>,
    one: Vec<u64>,
    value: BigUint,
}

impl Modulus {
    /// `None` unless `m` is odd and greater than one.
    pub fn new(m: &BigUint) -> Option<Modulus> {
        if !m.is_odd() || m.bits() < 2 {
            return None;
        }
        let k = m.limbs.len();
        let mv = m.limbs.clone();
        // Newton iteration for m0^-1 mod 2^64 (m0 is its own inverse mod 8).
        let m0 = mv[0];
        let mut inv = m0;
        for _ in 0..5 {
            inv = inv.wrapping_mul(2u64.wrapping_sub(m0.wrapping_mul(inv)));
        }
        // R mod m and R^2 mod m by repeated doubling of 1.
        let mut x = vec![0u64; k];
        x[0] = 1;
        let mut one = Vec::new();
        for i in 0..(128 * k) {
            let top = x[k - 1] >> 63;
            for j in (1..k).rev() {
                x[j] = (x[j] << 1) | (x[j - 1] >> 63);
            }
            x[0] <<= 1;
            if top == 1 || cmp_limbs(&x, &mv) != Ordering::Less {
                sub_in_place(&mut x, &mv);
            }
            if i + 1 == 64 * k {
                one = x.clone();
            }
        }
        Some(Modulus {
            m: mv,
            m0inv: inv.wrapping_neg(),
            r2: x,
            one,
            value: m.clone(),
        })
    }

    pub fn value(&self) -> &BigUint {
        &self.value
    }

    pub fn limbs(&self) -> usize {
        self.m.len()
    }

    /// Montgomery product a * b / R mod m.
    pub fn mul(&self, a: &[u64], b: &[u64]) -> Elem {
        let k = self.m.len();
        let mut t = vec![0u64; k + 2];
        for &bi in b.iter().take(k) {
            let mut c = 0u64;
            for j in 0..k {
                let (lo, hi) = mac(t[j], a[j], bi, c);
                t[j] = lo;
                c = hi;
            }
            let (s, o) = t[k].overflowing_add(c);
            t[k] = s;
            t[k + 1] = o as u64;
            let u = t[0].wrapping_mul(self.m0inv);
            let (_, mut c) = mac(t[0], u, self.m[0], 0);
            for j in 1..k {
                let (lo, hi) = mac(t[j], u, self.m[j], c);
                t[j - 1] = lo;
                c = hi;
            }
            let (s, o) = t[k].overflowing_add(c);
            t[k - 1] = s;
            t[k] = t[k + 1] + o as u64;
        }
        let high = t[k];
        t.truncate(k);
        if high != 0 || cmp_limbs(&t, &self.m) != Ordering::Less {
            sub_in_place(&mut t, &self.m);
        }
        t
    }

    pub fn square(&self, a: &[u64]) -> Elem {
        self.mul(a, a)
    }

    pub fn add(&self, a: &[u64], b: &[u64]) -> Elem {
        let mut t = a.to_vec();
        let carry = add_in_place(&mut t, b);
        if carry || cmp_limbs(&t, &self.m) != Ordering::Less {
            sub_in_place(&mut t, &self.m);
        }
        t
    }

    pub fn sub(&self, a: &[u64], b: &[u64]) -> Elem {
        let mut t = a.to_vec();
        if sub_in_place(&mut t, b) {
            add_in_place(&mut t, &self.m);
        }
        t
    }

    pub fn neg(&self, a: &[u64]) -> Elem {
        self.sub(&self.zero(), a)
    }

    pub fn zero(&self) -> Elem {
        vec![0; self.m.len()]
    }

    /// One in Montgomery form.
    pub fn one(&self) -> Elem {
        self.one.clone()
    }

    pub fn is_zero(&self, a: &[u64]) -> bool {
        a.iter().all(|&l| l == 0)
    }

    /// `x` into Montgomery form; `None` if `x` is not below the modulus.
    pub fn to_mont(&self, x: &BigUint) -> Option<Elem> {
        if *x >= self.value {
            return None;
        }
        Some(self.mul(&x.padded(self.m.len()), &self.r2))
    }

    /// `x mod m` for `x < 2m` into Montgomery form.
    pub fn reduce_once(&self, x: &BigUint) -> Option<Elem> {
        if x.limbs.len() > self.m.len() {
            return None;
        }
        let mut t = x.padded(self.m.len());
        if cmp_limbs(&t, &self.m) != Ordering::Less {
            sub_in_place(&mut t, &self.m);
            if cmp_limbs(&t, &self.m) != Ordering::Less {
                return None;
            }
        }
        Some(self.mul(&t, &self.r2))
    }

    pub fn from_mont(&self, a: &[u64]) -> BigUint {
        let mut unit = vec![0u64; self.m.len()];
        unit[0] = 1;
        BigUint::from_limbs(self.mul(a, &unit))
    }

    /// base^exp for a Montgomery-form base.
    pub fn pow(&self, base: &[u64], exp: &BigUint) -> Elem {
        let mut acc = self.one();
        for i in (0..exp.bits()).rev() {
            acc = self.square(&acc);
            if exp.bit(i) {
                acc = self.mul(&acc, base);
            }
        }
        acc
    }

    /// Inverse by Fermat's little theorem; only valid for a prime modulus.
    pub fn inv_prime(&self, a: &[u64]) -> Elem {
        self.pow(a, &self.value.sub_u64(2))
    }
}
