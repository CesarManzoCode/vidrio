//! A JPEG decoder (ITU-T T.81): baseline and extended sequential
//! (SOF0, SOF1) and progressive (SOF2) Huffman-coded images with 8-bit
//! samples, one to four components, any sampling factors and restart
//! intervals. The inverse DCT is the integer one of libjpeg
//! (`jpeg_idct_islow`), chroma is upsampled with libjpeg's triangle
//! filter for 2×1, 1×2 and 2×2 factors (replication otherwise), and
//! YCbCr is converted with libjpeg's fixed-point tables, so the output
//! matches `djpeg` with its defaults. Four components are CMYK (or
//! YCCK) as Adobe stores them, inverted. Arithmetic coding, lossless
//! and 12-bit images are not decoded; EXIF orientation is not applied.

use crate::image::{Image, ImageError, MAX_SIDE, err};

/// The zigzag order: natural index of the `k`-th coefficient.
const ZIGZAG: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20,
    13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59,
    52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

/// A canonical Huffman table.
#[derive(Clone, Debug, Default)]
struct Huffman {
    /// Largest code of each length (−1: none), lengths 1 to 16.
    maxcode: [i32; 17],
    mincode: [i32; 17],
    /// Index into `values` of the first code of each length.
    valptr: [i32; 17],
    values: Vec<u8>,
}

impl Huffman {
    fn new(counts: &[u8; 16], values: Vec<u8>) -> Huffman {
        let mut h = Huffman {
            maxcode: [-1; 17],
            mincode: [0; 17],
            valptr: [0; 17],
            values,
        };
        let mut code = 0i32;
        let mut k = 0i32;
        for len in 1..=16 {
            let n = i32::from(counts[len - 1]);
            if n > 0 {
                h.valptr[len] = k;
                h.mincode[len] = code;
                code += n;
                k += n;
                h.maxcode[len] = code - 1;
            }
            code <<= 1;
        }
        h
    }
}

/// The bits of entropy-coded data, with byte stuffing removed; a marker
/// ends the data (the decoder then reads zeros).
struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
    acc: u32,
    n: u32,
}

impl Bits<'_> {
    fn byte(&mut self) -> u32 {
        let Some(&b) = self.data.get(self.pos) else {
            return 0;
        };
        if b == 0xFF {
            match self.data.get(self.pos + 1) {
                Some(0) => {
                    self.pos += 2;
                    0xFF
                }
                // A marker: stay on it.
                _ => 0,
            }
        } else {
            self.pos += 1;
            u32::from(b)
        }
    }

    fn bit(&mut self) -> u32 {
        if self.n == 0 {
            self.acc = self.byte();
            self.n = 8;
        }
        self.n -= 1;
        (self.acc >> self.n) & 1
    }

    fn bits(&mut self, count: u32) -> u32 {
        let mut v = 0;
        for _ in 0..count {
            v = (v << 1) | self.bit();
        }
        v
    }

    /// `count` bits as a signed magnitude (T.81 F.2.2.1, EXTEND).
    fn receive_extend(&mut self, count: u32) -> i32 {
        if count == 0 {
            return 0;
        }
        let v = self.bits(count) as i32;
        if v < 1 << (count - 1) {
            v - (1 << count) + 1
        } else {
            v
        }
    }

    fn decode(&mut self, h: &Huffman) -> Result<u8, ImageError> {
        let mut code = 0i32;
        for len in 1..=16 {
            code = (code << 1) | self.bit() as i32;
            if code <= h.maxcode[len] {
                let i = (h.valptr[len] + code - h.mincode[len]) as usize;
                return h
                    .values
                    .get(i)
                    .copied()
                    .ok_or_else(|| ImageError("bad Huffman code".to_string()));
            }
        }
        err("bad Huffman code")
    }

    /// Skips to the byte after a restart marker.
    fn restart(&mut self) {
        self.n = 0;
        while self.pos + 1 < self.data.len() {
            if self.data[self.pos] == 0xFF && (0xD0..=0xD7).contains(&self.data[self.pos + 1]) {
                self.pos += 2;
                return;
            }
            self.pos += 1;
        }
    }
}

