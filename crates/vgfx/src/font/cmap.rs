//! Character to glyph mapping (`cmap`): formats 0, 4, 6, 12 and 13, and
//! the variation sequences of format 14.

use super::read::{Reader, u16_at, u32_at};

#[derive(Clone, Copy, Debug, Default)]
pub struct Cmap {
    /// Offset (inside the table) of the subtable used for lookups.
    main: Option<usize>,
    /// Offset of the format 14 subtable.
    variations: Option<usize>,
    /// The chosen subtable is a (3,0) symbol subtable.
    pub symbol: bool,
}

/// Result of a variation sequence lookup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Variant {
    /// The sequence exists and maps to the glyph of the base character.
    UseDefault,
    Glyph(u16),
}

impl Cmap {
    pub fn parse(d: &[u8]) -> Cmap {
        let mut c = Cmap::default();
        let n = u16_at(d, 2).unwrap_or(0) as usize;
        let mut best = 0u32;
        for i in 0..n {
            let rec = 4 + i * 8;
            let (Some(platform), Some(encoding), Some(off)) =
                (u16_at(d, rec), u16_at(d, rec + 2), u32_at(d, rec + 4))
            else {
                break;
            };
            let off = off as usize;
            let Some(format) = u16_at(d, off) else {
                continue;
            };
            if format == 14 {
                if platform == 0 && encoding == 5 {
                    c.variations = Some(off);
                }
                continue;
            }
            let score = match (platform, encoding, format) {
                (3, 10, 12) | (0, 6, 13) => 10,
                (0, 4, 12) => 9,
                (0, _, 12) => 8,
                (3, 1, 4) => 7,
                (0, 3, _) => 6,
                (0, _, _) => 5,
                (3, 1, _) => 4,
                (3, 0, _) => 3,
                (1, 0, _) => 1,
                _ => 0,
            };
            if score > best {
                best = score;
                c.main = Some(off);
                c.symbol = platform == 3 && encoding == 0;
            }
        }
        c
    }

    pub fn glyph(&self, d: &[u8], cp: u32) -> Option<u16> {
        let off = self.main?;
        let g = lookup(d, off, cp)?;
        if g == 0 && self.symbol && cp <= 0xff {
            // Symbol fonts map U+F000..U+F0FF.
            return lookup(d, off, 0xf000 + cp).filter(|&g| g != 0);
        }
        Some(g).filter(|&g| g != 0)
    }

    pub fn variant(&self, d: &[u8], cp: u32, selector: u32) -> Option<Variant> {
        let off = self.variations?;
        let mut r = Reader::at(d, off + 6);
        let n = r.u32()?;
        for _ in 0..n {
            let vs = r.u24()?;
            let def = r.u32()? as usize;
            let non_def = r.u32()? as usize;
            if vs != selector {
                continue;
            }
            if non_def != 0 {
                let mut q = Reader::at(d, off + non_def);
                let m = q.u32()?;
                // Sorted by code point: binary search.
                let (mut lo, mut hi) = (0u32, m);
                while lo < hi {
                    let mid = (lo + hi) / 2;
                    let mut e = Reader::at(d, off + non_def + 4 + mid as usize * 5);
                    let u = e.u24()?;
                    if u == cp {
                        return Some(Variant::Glyph(e.u16()?));
                    }
                    if u < cp {
                        lo = mid + 1;
                    } else {
                        hi = mid;
                    }
                }
            }
            if def != 0 {
                let mut q = Reader::at(d, off + def);
                let m = q.u32()?;
                for _ in 0..m {
                    let start = q.u24()?;
                    let extra = q.u8()? as u32;
                    if cp >= start && cp <= start + extra {
                        return Some(Variant::UseDefault);
                    }
                }
            }
            return None;
        }
        None
    }

    /// Calls `f` with every mapped code point (used to build coverage).
    pub fn for_each(&self, d: &[u8], mut f: impl FnMut(u32, u16)) {
        let Some(off) = self.main else {
            return;
        };
        let _ = for_each(d, off, &mut f);
    }
}

fn lookup(d: &[u8], off: usize, cp: u32) -> Option<u16> {
    match u16_at(d, off)? {
        0 => {
            if cp < 256 {
                Some(*d.get(off + 6 + cp as usize)? as u16)
            } else {
                None
            }
        }
        4 => format4(d, off, cp),
        6 => {
            let first = u16_at(d, off + 6)? as u32;
            let count = u16_at(d, off + 8)? as u32;
            if cp >= first && cp < first + count {
                u16_at(d, off + 10 + 2 * (cp - first) as usize)
            } else {
                None
            }
        }
        12 | 13 => {
            let is13 = u16_at(d, off)? == 13;
            let n = u32_at(d, off + 12)?;
            let (mut lo, mut hi) = (0u32, n);
            while lo < hi {
                let mid = (lo + hi) / 2;
                let g = off + 16 + mid as usize * 12;
                let start = u32_at(d, g)?;
                let end = u32_at(d, g + 4)?;
                if cp < start {
                    hi = mid;
                } else if cp > end {
                    lo = mid + 1;
                } else {
                    let gid = u32_at(d, g + 8)?;
                    let v = if is13 { gid } else { gid + (cp - start) };
                    return u16::try_from(v).ok();
                }
            }
            None
        }
        _ => None,
    }
}

