//! OpenType fonts: the sfnt container and collections (`.ttc`), WOFF and
//! WOFF2, the metric and naming tables, TrueType and CFF/CFF2 outlines,
//! `cmap`, `kern` and the OpenType layout tables; the index of system
//! fonts and CSS font matching ([`system`], [`matching`]).

pub mod cff;
pub mod cmap;
pub mod glyf;
pub mod kern;
pub mod matching;
pub mod ot;
pub mod read;
pub mod system;
pub mod woff;

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::outline::{Outline, Transform};
use read::{Reader, Tag, i16_at, u16_at, u32_at};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FontError {
    /// The data breaks the format (the text says where).
    Malformed(&'static str),
    /// A valid format this decoder does not handle.
    Unsupported(&'static str),
    /// The face index is past the end of the collection.
    NoSuchFace,
}

impl fmt::Display for FontError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FontError::Malformed(m) => write!(f, "malformed font: {m}"),
            FontError::Unsupported(m) => write!(f, "unsupported font: {m}"),
            FontError::NoSuchFace => f.write_str("no such face in the collection"),
        }
    }
}

impl std::error::Error for FontError {}

/// Turns WOFF and WOFF2 data into sfnt (or collection) bytes; other data is
/// returned unchanged.
pub fn decode_container(bytes: Vec<u8>) -> Result<Vec<u8>, FontError> {
    match u32_at(&bytes, 0) {
        Some(0x774f_4646) => woff::decode_woff1(&bytes),
        Some(0x774f_4632) => woff::decode_woff2(&bytes),
        Some(_) => Ok(bytes),
        None => Err(FontError::Malformed("empty font data")),
    }
}

/// Number of faces in sfnt or collection bytes.
pub fn face_count(data: &[u8]) -> u32 {
    match u32_at(data, 0) {
        Some(0x7474_6366) => u32_at(data, 8).unwrap_or(0),
        Some(0x0001_0000) | Some(0x4f54_544f) | Some(0x7472_7565) => 1,
        _ => 0,
    }
}

/// Font-wide metrics in font units (descent positive, below the baseline).
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct FontMetrics {
    pub units_per_em: u16,
    pub ascent: i32,
    pub descent: i32,
    pub line_gap: i32,
    pub x_height: i32,
    pub cap_height: i32,
    pub underline_position: i32,
    pub underline_thickness: i32,
    pub strikeout_position: i32,
    pub strikeout_size: i32,
    /// Advance of U+0030 DIGIT ZERO ("ch" unit), if mapped.
    pub zero_advance: Option<i32>,
    /// Advance of U+6C34 (the "ic" unit), if mapped.
    pub ideograph_advance: Option<i32>,
    pub space_advance: i32,
}

/// [`FontMetrics`] scaled to a font size in pixels.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct ScaledMetrics {
    pub ascent: f32,
    pub descent: f32,
    pub line_gap: f32,
    pub x_height: f32,
    pub cap_height: f32,
    pub underline_position: f32,
    pub underline_thickness: f32,
    pub strikeout_position: f32,
    pub strikeout_size: f32,
    pub zero_advance: Option<f32>,
    pub ideograph_advance: Option<f32>,
    pub space_advance: f32,
}

impl FontMetrics {
    pub fn scaled(&self, size: f32) -> ScaledMetrics {
        let s = size / self.units_per_em.max(1) as f32;
        ScaledMetrics {
            ascent: self.ascent as f32 * s,
            descent: self.descent as f32 * s,
            line_gap: self.line_gap as f32 * s,
            x_height: self.x_height as f32 * s,
            cap_height: self.cap_height as f32 * s,
            underline_position: self.underline_position as f32 * s,
            underline_thickness: self.underline_thickness as f32 * s,
            strikeout_position: self.strikeout_position as f32 * s,
            strikeout_size: self.strikeout_size as f32 * s,
            zero_advance: self.zero_advance.map(|v| v as f32 * s),
            ideograph_advance: self.ideograph_advance.map(|v| v as f32 * s),
            space_advance: self.space_advance as f32 * s,
        }
    }
}

