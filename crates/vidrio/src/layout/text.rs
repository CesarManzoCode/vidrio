//! Text for layout: the font of each character (the document's web fonts
//! first, then the system faces of `font-family` and the Noto fallback of
//! D-15), the metrics of the primary font, shaping by runs of one face
//! and one direction (with the mirrored glyphs of right-to-left runs),
//! and `text-transform` (css-text-3 §2.1, with `vjs::unicode`).

use std::collections::HashMap;
use std::sync::Arc;

use vgfx::font::Face;
use vgfx::font::matching::{Request, Style};
use vgfx::font::system::{Family, FontIndex, Generic, Selected, WebFace};
use vgfx::shape::{self, Caps, Kerning, Ligatures, ShapeOptions, ShapedGlyph};

use crate::css::style::ComputedStyle;
use crate::css::values::Value;

use super::bidi;
use super::values::{kw, lp};

/// The metrics of a style's primary font, in pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Metrics {
    pub size: f64,
    pub ascent: f64,
    pub descent: f64,
    pub line_gap: f64,
    pub x_height: f64,
    /// The advance of U+0020.
    pub space: f64,
    /// The advance of `0` (the `ch` unit and form control widths).
    pub zero: f64,
}

impl Metrics {
    /// The metrics used when no font is available at all.
    fn fallback(size: f64) -> Metrics {
        Metrics {
            size,
            ascent: 0.8 * size,
            descent: 0.2 * size,
            line_gap: 0.0,
            x_height: 0.5 * size,
            space: 0.25 * size,
            zero: 0.5 * size,
        }
    }

    /// The `normal` line height: ascent, descent and line gap.
    pub fn normal_line_height(&self) -> f64 {
        self.ascent + self.descent + self.line_gap
    }
}

/// A run of glyphs of one face.
#[derive(Clone, Debug)]
pub(crate) struct Chunk {
    pub face: Option<Arc<Face>>,
    /// The byte range of the run in the shaped text.
    pub start: usize,
    pub end: usize,
    /// The glyphs; `cluster` is a byte offset in the shaped text.
    pub glyphs: Vec<ShapedGlyph>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct FontKey {
    families: Vec<Family>,
    weight: u32,
    stretch: u32,
    style: (u8, u32),
}

/// What a computed style asks of fonts.
#[derive(Clone, Debug)]
pub(crate) struct FontSpec {
    key: FontKey,
    req: Request,
    pub size: f64,
}

/// The `font-family` list of a computed style.
fn families(style: &ComputedStyle) -> Vec<Family> {
    let mut out = Vec::new();
    let Some(v) = style.get("font-family") else {
        return vec![Family::Generic(Generic::Serif)];
    };
    match v.values.as_deref() {
        Some(values) => {
            let mut words: Vec<String> = Vec::new();
            let flush = |words: &mut Vec<String>, out: &mut Vec<Family>| {
                if !words.is_empty() {
                    out.push(Family::Named(words.join(" ")));
                    words.clear();
                }
            };
            for x in values {
                match x {
                    Value::String(s) => {
                        flush(&mut words, &mut out);
                        out.push(Family::Named(s.clone()));
                    }
                    Value::Keyword(k) => {
                        flush(&mut words, &mut out);
                        match Generic::from_keyword(k) {
                            Some(g) => out.push(Family::Generic(g)),
                            None => out.push(Family::Named(k.clone())),
                        }
                    }
                    Value::Ident(s) => words.push(s.clone()),
                    Value::Comma => flush(&mut words, &mut out),
                    _ => {}
                }
            }
            flush(&mut words, &mut out);
        }
        None => {
            for part in v.text.split(',') {
                let name = part.trim().trim_matches(|c| c == '"' || c == '\'');
                if name.is_empty() {
                    continue;
                }
                match Generic::from_keyword(name) {
                    Some(g) if !part.trim().starts_with(['"', '\'']) => {
                        out.push(Family::Generic(g))
                    }
                    _ => out.push(Family::Named(name.to_string())),
                }
            }
        }
    }
    if out.is_empty() {
        out.push(Family::Generic(Generic::Serif));
    }
    out
}

pub(crate) fn font_spec(style: &ComputedStyle) -> FontSpec {
    let fstyle = match kw(style, "font-style").as_str() {
        "italic" => Style::Italic,
        s if s.starts_with("oblique") => {
            let angle = s
                .split_ascii_whitespace()
                .nth(1)
                .and_then(|a| a.trim_end_matches("deg").parse::<f32>().ok())
                .unwrap_or(14.0);
            Style::Oblique(angle)
        }
        _ => Style::Normal,
    };
    let stretch = match style.get("font-stretch").and_then(|v| v.values.clone()) {
        Some(v) => match v.as_slice() {
            [Value::Percentage(p)] => *p as f32,
            _ => 100.0,
        },
        None => 100.0,
    };
    let req = Request {
        weight: style.font_weight as f32,
        stretch,
        style: fstyle,
    };
    let style_key = match fstyle {
        Style::Normal => (0, 0),
        Style::Italic => (1, 0),
        Style::Oblique(a) => (2, a.to_bits()),
    };
    FontSpec {
        key: FontKey {
            families: families(style),
            weight: req.weight.to_bits(),
            stretch: stretch.to_bits(),
            style: style_key,
        },
        req,
        size: style.font_size,
    }
}

/// Fonts during one layout pass.
pub(crate) struct FontContext {
    web: Vec<WebFace>,
    index: Option<&'static FontIndex>,
    selected: HashMap<(FontKey, u32), Option<Arc<Face>>>,
    primary: HashMap<FontKey, Option<Arc<Face>>>,
}

impl FontContext {
    pub fn new(web: Vec<WebFace>, system_fonts: bool) -> FontContext {
        FontContext {
            web,
            index: system_fonts.then(FontIndex::system),
            selected: HashMap::new(),
            primary: HashMap::new(),
        }
    }

