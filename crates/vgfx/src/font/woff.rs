//! WOFF 1.0 (zlib per table) and WOFF2 (Brotli, with the reconstruction of
//! the transformed `glyf`, `loca` and `hmtx` tables) to plain sfnt bytes.

use super::FontError;
use super::glyf::{MORE_COMPONENTS, WE_HAVE_INSTRUCTIONS, component_extra};
use super::read::{Reader, Tag, u16_at, u32_at};

/// Limit on the decoded size of a web font.
const MAX_SIZE: usize = 64 << 20;

fn checksum(d: &[u8]) -> u32 {
    let mut sum = 0u32;
    for c in d.chunks(4) {
        let mut b = [0u8; 4];
        b[..c.len()].copy_from_slice(c);
        sum = sum.wrapping_add(u32::from_be_bytes(b));
    }
    sum
}

/// Builds an sfnt from (tag, data) pairs; tables are sorted by tag.
pub fn build_sfnt(flavor: u32, tables: &mut [(Tag, Vec<u8>)]) -> Vec<u8> {
    tables.sort_by_key(|t| t.0);
    let n = tables.len() as u16;
    let mut out = Vec::new();
    write_offset_table(&mut out, flavor, n);
    let dir = out.len();
    out.resize(dir + 16 * n as usize, 0);
    for (i, (tag, data)) in tables.iter().enumerate() {
        let off = out.len();
        out.extend_from_slice(data);
        while out.len() % 4 != 0 {
            out.push(0);
        }
        let rec = dir + 16 * i;
        out[rec..rec + 4].copy_from_slice(&tag.bytes());
        out[rec + 4..rec + 8].copy_from_slice(&checksum(data).to_be_bytes());
        out[rec + 8..rec + 12].copy_from_slice(&(off as u32).to_be_bytes());
        out[rec + 12..rec + 16].copy_from_slice(&(data.len() as u32).to_be_bytes());
    }
    out
}

fn write_offset_table(out: &mut Vec<u8>, flavor: u32, n: u16) {
    let mut es = 0u16;
    while (2u32 << es) <= n as u32 {
        es += 1;
    }
    let sr = 16u16 << es;
    out.extend_from_slice(&flavor.to_be_bytes());
    out.extend_from_slice(&n.to_be_bytes());
    out.extend_from_slice(&sr.to_be_bytes());
    out.extend_from_slice(&es.to_be_bytes());
    out.extend_from_slice(&(n.wrapping_mul(16).wrapping_sub(sr)).to_be_bytes());
}

pub fn decode_woff1(d: &[u8]) -> Result<Vec<u8>, FontError> {
    let bad = || FontError::Malformed("woff");
    let mut r = Reader::new(d);
    let sig = r.u32().ok_or_else(bad)?;
    if sig != 0x774f_4646 {
        return Err(bad());
    }
    let flavor = r.u32().ok_or_else(bad)?;
    let _length = r.u32().ok_or_else(bad)?;
    let n = r.u16().ok_or_else(bad)? as usize;
    r.set_pos(44);
    let mut tables = Vec::with_capacity(n);
    let mut total = 0usize;
    for _ in 0..n {
        let tag = r.tag().ok_or_else(bad)?;
        let off = r.u32().ok_or_else(bad)? as usize;
        let comp = r.u32().ok_or_else(bad)? as usize;
        let orig = r.u32().ok_or_else(bad)? as usize;
        let _sum = r.u32().ok_or_else(bad)?;
        let src = d
            .get(off..off.checked_add(comp).ok_or_else(bad)?)
            .ok_or_else(bad)?;
        total += orig;
        if total > MAX_SIZE || comp > orig {
            return Err(bad());
        }
        let data = if comp < orig {
            let v = vnet::inflate::zlib_decompress(src)
                .map_err(|_| FontError::Malformed("woff zlib"))?;
            if v.len() != orig {
                return Err(bad());
            }
            v
        } else {
            src.to_vec()
        };
        tables.push((tag, data));
    }
    Ok(build_sfnt(flavor, &mut tables))
}