/// Names from the `name` table.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Names {
    pub family: Option<String>,
    pub subfamily: Option<String>,
    pub full: Option<String>,
    pub postscript: Option<String>,
    pub typographic_family: Option<String>,
    pub typographic_subfamily: Option<String>,
}

impl Names {
    /// The family name CSS matches against (typographic family first).
    pub fn css_family(&self) -> Option<&str> {
        self.typographic_family
            .as_deref()
            .or(self.family.as_deref())
    }
}

#[derive(Clone, Copy, Debug)]
enum Outlines {
    None,
    Glyf {
        glyf: (usize, usize),
        loca: (usize, usize),
    },
    Cff {
        table: (usize, usize),
        cff2: bool,
    },
}

static NEXT_FACE_ID: AtomicU64 = AtomicU64::new(1);

/// One font face, sharing its bytes with the other faces of its file.
pub struct Face {
    data: Arc<Vec<u8>>,
    /// Unique per process; keys the glyph cache.
    pub id: u64,
    pub index: u32,
    tables: Vec<(Tag, usize, usize)>,
    units_per_em: u16,
    num_glyphs: u16,
    long_loca: bool,
    num_h_metrics: u16,
    hmtx: (usize, usize),
    cmap_at: (usize, usize),
    cmap: cmap::Cmap,
    outlines: Outlines,
    cff: Option<cff::Cff>,
    metrics: FontMetrics,
    names: Names,
    weight: u16,
    width: u16,
    italic: bool,
    oblique: bool,
    monospace: bool,
    bbox: (i16, i16, i16, i16),
}

impl fmt::Debug for Face {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Face")
            .field("id", &self.id)
            .field("family", &self.names.css_family())
            .field("weight", &self.weight)
            .field("italic", &self.italic)
            .finish()
    }
}

const HEAD: Tag = Tag::new(b"head");
const HHEA: Tag = Tag::new(b"hhea");
const HMTX: Tag = Tag::new(b"hmtx");
const MAXP: Tag = Tag::new(b"maxp");
const CMAP: Tag = Tag::new(b"cmap");
const OS2: Tag = Tag::new(b"OS/2");
const POST: Tag = Tag::new(b"post");
const NAME: Tag = Tag::new(b"name");

impl Face {
    /// Parses face `index` of font file bytes (sfnt, collection, WOFF or WOFF2).
    pub fn parse(bytes: Vec<u8>, index: u32) -> Result<Face, FontError> {
        let data = decode_container(bytes)?;
        Face::from_shared(Arc::new(data), index)
    }