#[derive(Clone, Debug, Default)]
struct Component {
    id: u8,
    h: usize,
    v: usize,
    tq: usize,
    /// Width and height in samples (before upsampling).
    w: usize,
    hgt: usize,
    /// Blocks per line and block rows (padded to whole MCUs).
    bw: usize,
    bh: usize,
    /// Coefficients in zigzag order, 64 per block.
    coefs: Vec<i32>,
    dc_pred: i32,
}

struct Frame {
    width: usize,
    height: usize,
    progressive: bool,
    comps: Vec<Component>,
    hmax: usize,
    vmax: usize,
    mcux: usize,
    mcuy: usize,
}

struct Scan {
    /// (component index, DC table, AC table).
    comps: Vec<(usize, usize, usize)>,
    ss: usize,
    se: usize,
    ah: u32,
    al: u32,
}

/// Decodes a JPEG file.
pub fn decode(bytes: &[u8]) -> Result<Image, ImageError> {
    if !bytes.starts_with(&[0xFF, 0xD8]) {
        return err("not a JPEG file");
    }
    let mut pos = 2;
    let mut qt = [[1u16; 64]; 4];
    let mut dc: [Option<Huffman>; 4] = Default::default();
    let mut ac: [Option<Huffman>; 4] = Default::default();
    let mut frame: Option<Frame> = None;
    let mut restart_interval = 0usize;
    let mut adobe: Option<u8> = None;
    let mut eobrun = 0u32;
    loop {
        // Find the next marker (fill bytes 0xFF are skipped).
        while pos < bytes.len() && bytes[pos] != 0xFF {
            pos += 1;
        }
        while pos < bytes.len() && bytes[pos] == 0xFF {
            pos += 1;
        }
        let Some(&marker) = bytes.get(pos) else {
            break;
        };
        pos += 1;
        match marker {
            0xD8 | 0x01 | 0xD0..=0xD7 => continue,
            0xD9 => break,
            _ => {}
        }
        if pos + 2 > bytes.len() {
            break;
        }
        let len = usize::from(u16::from_be_bytes([bytes[pos], bytes[pos + 1]]));
        if len < 2 || pos + len > bytes.len() {
            return err("truncated JPEG segment");
        }
        let seg = &bytes[pos + 2..pos + len];
        pos += len;
        match marker {
            0xDB => {
                let mut i = 0;
                while i < seg.len() {
                    let pq = seg[i] >> 4;
                    let tq = (seg[i] & 3) as usize;
                    i += 1;
                    for k in 0..64 {
                        let v = if pq == 0 {
                            u16::from(*seg.get(i).unwrap_or(&1))
                        } else {
                            u16::from_be_bytes([
                                *seg.get(i).unwrap_or(&0),
                                *seg.get(i + 1).unwrap_or(&1),
                            ])
                        };
                        i += if pq == 0 { 1 } else { 2 };
                        qt[tq][k] = v;
                    }
                }
            }
            0xC4 => {
                let mut i = 0;
                while i + 17 <= seg.len() {
                    let class = seg[i] >> 4;
                    let id = (seg[i] & 3) as usize;
                    let mut counts = [0u8; 16];
                    counts.copy_from_slice(&seg[i + 1..i + 17]);
                    let total: usize = counts.iter().map(|&c| usize::from(c)).sum();
                    i += 17;
                    if i + total > seg.len() {
                        return err("truncated DHT");
                    }
                    let table = Huffman::new(&counts, seg[i..i + total].to_vec());
                    i += total;
                    if class == 0 {
                        dc[id] = Some(table);
                    } else {
                        ac[id] = Some(table);
                    }
                }
            }
            0xDD => {
                if seg.len() >= 2 {
                    restart_interval = usize::from(u16::from_be_bytes([seg[0], seg[1]]));
                }
            }
            0xEE => {
                if seg.len() >= 12 && seg.starts_with(b"Adobe") {
                    adobe = Some(seg[11]);
                }
            }
            0xC0 | 0xC1 | 0xC2 => frame = Some(parse_frame(seg, marker == 0xC2)?),
            0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF => {
                return err("unsupported JPEG coding");
            }
            0xDA => {
                let Some(f) = frame.as_mut() else {
                    return err("scan before frame");
                };
                let scan = parse_scan(seg, f)?;
                // The entropy-coded data runs to the next marker that is
                // not a restart marker.
                let mut end = pos;
                while end + 1 < bytes.len() {
                    if bytes[end] == 0xFF {
                        let m = bytes[end + 1];
                        if m != 0 && !(0xD0..=0xD7).contains(&m) {
                            break;
                        }
                    }
                    end += 1;
                }
                if end + 1 >= bytes.len() {
                    end = bytes.len();
                }
                let mut bits = Bits {
                    data: &bytes[pos..end],
                    pos: 0,
                    acc: 0,
                    n: 0,
                };
                decode_scan(f, &scan, &dc, &ac, restart_interval, &mut bits, &mut eobrun)?;
                pos = end;
            }
            _ => {}
        }
    }
    let Some(f) = frame else {
        return err("no JPEG frame");
    };
    Ok(output(&f, &qt, adobe))
}

