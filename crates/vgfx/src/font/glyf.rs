//! TrueType outlines (`glyf` and `loca`), simple and composite glyphs.

use super::read::{Reader, u16_at, u32_at};
use crate::outline::{Outline, Point};

/// Nesting limit of composite glyphs.
const MAX_DEPTH: u32 = 16;

pub struct Glyf<'a> {
    pub glyf: &'a [u8],
    pub loca: &'a [u8],
    pub long_loca: bool,
    pub num_glyphs: u16,
}

/// Points of a glyph in font units, with contour end indices: the shape
/// composite glyphs need for point matching.
#[derive(Default, Clone, Debug)]
struct Points {
    pts: Vec<(f32, f32, bool)>,
    ends: Vec<usize>,
}

impl<'a> Glyf<'a> {
    /// The byte range of glyph `gid` inside `glyf` (empty for blank glyphs).
    pub fn range(&self, gid: u16) -> Option<(usize, usize)> {
        if gid >= self.num_glyphs {
            return None;
        }
        let i = gid as usize;
        let (a, b) = if self.long_loca {
            (
                u32_at(self.loca, 4 * i)? as usize,
                u32_at(self.loca, 4 * i + 4)? as usize,
            )
        } else {
            (
                u16_at(self.loca, 2 * i)? as usize * 2,
                u16_at(self.loca, 2 * i + 2)? as usize * 2,
            )
        };
        if a > b || b > self.glyf.len() {
            return None;
        }
        Some((a, b))
    }

    /// The bounding box stored in the glyph header (xmin, ymin, xmax, ymax).
    pub fn bbox(&self, gid: u16) -> Option<(i16, i16, i16, i16)> {
        let (a, b) = self.range(gid)?;
        if b - a < 10 {
            return None;
        }
        let mut r = Reader::at(self.glyf, a + 2);
        Some((r.i16()?, r.i16()?, r.i16()?, r.i16()?))
    }

    pub fn outline(&self, gid: u16) -> Option<Outline> {
        let pts = self.points(gid, 0)?;
        Some(to_outline(&pts))
    }

    /// Number of components of a composite glyph (0 for simple glyphs).
    pub fn component_count(&self, gid: u16) -> usize {
        let Some((a, b)) = self.range(gid) else {
            return 0;
        };
        let g = &self.glyf[a..b];
        if g.len() < 10 || (u16_at(g, 0).unwrap_or(0) as i16) >= 0 {
            return 0;
        }
        let mut r = Reader::at(g, 10);
        let mut n = 0;
        while let Some(flags) = r.u16() {
            n += 1;
            if r.skip(2 + component_extra(flags)).is_none() || flags & MORE_COMPONENTS == 0 {
                break;
            }
        }
        n
    }

    fn points(&self, gid: u16, depth: u32) -> Option<Points> {
        if depth > MAX_DEPTH {
            return None;
        }
        let (a, b) = self.range(gid)?;
        if a == b {
            return Some(Points::default());
        }
        let g = &self.glyf[a..b];
        let mut r = Reader::new(g);
        let n = r.i16()?;
        r.skip(8)?;
        if n >= 0 {
            simple(r, n as usize)
        } else {
            self.composite(r, depth)
        }
    }