    /// Parses face `index` of sfnt or collection bytes already decoded.
    pub fn from_shared(data: Arc<Vec<u8>>, index: u32) -> Result<Face, FontError> {
        let d: &[u8] = &data;
        let start = match u32_at(d, 0) {
            Some(0x7474_6366) => {
                let n = u32_at(d, 8).ok_or(FontError::Malformed("ttc header"))?;
                if index >= n {
                    return Err(FontError::NoSuchFace);
                }
                u32_at(d, 12 + 4 * index as usize).ok_or(FontError::Malformed("ttc header"))?
                    as usize
            }
            Some(0x0001_0000) | Some(0x4f54_544f) | Some(0x7472_7565) => {
                if index != 0 {
                    return Err(FontError::NoSuchFace);
                }
                0
            }
            _ => return Err(FontError::Unsupported("not an OpenType font")),
        };
        let n = u16_at(d, start + 4).ok_or(FontError::Malformed("offset table"))? as usize;
        let mut tables = Vec::with_capacity(n);
        for i in 0..n {
            let rec = start + 12 + 16 * i;
            let mut r = Reader::at(d, rec);
            let tag = r.tag().ok_or(FontError::Malformed("table directory"))?;
            r.skip(4);
            let off = r.u32().ok_or(FontError::Malformed("table directory"))? as usize;
            let len = r.u32().ok_or(FontError::Malformed("table directory"))? as usize;
            if off.checked_add(len).is_none_or(|e| e > d.len()) {
                return Err(FontError::Malformed("table out of bounds"));
            }
            tables.push((tag, off, len));
        }
        let find = |t: Tag| tables.iter().find(|e| e.0 == t).map(|e| (e.1, e.2));
        let get = |t: Tag| find(t).map(|(o, l)| &d[o..o + l]);
        let head = get(HEAD).ok_or(FontError::Malformed("no head table"))?;
        let units_per_em = u16_at(head, 18).ok_or(FontError::Malformed("head"))?;
        if !(16..=16384).contains(&units_per_em) {
            return Err(FontError::Malformed("unitsPerEm"));
        }
        let bbox = (
            i16_at(head, 36).unwrap_or(0),
            i16_at(head, 38).unwrap_or(0),
            i16_at(head, 40).unwrap_or(0),
            i16_at(head, 42).unwrap_or(0),
        );
        let mac_style = u16_at(head, 44).unwrap_or(0);
        let long_loca = i16_at(head, 50).unwrap_or(0) != 0;
        let maxp = get(MAXP).ok_or(FontError::Malformed("no maxp table"))?;
        let num_glyphs = u16_at(maxp, 4).ok_or(FontError::Malformed("maxp"))?;
        let hhea = get(HHEA).ok_or(FontError::Malformed("no hhea table"))?;
        let num_h_metrics = u16_at(hhea, 34).ok_or(FontError::Malformed("hhea"))?;
        let hmtx = find(HMTX).unwrap_or((0, 0));
        let cmap_at = find(CMAP).unwrap_or((0, 0));
        let cmap = cmap::Cmap::parse(&d[cmap_at.0..cmap_at.0 + cmap_at.1]);
        let (outlines, cff) =
            if let (Some(g), Some(l)) = (find(Tag::new(b"glyf")), find(Tag::new(b"loca"))) {
                (Outlines::Glyf { glyf: g, loca: l }, None)
            } else if let Some(c) = find(Tag::new(b"CFF2")) {
                let p = cff::Cff::parse(&d[c.0..c.0 + c.1], true);
                (
                    Outlines::Cff {
                        table: c,
                        cff2: true,
                    },
                    p,
                )
            } else if let Some(c) = find(Tag::new(b"CFF ")) {
                let p = cff::Cff::parse(&d[c.0..c.0 + c.1], false);
                (
                    Outlines::Cff {
                        table: c,
                        cff2: false,
                    },
                    p,
                )
            } else {
                (Outlines::None, None)
            };
        let names = get(NAME).map(parse_names).unwrap_or_default();
        let os2 = get(OS2);
        let post = get(POST);
        let fs_selection = os2.and_then(|o| u16_at(o, 62)).unwrap_or(0);
        let weight = os2
            .and_then(|o| u16_at(o, 4))
            .filter(|w| (1..=1000).contains(w))
            .unwrap_or(if mac_style & 1 != 0 { 700 } else { 400 });
        let width = os2
            .and_then(|o| u16_at(o, 6))
            .filter(|w| (1..=9).contains(w))
            .unwrap_or(5);
        let italic = if os2.is_some() {
            fs_selection & 1 != 0
        } else {
            mac_style & 2 != 0
        };
        let oblique = fs_selection & (1 << 9) != 0;
        let monospace = post.and_then(|p| u32_at(p, 12)).unwrap_or(0) != 0;
        let mut face = Face {
            data: data.clone(),
            id: NEXT_FACE_ID.fetch_add(1, Ordering::Relaxed),
            index,
            tables: tables.clone(),
            units_per_em,
            num_glyphs,
            long_loca,
            num_h_metrics,
            hmtx,
            cmap_at,
            cmap,
            outlines,
            cff,
            metrics: FontMetrics::default(),
            names,
            weight,
            width,
            italic,
            oblique,
            monospace,
            bbox,
        };
        face.metrics = face.compute_metrics(hhea, os2, post, fs_selection);
        Ok(face)
    }

