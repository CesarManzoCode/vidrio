//! X25519 (RFC 7748): Montgomery ladder over GF(2^255 - 19) with sixteen
//! 16-bit limbs held in `i64`.

type Fe = [i64; 16];

const A24: Fe = [0xdb41, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];

fn carry(o: &mut Fe) {
    for i in 0..16 {
        o[i] += 1 << 16;
        let c = o[i] >> 16;
        if i < 15 {
            o[i + 1] += c - 1;
        } else {
            o[0] += 38 * (c - 1);
        }
        o[i] -= c << 16;
    }
}

/// Swap `p` and `q` when `b` is 1, without branching on `b`.
fn cswap(p: &mut Fe, q: &mut Fe, b: i64) {
    let mask = !(b - 1);
    for i in 0..16 {
        let t = mask & (p[i] ^ q[i]);
        p[i] ^= t;
        q[i] ^= t;
    }
}

fn pack(n: &Fe) -> [u8; 32] {
    let mut t = *n;
    carry(&mut t);
    carry(&mut t);
    carry(&mut t);
    let mut m: Fe = [0; 16];
    for _ in 0..2 {
        m[0] = t[0] - 0xffed;
        for i in 1..15 {
            m[i] = t[i] - 0xffff - ((m[i - 1] >> 16) & 1);
            m[i - 1] &= 0xffff;
        }
        m[15] = t[15] - 0x7fff - ((m[14] >> 16) & 1);
        let b = (m[15] >> 16) & 1;
        m[14] &= 0xffff;
        cswap(&mut t, &mut m, 1 - b);
    }
    let mut out = [0u8; 32];
    for i in 0..16 {
        out[2 * i] = (t[i] & 0xff) as u8;
        out[2 * i + 1] = ((t[i] >> 8) & 0xff) as u8;
    }
    out
}

fn unpack(n: &[u8; 32]) -> Fe {
    let mut o: Fe = [0; 16];
    for i in 0..16 {
        o[i] = n[2 * i] as i64 + ((n[2 * i + 1] as i64) << 8);
    }
    o[15] &= 0x7fff;
    o
}

fn add(a: &Fe, b: &Fe) -> Fe {
    let mut o: Fe = [0; 16];
    for i in 0..16 {
        o[i] = a[i] + b[i];
    }
    o
}

fn sub(a: &Fe, b: &Fe) -> Fe {
    let mut o: Fe = [0; 16];
    for i in 0..16 {
        o[i] = a[i] - b[i];
    }
    o
}

fn mul(a: &Fe, b: &Fe) -> Fe {
    let mut t = [0i64; 31];
    for i in 0..16 {
        for j in 0..16 {
            t[i + j] += a[i] * b[j];
        }
    }
    for i in 0..15 {
        t[i] += 38 * t[i + 16];
    }
    let mut o: Fe = [0; 16];
    o.copy_from_slice(&t[..16]);
    carry(&mut o);
    carry(&mut o);
    o
}

fn square(a: &Fe) -> Fe {
    mul(a, a)
}

/// a^(p-2).
fn invert(a: &Fe) -> Fe {
    let mut c = *a;
    for i in (0..=253).rev() {
        c = square(&c);
        if i != 2 && i != 4 {
            c = mul(&c, a);
        }
    }
    c
}

/// X25519(scalar, u): the scalar is clamped as RFC 7748 §5 says.
pub fn x25519(scalar: &[u8; 32], u: &[u8; 32]) -> [u8; 32] {
    let mut z = *scalar;
    z[31] = (z[31] & 127) | 64;
    z[0] &= 248;
    let x = unpack(u);
    let mut a: Fe = [0; 16];
    let mut b = x;
    let mut c: Fe = [0; 16];
    let mut d: Fe = [0; 16];
    a[0] = 1;
    d[0] = 1;
    for i in (0..=254).rev() {
        let r = ((z[i >> 3] >> (i & 7)) & 1) as i64;
        cswap(&mut a, &mut b, r);
        cswap(&mut c, &mut d, r);
        let mut e = add(&a, &c);
        a = sub(&a, &c);
        c = add(&b, &d);
        b = sub(&b, &d);
        d = square(&e);
        let f = square(&a);
        a = mul(&c, &a);
        c = mul(&b, &e);
        e = add(&a, &c);
        a = sub(&a, &c);
        b = square(&a);
        c = sub(&d, &f);
        a = mul(&c, &A24);
        a = add(&a, &d);
        c = mul(&c, &a);
        a = mul(&d, &f);
        d = mul(&b, &x);
        b = square(&e);
        cswap(&mut a, &mut b, r);
        cswap(&mut c, &mut d, r);
    }
    let zi = invert(&c);
    pack(&mul(&a, &zi))
}

/// The standard base point u = 9.
pub const BASE_POINT: [u8; 32] = {
    let mut b = [0u8; 32];
    b[0] = 9;
    b
};

/// Public key for a 32-byte private key.
pub fn x25519_public(private: &[u8; 32]) -> [u8; 32] {
    x25519(private, &BASE_POINT)
}
