//! NIST P-256 (secp256r1): ECDH and ECDSA verification, with Jacobian
//! coordinates in Montgomery form.

use super::bigint::{BigUint, Elem, Modulus};
use super::random;
use std::sync::OnceLock;

const P_HEX: &str = "ffffffff00000001000000000000000000000000ffffffffffffffffffffffff";
const B_HEX: &str = "5ac635d8aa3a93e7b3ebbd55769886bc651d06b0cc53b0f63bce3c3e27d2604b";
const N_HEX: &str = "ffffffff00000000ffffffffffffffffbce6faada7179e84f3b9cac2fc632551";
const GX_HEX: &str = "6b17d1f2e12c4247f8bce6e563a440f277037d812deb33a0f4a13945d898c296";
const GY_HEX: &str = "4fe342e2fe1a7f9b8ee7eb4a7c0f9e162bce33576b315ececbb6406837bf51f5";

fn from_hex(s: &str) -> BigUint {
    let bytes: Vec<u8> = (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect();
    BigUint::from_be_bytes(&bytes)
}

struct Curve {
    p: Modulus,
    n: Modulus,
    b: Elem,
    three: Elem,
    g: Point,
}

fn curve() -> &'static Curve {
    static CURVE: OnceLock<Curve> = OnceLock::new();
    CURVE.get_or_init(|| {
        let p = Modulus::new(&from_hex(P_HEX)).unwrap();
        let n = Modulus::new(&from_hex(N_HEX)).unwrap();
        let b = p.to_mont(&from_hex(B_HEX)).unwrap();
        let three = p.to_mont(&BigUint::from_u64(3)).unwrap();
        let g = Point {
            x: p.to_mont(&from_hex(GX_HEX)).unwrap(),
            y: p.to_mont(&from_hex(GY_HEX)).unwrap(),
            z: p.one(),
        };
        Curve { p, n, b, three, g }
    })
}

/// Jacobian point; `z == 0` is the point at infinity.
#[derive(Clone, Debug)]
struct Point {
    x: Elem,
    y: Elem,
    z: Elem,
}

impl Point {
    fn infinity(c: &Curve) -> Point {
        Point {
            x: c.p.one(),
            y: c.p.one(),
            z: c.p.zero(),
        }
    }

    fn is_infinity(&self) -> bool {
        self.z.iter().all(|&l| l == 0)
    }
}

fn double(c: &Curve, pt: &Point) -> Point {
    let f = &c.p;
    if pt.is_infinity() || f.is_zero(&pt.y) {
        return Point::infinity(c);
    }
    let delta = f.square(&pt.z);
    let gamma = f.square(&pt.y);
    let beta = f.mul(&pt.x, &gamma);
    let t = f.mul(&f.sub(&pt.x, &delta), &f.add(&pt.x, &delta));
    let alpha = f.mul(&c.three, &t);
    let beta2 = f.add(&beta, &beta);
    let beta4 = f.add(&beta2, &beta2);
    let beta8 = f.add(&beta4, &beta4);
    let x3 = f.sub(&f.square(&alpha), &beta8);
    let yz = f.add(&pt.y, &pt.z);
    let z3 = f.sub(&f.sub(&f.square(&yz), &gamma), &delta);
    let gamma_sq = f.square(&gamma);
    let g2 = f.add(&gamma_sq, &gamma_sq);
    let g4 = f.add(&g2, &g2);
    let g8 = f.add(&g4, &g4);
    let y3 = f.sub(&f.mul(&alpha, &f.sub(&beta4, &x3)), &g8);
    Point {
        x: x3,
        y: y3,
        z: z3,
    }
}

fn add(c: &Curve, p1: &Point, p2: &Point) -> Point {
    let f = &c.p;
    if p1.is_infinity() {
        return p2.clone();
    }
    if p2.is_infinity() {
        return p1.clone();
    }
    let z1z1 = f.square(&p1.z);
    let z2z2 = f.square(&p2.z);
    let u1 = f.mul(&p1.x, &z2z2);
    let u2 = f.mul(&p2.x, &z1z1);
    let s1 = f.mul(&f.mul(&p1.y, &p2.z), &z2z2);
    let s2 = f.mul(&f.mul(&p2.y, &p1.z), &z1z1);
    let h = f.sub(&u2, &u1);
    let rr = f.sub(&s2, &s1);
    if f.is_zero(&h) {
        if f.is_zero(&rr) {
            return double(c, p1);
        }
        return Point::infinity(c);
    }
    let h2 = f.add(&h, &h);
    let i = f.square(&h2);
    let j = f.mul(&h, &i);
    let r = f.add(&rr, &rr);
    let v = f.mul(&u1, &i);
    let v2 = f.add(&v, &v);
    let x3 = f.sub(&f.sub(&f.square(&r), &j), &v2);
    let s1j = f.mul(&s1, &j);
    let y3 = f.sub(&f.mul(&r, &f.sub(&v, &x3)), &f.add(&s1j, &s1j));
    let zz = f.add(&p1.z, &p2.z);
    let z3 = f.mul(&f.sub(&f.sub(&f.square(&zz), &z1z1), &z2z2), &h);
    Point {
        x: x3,
        y: y3,
        z: z3,
    }
}

fn scalar_mul(c: &Curve, k: &BigUint, pt: &Point) -> Point {
    let mut acc = Point::infinity(c);
    for i in (0..k.bits()).rev() {
        acc = double(c, &acc);
        if k.bit(i) {
            acc = add(c, &acc, pt);
        }
    }
    acc
}