const KNOWN_TAGS: [&[u8; 4]; 63] = [
    b"cmap", b"head", b"hhea", b"hmtx", b"maxp", b"name", b"OS/2", b"post", b"cvt ", b"fpgm",
    b"glyf", b"loca", b"prep", b"CFF ", b"VORG", b"EBDT", b"EBLC", b"gasp", b"hdmx", b"kern",
    b"LTSH", b"PCLT", b"VDMX", b"vhea", b"vmtx", b"BASE", b"GDEF", b"GPOS", b"GSUB", b"EBSC",
    b"JSTF", b"MATH", b"CBDT", b"CBLC", b"COLR", b"CPAL", b"SVG ", b"sbix", b"acnt", b"avar",
    b"bdat", b"bloc", b"bsln", b"cvar", b"fdsc", b"feat", b"fmtx", b"fvar", b"gvar", b"hsty",
    b"just", b"lcar", b"mort", b"morx", b"opbd", b"prop", b"trak", b"Zapf", b"Silf", b"Glat",
    b"Gloc", b"Feat", b"Sill",
];

const GLYF: Tag = Tag::new(b"glyf");
const LOCA: Tag = Tag::new(b"loca");
const HMTX: Tag = Tag::new(b"hmtx");

fn base128(r: &mut Reader) -> Option<u32> {
    let mut v = 0u32;
    for i in 0..5 {
        let b = r.u8()?;
        if i == 0 && b == 0x80 {
            return None;
        }
        if v & 0xfe00_0000 != 0 {
            return None;
        }
        v = (v << 7) | (b & 0x7f) as u32;
        if b & 0x80 == 0 {
            return Some(v);
        }
    }
    None
}

fn u255(r: &mut Reader) -> Option<u16> {
    let code = r.u8()?;
    Some(match code {
        253 => r.u16()?,
        255 => r.u8()? as u16 + 253,
        254 => r.u8()? as u16 + 506,
        c => c as u16,
    })
}

struct Entry {
    tag: Tag,
    transformed: bool,
    len: usize,
    offset: usize,
}

