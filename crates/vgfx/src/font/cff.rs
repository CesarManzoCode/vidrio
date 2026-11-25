//! Compact Font Format outlines: CFF (version 1, Type 2 charstrings, with
//! CID-keyed fonts) and CFF2 (default instance of variable fonts).

use super::read::Reader;
use crate::outline::Outline;

#[derive(Clone, Copy, Debug, Default)]
struct Index {
    count: u32,
    off_size: u8,
    /// Position of the offset array.
    offsets: usize,
    /// Position of the byte before the object data (offsets are 1-based).
    data: usize,
    /// Position after the index.
    end: usize,
}

impl Index {
    fn parse(d: &[u8], pos: usize, cff2: bool) -> Option<Index> {
        let mut r = Reader::at(d, pos);
        let count = if cff2 { r.u32()? } else { r.u16()? as u32 };
        if count == 0 {
            return Some(Index {
                end: r.pos(),
                ..Index::default()
            });
        }
        let off_size = r.u8()?;
        if !(1..=4).contains(&off_size) {
            return None;
        }
        let offsets = r.pos();
        let data = offsets + (count as usize + 1) * off_size as usize - 1;
        let mut idx = Index {
            count,
            off_size,
            offsets,
            data,
            end: 0,
        };
        let last = idx.offset(d, count)?;
        idx.end = data + last;
        if idx.end > d.len() {
            return None;
        }
        Some(idx)
    }

    fn offset(&self, d: &[u8], i: u32) -> Option<usize> {
        let mut r = Reader::at(d, self.offsets + i as usize * self.off_size as usize);
        let mut v = 0usize;
        for _ in 0..self.off_size {
            v = (v << 8) | r.u8()? as usize;
        }
        Some(v)
    }

    fn get<'a>(&self, d: &'a [u8], i: u32) -> Option<&'a [u8]> {
        if i >= self.count {
            return None;
        }
        let a = self.offset(d, i)?;
        let b = self.offset(d, i + 1)?;
        if a == 0 || a > b {
            return None;
        }
        d.get(self.data + a..self.data + b)
    }
}

/// A parsed DICT: (operator, operands); escaped operators are 1200 + n.
fn parse_dict(d: &[u8], vstore_regions: &dyn Fn(u16) -> u16) -> Option<Vec<(u16, Vec<f32>)>> {
    let mut out = Vec::new();
    let mut ops: Vec<f32> = Vec::new();
    let mut r = Reader::new(d);
    let mut vsindex = 0u16;
    while let Some(b0) = r.u8() {
        match b0 {
            0..=21 => {
                let op = if b0 == 12 {
                    1200 + r.u8()? as u16
                } else {
                    b0 as u16
                };
                out.push((op, std::mem::take(&mut ops)));
            }
            22 => {
                // vsindex (CFF2 Private DICT).
                vsindex = ops.pop()? as u16;
                ops.clear();
            }
            23 => {
                // blend at the default instance: keep the defaults.
                let n = ops.pop()? as usize;
                let k = vstore_regions(vsindex) as usize;
                let total = n * (k + 1);
                if ops.len() < total {
                    return None;
                }
                let base = ops.len() - total;
                ops.truncate(base + n);
            }
            24 => out.push((24, std::mem::take(&mut ops))),
            28 => ops.push(r.i16()? as f32),
            29 => ops.push(r.i32()? as f32),
            30 => ops.push(parse_real(&mut r)?),
            32..=246 => ops.push(b0 as f32 - 139.0),
            247..=250 => ops.push(((b0 as f32 - 247.0) * 256.0) + r.u8()? as f32 + 108.0),
            251..=254 => ops.push(-((b0 as f32 - 251.0) * 256.0) - r.u8()? as f32 - 108.0),
            _ => return None,
        }
    }
    Some(out)
}

fn parse_real(r: &mut Reader) -> Option<f32> {
    let mut s = String::new();
    'outer: loop {
        let b = r.u8()?;
        for nib in [b >> 4, b & 15] {
            match nib {
                0..=9 => s.push((b'0' + nib) as char),
                0xa => s.push('.'),
                0xb => s.push('E'),
                0xc => s.push_str("E-"),
                0xe => s.push('-'),
                0xf => break 'outer,
                _ => {}
            }
        }
    }
    Some(s.parse::<f32>().unwrap_or(0.0))
}