    fn select(&mut self, spec: &FontSpec, cp: u32) -> Option<Arc<Face>> {
        let key = (spec.key.clone(), cp);
        if let Some(f) = self.selected.get(&key) {
            return f.clone();
        }
        let face = self.select_uncached(spec, cp);
        self.selected.insert(key, face.clone());
        face
    }

    fn select_uncached(&self, spec: &FontSpec, cp: u32) -> Option<Arc<Face>> {
        let families = &spec.key.families;
        match self.index {
            Some(index) => match index.select_with(&self.web, families, &spec.req, cp)? {
                Selected::Web(f) => Some(f),
                s @ Selected::System(_) => s.face(index),
            },
            None => {
                // Web fonts only.
                for fam in families {
                    if let Family::Named(name) = fam {
                        let f = self.web.iter().rev().find(|w| {
                            w.family.eq_ignore_ascii_case(name)
                                && w.covers(cp)
                                && w.face.has_char(cp)
                        });
                        if let Some(w) = f {
                            return Some(w.face.clone());
                        }
                    }
                }
                None
            }
        }
    }

    /// The primary font: the first available face of the family list
    /// (CSS Fonts 4 §5.4), whose metrics size lines.
    pub fn primary(&mut self, spec: &FontSpec) -> Option<Arc<Face>> {
        if let Some(f) = self.primary.get(&spec.key) {
            return f.clone();
        }
        let mut found = None;
        for fam in spec.key.families.clone() {
            let single = FontSpec {
                key: FontKey {
                    families: vec![fam.clone()],
                    ..spec.key.clone()
                },
                req: spec.req,
                size: spec.size,
            };
            let web_family = matches!(&fam, Family::Named(n) if self.web.iter().any(|w| w.family.eq_ignore_ascii_case(n)));
            if web_family {
                if let Some(f) = self.select_uncached(&single, ' ' as u32) {
                    found = Some(f);
                    break;
                }
                if let Family::Named(n) = &fam {
                    if let Some(w) = self
                        .web
                        .iter()
                        .rev()
                        .find(|w| w.family.eq_ignore_ascii_case(n))
                    {
                        found = Some(w.face.clone());
                        break;
                    }
                }
                continue;
            }
            if let Some(index) = self.index {
                if let Some(i) = index.match_family(&fam, &spec.req) {
                    found = index.load(i);
                    if found.is_some() {
                        break;
                    }
                }
            }
        }
        if found.is_none() {
            found = self.select(spec, 'a' as u32);
        }
        self.primary.insert(spec.key.clone(), found.clone());
        found
    }

    /// The metrics of `style`'s primary font.
    pub fn metrics(&mut self, style: &ComputedStyle) -> Metrics {
        let spec = font_spec(style);
        self.metrics_of(&spec)
    }