    fn compute_metrics(
        &self,
        hhea: &[u8],
        os2: Option<&[u8]>,
        post: Option<&[u8]>,
        fs_selection: u16,
    ) -> FontMetrics {
        let upem = self.units_per_em as i32;
        let h_asc = i16_at(hhea, 4).unwrap_or(0) as i32;
        let h_desc = i16_at(hhea, 6).unwrap_or(0) as i32;
        let h_gap = i16_at(hhea, 8).unwrap_or(0) as i32;
        let typo = os2.map(|o| {
            (
                i16_at(o, 68).unwrap_or(0) as i32,
                i16_at(o, 70).unwrap_or(0) as i32,
                i16_at(o, 72).unwrap_or(0) as i32,
            )
        });
        let win = os2.map(|o| {
            (
                u16_at(o, 74).unwrap_or(0) as i32,
                u16_at(o, 76).unwrap_or(0) as i32,
            )
        });
        let (ascent, descent, line_gap) = match (typo, win) {
            (Some((a, d, g)), _) if fs_selection & 0x80 != 0 => (a, -d, g),
            _ if h_asc != 0 || h_desc != 0 => (h_asc, -h_desc, h_gap),
            (Some((a, d, g)), _) if a != 0 || d != 0 => (a, -d, g),
            (_, Some((a, d))) if a != 0 || d != 0 => (a, d, 0),
            _ => (upem * 4 / 5, upem / 5, 0),
        };
        let version = os2.and_then(|o| u16_at(o, 0)).unwrap_or(0);
        let glyph_top = |cp: u32| -> Option<i32> {
            let g = self.glyph_index(cp)?;
            let b = self.outline(g)?.control_bounds()?;
            Some(b.3.round() as i32)
        };
        let x_height = os2
            .filter(|_| version >= 2)
            .and_then(|o| i16_at(o, 86))
            .map(|v| v as i32)
            .filter(|&v| v > 0)
            .or_else(|| glyph_top('x' as u32))
            .unwrap_or(upem / 2);
        let cap_height = os2
            .filter(|_| version >= 2)
            .and_then(|o| i16_at(o, 88))
            .map(|v| v as i32)
            .filter(|&v| v > 0)
            .or_else(|| glyph_top('H' as u32))
            .unwrap_or(ascent * 7 / 10);
        let underline_position = post
            .and_then(|p| i16_at(p, 8))
            .map(|v| v as i32)
            .unwrap_or(-upem / 10);
        let underline_thickness = post
            .and_then(|p| i16_at(p, 10))
            .map(|v| v as i32)
            .filter(|&v| v > 0)
            .unwrap_or(upem / 20);
        let strikeout_size = os2
            .and_then(|o| i16_at(o, 26))
            .map(|v| v as i32)
            .filter(|&v| v > 0)
            .unwrap_or(underline_thickness);
        let strikeout_position = os2
            .and_then(|o| i16_at(o, 28))
            .map(|v| v as i32)
            .filter(|&v| v > 0)
            .unwrap_or(x_height / 2);
        let adv = |cp: u32| self.glyph_index(cp).map(|g| self.advance(g) as i32);
        FontMetrics {
            units_per_em: self.units_per_em,
            ascent,
            descent,
            line_gap,
            x_height,
            cap_height,
            underline_position,
            underline_thickness,
            strikeout_position,
            strikeout_size,
            zero_advance: adv('0' as u32),
            ideograph_advance: adv(0x6c34),
            space_advance: adv(' ' as u32).unwrap_or(upem / 4),
        }
    }

    pub fn data(&self) -> &Arc<Vec<u8>> {
        &self.data
    }

    pub fn table(&self, tag: Tag) -> Option<&[u8]> {
        let e = self.tables.iter().find(|e| e.0 == tag)?;
        self.data.get(e.1..e.1 + e.2)
    }

    pub fn has_table(&self, tag: Tag) -> bool {
        self.tables.iter().any(|e| e.0 == tag)
    }

    pub fn units_per_em(&self) -> u16 {
        self.units_per_em
    }

    pub fn num_glyphs(&self) -> u16 {
        self.num_glyphs
    }

    pub fn metrics(&self) -> &FontMetrics {
        &self.metrics
    }

    pub fn names(&self) -> &Names {
        &self.names
    }

    /// `usWeightClass` (100–900), or 400/700 from `macStyle`.
    pub fn weight(&self) -> u16 {
        self.weight
    }