fn parse_frame(seg: &[u8], progressive: bool) -> Result<Frame, ImageError> {
    if seg.len() < 6 {
        return err("short SOF");
    }
    if seg[0] != 8 {
        return err("unsupported sample precision");
    }
    let height = usize::from(u16::from_be_bytes([seg[1], seg[2]]));
    let width = usize::from(u16::from_be_bytes([seg[3], seg[4]]));
    let n = usize::from(seg[5]);
    if width == 0
        || height == 0
        || width > MAX_SIDE as usize
        || height > MAX_SIDE as usize
        || !(1..=4).contains(&n)
        || seg.len() < 6 + 3 * n
    {
        return err("bad SOF");
    }
    let mut comps = Vec::with_capacity(n);
    for i in 0..n {
        let c = &seg[6 + 3 * i..9 + 3 * i];
        let (h, v) = (usize::from(c[1] >> 4), usize::from(c[1] & 15));
        if !(1..=4).contains(&h) || !(1..=4).contains(&v) {
            return err("bad sampling factor");
        }
        comps.push(Component {
            id: c[0],
            h,
            v,
            tq: usize::from(c[2] & 3),
            ..Component::default()
        });
    }
    let hmax = comps.iter().map(|c| c.h).max().unwrap_or(1);
    let vmax = comps.iter().map(|c| c.v).max().unwrap_or(1);
    let mcux = width.div_ceil(8 * hmax);
    let mcuy = height.div_ceil(8 * vmax);
    for c in &mut comps {
        c.w = (width * c.h).div_ceil(hmax);
        c.hgt = (height * c.v).div_ceil(vmax);
        c.bw = mcux * c.h;
        c.bh = mcuy * c.v;
        c.coefs = vec![0; c.bw * c.bh * 64];
    }
    Ok(Frame {
        width,
        height,
        progressive,
        comps,
        hmax,
        vmax,
        mcux,
        mcuy,
    })
}

fn parse_scan(seg: &[u8], f: &Frame) -> Result<Scan, ImageError> {
    let n = usize::from(*seg.first().unwrap_or(&0));
    if n == 0 || n > 4 || seg.len() < 1 + 2 * n + 3 {
        return err("bad SOS");
    }
    let mut comps = Vec::with_capacity(n);
    for i in 0..n {
        let id = seg[1 + 2 * i];
        let t = seg[2 + 2 * i];
        let Some(ci) = f.comps.iter().position(|c| c.id == id) else {
            return err("unknown scan component");
        };
        comps.push((ci, usize::from(t >> 4 & 3), usize::from(t & 3)));
    }
    let p = 1 + 2 * n;
    let (ss, se) = (usize::from(seg[p]), usize::from(seg[p + 1]));
    let (ah, al) = (u32::from(seg[p + 2] >> 4), u32::from(seg[p + 2] & 15));
    if !f.progressive {
        return Ok(Scan {
            comps,
            ss: 0,
            se: 63,
            ah: 0,
            al: 0,
        });
    }
    if se > 63 || ss > se || (ss == 0 && se != 0) || (ss > 0 && n != 1) || al > 13 {
        return err("bad progressive scan");
    }
    Ok(Scan {
        comps,
        ss,
        se,
        ah,
        al,
    })
}

