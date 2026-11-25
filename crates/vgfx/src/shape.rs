//! Text shaping for one face: `cmap` mapping (with variation sequences),
//! the `GSUB` features (ligatures, `font-variant-*` and
//! `font-feature-settings`), and kerning and mark attachment from `GPOS`,
//! or the legacy `kern` table when `GPOS` has no kerning.
//!
//! The text of a call is one run: one face, one direction (left to right)
//! and one script. Choosing the face per character (fallback) happens
//! before, in [`crate::font::system`].

use crate::font::Face;
use crate::font::ot::{self, Applier, Buffer, GlyphInfo};
use crate::font::read::Tag;

/// `font-kerning`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Kerning {
    #[default]
    Auto,
    Normal,
    None,
}

/// `font-variant-ligatures` as four switches (`normal` = the default).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ligatures {
    pub common: bool,
    pub discretionary: bool,
    pub historical: bool,
    pub contextual: bool,
}

impl Default for Ligatures {
    fn default() -> Ligatures {
        Ligatures {
            common: true,
            discretionary: false,
            historical: false,
            contextual: true,
        }
    }
}

impl Ligatures {
    /// `font-variant-ligatures: none`.
    pub const NONE: Ligatures = Ligatures {
        common: false,
        discretionary: false,
        historical: false,
        contextual: false,
    };
}

/// `font-variant-caps`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Caps {
    #[default]
    Normal,
    SmallCaps,
    AllSmallCaps,
    PetiteCaps,
    AllPetiteCaps,
    Unicase,
    TitlingCaps,
}

/// `font-variant-position`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Position {
    #[default]
    Normal,
    Sub,
    Super,
}

/// `font-variant-numeric` keywords.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Numeric {
    pub lining: bool,
    pub oldstyle: bool,
    pub proportional: bool,
    pub tabular: bool,
    pub diagonal_fractions: bool,
    pub stacked_fractions: bool,
    pub ordinal: bool,
    pub slashed_zero: bool,
}

/// `font-variant-east-asian` keywords.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EastAsian {
    pub jis78: bool,
    pub jis83: bool,
    pub jis90: bool,
    pub jis04: bool,
    pub simplified: bool,
    pub traditional: bool,
    pub full_width: bool,
    pub proportional_width: bool,
    pub ruby: bool,
}

/// The `font-variant-*` longhands that select OpenType features.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Variants {
    pub ligatures: Ligatures,
    pub caps: Caps,
    pub numeric: Numeric,
    pub east_asian: EastAsian,
    pub position: Position,
    /// `font-variant-alternates: historical-forms`.
    pub historical_forms: bool,
}

/// What a run is shaped with.
#[derive(Clone, Debug, PartialEq)]
pub struct ShapeOptions {
    /// OpenType script tag (`DFLT` when unknown; lookups fall back to
    /// `DFLT` and `latn`).
    pub script: Tag,
    /// OpenType language system tag, if the content language maps to one.
    pub language: Option<Tag>,
    pub variants: Variants,
    pub kerning: Kerning,
    /// `font-feature-settings`, in specified order (later wins).
    pub feature_settings: Vec<(Tag, u32)>,
}

impl Default for ShapeOptions {
    fn default() -> ShapeOptions {
        ShapeOptions {
            script: Tag::new(b"DFLT"),
            language: None,
            variants: Variants::default(),
            kerning: Kerning::Auto,
            feature_settings: Vec::new(),
        }
    }
}

/// One positioned glyph, in pixels (y down for offsets, as on screen).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShapedGlyph {
    pub glyph: u16,
    /// Byte offset in the shaped text of the first character of the
    /// cluster this glyph belongs to.
    pub cluster: u32,
    pub x_advance: f32,
    pub y_advance: f32,
    pub x_offset: f32,
    pub y_offset: f32,
}