pub fn decode_woff2(d: &[u8]) -> Result<Vec<u8>, FontError> {
    let bad = |m: &'static str| FontError::Malformed(m);
    let mut r = Reader::new(d);
    if r.u32() != Some(0x774f_4632) {
        return Err(bad("woff2 signature"));
    }
    let flavor = r.u32().ok_or(bad("woff2 header"))?;
    let _length = r.u32().ok_or(bad("woff2 header"))?;
    let n = r.u16().ok_or(bad("woff2 header"))? as usize;
    r.skip(2).ok_or(bad("woff2 header"))?;
    let _total_sfnt = r.u32().ok_or(bad("woff2 header"))?;
    let total_compressed = r.u32().ok_or(bad("woff2 header"))? as usize;
    r.set_pos(48);
    let mut entries = Vec::with_capacity(n);
    let mut offset = 0usize;
    for _ in 0..n {
        let flags = r.u8().ok_or(bad("woff2 directory"))?;
        let tag = if flags & 63 == 63 {
            r.tag().ok_or(bad("woff2 directory"))?
        } else {
            Tag::new(KNOWN_TAGS[(flags & 63) as usize])
        };
        let version = flags >> 6;
        let orig_len = base128(&mut r).ok_or(bad("woff2 directory"))? as usize;
        let transformed = if tag == GLYF || tag == LOCA {
            version == 0
        } else {
            version != 0
        };
        let len = if transformed {
            base128(&mut r).ok_or(bad("woff2 directory"))? as usize
        } else {
            orig_len
        };
        if tag == LOCA && transformed && len != 0 {
            return Err(bad("woff2 loca"));
        }
        entries.push(Entry {
            tag,
            transformed,
            len,
            offset,
        });
        offset = offset.checked_add(len).ok_or(bad("woff2 directory"))?;
        if offset > MAX_SIZE {
            return Err(bad("woff2 too large"));
        }
    }
    // Collection directory.
    let mut fonts: Vec<(u32, Vec<usize>)> = Vec::new();
    if flavor == 0x7474_6366 {
        let _version = r.u32().ok_or(bad("woff2 collection"))?;
        let nf = u255(&mut r).ok_or(bad("woff2 collection"))?;
        for _ in 0..nf {
            let nt = u255(&mut r).ok_or(bad("woff2 collection"))?;
            let fl = r.u32().ok_or(bad("woff2 collection"))?;
            let mut idx = Vec::with_capacity(nt as usize);
            for _ in 0..nt {
                let i = u255(&mut r).ok_or(bad("woff2 collection"))? as usize;
                if i >= n {
                    return Err(bad("woff2 collection"));
                }
                idx.push(i);
            }
            fonts.push((fl, idx));
        }
    } else {
        fonts.push((flavor, (0..n).collect()));
    }
    let start = r.pos();
    let comp = d
        .get(start..start + total_compressed)
        .ok_or(bad("woff2 truncated"))?;
    let stream =
        vnet::brotli::decompress_limited(comp, MAX_SIZE).map_err(|_| bad("woff2 brotli"))?;
    if stream.len() < offset {
        return Err(bad("woff2 stream too short"));
    }
    let raw = |e: &Entry| &stream[e.offset..e.offset + e.len];
    // Decoded tables, shared between the fonts of a collection.
    let mut decoded: Vec<Option<Vec<u8>>> = vec![None; n];
    for (_, idx) in &fonts {
        let find = |t: Tag| idx.iter().copied().find(|&i| entries[i].tag == t);
        for &i in idx {
            if decoded[i].is_some() {
                continue;
            }
            let e = &entries[i];
            if !e.transformed {
                decoded[i] = Some(raw(e).to_vec());
            }
        }
        if let Some(gi) = find(GLYF) {
            if entries[gi].transformed && decoded[gi].is_none() {
                let (glyf, loca) = reconstruct_glyf(raw(&entries[gi])).ok_or(bad("woff2 glyf"))?;
                decoded[gi] = Some(glyf);
                // The loca length in the directory is advisory: the rebuilt
                // table is used as is.
                if let Some(li) = find(LOCA) {
                    decoded[li] = Some(loca);
                }
            }
        }
        if let Some(hi) = find(HMTX) {
            if entries[hi].transformed && decoded[hi].is_none() {
                let get = |t: Tag| find(t).and_then(|i| decoded[i].clone());
                let hhea = get(Tag::new(b"hhea")).ok_or(bad("woff2 hmtx"))?;
                let maxp = get(Tag::new(b"maxp")).ok_or(bad("woff2 hmtx"))?;
                let glyf = get(GLYF).ok_or(bad("woff2 hmtx"))?;
                let loca = get(LOCA).ok_or(bad("woff2 hmtx"))?;
                let head = get(Tag::new(b"head")).ok_or(bad("woff2 hmtx"))?;
                let h = reconstruct_hmtx(raw(&entries[hi]), &hhea, &maxp, &head, &glyf, &loca)
                    .ok_or(bad("woff2 hmtx"))?;
                decoded[hi] = Some(h);
            }
        }
    }
    if fonts.len() == 1 && flavor != 0x7474_6366 {
        let mut tables: Vec<(Tag, Vec<u8>)> = fonts[0]
            .1
            .iter()
            .map(|&i| (entries[i].tag, decoded[i].take().unwrap_or_default()))
            .collect();
        return Ok(build_sfnt(fonts[0].0, &mut tables));
    }
    Ok(build_ttc(&fonts, &entries, &decoded))
}

fn build_ttc(
    fonts: &[(u32, Vec<usize>)],
    entries: &[Entry],
    decoded: &[Option<Vec<u8>>],
) -> Vec<u8> {
    let nf = fonts.len();
    let mut out = Vec::new();
    out.extend_from_slice(b"ttcf");
    out.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    out.extend_from_slice(&(nf as u32).to_be_bytes());
    let offs_at = out.len();
    out.resize(offs_at + 4 * nf, 0);
    let mut dirs = Vec::new();
    for (k, (flavor, idx)) in fonts.iter().enumerate() {
        let at = out.len();
        out[offs_at + 4 * k..offs_at + 4 * k + 4].copy_from_slice(&(at as u32).to_be_bytes());
        let mut sorted = idx.clone();
        sorted.sort_by_key(|&i| entries[i].tag);
        write_offset_table(&mut out, *flavor, sorted.len() as u16);
        let dir = out.len();
        out.resize(dir + 16 * sorted.len(), 0);
        dirs.push((dir, sorted));
    }
    let mut placed: Vec<Option<(usize, usize)>> = vec![None; entries.len()];
    for (dir, sorted) in dirs {
        for (j, &i) in sorted.iter().enumerate() {
            let (off, len) = match placed[i] {
                Some(p) => p,
                None => {
                    while out.len() % 4 != 0 {
                        out.push(0);
                    }
                    let off = out.len();
                    let data = decoded[i].as_deref().unwrap_or(&[]);
                    out.extend_from_slice(data);
                    placed[i] = Some((off, data.len()));
                    (off, data.len())
                }
            };
            let rec = dir + 16 * j;
            let data = &decoded[i].as_deref().unwrap_or(&[]);
            out[rec..rec + 4].copy_from_slice(&entries[i].tag.bytes());
            out[rec + 4..rec + 8].copy_from_slice(&checksum(data).to_be_bytes());
            out[rec + 8..rec + 12].copy_from_slice(&(off as u32).to_be_bytes());
            out[rec + 12..rec + 16].copy_from_slice(&(len as u32).to_be_bytes());
        }
    }
    while out.len() % 4 != 0 {
        out.push(0);
    }
    out
}