fn decode_scan(
    f: &mut Frame,
    scan: &Scan,
    dc: &[Option<Huffman>; 4],
    ac: &[Option<Huffman>; 4],
    restart_interval: usize,
    bits: &mut Bits,
    eobrun: &mut u32,
) -> Result<(), ImageError> {
    for &(ci, _, _) in &scan.comps {
        f.comps[ci].dc_pred = 0;
    }
    *eobrun = 0;
    // The blocks of each MCU: (component, block x, block y, tables).
    let single = scan.comps.len() == 1;
    let (units_x, units_y) = if single {
        let c = &f.comps[scan.comps[0].0];
        (c.w.div_ceil(8), c.hgt.div_ceil(8))
    } else {
        (f.mcux, f.mcuy)
    };
    let needs_dc = scan.ss == 0;
    let mut count = 0usize;
    for uy in 0..units_y {
        for ux in 0..units_x {
            if restart_interval > 0 && count > 0 && count % restart_interval == 0 {
                bits.restart();
                for &(ci, _, _) in &scan.comps {
                    f.comps[ci].dc_pred = 0;
                }
                *eobrun = 0;
            }
            count += 1;
            for &(ci, td, ta) in &scan.comps {
                let c = &mut f.comps[ci];
                let (bh, bv) = if single { (1, 1) } else { (c.h, c.v) };
                for v in 0..bv {
                    for h in 0..bh {
                        let (bx, by) = if single {
                            (ux, uy)
                        } else {
                            (ux * c.h + h, uy * c.v + v)
                        };
                        let start = (by * c.bw + bx) * 64;
                        let mut pred = c.dc_pred;
                        let block = &mut c.coefs[start..start + 64];
                        let dct = if needs_dc { dc[td].as_ref() } else { None };
                        let act = if scan.se > 0 { ac[ta].as_ref() } else { None };
                        if needs_dc && dct.is_none() && !(f.progressive && scan.ah > 0) {
                            return err("missing DC table");
                        }
                        if scan.se > 0 && act.is_none() {
                            return err("missing AC table");
                        }
                        if !f.progressive {
                            sequential(bits, block, &mut pred, dct.expect("dc"), act.expect("ac"))?;
                        } else if scan.ss == 0 {
                            if scan.ah == 0 {
                                let t = bits.decode(dct.expect("dc"))?;
                                pred += bits.receive_extend(u32::from(t));
                                block[0] = pred * (1 << scan.al);
                            } else if bits.bit() == 1 {
                                block[0] |= 1 << scan.al;
                            }
                        } else if scan.ah == 0 {
                            ac_first(bits, block, scan, act.expect("ac"), eobrun)?;
                        } else {
                            ac_refine(bits, block, scan, act.expect("ac"), eobrun)?;
                        }
                        c.dc_pred = pred;
                    }
                }
            }
        }
    }
    Ok(())
}

fn sequential(
    bits: &mut Bits,
    block: &mut [i32],
    pred: &mut i32,
    dc: &Huffman,
    ac: &Huffman,
) -> Result<(), ImageError> {
    let t = bits.decode(dc)?;
    *pred += bits.receive_extend(u32::from(t));
    block[0] = *pred;
    let mut k = 1;
    while k < 64 {
        let rs = bits.decode(ac)?;
        let (r, s) = (usize::from(rs >> 4), u32::from(rs & 15));
        if s == 0 {
            if r == 15 {
                k += 16;
                continue;
            }
            break;
        }
        k += r;
        if k > 63 {
            break;
        }
        block[k] = bits.receive_extend(s);
        k += 1;
    }
    Ok(())
}

fn ac_first(
    bits: &mut Bits,
    block: &mut [i32],
    scan: &Scan,
    ac: &Huffman,
    eobrun: &mut u32,
) -> Result<(), ImageError> {
    if *eobrun > 0 {
        *eobrun -= 1;
        return Ok(());
    }
    let mut k = scan.ss;
    while k <= scan.se {
        let rs = bits.decode(ac)?;
        let (r, s) = (u32::from(rs >> 4), u32::from(rs & 15));
        if s == 0 {
            if r < 15 {
                *eobrun = (1 << r) - 1;
                if r > 0 {
                    *eobrun += bits.bits(r);
                }
                break;
            }
            k += 16;
            continue;
        }
        k += r as usize;
        if k > 63 {
            break;
        }
        block[k] = bits.receive_extend(s) * (1 << scan.al);
        k += 1;
    }
    Ok(())
}