    pub fn metrics_of(&mut self, spec: &FontSpec) -> Metrics {
        let size = spec.size;
        let Some(face) = self.primary(spec) else {
            return Metrics::fallback(size);
        };
        let m = face.metrics().scaled(size as f32);
        let upem = face.units_per_em() as f64;
        let zero = face
            .glyph_index('0' as u32)
            .map(|g| face.advance(g) as f64 * size / upem)
            .unwrap_or(0.5 * size);
        Metrics {
            size,
            ascent: m.ascent as f64,
            descent: m.descent.abs() as f64,
            line_gap: m.line_gap.max(0.0) as f64,
            x_height: if m.x_height > 0.0 {
                m.x_height as f64
            } else {
                0.5 * size
            },
            space: m.space_advance as f64,
            zero,
        }
    }

    /// Shapes `text` with `style`'s fonts, as one direction. Right to
    /// left text is shaped with its mirrored characters; glyphs keep the
    /// logical order.
    pub fn shape(&mut self, text: &str, style: &ComputedStyle, rtl: bool) -> Vec<Chunk> {
        let spec = font_spec(style);
        let opts = shape_options(style);
        let size = spec.size;
        // Runs of one face.
        let mut runs: Vec<(Option<Arc<Face>>, usize, usize)> = Vec::new();
        for (at, c) in text.char_indices() {
            let end = at + c.len_utf8();
            let sticky = is_cluster_extender(c)
                || (c == ' '
                    && runs
                        .last()
                        .is_some_and(|r| r.0.as_ref().is_some_and(|f| f.has_char(32))));
            if sticky {
                if let Some(last) = runs.last_mut() {
                    last.2 = end;
                    continue;
                }
            }
            let face = if c == '\u{AD}' || is_default_ignorable(c) {
                runs.last()
                    .and_then(|r| r.0.clone())
                    .or_else(|| self.primary(&spec))
            } else {
                self.select(&spec, c as u32).or_else(|| self.primary(&spec))
            };
            match runs.last_mut() {
                Some(last) if same_face(&last.0, &face) => last.2 = end,
                _ => runs.push((face, at, end)),
            }
        }
        let mut out = Vec::new();
        for (face, start, end) in runs {
            let piece = &text[start..end];
            let glyphs = match &face {
                Some(f) => {
                    if rtl {
                        shape_mirrored(f, piece, size, &opts, start)
                    } else {
                        let mut g = shape::shape(f, piece, size as f32, &opts);
                        for x in &mut g {
                            x.cluster += start as u32;
                        }
                        g
                    }
                }
                None => piece
                    .char_indices()
                    .map(|(i, c)| ShapedGlyph {
                        glyph: 0,
                        cluster: (start + i) as u32,
                        x_advance: if is_default_ignorable(c) {
                            0.0
                        } else {
                            (0.5 * size) as f32
                        },
                        y_advance: 0.0,
                        x_offset: 0.0,
                        y_offset: 0.0,
                    })
                    .collect(),
            };
            out.push(Chunk {
                face,
                start,
                end,
                glyphs,
            });
        }
        out
    }
}

fn same_face(a: &Option<Arc<Face>>, b: &Option<Arc<Face>>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => Arc::ptr_eq(a, b) || a.id == b.id,
        (None, None) => true,
        _ => false,
    }
}

/// Shapes the mirrored form of `text` (rule L4 of UAX #9) and maps the
/// clusters back to byte offsets of `text` plus `base`.
fn shape_mirrored(
    face: &Face,
    text: &str,
    size: f64,
    opts: &ShapeOptions,
    base: usize,
) -> Vec<ShapedGlyph> {
    let mut mirrored = String::with_capacity(text.len());
    let mut map: Vec<(usize, usize)> = Vec::new();
    for (at, c) in text.char_indices() {
        map.push((mirrored.len(), at));
        let m = bidi::mirror(c as u32).and_then(char::from_u32).unwrap_or(c);
        mirrored.push(m);
    }
    let mut glyphs = shape::shape(face, &mirrored, size as f32, opts);
    for g in &mut glyphs {
        let i = match map.binary_search_by_key(&(g.cluster as usize), |p| p.0) {
            Ok(i) => i,
            Err(i) => i.saturating_sub(1),
        };
        g.cluster = (map.get(i).map_or(0, |p| p.1) + base) as u32;
    }
    glyphs
}