/// The OpenType features of `opts`, in the order CSS Fonts 4 §7.2 applies
/// them: defaults, then `font-variant-*`, then `font-feature-settings`.
/// Later entries of the same tag win.
pub fn features(opts: &ShapeOptions) -> Vec<(Tag, u32)> {
    let mut f: Vec<(Tag, u32)> = Vec::new();
    let mut on = |tag: &[u8; 4], v: bool| f.push((Tag::new(tag), v as u32));
    // Features required for correct rendering (CSS Fonts 4 §7.1).
    for t in [b"ccmp", b"locl", b"rlig", b"mark", b"mkmk", b"rclt"] {
        on(t, true);
    }
    let v = &opts.variants;
    on(b"liga", v.ligatures.common);
    on(b"clig", v.ligatures.common);
    on(b"dlig", v.ligatures.discretionary);
    on(b"hlig", v.ligatures.historical);
    on(b"calt", v.ligatures.contextual);
    let kern = opts.kerning != Kerning::None;
    on(b"kern", kern);
    on(b"palt", false);
    match v.caps {
        Caps::Normal => {}
        Caps::SmallCaps => on(b"smcp", true),
        Caps::AllSmallCaps => {
            on(b"smcp", true);
            on(b"c2sc", true);
        }
        Caps::PetiteCaps => on(b"pcap", true),
        Caps::AllPetiteCaps => {
            on(b"pcap", true);
            on(b"c2pc", true);
        }
        Caps::Unicase => on(b"unic", true),
        Caps::TitlingCaps => on(b"titl", true),
    }
    let n = &v.numeric;
    for (set, tag) in [
        (n.lining, b"lnum"),
        (n.oldstyle, b"onum"),
        (n.proportional, b"pnum"),
        (n.tabular, b"tnum"),
        (n.diagonal_fractions, b"frac"),
        (n.stacked_fractions, b"afrc"),
        (n.ordinal, b"ordn"),
        (n.slashed_zero, b"zero"),
    ] {
        if set {
            on(tag, true);
        }
    }
    let e = &v.east_asian;
    for (set, tag) in [
        (e.jis78, b"jp78"),
        (e.jis83, b"jp83"),
        (e.jis90, b"jp90"),
        (e.jis04, b"jp04"),
        (e.simplified, b"smpl"),
        (e.traditional, b"trad"),
        (e.full_width, b"fwid"),
        (e.proportional_width, b"pwid"),
        (e.ruby, b"ruby"),
    ] {
        if set {
            on(tag, true);
        }
    }
    match v.position {
        Position::Normal => {}
        Position::Sub => on(b"subs", true),
        Position::Super => on(b"sups", true),
    }
    if v.historical_forms {
        on(b"hist", true);
    }
    f.extend(opts.feature_settings.iter().copied());
    f
}

fn is_variation_selector(c: char) -> bool {
    matches!(c as u32, 0xfe00..=0xfe0f | 0xe0100..=0xe01ef)
}

/// Default ignorable characters that are not drawn when the font has no
/// glyph for them (UAX #44 `Default_Ignorable_Code_Point`, the common
/// format controls).
fn is_ignorable(c: char) -> bool {
    matches!(
        c as u32,
        0xad | 0x34f
            | 0x61c
            | 0x115f..=0x1160
            | 0x17b4..=0x17b5
            | 0x180b..=0x180f
            | 0x200b..=0x200f
            | 0x202a..=0x202e
            | 0x2060..=0x206f
            | 0x3164
            | 0xfeff
            | 0xffa0
            | 0xfff0..=0xfff8
            | 0x1bca0..=0x1bca3
            | 0x1d173..=0x1d17a
            | 0xe0000..=0xe0fff
    ) || is_variation_selector(c)
}

/// Maps `text` to glyphs through `cmap`: a variation selector after a
/// character picks the variant glyph (format 14) and joins its cluster;
/// unmapped ignorables are dropped; other unmapped characters become
/// glyph 0 (`.notdef`).
pub fn map_glyphs(face: &Face, text: &str) -> Vec<GlyphInfo> {
    let mut out: Vec<GlyphInfo> = Vec::with_capacity(text.len());
    let mut last_cp: Option<u32> = None;
    for (at, c) in text.char_indices() {
        if is_variation_selector(c) {
            if let (Some(base), Some(prev)) = (last_cp, out.last_mut()) {
                if let Some(g) = face.glyph_variant(base, c as u32) {
                    prev.glyph = g;
                }
            }
            continue;
        }
        let g = face.glyph_index(c as u32);
        last_cp = Some(c as u32);
        match g {
            Some(g) => out.push(GlyphInfo {
                glyph: g,
                cluster: at as u32,
                ..GlyphInfo::default()
            }),
            None if is_ignorable(c) => {}
            None => out.push(GlyphInfo {
                glyph: 0,
                cluster: at as u32,
                ..GlyphInfo::default()
            }),
        }
    }
    out
}