fn ac_refine(
    bits: &mut Bits,
    block: &mut [i32],
    scan: &Scan,
    ac: &Huffman,
    eobrun: &mut u32,
) -> Result<(), ImageError> {
    let p1 = 1i32 << scan.al;
    let m1 = -1i32 << scan.al;
    let mut k = scan.ss;
    let refine = |bits: &mut Bits, c: &mut i32| {
        if bits.bit() == 1 && *c & p1 == 0 {
            *c += if *c >= 0 { p1 } else { m1 };
        }
    };
    if *eobrun == 0 {
        while k <= scan.se {
            let rs = bits.decode(ac)?;
            let mut r = i32::from(rs >> 4);
            let s = rs & 15;
            let mut val = 0;
            if s == 0 {
                if r < 15 {
                    *eobrun = 1 << r;
                    if r > 0 {
                        *eobrun += bits.bits(r as u32);
                    }
                    break;
                }
            } else {
                val = if bits.bit() == 1 { p1 } else { m1 };
            }
            while k <= scan.se {
                if block[k] != 0 {
                    refine(bits, &mut block[k]);
                } else {
                    r -= 1;
                    if r < 0 {
                        break;
                    }
                }
                k += 1;
            }
            if val != 0 && k <= scan.se {
                block[k] = val;
            }
            k += 1;
        }
    }
    if *eobrun > 0 {
        while k <= scan.se {
            if block[k] != 0 {
                refine(bits, &mut block[k]);
            }
            k += 1;
        }
        *eobrun -= 1;
    }
    Ok(())
}

// ----- reconstruction --------------------------------------------------------------

const CONST_BITS: i32 = 13;
const PASS1_BITS: i32 = 2;
const FIX_0_298631336: i64 = 2446;
const FIX_0_390180644: i64 = 3196;
const FIX_0_541196100: i64 = 4433;
const FIX_0_765366865: i64 = 6270;
const FIX_0_899976223: i64 = 7373;
const FIX_1_175875602: i64 = 9633;
const FIX_1_501321110: i64 = 12299;
const FIX_1_847759065: i64 = 15137;
const FIX_1_961570560: i64 = 16069;
const FIX_2_053119869: i64 = 16819;
const FIX_2_562915447: i64 = 20995;
const FIX_3_072711026: i64 = 25172;

fn descale(x: i64, n: i32) -> i64 {
    (x + (1 << (n - 1))) >> n
}

/// One 1-D pass of libjpeg's slow integer IDCT over eight values; the
/// outputs keep `CONST_BITS` of fraction.
fn idct_1d(i: [i64; 8]) -> [i64; 8] {
    let (z2, z3) = (i[2], i[6]);
    let z1 = (z2 + z3) * FIX_0_541196100;
    let tmp2 = z1 - z3 * FIX_1_847759065;
    let tmp3 = z1 + z2 * FIX_0_765366865;
    let tmp0 = (i[0] + i[4]) << CONST_BITS;
    let tmp1 = (i[0] - i[4]) << CONST_BITS;
    let (tmp10, tmp13, tmp11, tmp12) = (tmp0 + tmp3, tmp0 - tmp3, tmp1 + tmp2, tmp1 - tmp2);
    let (mut t0, mut t1, mut t2, mut t3) = (i[7], i[5], i[3], i[1]);
    let mut z1 = t0 + t3;
    let mut z2 = t1 + t2;
    let mut z3 = t0 + t2;
    let mut z4 = t1 + t3;
    let z5 = (z3 + z4) * FIX_1_175875602;
    t0 *= FIX_0_298631336;
    t1 *= FIX_2_053119869;
    t2 *= FIX_3_072711026;
    t3 *= FIX_1_501321110;
    z1 *= -FIX_0_899976223;
    z2 *= -FIX_2_562915447;
    z3 *= -FIX_1_961570560;
    z4 *= -FIX_0_390180644;
    z3 += z5;
    z4 += z5;
    t0 += z1 + z3;
    t1 += z2 + z4;
    t2 += z2 + z3;
    t3 += z1 + z4;
    [
        tmp10 + t3,
        tmp11 + t2,
        tmp12 + t1,
        tmp13 + t0,
        tmp13 - t0,
        tmp12 - t1,
        tmp11 - t2,
        tmp10 - t3,
    ]
}