    /// `usWidthClass` (1 ultra-condensed … 5 normal … 9 ultra-expanded).
    pub fn width_class(&self) -> u16 {
        self.width
    }

    /// The CSS `font-stretch` percentage of the width class.
    pub fn stretch_percent(&self) -> f32 {
        [50.0, 62.5, 75.0, 87.5, 100.0, 112.5, 125.0, 150.0, 200.0]
            [(self.width.clamp(1, 9) - 1) as usize]
    }

    pub fn is_italic(&self) -> bool {
        self.italic
    }

    pub fn is_oblique(&self) -> bool {
        self.oblique
    }

    pub fn is_monospace(&self) -> bool {
        self.monospace
    }

    /// The font bounding box from `head` (xmin, ymin, xmax, ymax).
    pub fn bbox(&self) -> (i16, i16, i16, i16) {
        self.bbox
    }

    fn cmap_data(&self) -> &[u8] {
        &self.data[self.cmap_at.0..self.cmap_at.0 + self.cmap_at.1]
    }

    pub fn glyph_index(&self, cp: u32) -> Option<u16> {
        self.cmap.glyph(self.cmap_data(), cp)
    }

    pub fn has_char(&self, cp: u32) -> bool {
        self.glyph_index(cp).is_some()
    }

    /// The glyph of a variation sequence (format 14), if the font has one.
    pub fn glyph_variant(&self, cp: u32, selector: u32) -> Option<u16> {
        match self.cmap.variant(self.cmap_data(), cp, selector)? {
            cmap::Variant::UseDefault => self.glyph_index(cp),
            cmap::Variant::Glyph(g) => Some(g),
        }
    }

    /// Calls `f` with every character the font maps.
    pub fn for_each_char(&self, f: impl FnMut(u32, u16)) {
        self.cmap.for_each(self.cmap_data(), f);
    }

    /// Horizontal advance in font units.
    pub fn advance(&self, gid: u16) -> u16 {
        let h = &self.data[self.hmtx.0..self.hmtx.0 + self.hmtx.1];
        let n = self.num_h_metrics as usize;
        if n == 0 {
            return 0;
        }
        let i = (gid as usize).min(n - 1);
        u16_at(h, 4 * i).unwrap_or(0)
    }

    /// Left side bearing in font units.
    pub fn left_side_bearing(&self, gid: u16) -> i16 {
        let h = &self.data[self.hmtx.0..self.hmtx.0 + self.hmtx.1];
        let n = self.num_h_metrics as usize;
        let g = gid as usize;
        if g < n {
            i16_at(h, 4 * g + 2).unwrap_or(0)
        } else {
            i16_at(h, 4 * n + 2 * (g - n)).unwrap_or(0)
        }
    }