/// Rebuilds `glyf` and `loca` from the WOFF2 transformed `glyf` (§5.1).
fn reconstruct_glyf(t: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
    let mut r = Reader::new(t);
    r.skip(2)?;
    let option_flags = r.u16()?;
    let num_glyphs = r.u16()? as usize;
    let index_format = r.u16()?;
    let mut sizes = [0usize; 7];
    for s in sizes.iter_mut() {
        *s = r.u32()? as usize;
    }
    let mut at = r.pos();
    let mut streams: Vec<&[u8]> = Vec::with_capacity(7);
    for &s in &sizes {
        streams.push(t.get(at..at.checked_add(s)?)?);
        at += s;
    }
    let overlap = if option_flags & 1 != 0 {
        Some(t.get(at..at + num_glyphs.div_ceil(8))?)
    } else {
        None
    };
    let mut n_contours = Reader::new(streams[0]);
    let mut n_points = Reader::new(streams[1]);
    let mut flags = Reader::new(streams[2]);
    let mut glyphs = Reader::new(streams[3]);
    let mut composite = Reader::new(streams[4]);
    let bbox_bitmap_len = 4 * num_glyphs.div_ceil(32);
    let bbox_bitmap = streams[5].get(..bbox_bitmap_len)?;
    let mut bbox = Reader::at(streams[5], bbox_bitmap_len);
    let mut instructions = Reader::new(streams[6]);
    let mut glyf = Vec::new();
    let mut loca = Vec::with_capacity(num_glyphs + 1);
    let w16 = |v: &mut Vec<u8>, x: i32| v.extend_from_slice(&(x as u16).to_be_bytes());
    for g in 0..num_glyphs {
        loca.push(glyf.len());
        let nc = n_contours.i16()?;
        let has_bbox = bbox_bitmap[g >> 3] & (0x80 >> (g & 7)) != 0;
        if nc == 0 {
            if has_bbox {
                return None;
            }
            continue;
        }
        if nc < 0 {
            if !has_bbox {
                return None;
            }
            let start = composite.pos();
            let mut have_instructions = false;
            loop {
                let f = composite.u16()?;
                composite.skip(2 + component_extra(f))?;
                if f & WE_HAVE_INSTRUCTIONS != 0 {
                    have_instructions = true;
                }
                if f & MORE_COMPONENTS == 0 {
                    break;
                }
            }
            let comp = &streams[4][start..composite.pos()];
            w16(&mut glyf, -1);
            for _ in 0..4 {
                w16(&mut glyf, bbox.i16()? as i32);
            }
            glyf.extend_from_slice(comp);
            if have_instructions {
                let ilen = u255(&mut glyphs)? as usize;
                w16(&mut glyf, ilen as i32);
                glyf.extend_from_slice(instructions.bytes(ilen)?);
            }
        } else {
            let nc = nc as usize;
            let mut ends = Vec::with_capacity(nc);
            let mut total = 0usize;
            for _ in 0..nc {
                total += u255(&mut n_points)? as usize;
                if total == 0 {
                    return None;
                }
                ends.push(total - 1);
            }
            let mut pts: Vec<(i32, i32, bool)> = Vec::with_capacity(total);
            let (mut x, mut y) = (0i32, 0i32);
            for _ in 0..total {
                let flag = flags.u8()?;
                let on = flag >> 7 == 0;
                let f = (flag & 0x7f) as i32;
                let sign = |fl: i32, v: i32| if fl & 1 != 0 { v } else { -v };
                let (dx, dy);
                if f < 10 {
                    let b0 = glyphs.u8()? as i32;
                    dx = 0;
                    dy = sign(f, ((f & 14) << 7) + b0);
                } else if f < 20 {
                    let b0 = glyphs.u8()? as i32;
                    dx = sign(f, (((f - 10) & 14) << 7) + b0);
                    dy = 0;
                } else if f < 84 {
                    let b0 = f - 20;
                    let b1 = glyphs.u8()? as i32;
                    dx = sign(f, 1 + (b0 & 0x30) + (b1 >> 4));
                    dy = sign(f >> 1, 1 + ((b0 & 0x0c) << 2) + (b1 & 0x0f));
                } else if f < 120 {
                    let b0 = f - 84;
                    let b1 = glyphs.u8()? as i32;
                    let b2 = glyphs.u8()? as i32;
                    dx = sign(f, 1 + ((b0 / 12) << 8) + b1);
                    dy = sign(f >> 1, 1 + (((b0 % 12) >> 2) << 8) + b2);
                } else if f < 124 {
                    let b1 = glyphs.u8()? as i32;
                    let b2 = glyphs.u8()? as i32;
                    let b3 = glyphs.u8()? as i32;
                    dx = sign(f, (b1 << 4) + (b2 >> 4));
                    dy = sign(f >> 1, ((b2 & 0x0f) << 8) + b3);
                } else {
                    let b1 = glyphs.u8()? as i32;
                    let b2 = glyphs.u8()? as i32;
                    let b3 = glyphs.u8()? as i32;
                    let b4 = glyphs.u8()? as i32;
                    dx = sign(f, (b1 << 8) + b2);
                    dy = sign(f >> 1, (b3 << 8) + b4);
                }
                x += dx;
                y += dy;
                pts.push((x, y, on));
            }
            let ilen = u255(&mut glyphs)? as usize;
            let instr = instructions.bytes(ilen)?;
            let (bx0, by0, bx1, by1) = if has_bbox {
                (
                    bbox.i16()? as i32,
                    bbox.i16()? as i32,
                    bbox.i16()? as i32,
                    bbox.i16()? as i32,
                )
            } else {
                let xs = pts.iter().map(|p| p.0);
                let ys = pts.iter().map(|p| p.1);
                (
                    xs.clone().min().unwrap_or(0),
                    ys.clone().min().unwrap_or(0),
                    xs.max().unwrap_or(0),
                    ys.max().unwrap_or(0),
                )
            };
            w16(&mut glyf, nc as i32);
            for v in [bx0, by0, bx1, by1] {
                w16(&mut glyf, v);
            }
            for e in &ends {
                w16(&mut glyf, *e as i32);
            }
            w16(&mut glyf, ilen as i32);
            glyf.extend_from_slice(instr);
            let overlaps = overlap.is_some_and(|o| o[g >> 3] & (0x80 >> (g & 7)) != 0);
            for (k, p) in pts.iter().enumerate() {
                let mut f = if p.2 { 1u8 } else { 0 };
                if k == 0 && overlaps {
                    f |= 0x40;
                }
                glyf.push(f);
            }
            let (mut px, mut py) = (0i32, 0i32);
            for p in &pts {
                w16(&mut glyf, p.0 - px);
                px = p.0;
            }
            for p in &pts {
                w16(&mut glyf, p.1 - py);
                py = p.1;
            }
        }
        while glyf.len() % 4 != 0 {
            glyf.push(0);
        }
    }
    loca.push(glyf.len());
    let mut loca_bytes = Vec::new();
    for &o in &loca {
        if index_format == 0 {
            loca_bytes.extend_from_slice(&((o / 2) as u16).to_be_bytes());
        } else {
            loca_bytes.extend_from_slice(&(o as u32).to_be_bytes());
        }
    }
    if index_format == 0 && glyf.len() / 2 > 0xffff {
        return None;
    }
    Some((glyf, loca_bytes))
}