/// Dequantizes and inverse-transforms one block (zigzag coefficients)
/// into 8×8 samples at `out[y * stride + x]`.
fn idct_block(coefs: &[i32], q: &[u16; 64], out: &mut [u8], stride: usize) {
    let mut natural = [0i64; 64];
    for k in 0..64 {
        natural[ZIGZAG[k]] = i64::from(coefs[k]) * i64::from(q[k]);
    }
    let mut ws = [0i64; 64];
    for col in 0..8 {
        let column = std::array::from_fn(|r| natural[r * 8 + col]);
        let o = idct_1d(column);
        for r in 0..8 {
            ws[r * 8 + col] = descale(o[r], CONST_BITS - PASS1_BITS);
        }
    }
    for row in 0..8 {
        let line = std::array::from_fn(|c| ws[row * 8 + c]);
        let o = idct_1d(line);
        for c in 0..8 {
            let v = descale(o[c], CONST_BITS + PASS1_BITS + 3) + 128;
            out[row * stride + c] = v.clamp(0, 255) as u8;
        }
    }
}

/// A component's samples at full resolution (`width` × `height`).
fn upsample(f: &Frame, c: &Component, plane: &[u8], stride: usize) -> Vec<u8> {
    let (sx, sy) = (f.hmax / c.h, f.vmax / c.v);
    let exact = f.hmax % c.h == 0 && f.vmax % c.v == 0;
    let (w, h) = (f.width, f.height);
    let at = |x: isize, y: isize| -> i32 {
        let x = x.clamp(0, c.w as isize - 1) as usize;
        let y = y.clamp(0, c.hgt as isize - 1) as usize;
        i32::from(plane[y * stride + x])
    };
    let mut out = vec![0u8; w * h];
    match (exact, sx, sy) {
        (true, 1, 1) => {
            for y in 0..h {
                out[y * w..(y + 1) * w].copy_from_slice(&plane[y * stride..y * stride + w]);
            }
        }
        (true, 2, 1) => {
            for y in 0..h {
                for x in 0..w {
                    let (ix, iy) = ((x / 2) as isize, y as isize);
                    let v = at(ix, iy) * 3;
                    out[y * w + x] = if x % 2 == 0 {
                        (v + at(ix - 1, iy) + 1) >> 2
                    } else {
                        (v + at(ix + 1, iy) + 2) >> 2
                    } as u8;
                }
            }
        }
        (true, 1, 2) => {
            for y in 0..h {
                for x in 0..w {
                    let (ix, iy) = (x as isize, (y / 2) as isize);
                    let v = at(ix, iy) * 3;
                    out[y * w + x] = if y % 2 == 0 {
                        (v + at(ix, iy - 1) + 1) >> 2
                    } else {
                        (v + at(ix, iy + 1) + 2) >> 2
                    } as u8;
                }
            }
        }
        (true, 2, 2) => {
            for y in 0..h {
                let iy = (y / 2) as isize;
                let other = if y % 2 == 0 { iy - 1 } else { iy + 1 };
                let colsum = |ix: isize| at(ix, iy) * 3 + at(ix, other);
                for x in 0..w {
                    let ix = (x / 2) as isize;
                    let this = colsum(ix);
                    out[y * w + x] = if x % 2 == 0 {
                        (this * 3 + colsum(ix - 1) + 8) >> 4
                    } else {
                        (this * 3 + colsum(ix + 1) + 7) >> 4
                    } as u8;
                }
            }
        }
        _ => {
            for y in 0..h {
                for x in 0..w {
                    let ix = (x * c.h / f.hmax) as isize;
                    let iy = (y * c.v / f.vmax) as isize;
                    out[y * w + x] = at(ix, iy) as u8;
                }
            }
        }
    }
    out
}

fn fix(x: f64) -> i32 {
    (x * 65536.0 + 0.5) as i32
}