fn dict_get(dict: &[(u16, Vec<f32>)], op: u16) -> Option<&[f32]> {
    dict.iter()
        .find(|(o, _)| *o == op)
        .map(|(_, v)| v.as_slice())
}

#[derive(Clone, Debug, Default)]
struct Private {
    subrs: Option<Index>,
    nominal_width: f32,
}

#[derive(Clone, Debug)]
enum FdSelect {
    None,
    At { pos: usize, format: u8 },
}

/// A parsed CFF or CFF2 table (positions are relative to the table).
#[derive(Clone, Debug)]
pub struct Cff {
    cff2: bool,
    charstrings: Index,
    global_subrs: Index,
    privates: Vec<Private>,
    fd_select: FdSelect,
    /// Region count of each item variation data (CFF2 blend).
    region_counts: Vec<u16>,
    pub font_matrix: Option<[f32; 6]>,
    pub is_cid: bool,
}

fn parse_private(
    d: &[u8],
    font_dict: &[(u16, Vec<f32>)],
    cff2: bool,
    regions: &dyn Fn(u16) -> u16,
) -> Option<Private> {
    let Some(p) = dict_get(font_dict, 18) else {
        return Some(Private::default());
    };
    if p.len() < 2 {
        return None;
    }
    let (size, off) = (p[0] as usize, p[1] as usize);
    let pd = d.get(off..off.checked_add(size)?)?;
    let dict = parse_dict(pd, regions)?;
    let subrs = match dict_get(&dict, 19) {
        Some(s) if !s.is_empty() => Some(Index::parse(d, off + s[0] as usize, cff2)?),
        _ => None,
    };
    let nominal_width = dict_get(&dict, 21)
        .and_then(|v| v.first().copied())
        .unwrap_or(0.0);
    Some(Private {
        subrs,
        nominal_width,
    })
}

fn item_variation_region_counts(d: &[u8], pos: usize) -> Option<Vec<u16>> {
    // CFF2 VariationStore: u16 length, then an ItemVariationStore.
    let base = pos + 2;
    let mut r = Reader::at(d, base);
    let _format = r.u16()?;
    let _regions = r.u32()?;
    let count = r.u16()?;
    let mut out = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let off = r.u32()? as usize;
        let mut q = Reader::at(d, base + off);
        q.skip(4)?;
        out.push(q.u16()?);
    }
    Some(out)
}

impl Cff {
    pub fn parse(d: &[u8], cff2: bool) -> Option<Cff> {
        let mut r = Reader::new(d);
        let major = r.u8()?;
        let _minor = r.u8()?;
        let hdr_size = r.u8()? as usize;
        if cff2 != (major == 2) {
            return None;
        }
        let (top, global_subrs) = if cff2 {
            let top_len = r.u16()? as usize;
            let top = d.get(hdr_size..hdr_size + top_len)?;
            let gs = Index::parse(d, hdr_size + top_len, true)?;
            (top, gs)
        } else {
            let names = Index::parse(d, hdr_size, false)?;
            let tops = Index::parse(d, names.end, false)?;
            let strings = Index::parse(d, tops.end, false)?;
            let gs = Index::parse(d, strings.end, false)?;
            (tops.get(d, 0)?, gs)
        };
        let none = |_: u16| 0u16;
        // The top DICT has no blend, so a first pass finds the vstore.
        let top_dict = parse_dict(top, &none)?;
        let region_counts = match dict_get(&top_dict, 24) {
            Some(v) if cff2 && !v.is_empty() => {
                item_variation_region_counts(d, v[0] as usize).unwrap_or_default()
            }
            _ => Vec::new(),
        };
        let rc = region_counts.clone();
        let regions = move |i: u16| rc.get(i as usize).copied().unwrap_or(0);
        if !cff2 {
            if let Some(t) = dict_get(&top_dict, 1206) {
                if t.first().copied().unwrap_or(2.0) != 2.0 {
                    return None;
                }
            }
        }
        let cs_off = dict_get(&top_dict, 17)?.first().copied()? as usize;
        let charstrings = Index::parse(d, cs_off, cff2)?;
        let font_matrix = dict_get(&top_dict, 1207).and_then(|m| {
            if m.len() == 6 {
                Some([m[0], m[1], m[2], m[3], m[4], m[5]])
            } else {
                None
            }
        });
        let is_cid = dict_get(&top_dict, 1230).is_some();
        let mut privates = Vec::new();
        let mut fd_select = FdSelect::None;
        if let Some(fda) = dict_get(&top_dict, 1236).and_then(|v| v.first().copied()) {
            let fd_array = Index::parse(d, fda as usize, cff2)?;
            for i in 0..fd_array.count {
                let fd = parse_dict(fd_array.get(d, i)?, &none)?;
                privates.push(parse_private(d, &fd, cff2, &regions)?);
            }
            if let Some(sel) = dict_get(&top_dict, 1237).and_then(|v| v.first().copied()) {
                let pos = sel as usize;
                let format = *d.get(pos)?;
                fd_select = FdSelect::At { pos, format };
            }
        } else {
            privates.push(parse_private(d, &top_dict, cff2, &regions)?);
        }
        Some(Cff {
            cff2,
            charstrings,
            global_subrs,
            privates,
            fd_select,
            region_counts,
            font_matrix,
            is_cid,
        })
    }