    fn glyf(&self) -> Option<glyf::Glyf<'_>> {
        let Outlines::Glyf { glyf, loca } = self.outlines else {
            return None;
        };
        Some(glyf::Glyf {
            glyf: &self.data[glyf.0..glyf.0 + glyf.1],
            loca: &self.data[loca.0..loca.0 + loca.1],
            long_loca: self.long_loca,
            num_glyphs: self.num_glyphs,
        })
    }

    /// The outline of a glyph in font units (y up); `None` if the glyph
    /// does not exist or its data is broken, an empty outline for blanks.
    pub fn outline(&self, gid: u16) -> Option<Outline> {
        if gid >= self.num_glyphs {
            return None;
        }
        match self.outlines {
            Outlines::Glyf { .. } => self.glyf()?.outline(gid),
            Outlines::Cff { table, .. } => {
                let cff = self.cff.as_ref()?;
                let d = &self.data[table.0..table.0 + table.1];
                let o = cff.outline(d, gid)?;
                match cff.font_matrix {
                    Some(m)
                        if (m[0] * self.units_per_em as f32 - 1.0).abs() > 1e-4
                            || m[1] != 0.0
                            || m[2] != 0.0 =>
                    {
                        let u = self.units_per_em as f32;
                        let t = Transform {
                            a: m[0] * u,
                            b: m[1] * u,
                            c: m[2] * u,
                            d: m[3] * u,
                            e: m[4] * u,
                            f: m[5] * u,
                        };
                        Some(o.transformed(&t))
                    }
                    _ => Some(o),
                }
            }
            Outlines::None => None,
        }
    }

    /// Number of components of a composite TrueType glyph (0 otherwise).
    pub fn component_count(&self, gid: u16) -> usize {
        self.glyf().map(|g| g.component_count(gid)).unwrap_or(0)
    }

    pub fn is_cff(&self) -> bool {
        matches!(self.outlines, Outlines::Cff { .. })
    }

    /// Kerning from the legacy `kern` table, in font units.
    pub fn kern_pair(&self, left: u16, right: u16) -> i32 {
        self.table(Tag::new(b"kern"))
            .map(|k| kern::pair(k, left, right))
            .unwrap_or(0)
    }

    pub fn gdef(&self) -> ot::Gdef<'_> {
        self.table(Tag::new(b"GDEF"))
            .map(ot::Gdef::parse)
            .unwrap_or_default()
    }

    pub fn gsub(&self) -> Option<&[u8]> {
        self.table(Tag::new(b"GSUB"))
    }

    pub fn gpos(&self) -> Option<&[u8]> {
        self.table(Tag::new(b"GPOS"))
    }

    /// The glyph name from `post` (version 2.0) or derived for 1.0.
    pub fn glyph_name(&self, gid: u16) -> Option<String> {
        let post = self.table(POST)?;
        let version = u32_at(post, 0)?;
        let standard = |i: usize| MAC_GLYPH_NAMES.split(' ').nth(i).map(str::to_string);
        match version {
            0x0001_0000 => standard(gid as usize).filter(|_| gid < 258),
            0x0002_0000 => {
                let n = u16_at(post, 32)?;
                if gid >= n {
                    return None;
                }
                let idx = u16_at(post, 34 + 2 * gid as usize)? as usize;
                if idx < 258 {
                    return standard(idx);
                }
                let mut p = 34 + 2 * n as usize;
                for _ in 0..idx - 258 {
                    p += 1 + *post.get(p)? as usize;
                }
                let len = *post.get(p)? as usize;
                post.get(p + 1..p + 1 + len)
                    .map(|b| String::from_utf8_lossy(b).into_owned())
            }
            _ => None,
        }
    }
}

fn parse_names(d: &[u8]) -> Names {
    let count = u16_at(d, 2).unwrap_or(0) as usize;
    let storage = u16_at(d, 4).unwrap_or(0) as usize;
    // Best (score, value) per name id.
    let mut best: [(u8, Option<String>); 18] = Default::default();
    for i in 0..count {
        let rec = 6 + 12 * i;
        let (Some(platform), Some(encoding), Some(lang), Some(id), Some(len), Some(off)) = (
            u16_at(d, rec),
            u16_at(d, rec + 2),
            u16_at(d, rec + 4),
            u16_at(d, rec + 6),
            u16_at(d, rec + 8),
            u16_at(d, rec + 10),
        ) else {
            break;
        };
        let id = id as usize;
        if id >= best.len() || !matches!(id, 1 | 2 | 4 | 6 | 16 | 17) {
            continue;
        }
        let score = match (platform, encoding, lang) {
            (3, 1 | 10, 0x409) => 5,
            (3, 1 | 10, _) => 4,
            (0, _, _) => 3,
            (1, 0, 0) => 2,
            (3, 0, _) => 1,
            _ => 0,
        };
        if score == 0 || score <= best[id].0 {
            continue;
        }
        let start = storage + off as usize;
        let Some(raw) = d.get(start..start + len as usize) else {
            continue;
        };
        let s = if platform == 1 {
            raw.iter().map(|&b| mac_roman(b)).collect()
        } else {
            let units: Vec<u16> = raw
                .chunks_exact(2)
                .map(|c| u16::from_be_bytes([c[0], c[1]]))
                .collect();
            String::from_utf16_lossy(&units)
        };
        best[id] = (score, Some(s));
    }
    let mut take = |i: usize| best[i].1.take();
    Names {
        family: take(1),
        subfamily: take(2),
        full: take(4),
        postscript: take(6),
        typographic_family: take(16),
        typographic_subfamily: take(17),
    }
}