    fn composite(&self, mut r: Reader, depth: u32) -> Option<Points> {
        let mut out = Points::default();
        loop {
            let flags = r.u16()?;
            let gid = r.u16()?;
            let xy = flags & ARGS_ARE_XY_VALUES != 0;
            let (a1, a2) = if flags & ARG_1_AND_2_ARE_WORDS != 0 {
                if xy {
                    (r.i16()? as i32, r.i16()? as i32)
                } else {
                    (r.u16()? as i32, r.u16()? as i32)
                }
            } else if xy {
                (r.i8()? as i32, r.i8()? as i32)
            } else {
                (r.u8()? as i32, r.u8()? as i32)
            };
            let (mut a, mut b, mut c, mut d) = (1.0f32, 0.0f32, 0.0f32, 1.0f32);
            if flags & WE_HAVE_A_SCALE != 0 {
                a = r.f2dot14()?;
                d = a;
            } else if flags & WE_HAVE_AN_X_AND_Y_SCALE != 0 {
                a = r.f2dot14()?;
                d = r.f2dot14()?;
            } else if flags & WE_HAVE_A_TWO_BY_TWO != 0 {
                a = r.f2dot14()?;
                b = r.f2dot14()?;
                c = r.f2dot14()?;
                d = r.f2dot14()?;
            }
            let child = self.points(gid, depth + 1)?;
            let mut pts: Vec<(f32, f32, bool)> = child
                .pts
                .iter()
                .map(|&(x, y, on)| (a * x + c * y, b * x + d * y, on))
                .collect();
            let (dx, dy) = if xy {
                let (ox, oy) = (a1 as f32, a2 as f32);
                if flags & SCALED_COMPONENT_OFFSET != 0 && flags & UNSCALED_COMPONENT_OFFSET == 0 {
                    (a * ox + c * oy, b * ox + d * oy)
                } else {
                    (ox, oy)
                }
            } else {
                let p = out.pts.get(a1 as usize)?;
                let q = pts.get(a2 as usize)?;
                (p.0 - q.0, p.1 - q.1)
            };
            for p in pts.iter_mut() {
                p.0 += dx;
                p.1 += dy;
            }
            let base = out.pts.len();
            out.pts.extend(pts);
            out.ends.extend(child.ends.iter().map(|e| e + base));
            if flags & MORE_COMPONENTS == 0 {
                break;
            }
        }
        Some(out)
    }
}

pub const ARG_1_AND_2_ARE_WORDS: u16 = 0x0001;
pub const ARGS_ARE_XY_VALUES: u16 = 0x0002;
pub const WE_HAVE_A_SCALE: u16 = 0x0008;
pub const MORE_COMPONENTS: u16 = 0x0020;
pub const WE_HAVE_AN_X_AND_Y_SCALE: u16 = 0x0040;
pub const WE_HAVE_A_TWO_BY_TWO: u16 = 0x0080;
pub const WE_HAVE_INSTRUCTIONS: u16 = 0x0100;
pub const SCALED_COMPONENT_OFFSET: u16 = 0x0800;
pub const UNSCALED_COMPONENT_OFFSET: u16 = 0x1000;

/// Bytes of a component record after its flags and glyph index.
pub fn component_extra(flags: u16) -> usize {
    let args = if flags & ARG_1_AND_2_ARE_WORDS != 0 {
        4
    } else {
        2
    };
    let scale = if flags & WE_HAVE_A_SCALE != 0 {
        2
    } else if flags & WE_HAVE_AN_X_AND_Y_SCALE != 0 {
        4
    } else if flags & WE_HAVE_A_TWO_BY_TWO != 0 {
        8
    } else {
        0
    };
    args + scale
}

fn simple(mut r: Reader, n: usize) -> Option<Points> {
    let mut ends = Vec::with_capacity(n);
    let mut last = 0usize;
    for i in 0..n {
        let e = r.u16()? as usize;
        if i > 0 && e < last {
            return None;
        }
        last = e;
        ends.push(e);
    }
    let count = if n == 0 { 0 } else { last + 1 };
    let ilen = r.u16()? as usize;
    r.skip(ilen)?;
    let mut flags = Vec::with_capacity(count);
    while flags.len() < count {
        let f = r.u8()?;
        flags.push(f);
        if f & 8 != 0 {
            let rep = r.u8()?;
            for _ in 0..rep {
                if flags.len() < count {
                    flags.push(f);
                }
            }
        }
    }
    let mut xs = Vec::with_capacity(count);
    let mut x = 0i32;
    for &f in &flags {
        if f & 2 != 0 {
            let v = r.u8()? as i32;
            x += if f & 0x10 != 0 { v } else { -v };
        } else if f & 0x10 == 0 {
            x += r.i16()? as i32;
        }
        xs.push(x);
    }
    let mut pts = Vec::with_capacity(count);
    let mut y = 0i32;
    for (i, &f) in flags.iter().enumerate() {
        if f & 4 != 0 {
            let v = r.u8()? as i32;
            y += if f & 0x20 != 0 { v } else { -v };
        } else if f & 0x20 == 0 {
            y += r.i16()? as i32;
        }
        pts.push((xs[i] as f32, y as f32, f & 1 != 0));
    }
    Some(Points { pts, ends })
}