    pub fn glyph_count(&self) -> u32 {
        self.charstrings.count
    }

    fn fd_index(&self, d: &[u8], gid: u16) -> usize {
        let FdSelect::At { pos, format } = self.fd_select else {
            return 0;
        };
        let g = gid as u32;
        let mut r = Reader::at(d, pos + 1);
        let found = match format {
            0 => {
                r.skip(gid as usize);
                r.u8().map(|v| v as usize)
            }
            3 => (|| {
                let n = r.u16()?;
                let mut first = r.u16()? as u32;
                for _ in 0..n {
                    let fd = r.u8()?;
                    let next = r.u16()? as u32;
                    if g >= first && g < next {
                        return Some(fd as usize);
                    }
                    first = next;
                }
                None
            })(),
            4 => (|| {
                let n = r.u32()?;
                let mut first = r.u32()?;
                for _ in 0..n {
                    let fd = r.u16()?;
                    let next = r.u32()?;
                    if g >= first && g < next {
                        return Some(fd as usize);
                    }
                    first = next;
                }
                None
            })(),
            _ => None,
        };
        found.filter(|&i| i < self.privates.len()).unwrap_or(0)
    }

    pub fn outline(&self, d: &[u8], gid: u16) -> Option<Outline> {
        let cs = self.charstrings.get(d, gid as u32)?;
        let private = self.privates.get(self.fd_index(d, gid))?;
        let mut st = State {
            cff: self,
            d,
            private,
            stack: Vec::with_capacity(48),
            x: 0.0,
            y: 0.0,
            nstems: 0,
            have_width: self.cff2,
            open: false,
            outline: Outline::new(),
            transient: [0.0; 32],
            vsindex: 0,
            done: false,
            seed: gid as u32 ^ 0x9e37_79b9,
        };
        st.run(cs, 0)?;
        if st.open {
            st.outline.close();
        }
        Some(st.outline)
    }
}

fn bias(count: u32) -> i32 {
    if count < 1240 {
        107
    } else if count < 33900 {
        1131
    } else {
        32768
    }
}

struct State<'a> {
    cff: &'a Cff,
    d: &'a [u8],
    private: &'a Private,
    stack: Vec<f32>,
    x: f32,
    y: f32,
    nstems: usize,
    have_width: bool,
    open: bool,
    outline: Outline,
    transient: [f32; 32],
    vsindex: u16,
    done: bool,
    seed: u32,
}