fn format4(d: &[u8], off: usize, cp: u32) -> Option<u16> {
    if cp > 0xffff {
        return None;
    }
    let seg2 = u16_at(d, off + 6)? as usize;
    let ends = off + 14;
    let starts = ends + seg2 + 2;
    let deltas = starts + seg2;
    let ranges = deltas + seg2;
    let segs = seg2 / 2;
    let (mut lo, mut hi) = (0usize, segs);
    while lo < hi {
        let mid = (lo + hi) / 2;
        let end = u16_at(d, ends + 2 * mid)? as u32;
        if end < cp {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    if lo >= segs {
        return None;
    }
    let i = lo;
    let start = u16_at(d, starts + 2 * i)? as u32;
    if cp < start {
        return None;
    }
    let delta = u16_at(d, deltas + 2 * i)?;
    let ro = u16_at(d, ranges + 2 * i)? as usize;
    if ro == 0 {
        return Some((cp as u16).wrapping_add(delta));
    }
    let at = ranges + 2 * i + ro + 2 * (cp - start) as usize;
    let g = u16_at(d, at)?;
    if g == 0 {
        return Some(0);
    }
    Some(g.wrapping_add(delta))
}

fn for_each(d: &[u8], off: usize, f: &mut dyn FnMut(u32, u16)) -> Option<()> {
    match u16_at(d, off)? {
        0 => {
            for cp in 0..256u32 {
                let g = *d.get(off + 6 + cp as usize)? as u16;
                if g != 0 {
                    f(cp, g);
                }
            }
        }
        4 => {
            let seg2 = u16_at(d, off + 6)? as usize;
            let ends = off + 14;
            let starts = ends + seg2 + 2;
            for i in 0..seg2 / 2 {
                let s = u16_at(d, starts + 2 * i)? as u32;
                let e = u16_at(d, ends + 2 * i)? as u32;
                for cp in s..=e.min(0xfffe) {
                    if let Some(g) = format4(d, off, cp).filter(|&g| g != 0) {
                        f(cp, g);
                    }
                }
            }
        }
        6 => {
            let first = u16_at(d, off + 6)? as u32;
            let count = u16_at(d, off + 8)? as u32;
            for i in 0..count {
                let g = u16_at(d, off + 10 + 2 * i as usize)?;
                if g != 0 {
                    f(first + i, g);
                }
            }
        }
        12 | 13 => {
            let is13 = u16_at(d, off)? == 13;
            let n = u32_at(d, off + 12)?;
            for k in 0..n {
                let g = off + 16 + k as usize * 12;
                let start = u32_at(d, g)?;
                let end = u32_at(d, g + 4)?.min(0x10ffff);
                let gid = u32_at(d, g + 8)?;
                for cp in start..=end {
                    let v = if is13 { gid } else { gid + (cp - start) };
                    if let Ok(v) = u16::try_from(v) {
                        if v != 0 {
                            f(cp, v);
                        }
                    }
                }
            }
        }
        _ => {}
    }
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table_with(format_sub: Vec<u8>, platform: u16, encoding: u16) -> Vec<u8> {
        let mut t = vec![0, 0, 0, 1];
        t.extend_from_slice(&platform.to_be_bytes());
        t.extend_from_slice(&encoding.to_be_bytes());
        t.extend_from_slice(&12u32.to_be_bytes());
        t.extend(format_sub);
        t
    }

    #[test]
    fn format4_lookup() {
        // Segments: 'A'..'C' → 10..12 by delta; 0xFFFF terminator.
        let mut s = Vec::new();
        let w = |s: &mut Vec<u8>, v: u16| s.extend_from_slice(&v.to_be_bytes());
        w(&mut s, 4);
        w(&mut s, 32);
        w(&mut s, 0);
        w(&mut s, 4); // segCountX2
        w(&mut s, 0);
        w(&mut s, 0);
        w(&mut s, 0);
        w(&mut s, 0x43);
        w(&mut s, 0xffff);
        w(&mut s, 0);
        w(&mut s, 0x41);
        w(&mut s, 0xffff);
        w(&mut s, 10u16.wrapping_sub(0x41));
        w(&mut s, 1);
        w(&mut s, 0);
        w(&mut s, 0);
        let t = table_with(s, 3, 1);
        let c = Cmap::parse(&t);
        assert_eq!(c.glyph(&t, 0x41), Some(10));
        assert_eq!(c.glyph(&t, 0x43), Some(12));
        assert_eq!(c.glyph(&t, 0x44), None);
    }

    #[test]
    fn format12_lookup() {
        let mut s = Vec::new();
        s.extend_from_slice(&12u16.to_be_bytes());
        s.extend_from_slice(&0u16.to_be_bytes());
        s.extend_from_slice(&28u32.to_be_bytes());
        s.extend_from_slice(&0u32.to_be_bytes());
        s.extend_from_slice(&1u32.to_be_bytes());
        for v in [0x1f600u32, 0x1f602, 50] {
            s.extend_from_slice(&v.to_be_bytes());
        }
        let t = table_with(s, 3, 10);
        let c = Cmap::parse(&t);
        assert_eq!(c.glyph(&t, 0x1f601), Some(51));
        assert_eq!(c.glyph(&t, 0x1f603), None);
    }
}