/// Mac Roman to Unicode (the ASCII half exactly; the upper half through
/// the table of the encoding).
fn mac_roman(b: u8) -> char {
    const HIGH: &str = "ÄÅÇÉÑÖÜáàâäãåçéèêëíìîïñóòôöõúùûü†°¢£§•¶ß®©™´¨≠ÆØ∞±≤≥¥µ∂∑∏π∫ªºΩæø¿¡¬√ƒ≈∆«»…\u{a0}ÀÃÕŒœ–—“”‘’÷◊ÿŸ⁄€‹›ﬁﬂ‡·‚„‰ÂÊÁËÈÍÎÏÌÓÔÒÚÛÙıˆ˜¯˘˙˚¸˝˛ˇ";
    if b < 0x80 {
        b as char
    } else {
        HIGH.chars().nth((b - 0x80) as usize).unwrap_or('\u{fffd}')
    }
}

const MAC_GLYPH_NAMES: &str = concat!(
    ".notdef .null nonmarkingreturn space exclam quotedbl numbersign dollar percent ampersand ",
    "quotesingle parenleft parenright asterisk plus comma hyphen period slash zero one two ",
    "three four five six seven eight nine colon semicolon less equal greater question at A B C ",
    "D E F G H I J K L M N O P Q R S T U V W X Y Z bracketleft backslash bracketright ",
    "asciicircum underscore grave a b c d e f g h i j k l m n o p q r s t u v w x y z ",
    "braceleft bar braceright asciitilde Adieresis Aring Ccedilla Eacute Ntilde Odieresis ",
    "Udieresis aacute agrave acircumflex adieresis atilde aring ccedilla eacute egrave ",
    "ecircumflex edieresis iacute igrave icircumflex idieresis ntilde oacute ograve ",
    "ocircumflex odieresis otilde uacute ugrave ucircumflex udieresis dagger degree cent ",
    "sterling section bullet paragraph germandbls registered copyright trademark acute ",
    "dieresis notequal AE Oslash infinity plusminus lessequal greaterequal yen mu partialdiff ",
    "summation product pi integral ordfeminine ordmasculine Omega ae oslash questiondown ",
    "exclamdown logicalnot radical florin approxequal Delta guillemotleft guillemotright ",
    "ellipsis nonbreakingspace Agrave Atilde Otilde OE oe endash emdash quotedblleft ",
    "quotedblright quoteleft quoteright divide lozenge ydieresis Ydieresis fraction currency ",
    "guilsinglleft guilsinglright fi fl daggerdbl periodcentered quotesinglbase quotedblbase ",
    "perthousand Acircumflex Ecircumflex Aacute Edieresis Egrave Iacute Icircumflex Idieresis ",
    "Igrave Oacute Ocircumflex apple Ograve Uacute Ucircumflex Ugrave dotlessi circumflex ",
    "tilde macron breve dotaccent ring cedilla hungarumlaut ogonek caron Lslash lslash Scaron ",
    "scaron Zcaron zcaron brokenbar Eth eth Yacute yacute Thorn thorn minus multiply ",
    "onesuperior twosuperior threesuperior onehalf onequarter threequarters franc Gbreve ",
    "gbreve Idotaccent Scedilla scedilla Cacute cacute Ccaron ccaron dcroat ",
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_glyph_names() {
        assert_eq!(
            MAC_GLYPH_NAMES.split(' ').filter(|s| !s.is_empty()).count(),
            258
        );
        assert_eq!(MAC_GLYPH_NAMES.split(' ').nth(36), Some("A"));
    }

    #[test]
    fn mac_roman_upper_half() {
        assert_eq!(mac_roman(0x80), 'Ä');
        assert_eq!(mac_roman(0xff), 'ˇ');
        assert_eq!(mac_roman(b'A'), 'A');
    }

    #[test]
    fn rejects_non_fonts() {
        assert!(Face::parse(b"not a font at all".to_vec(), 0).is_err());
        assert_eq!(face_count(b"ttcf\0\x01\0\0\0\0\0\x02"), 2);
    }
}