/// Affine coordinates as plain integers; `None` at infinity.
fn to_affine(c: &Curve, pt: &Point) -> Option<(BigUint, BigUint)> {
    if pt.is_infinity() {
        return None;
    }
    let f = &c.p;
    let zinv = f.inv_prime(&pt.z);
    let zinv2 = f.square(&zinv);
    let x = f.mul(&pt.x, &zinv2);
    let y = f.mul(&pt.y, &f.mul(&zinv2, &zinv));
    Some((f.from_mont(&x), f.from_mont(&y)))
}

/// Parse an uncompressed SEC 1 point (0x04 || X || Y) and check it lies on the curve.
fn decode_point(c: &Curve, bytes: &[u8]) -> Option<Point> {
    if bytes.len() != 65 || bytes[0] != 4 {
        return None;
    }
    let f = &c.p;
    let x = f.to_mont(&BigUint::from_be_bytes(&bytes[1..33]))?;
    let y = f.to_mont(&BigUint::from_be_bytes(&bytes[33..65]))?;
    // y^2 == x^3 - 3x + b
    let lhs = f.square(&y);
    let x3 = f.mul(&f.square(&x), &x);
    let rhs = f.add(&f.sub(&x3, &f.mul(&c.three, &x)), &c.b);
    if lhs != rhs {
        return None;
    }
    Some(Point { x, y, z: f.one() })
}

fn encode_point(x: &BigUint, y: &BigUint) -> Vec<u8> {
    let mut out = vec![4u8];
    out.extend(x.to_be_bytes(32).unwrap());
    out.extend(y.to_be_bytes(32).unwrap());
    out
}

/// A private scalar for ECDH.
#[derive(Clone)]
pub struct EcdhPrivate {
    scalar: BigUint,
}

impl EcdhPrivate {
    /// A fresh random scalar in [1, n-1].
    pub fn generate() -> std::io::Result<EcdhPrivate> {
        let c = curve();
        loop {
            let mut bytes = [0u8; 32];
            random::fill(&mut bytes)?;
            let k = BigUint::from_be_bytes(&bytes);
            if !k.is_zero() && k < *c.n.value() {
                return Ok(EcdhPrivate { scalar: k });
            }
        }
    }

    /// From a 32-byte big-endian scalar in [1, n-1].
    pub fn from_bytes(bytes: &[u8]) -> Option<EcdhPrivate> {
        let k = BigUint::from_be_bytes(bytes);
        if bytes.len() != 32 || k.is_zero() || k >= *curve().n.value() {
            return None;
        }
        Some(EcdhPrivate { scalar: k })
    }

    /// Uncompressed public point (65 bytes).
    pub fn public_key(&self) -> Vec<u8> {
        let c = curve();
        let (x, y) = to_affine(c, &scalar_mul(c, &self.scalar, &c.g)).expect("nonzero scalar");
        encode_point(&x, &y)
    }

    /// Shared secret: the X coordinate of k * peer (32 bytes).
    pub fn agree(&self, peer: &[u8]) -> Option<Vec<u8>> {
        let c = curve();
        let q = decode_point(c, peer)?;
        let (x, _) = to_affine(c, &scalar_mul(c, &self.scalar, &q))?;
        x.to_be_bytes(32)
    }
}

/// Verify an ECDSA signature given as raw `r` and `s` over `hash` (any
/// length; only its leftmost 256 bits count) with an uncompressed public key.
pub fn verify_raw(public_key: &[u8], hash: &[u8], r: &BigUint, s: &BigUint) -> bool {
    let c = curve();
    let Some(q) = decode_point(c, public_key) else {
        return false;
    };
    let n = &c.n;
    if r.is_zero() || s.is_zero() || r >= n.value() || s >= n.value() {
        return false;
    }
    // Leftmost 256 bits of the hash; below 2^256 < 2n, so one subtraction reduces it.
    let e = BigUint::from_be_bytes(&hash[..hash.len().min(32)]);
    let Some(e_m) = n.reduce_once(&e) else {
        return false;
    };
    let (Some(r_m), Some(s_m)) = (n.to_mont(r), n.to_mont(s)) else {
        return false;
    };
    let w = n.inv_prime(&s_m);
    let u1 = n.from_mont(&n.mul(&e_m, &w));
    let u2 = n.from_mont(&n.mul(&r_m, &w));
    let pt = add(c, &scalar_mul(c, &u1, &c.g), &scalar_mul(c, &u2, &q));
    let Some((x, _)) = to_affine(c, &pt) else {
        return false;
    };
    match n.reduce_once(&x) {
        Some(v) => n.from_mont(&v) == *r,
        None => false,
    }
}

/// Verify a DER-encoded `ECDSA-Sig-Value` (SEQUENCE of two INTEGERs).
pub fn verify_der(public_key: &[u8], hash: &[u8], sig: &[u8]) -> bool {
    match parse_der_signature(sig) {
        Some((r, s)) => verify_raw(public_key, hash, &r, &s),
        None => false,
    }
}

fn parse_der_signature(sig: &[u8]) -> Option<(BigUint, BigUint)> {
    use crate::x509::der::{Reader, tag};
    let mut outer = Reader::new(sig);
    let seq = outer.expect(tag::SEQUENCE).ok()?;
    if !outer.is_empty() {
        return None;
    }
    let mut inner = Reader::new(seq);
    let r = inner.unsigned_integer().ok()?;
    let s = inner.unsigned_integer().ok()?;
    if !inner.is_empty() {
        return None;
    }
    Some((BigUint::from_be_bytes(r), BigUint::from_be_bytes(s)))
}