fn shape_options(style: &ComputedStyle) -> ShapeOptions {
    let mut opts = ShapeOptions::default();
    opts.kerning = match kw(style, "font-kerning").as_str() {
        "none" => Kerning::None,
        "normal" => Kerning::Normal,
        _ => Kerning::Auto,
    };
    let ligatures = kw(style, "font-variant-ligatures");
    if ligatures == "none" {
        opts.variants.ligatures = Ligatures::NONE;
    } else {
        for w in ligatures.split_ascii_whitespace() {
            match w {
                "no-common-ligatures" => opts.variants.ligatures.common = false,
                "common-ligatures" => opts.variants.ligatures.common = true,
                "discretionary-ligatures" => opts.variants.ligatures.discretionary = true,
                "historical-ligatures" => opts.variants.ligatures.historical = true,
                "no-contextual" => opts.variants.ligatures.contextual = false,
                _ => {}
            }
        }
    }
    // Letter spacing turns optional ligatures off (css-text-3 §8.2).
    if letter_spacing(style) != 0.0 {
        opts.variants.ligatures = Ligatures::NONE;
    }
    opts.variants.caps = match kw(style, "font-variant-caps").as_str() {
        "small-caps" => Caps::SmallCaps,
        "all-small-caps" => Caps::AllSmallCaps,
        "petite-caps" => Caps::PetiteCaps,
        "all-petite-caps" => Caps::AllPetiteCaps,
        "unicase" => Caps::Unicase,
        "titling-caps" => Caps::TitlingCaps,
        _ => Caps::Normal,
    };
    opts
}

/// The used `letter-spacing` in pixels (percentages of the font size).
pub(crate) fn letter_spacing(style: &ComputedStyle) -> f64 {
    lp(style, "letter-spacing").map_or(0.0, |l| l.resolve(style.font_size))
}

/// The used `word-spacing` in pixels (percentages of the advance of the
/// space, approximated by the font size's fraction here).
pub(crate) fn word_spacing(style: &ComputedStyle, space: f64) -> f64 {
    lp(style, "word-spacing").map_or(0.0, |l| l.resolve(space))
}

/// Combining marks, joiners and variation selectors stay in the run of
/// the character before them.
fn is_cluster_extender(c: char) -> bool {
    let cp = c as u32;
    matches!(cp, 0x200C | 0x200D | 0xFE00..=0xFE0F | 0xE0100..=0xE01EF)
        || matches!(vjs::unicode::general_category(cp), "Mn" | "Me" | "Mc")
}

/// Default ignorable characters that never show (bidi controls, soft
/// hyphen, zero width characters).
pub(crate) fn is_default_ignorable(c: char) -> bool {
    matches!(
        c as u32,
        0x00AD | 0x034F | 0x061C | 0x115F..=0x1160 | 0x17B4..=0x17B5 | 0x180B..=0x180F
            | 0x200B..=0x200F | 0x202A..=0x202E | 0x2060..=0x206F | 0x3164 | 0xFE00..=0xFE0F
            | 0xFEFF | 0xFFA0 | 0xFFF0..=0xFFF8 | 0x1BCA0..=0x1BCA3 | 0x1D173..=0x1D17A
            | 0xE0000..=0xE0FFF
    )
}

/// Word-separator characters (css-text-3 §8.1): `word-spacing` and
/// justification apply to them.
pub(crate) fn is_word_separator(c: char) -> bool {
    matches!(
        c as u32,
        0x0020 | 0x00A0 | 0x1361 | 0x10100 | 0x10101 | 0x1039F | 0x1091F
    )
}

/// The `text-transform` of `style` applied to `text` (css-text-3 §2.1):
/// `uppercase`, `lowercase` and `capitalize` with the full case mappings,
/// `full-width` and `full-size-kana`. `prev_letter` says whether the
/// character before `text` in the inline formatting context ends a word
/// in progress (for `capitalize`).
pub(crate) fn transform(text: &str, style: &ComputedStyle, prev_letter: bool) -> String {
    let t = kw(style, "text-transform");
    if t == "none" || t.is_empty() {
        return text.to_string();
    }
    let mut cps: Vec<u32> = text.chars().map(|c| c as u32).collect();
    let words: Vec<&str> = t.split_ascii_whitespace().collect();
    if words.contains(&"uppercase") {
        cps = vjs::unicode::to_uppercase(&cps);
    } else if words.contains(&"lowercase") {
        cps = vjs::unicode::to_lowercase(&cps);
    } else if words.contains(&"capitalize") {
        let mut out = Vec::with_capacity(cps.len());
        let mut in_word = prev_letter;
        for &c in &cps {
            let letter = is_word_letter(c);
            if letter && !in_word {
                vjs::unicode::push_titlecase(&mut out, c);
            } else {
                out.push(c);
            }
            if letter {
                in_word = true;
            } else if !is_word_joiner(c) {
                in_word = false;
            }
        }
        cps = out;
    }
    if words.contains(&"full-width") {
        for c in &mut cps {
            *c = match *c {
                0x20 => 0x3000,
                0x21..=0x7E => *c - 0x21 + 0xFF01,
                _ => *c,
            };
        }
    }
    if words.contains(&"full-size-kana") {
        for c in &mut cps {
            *c = full_size_kana(*c);
        }
    }
    cps.into_iter().filter_map(char::from_u32).collect()
}