/// Shapes `text` with `face` at `size` pixels per em.
pub fn shape(face: &Face, text: &str, size: f32, opts: &ShapeOptions) -> Vec<ShapedGlyph> {
    let gdef = face.gdef();
    let mut info = map_glyphs(face, text);
    for g in &mut info {
        gdef.classify(g);
    }
    let mut buf = Buffer::new(info);
    let feats = features(opts);
    if let Some(gsub) = face.gsub() {
        let lookups = ot::select_lookups(gsub, opts.script, opts.language, &feats);
        let mut a = Applier::new(gsub, gdef, true, &mut buf);
        for (index, value) in lookups {
            a.apply_lookup(index, value);
        }
    }
    // Positions start from the advances of `hmtx`; with `GPOS`, marks of
    // `GDEF` do not advance the pen (mark attachment places them from
    // their own pen position).
    let has_gpos = face.gpos().is_some();
    buf.pos.resize(buf.info.len(), ot::GlyphPos::default());
    for (p, i) in buf.pos.iter_mut().zip(&buf.info) {
        let zero = has_gpos && i.class == 3;
        *p = ot::GlyphPos {
            x_advance: if zero {
                0
            } else {
                face.advance(i.glyph) as i32
            },
            ..ot::GlyphPos::default()
        };
    }
    let kern_tag = Tag::new(b"kern");
    let mut gpos_kerned = false;
    if let Some(gpos) = face.gpos() {
        gpos_kerned = ot::feature_tags(gpos).contains(&kern_tag);
        let lookups = ot::select_lookups(gpos, opts.script, opts.language, &feats);
        let mut a = Applier::new(gpos, gdef, false, &mut buf);
        for (index, value) in lookups {
            a.apply_lookup(index, value);
        }
    }
    let kerning_on = feats
        .iter()
        .rev()
        .find(|f| f.0 == kern_tag)
        .is_none_or(|f| f.1 != 0);
    if kerning_on && !gpos_kerned {
        for i in 1..buf.info.len() {
            let k = face.kern_pair(buf.info[i - 1].glyph, buf.info[i].glyph);
            buf.pos[i - 1].x_advance += k;
        }
    }
    let scale = size / face.units_per_em() as f32;
    buf.info
        .iter()
        .zip(&buf.pos)
        .map(|(i, p)| ShapedGlyph {
            glyph: i.glyph,
            cluster: i.cluster,
            x_advance: p.x_advance as f32 * scale,
            y_advance: -(p.y_advance as f32) * scale,
            x_offset: p.x_offset as f32 * scale,
            y_offset: -(p.y_offset as f32) * scale,
        })
        .collect()
}

/// The total advance of shaped glyphs, in pixels.
pub fn advance_width(glyphs: &[ShapedGlyph]) -> f32 {
    glyphs.iter().map(|g| g.x_advance).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tag_value(f: &[(Tag, u32)], t: &[u8; 4]) -> Option<u32> {
        f.iter().rev().find(|e| e.0 == Tag::new(t)).map(|e| e.1)
    }

    #[test]
    fn default_features_enable_ligatures_and_kerning() {
        let f = features(&ShapeOptions::default());
        assert_eq!(tag_value(&f, b"liga"), Some(1));
        assert_eq!(tag_value(&f, b"kern"), Some(1));
        assert_eq!(tag_value(&f, b"dlig"), Some(0));
        assert_eq!(tag_value(&f, b"smcp"), None);
    }

    #[test]
    fn feature_settings_override_variants() {
        let opts = ShapeOptions {
            variants: Variants {
                ligatures: Ligatures::NONE,
                caps: Caps::AllSmallCaps,
                ..Variants::default()
            },
            kerning: Kerning::None,
            feature_settings: vec![(Tag::new(b"liga"), 1)],
            ..ShapeOptions::default()
        };
        let f = features(&opts);
        assert_eq!(tag_value(&f, b"liga"), Some(1));
        assert_eq!(tag_value(&f, b"clig"), Some(0));
        assert_eq!(tag_value(&f, b"kern"), Some(0));
        assert_eq!(tag_value(&f, b"c2sc"), Some(1));
    }

    #[test]
    fn ignorables_and_selectors() {
        assert!(is_ignorable('\u{200b}'));
        assert!(is_ignorable('\u{fe0f}'));
        assert!(!is_ignorable('a'));
    }
}