/// Converts quadratic contours (on- and off-curve points) to segments.
fn to_outline(p: &Points) -> Outline {
    let mut o = Outline::new();
    let mut start = 0usize;
    for &end in &p.ends {
        if end < start || end >= p.pts.len() {
            break;
        }
        contour(&mut o, &p.pts[start..=end]);
        start = end + 1;
    }
    o
}

fn contour(o: &mut Outline, pts: &[(f32, f32, bool)]) {
    let n = pts.len();
    if n == 0 {
        return;
    }
    let pt = |i: usize| Point::new(pts[i % n].0, pts[i % n].1);
    let on = |i: usize| pts[i % n].2;
    // Find the start point: the first on-curve point, else the midpoint of
    // the first two off-curve points.
    let (first, begin) = match (0..n).find(|&i| on(i)) {
        Some(i) => (pt(i), i),
        None => (pt(0).lerp(pt(1), 0.5), 0),
    };
    o.move_to(first.x, first.y);
    let mut ctrl: Option<Point> = None;
    for k in 1..=n {
        let i = begin + k;
        let p = pt(i);
        let is_last = k == n;
        if on(i) {
            match ctrl.take() {
                Some(c) => o.quad_to(c.x, c.y, p.x, p.y),
                None => {
                    if !is_last {
                        o.line_to(p.x, p.y);
                    }
                }
            }
        } else {
            if let Some(c) = ctrl {
                let m = c.lerp(p, 0.5);
                o.quad_to(c.x, c.y, m.x, m.y);
            }
            ctrl = Some(p);
            if is_last {
                // Back at the start point, which was implied.
                if let Some(c) = ctrl.take() {
                    o.quad_to(c.x, c.y, first.x, first.y);
                }
            }
        }
    }
    o.close();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::outline::Segment;

    fn square_glyph() -> Vec<u8> {
        // One contour, four on-curve points: (0,0) (0,100) (100,100) (100,0).
        let mut g = Vec::new();
        for v in [1i16, 0, 0, 100, 100] {
            g.extend_from_slice(&v.to_be_bytes());
        }
        g.extend_from_slice(&3u16.to_be_bytes());
        g.extend_from_slice(&0u16.to_be_bytes());
        // Flags: on-curve; x: same,+100(short),same,... use full words.
        g.extend_from_slice(&[1, 1, 1, 1]);
        for dx in [0i16, 0, 100, 0] {
            g.extend_from_slice(&dx.to_be_bytes());
        }
        for dy in [0i16, 100, 0, -100] {
            g.extend_from_slice(&dy.to_be_bytes());
        }
        g
    }

    #[test]
    fn simple_square() {
        let g = square_glyph();
        let loca: Vec<u8> = [0u16, (g.len() / 2) as u16]
            .iter()
            .flat_map(|v| v.to_be_bytes())
            .collect();
        let glyf = Glyf {
            glyf: &g,
            loca: &loca,
            long_loca: false,
            num_glyphs: 1,
        };
        let o = glyf.outline(0).unwrap();
        assert_eq!(o.segments[0], Segment::MoveTo(Point::new(0.0, 0.0)));
        assert_eq!(o.segments.len(), 5);
        assert_eq!(o.control_bounds(), Some((0.0, 0.0, 100.0, 100.0)));
    }

    #[test]
    fn composite_with_offset() {
        let mut g = square_glyph();
        let comp_at = g.len();
        for v in [-1i16, 0, 0, 150, 100] {
            g.extend_from_slice(&v.to_be_bytes());
        }
        g.extend_from_slice(&(ARG_1_AND_2_ARE_WORDS | ARGS_ARE_XY_VALUES).to_be_bytes());
        g.extend_from_slice(&0u16.to_be_bytes());
        g.extend_from_slice(&50i16.to_be_bytes());
        g.extend_from_slice(&0i16.to_be_bytes());
        let loca: Vec<u8> = [0u32, comp_at as u32, g.len() as u32]
            .iter()
            .flat_map(|v| v.to_be_bytes())
            .collect();
        let glyf = Glyf {
            glyf: &g,
            loca: &loca,
            long_loca: true,
            num_glyphs: 2,
        };
        assert_eq!(glyf.component_count(1), 1);
        let o = glyf.outline(1).unwrap();
        assert_eq!(o.control_bounds(), Some((50.0, 0.0, 150.0, 100.0)));
    }
}