/// libjpeg's YCbCr to RGB conversion.
fn ycc_to_rgb(y: u8, cb: u8, cr: u8) -> [u8; 3] {
    let (y, cb, cr) = (i32::from(y), i32::from(cb) - 128, i32::from(cr) - 128);
    let half = 1 << 15;
    let r = y + ((fix(1.40200) * cr + half) >> 16);
    let g = y + ((-fix(0.34414) * cb - fix(0.71414) * cr + half) >> 16);
    let b = y + ((fix(1.77200) * cb + half) >> 16);
    [
        r.clamp(0, 255) as u8,
        g.clamp(0, 255) as u8,
        b.clamp(0, 255) as u8,
    ]
}

fn output(f: &Frame, qt: &[[u16; 64]; 4], adobe: Option<u8>) -> Image {
    let planes: Vec<Vec<u8>> = f
        .comps
        .iter()
        .map(|c| {
            let stride = c.bw * 8;
            let mut plane = vec![0u8; stride * c.bh * 8];
            for by in 0..c.bh {
                for bx in 0..c.bw {
                    let start = (by * c.bw + bx) * 64;
                    let o = by * 8 * stride + bx * 8;
                    idct_block(
                        &c.coefs[start..start + 64],
                        &qt[c.tq],
                        &mut plane[o..],
                        stride,
                    );
                }
            }
            upsample(f, c, &plane, stride)
        })
        .collect();
    let n = f.width * f.height;
    let mut out = Vec::with_capacity(n * 4);
    let ids: Vec<u8> = f.comps.iter().map(|c| c.id).collect();
    let rgb_ids = ids == [b'R', b'G', b'B'];
    for i in 0..n {
        let px = match planes.len() {
            1 => {
                let g = planes[0][i];
                [g, g, g]
            }
            3 => {
                if adobe == Some(0) || (adobe.is_none() && rgb_ids) {
                    [planes[0][i], planes[1][i], planes[2][i]]
                } else {
                    ycc_to_rgb(planes[0][i], planes[1][i], planes[2][i])
                }
            }
            4 => {
                let (c, m, y) = if adobe == Some(2) {
                    let [r, g, b] = ycc_to_rgb(planes[0][i], planes[1][i], planes[2][i]);
                    (255 - r, 255 - g, 255 - b)
                } else {
                    (planes[0][i], planes[1][i], planes[2][i])
                };
                let k = planes[3][i];
                // Adobe writes inverted CMYK: 255 is no ink.
                let (c, m, y, k) = if adobe.is_some() {
                    (c, m, y, k)
                } else {
                    (255 - c, 255 - m, 255 - y, 255 - k)
                };
                let mul = |a: u8, b: u8| ((u32::from(a) * u32::from(b) + 127) / 255) as u8;
                [mul(c, k), mul(m, k), mul(y, k)]
            }
            _ => [planes[0][i], planes[1][i], 0],
        };
        out.extend_from_slice(&[px[0], px[1], px[2], 255]);
    }
    Image {
        width: f.width as u32,
        height: f.height as u32,
        data: out,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dc_only_block_is_flat() {
        let mut coefs = [0i32; 64];
        // DC 80 with quantizer 1: (80 / 8) + 128 = 138 everywhere.
        coefs[0] = 80;
        let mut out = [0u8; 64];
        idct_block(&coefs, &[1; 64], &mut out, 8);
        assert!(out.iter().all(|&v| v == 138));
    }

    #[test]
    fn neutral_chroma_is_gray() {
        assert_eq!(ycc_to_rgb(100, 128, 128), [100, 100, 100]);
        assert_eq!(ycc_to_rgb(76, 85, 255), [254, 0, 0]);
    }

    #[test]
    fn huffman_codes_are_canonical() {
        let mut counts = [0u8; 16];
        counts[1] = 3;
        let h = Huffman::new(&counts, vec![5, 6, 7]);
        let data = [0b0110_0000];
        let mut b = Bits {
            data: &data,
            pos: 0,
            acc: 0,
            n: 0,
        };
        assert_eq!(b.decode(&h).unwrap(), 6);
        assert_eq!(b.decode(&h).unwrap(), 7);
    }
}