/// Letters and numbers start and continue words for `capitalize`.
pub(crate) fn is_word_letter(c: u32) -> bool {
    matches!(
        vjs::unicode::general_category(c).as_bytes().first(),
        Some(b'L') | Some(b'N')
    )
}

/// Characters that do not end a word for `capitalize` (apostrophes,
/// marks).
fn is_word_joiner(c: u32) -> bool {
    matches!(c, 0x27 | 0x2019 | 0xAD)
        || matches!(vjs::unicode::general_category(c), "Mn" | "Mc" | "Me")
}

/// The full-size form of a small kana (css-text-3 Appendix G).
fn full_size_kana(c: u32) -> u32 {
    const MAP: &[(u32, u32)] = &[
        (0x3041, 0x3042),
        (0x3043, 0x3044),
        (0x3045, 0x3046),
        (0x3047, 0x3048),
        (0x3049, 0x304A),
        (0x3063, 0x3064),
        (0x3083, 0x3084),
        (0x3085, 0x3086),
        (0x3087, 0x3088),
        (0x308E, 0x308F),
        (0x3095, 0x304B),
        (0x3096, 0x3051),
        (0x30A1, 0x30A2),
        (0x30A3, 0x30A4),
        (0x30A5, 0x30A6),
        (0x30A7, 0x30A8),
        (0x30A9, 0x30AA),
        (0x30C3, 0x30C4),
        (0x30E3, 0x30E4),
        (0x30E5, 0x30E6),
        (0x30E7, 0x30E8),
        (0x30EE, 0x30EF),
        (0x30F5, 0x30AB),
        (0x30F6, 0x30B1),
        (0x31F0, 0x30AF),
        (0x31F1, 0x30B7),
        (0x31F2, 0x30B9),
        (0x31F3, 0x30C8),
        (0x31F4, 0x30CC),
        (0x31F5, 0x30CF),
        (0x31F6, 0x30D2),
        (0x31F7, 0x30D5),
        (0x31F8, 0x30D8),
        (0x31F9, 0x30DB),
        (0x31FA, 0x30E0),
        (0x31FB, 0x30E9),
        (0x31FC, 0x30EA),
        (0x31FD, 0x30EB),
        (0x31FE, 0x30EC),
        (0x31FF, 0x30ED),
        (0xFF67, 0xFF71),
        (0xFF68, 0xFF72),
        (0xFF69, 0xFF73),
        (0xFF6A, 0xFF74),
        (0xFF6B, 0xFF75),
        (0xFF6C, 0xFF94),
        (0xFF6D, 0xFF95),
        (0xFF6E, 0xFF96),
        (0xFF6F, 0xFF82),
        (0x1B132, 0x3053),
        (0x1B150, 0x3090),
        (0x1B151, 0x3091),
        (0x1B152, 0x3092),
        (0x1B155, 0x30B3),
        (0x1B164, 0x30F0),
        (0x1B165, 0x30F1),
        (0x1B166, 0x30F2),
        (0x1B167, 0x30F3),
    ];
    MAP.binary_search_by_key(&c, |p| p.0)
        .map_or(c, |i| MAP[i].1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn separators_and_ignorables() {
        assert!(is_word_separator(' '));
        assert!(is_word_separator('\u{A0}'));
        assert!(!is_word_separator('\t'));
        assert!(is_default_ignorable('\u{AD}'));
        assert!(is_default_ignorable('\u{200B}'));
        assert!(!is_default_ignorable('a'));
        assert_eq!(full_size_kana(0x3041), 0x3042);
        assert_eq!(full_size_kana(0x3042), 0x3042);
    }
}