/// Rebuilds `hmtx` from its WOFF2 transform (§5.4).
fn reconstruct_hmtx(
    t: &[u8],
    hhea: &[u8],
    maxp: &[u8],
    head: &[u8],
    glyf: &[u8],
    loca: &[u8],
) -> Option<Vec<u8>> {
    let num_h = u16_at(hhea, 34)? as usize;
    let num_glyphs = u16_at(maxp, 4)? as usize;
    let long_loca = u16_at(head, 50)? != 0;
    let mut r = Reader::new(t);
    let flags = r.u8()?;
    let mut adv = Vec::with_capacity(num_h);
    for _ in 0..num_h {
        adv.push(r.u16()?);
    }
    let xmin = |g: usize| -> i16 {
        let (a, b) = if long_loca {
            (
                u32_at(loca, 4 * g).unwrap_or(0) as usize,
                u32_at(loca, 4 * g + 4).unwrap_or(0) as usize,
            )
        } else {
            (
                u16_at(loca, 2 * g).unwrap_or(0) as usize * 2,
                u16_at(loca, 2 * g + 2).unwrap_or(0) as usize * 2,
            )
        };
        if b <= a {
            0
        } else {
            u16_at(glyf, a + 2).unwrap_or(0) as i16
        }
    };
    let mut out = Vec::with_capacity(4 * num_h + 2 * num_glyphs.saturating_sub(num_h));
    for (g, a) in adv.iter().enumerate() {
        let lsb = if flags & 1 != 0 { xmin(g) } else { r.i16()? };
        out.extend_from_slice(&a.to_be_bytes());
        out.extend_from_slice(&lsb.to_be_bytes());
    }
    for g in num_h..num_glyphs {
        let lsb = if flags & 2 != 0 { xmin(g) } else { r.i16()? };
        out.extend_from_slice(&lsb.to_be_bytes());
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variable_length_integers() {
        assert_eq!(base128(&mut Reader::new(&[0x3f])), Some(63));
        assert_eq!(base128(&mut Reader::new(&[0x81, 0x00])), Some(128));
        assert_eq!(base128(&mut Reader::new(&[0x80, 0x01])), None);
        assert_eq!(u255(&mut Reader::new(&[252])), Some(252));
        assert_eq!(u255(&mut Reader::new(&[255, 0])), Some(253));
        assert_eq!(u255(&mut Reader::new(&[254, 0])), Some(506));
        assert_eq!(u255(&mut Reader::new(&[253, 0x01, 0x00])), Some(256));
    }

    #[test]
    fn sfnt_builder_sorts_and_aligns() {
        let mut t = vec![
            (Tag::new(b"post"), vec![1, 2, 3]),
            (Tag::new(b"head"), vec![9; 5]),
        ];
        let s = build_sfnt(0x0001_0000, &mut t);
        assert_eq!(u16_at(&s, 4), Some(2));
        assert_eq!(&s[12..16], b"head");
        let off = u32_at(&s, 12 + 8).unwrap() as usize;
        assert_eq!(off % 4, 0);
        assert_eq!(s.len() % 4, 0);
    }

    #[test]
    fn transformed_glyf_triplets() {
        // One glyph with one contour of two on-curve points: triplet flag 1
        // (dx = 0, dy = +b0) and flag 11 (dx = +b0, dy = 0).
        let mut t = Vec::new();
        t.extend_from_slice(&[0, 0, 0, 0, 0, 1, 0, 0]);
        let streams: [Vec<u8>; 7] = [
            vec![0, 1],       // nContours = 1
            vec![2],          // two points
            vec![1, 11],      // flags (on-curve)
            vec![10, 20, 0],  // dy=+10, dx=+20, instruction length 0
            vec![],           // composites
            vec![0, 0, 0, 0], // bbox bitmap (none)
            vec![],           // instructions
        ];
        for s in &streams {
            t.extend_from_slice(&(s.len() as u32).to_be_bytes());
        }
        for s in &streams {
            t.extend_from_slice(s);
        }
        let (glyf, loca) = reconstruct_glyf(&t).unwrap();
        assert_eq!(u16_at(&glyf, 0), Some(1));
        // bbox: x 0..20, y 10..10
        assert_eq!(u16_at(&glyf, 2), Some(0));
        assert_eq!(u16_at(&glyf, 4), Some(10));
        assert_eq!(u16_at(&glyf, 6), Some(20));
        assert_eq!(loca.len(), 4);
        let g = super::super::glyf::Glyf {
            glyf: &glyf,
            loca: &loca,
            long_loca: false,
            num_glyphs: 1,
        };
        let o = g.outline(0).unwrap();
        assert_eq!(o.control_bounds(), Some((0.0, 10.0, 20.0, 10.0)));
    }
}