impl State<'_> {
    /// Drops a leading width operand (CFF 1 only) if `extra` says there is one.
    fn width(&mut self, extra: bool) {
        if !self.have_width {
            self.have_width = true;
            if extra && !self.stack.is_empty() {
                let _width = self.stack.remove(0) + self.private.nominal_width;
            }
        }
    }

    fn move_to(&mut self, dx: f32, dy: f32) {
        if self.open {
            self.outline.close();
        }
        self.x += dx;
        self.y += dy;
        self.outline.move_to(self.x, self.y);
        self.open = true;
    }

    fn line_to(&mut self, dx: f32, dy: f32) {
        if !self.open {
            self.move_to(0.0, 0.0);
        }
        self.x += dx;
        self.y += dy;
        self.outline.line_to(self.x, self.y);
    }

    fn curve_to(&mut self, d: [f32; 6]) {
        if !self.open {
            self.move_to(0.0, 0.0);
        }
        let x1 = self.x + d[0];
        let y1 = self.y + d[1];
        let x2 = x1 + d[2];
        let y2 = y1 + d[3];
        self.x = x2 + d[4];
        self.y = y2 + d[5];
        self.outline.cubic_to(x1, y1, x2, y2, self.x, self.y);
    }

    fn stems(&mut self) {
        let odd = self.stack.len() % 2 == 1;
        self.width(odd);
        self.nstems += self.stack.len() / 2;
        self.stack.clear();
    }

    fn run(&mut self, cs: &[u8], depth: u32) -> Option<()> {
        if depth > 10 {
            return None;
        }
        let mut r = Reader::new(cs);
        while let Some(b0) = r.u8() {
            if self.done {
                return Some(());
            }
            if self.stack.len() > 513 {
                return None;
            }
            match b0 {
                1 | 3 | 18 | 23 => self.stems(),
                19 | 20 => {
                    if !self.stack.is_empty() {
                        self.stems();
                    }
                    self.width(false);
                    r.skip(self.nstems.div_ceil(8))?;
                }
                21 => {
                    let extra = self.stack.len() > 2;
                    self.width(extra);
                    let n = self.stack.len();
                    let (dx, dy) = (*self.stack.get(n.wrapping_sub(2))?, self.stack[n - 1]);
                    self.move_to(dx, dy);
                    self.stack.clear();
                }
                22 | 4 => {
                    let extra = self.stack.len() > 1;
                    self.width(extra);
                    let v = *self.stack.last()?;
                    if b0 == 22 {
                        self.move_to(v, 0.0);
                    } else {
                        self.move_to(0.0, v);
                    }
                    self.stack.clear();
                }
                5 => {
                    let s = std::mem::take(&mut self.stack);
                    for p in s.chunks_exact(2) {
                        self.line_to(p[0], p[1]);
                    }
                }
                6 | 7 => {
                    let s = std::mem::take(&mut self.stack);
                    let mut horizontal = b0 == 6;
                    for &v in &s {
                        if horizontal {
                            self.line_to(v, 0.0);
                        } else {
                            self.line_to(0.0, v);
                        }
                        horizontal = !horizontal;
                    }
                }
                8 => {
                    let s = std::mem::take(&mut self.stack);
                    for c in s.chunks_exact(6) {
                        self.curve_to([c[0], c[1], c[2], c[3], c[4], c[5]]);
                    }
                }
                24 => {
                    // rcurveline
                    let s = std::mem::take(&mut self.stack);
                    if s.len() < 8 {
                        return None;
                    }
                    let curves = (s.len() - 2) / 6;
                    for c in s.chunks_exact(6).take(curves) {
                        self.curve_to([c[0], c[1], c[2], c[3], c[4], c[5]]);
                    }
                    let n = s.len();
                    self.line_to(s[n - 2], s[n - 1]);
                }
                25 => {
                    // rlinecurve
                    let s = std::mem::take(&mut self.stack);
                    if s.len() < 8 {
                        return None;
                    }
                    let lines = (s.len() - 6) / 2;
                    for p in s.chunks_exact(2).take(lines) {
                        self.line_to(p[0], p[1]);
                    }
                    let c = &s[lines * 2..];
                    self.curve_to([c[0], c[1], c[2], c[3], c[4], c[5]]);
                }
                26 | 27 => {
                    // vvcurveto / hhcurveto
                    let s = std::mem::take(&mut self.stack);
                    let mut i = 0;
                    let mut first = 0.0;
                    if s.len() % 4 == 1 {
                        first = s[0];
                        i = 1;
                    }
                    while i + 4 <= s.len() {
                        if b0 == 27 {
                            self.curve_to([s[i], first, s[i + 1], s[i + 2], s[i + 3], 0.0]);
                        } else {
                            self.curve_to([first, s[i], s[i + 1], s[i + 2], 0.0, s[i + 3]]);
                        }
                        first = 0.0;
                        i += 4;
                    }
                }
                30 | 31 => {
                    // vhcurveto / hvcurveto
                    let s = std::mem::take(&mut self.stack);
                    let mut horizontal = b0 == 31;
                    let mut i = 0;
                    while i + 4 <= s.len() {
                        let last = if s.len() - i == 5 { s[i + 4] } else { 0.0 };
                        if horizontal {
                            self.curve_to([s[i], 0.0, s[i + 1], s[i + 2], last, s[i + 3]]);
                        } else {
                            self.curve_to([0.0, s[i], s[i + 1], s[i + 2], s[i + 3], last]);
                        }
                        horizontal = !horizontal;
                        i += 4;
                    }
                }
                10 | 29 => {
                    let idx = self.stack.pop()? as i32;
                    let subrs = if b0 == 10 {
                        self.private.subrs?
                    } else {
                        self.cff.global_subrs
                    };
                    let i = idx + bias(subrs.count);
                    if i < 0 {
                        return None;
                    }
                    let sub = subrs.get(self.d, i as u32)?;
                    self.run(sub, depth + 1)?;
                    if self.done {
                        return Some(());
                    }
                }
                11 => return Some(()),
                14 => {
                    let extra = self.stack.len() == 1 || self.stack.len() == 5;
                    self.width(extra);
                    self.stack.clear();
                    if self.open {
                        self.outline.close();
                        self.open = false;
                    }
                    self.done = true;
                    return Some(());
                }
                15 => {
                    self.vsindex = self.stack.pop()? as u16;
                    self.stack.clear();
                }
                16 => {
                    let n = self.stack.pop()? as usize;
                    let k = self
                        .cff
                        .region_counts
                        .get(self.vsindex as usize)
                        .copied()
                        .unwrap_or(0) as usize;
                    let total = n * (k + 1);
                    if self.stack.len() < total {
                        return None;
                    }
                    let base = self.stack.len() - total;
                    self.stack.truncate(base + n);
                }
                12 => {
                    let b1 = r.u8()?;
                    self.escape(b1)?;
                }
                28 => self.stack.push(r.i16()? as f32),
                32..=246 => self.stack.push(b0 as f32 - 139.0),
                247..=250 => self
                    .stack
                    .push((b0 as f32 - 247.0) * 256.0 + r.u8()? as f32 + 108.0),
                251..=254 => self
                    .stack
                    .push(-(b0 as f32 - 251.0) * 256.0 - r.u8()? as f32 - 108.0),
                255 => self.stack.push(r.i32()? as f32 / 65536.0),
                _ => self.stack.clear(),
            }
        }
        Some(())
    }

    fn escape(&mut self, op: u8) -> Option<()> {
        let s = &mut self.stack;
        match op {
            35 => {
                // flex
                let v = std::mem::take(s);
                if v.len() < 13 {
                    return None;
                }
                self.curve_to([v[0], v[1], v[2], v[3], v[4], v[5]]);
                self.curve_to([v[6], v[7], v[8], v[9], v[10], v[11]]);
            }
            34 => {
                // hflex
                let v = std::mem::take(s);
                if v.len() < 7 {
                    return None;
                }
                self.curve_to([v[0], 0.0, v[1], v[2], v[3], 0.0]);
                self.curve_to([v[4], 0.0, v[5], -v[2], v[6], 0.0]);
            }
            36 => {
                // hflex1
                let v = std::mem::take(s);
                if v.len() < 9 {
                    return None;
                }
                self.curve_to([v[0], v[1], v[2], v[3], v[4], 0.0]);
                self.curve_to([v[5], 0.0, v[6], v[7], v[8], -(v[1] + v[3] + v[7])]);
            }
            37 => {
                // flex1
                let v = std::mem::take(s);
                if v.len() < 11 {
                    return None;
                }
                let dx: f32 = v[0] + v[2] + v[4] + v[6] + v[8];
                let dy: f32 = v[1] + v[3] + v[5] + v[7] + v[9];
                let (dx6, dy6) = if dx.abs() > dy.abs() {
                    (v[10], -dy)
                } else {
                    (-dx, v[10])
                };
                self.curve_to([v[0], v[1], v[2], v[3], v[4], v[5]]);
                self.curve_to([v[6], v[7], v[8], v[9], dx6, dy6]);
            }
            3 | 4 | 10 | 11 | 12 | 15 | 24 => {
                let b = s.pop()?;
                let a = s.pop()?;
                s.push(match op {
                    3 => (a != 0.0 && b != 0.0) as u8 as f32,
                    4 => (a != 0.0 || b != 0.0) as u8 as f32,
                    10 => a + b,
                    11 => a - b,
                    12 => {
                        if b == 0.0 {
                            0.0
                        } else {
                            a / b
                        }
                    }
                    15 => (a == b) as u8 as f32,
                    _ => a * b,
                });
            }
            5 => {
                let a = s.pop()?;
                s.push((a == 0.0) as u8 as f32);
            }
            9 => {
                let a = s.pop()?;
                s.push(a.abs());
            }
            14 => {
                let a = s.pop()?;
                s.push(-a);
            }
            26 => {
                let a = s.pop()?;
                s.push(a.max(0.0).sqrt());
            }
            18 => {
                s.pop()?;
            }
            20 => {
                let i = s.pop()? as usize;
                let v = s.pop()?;
                *self.transient.get_mut(i)? = v;
            }
            21 => {
                let i = s.pop()? as usize;
                let v = *self.transient.get(i)?;
                s.push(v);
            }
            22 => {
                let v2 = s.pop()?;
                let v1 = s.pop()?;
                let b = s.pop()?;
                let a = s.pop()?;
                s.push(if v1 <= v2 { a } else { b });
            }
            23 => {
                self.seed = self.seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
                let v = ((self.seed >> 8) & 0xffff) as f32 / 65536.0;
                s.push(v + 1.0 / 65536.0);
            }
            27 => {
                let a = *s.last()?;
                s.push(a);
            }
            28 => {
                let n = s.len();
                if n < 2 {
                    return None;
                }
                s.swap(n - 1, n - 2);
            }
            29 => {
                let i = s.pop()?;
                let n = s.len();
                let idx = if i < 0.0 { 0 } else { i as usize };
                if idx >= n {
                    return None;
                }
                let v = s[n - 1 - idx];
                s.push(v);
            }
            30 => {
                let j = s.pop()? as i64;
                let nn = s.pop()? as usize;
                let n = s.len();
                if nn == 0 || nn > n {
                    return None;
                }
                let part = &mut s[n - nn..];
                let shift = j.rem_euclid(nn as i64) as usize;
                part.rotate_right(shift);
            }
            _ => s.clear(),
        }
        Some(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index(objects: &[&[u8]]) -> Vec<u8> {
        let mut v = (objects.len() as u16).to_be_bytes().to_vec();
        if objects.is_empty() {
            return v;
        }
        v.push(2);
        let mut off = 1u16;
        v.extend_from_slice(&off.to_be_bytes());
        for o in objects {
            off += o.len() as u16;
            v.extend_from_slice(&off.to_be_bytes());
        }
        for o in objects {
            v.extend_from_slice(o);
        }
        v
    }

    /// A one-glyph CFF whose charstring draws a 100×200 rectangle.
    fn rect_cff() -> Vec<u8> {
        // 100 0 rmoveto 100 hlineto 200 vlineto -100 hlineto endchar
        let cs: Vec<u8> = vec![
            239, 139, 21, //
            239, 6, //
            247, 92, 7, // 200
            39, 6, // -100
            14,
        ];
        let name = index(&[b"T"]);
        let strings = index(&[]);
        let gsubrs = index(&[]);
        // Top DICT: CharStrings offset (fixed 5-byte int) and Private.
        let top_len = 5 + 1 + 2 + 1;
        let header = 4;
        let top_index_len = 2 + 1 + 4 + top_len;
        let cs_off = header + name.len() + top_index_len + strings.len() + gsubrs.len();
        let charstrings = index(&[&cs]);
        let mut top = vec![29];
        top.extend_from_slice(&(cs_off as i32).to_be_bytes());
        top.push(17);
        // Private: size 0 at offset 0.
        top.extend_from_slice(&[139, 139, 18]);
        let mut out = vec![1, 0, 4, 2];
        out.extend_from_slice(&name);
        out.extend_from_slice(&index(&[&top]));
        out.extend_from_slice(&strings);
        out.extend_from_slice(&gsubrs);
        assert_eq!(out.len(), cs_off);
        out.extend_from_slice(&charstrings);
        out
    }

    #[test]
    fn rectangle_charstring() {
        let d = rect_cff();
        let cff = Cff::parse(&d, false).unwrap();
        assert_eq!(cff.glyph_count(), 1);
        let o = cff.outline(&d, 0).unwrap();
        assert_eq!(o.contours(), 1);
        assert_eq!(o.control_bounds(), Some((100.0, 0.0, 200.0, 200.0)));
    }

    #[test]
    fn real_numbers_in_dicts() {
        // -2.25 = nibbles e 2 a 2 5 f
        let mut r = Reader::new(&[0xe2, 0xa2, 0x5f]);
        assert_eq!(parse_real(&mut r), Some(-2.25));
    }
}
