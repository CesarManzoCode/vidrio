//! Fonts end to end: the metrics and glyphs of Ahem (served by WPT, read
//! from `bench/cache/wpt/fonts/Ahem.ttf`), composite outlines and `GPOS`
//! kerning of Liberation Sans, the system font index of D-15, and exact
//! coverage of a pixel-aligned square. Tests whose font is missing are
//! skipped with a message.

use std::path::{Path, PathBuf};

use vgfx::cache::GlyphCache;
use vgfx::font::Face;
use vgfx::font::matching::{Request, Style};
use vgfx::font::system::{Family, FontIndex, Generic};
use vgfx::raster::rasterize;
use vgfx::shape::{Kerning, ShapeOptions, shape};

fn ahem_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../bench/cache/wpt/fonts/Ahem.ttf")
}

const LIBERATION_SANS: &str = "/usr/share/fonts/liberation/LiberationSans-Regular.ttf";

fn load(path: &Path) -> Option<Face> {
    match std::fs::read(path) {
        Ok(bytes) => Some(Face::parse(bytes, 0).expect("the font parses")),
        Err(_) => {
            eprintln!(
                "skipped: {} does not exist (run `python3 bench/fetch.py`)",
                path.display()
            );
            None
        }
    }
}

#[test]
fn ahem_metrics() {
    let Some(face) = load(&ahem_path()) else {
        return;
    };
    assert_eq!(face.units_per_em(), 1000);
    let m = face.metrics();
    // Ahem: ascent 0.8 em, descent 0.2 em, no line gap (hhea and OS/2
    // typo agree; USE_TYPO_METRICS is off).
    assert_eq!((m.ascent, m.descent, m.line_gap), (800, 200, 0));
    assert_eq!((m.x_height, m.cap_height), (800, 800));
    assert_eq!(m.space_advance, 1000);
    assert_eq!(m.zero_advance, Some(1000));
    let s = m.scaled(20.0);
    assert_eq!((s.ascent, s.descent, s.line_gap), (16.0, 4.0, 0.0));
    assert_eq!(face.names().css_family(), Some("Ahem"));
    assert_eq!(face.weight(), 400);
    assert!(!face.is_italic());
}

#[test]
fn ahem_glyphs() {
    let Some(face) = load(&ahem_path()) else {
        return;
    };
    let x = face.glyph_index('X' as u32).expect("X is mapped");
    assert_eq!(face.advance(x), 1000);
    let b = face.outline(x).unwrap().control_bounds().unwrap();
    assert_eq!(b, (0.0, -200.0, 1000.0, 800.0));
    // 'p' is the descent box, 'É' the ascent box, space has no outline.
    let p = face.glyph_index('p' as u32).unwrap();
    assert_eq!(
        face.outline(p).unwrap().control_bounds(),
        Some((0.0, -200.0, 1000.0, 0.0))
    );
    let e = face.glyph_index('É' as u32).unwrap();
    assert_eq!(
        face.outline(e).unwrap().control_bounds(),
        Some((0.0, 0.0, 1000.0, 800.0))
    );
    let space = face.glyph_index(' ' as u32).unwrap();
    assert!(face.outline(space).unwrap().is_empty());
    // Shaping: every glyph advances one em, no kerning or ligatures.
    let glyphs = shape(&face, "XpX X", 10.0, &ShapeOptions::default());
    assert_eq!(glyphs.len(), 5);
    assert!(
        glyphs
            .iter()
            .all(|g| g.x_advance == 10.0 && g.x_offset == 0.0)
    );
    assert_eq!(
        glyphs.iter().map(|g| g.cluster).collect::<Vec<_>>(),
        vec![0, 1, 2, 3, 4]
    );
}

#[test]
fn ahem_square_coverage_is_exact() {
    let Some(face) = load(&ahem_path()) else {
        return;
    };
    let x = face.glyph_index('X' as u32).unwrap();
    // At 20px the em box is 20×20 pixels from 16 above the baseline.
    let mut cache = GlyphCache::new();
    let m = cache.get(&face, x, 20.0, 0.0);
    assert_eq!((m.width, m.height, m.left, m.top), (20, 20, 0, -16));
    assert!(
        m.data.iter().all(|&c| c == 255),
        "every pixel fully covered"
    );
    let again = cache.get(&face, x, 20.0, 0.0);
    assert!(std::sync::Arc::ptr_eq(&m, &again));
    assert_eq!(cache.stats(), (1, 1));
    // The ascent box at 10px: 10×8 pixels, exact too.
    let e = face.glyph_index('É' as u32).unwrap();
    let m = rasterize(&face.outline(e).unwrap(), 0.01, 0.0, 0.0);
    assert_eq!((m.width, m.height, m.left, m.top), (10, 8, 0, -8));
    assert!(m.data.iter().all(|&c| c == 255));
}

#[test]
fn pixel_aligned_square_coverage() {
    let mut o = vgfx::outline::Outline::new();
    o.move_to(2.0, 1.0);
    o.line_to(2.0, 6.0);
    o.line_to(7.0, 6.0);
    o.line_to(7.0, 1.0);
    o.close();
    let m = rasterize(&o, 1.0, 0.0, 0.0);
    assert_eq!((m.width, m.height, m.left, m.top), (5, 5, 2, -6));
    assert_eq!(m.data, vec![255; 25]);
    // Shifted by half a pixel, edge columns are exactly half covered.
    let m = rasterize(&o, 1.0, 0.5, 0.0);
    assert_eq!(m.width, 6);
    for y in 0..5 {
        assert_eq!(m.get(0, y), 128);
        assert_eq!(m.get(5, y), 128);
        assert!((1..5).all(|x| m.get(x, y) == 255));
    }
}

#[test]
fn liberation_sans_composite_glyphs() {
    let Some(face) = load(Path::new(LIBERATION_SANS)) else {
        return;
    };
    let a = face.glyph_index('A' as u32).unwrap();
    let aacute = face.glyph_index('Á' as u32).unwrap();
    // Á is A plus the acute accent, both by offset (measured with the
    // font's glyf: 2 components, bounds 4,0–1362,1776).
    assert_eq!(face.component_count(aacute), 2);
    let base = face.outline(a).unwrap();
    let comp = face.outline(aacute).unwrap();
    assert_eq!(comp.contours(), base.contours() + 1);
    let (x0, y0, x1, y1) = comp.control_bounds().unwrap();
    assert_eq!(
        (x0.round(), y0.round(), x1.round(), y1.round()),
        (4.0, 0.0, 1362.0, 1776.0)
    );
    assert_eq!(face.advance(aacute), 1366);
    let m = face.metrics();
    assert_eq!(m.units_per_em, 2048);
    // No USE_TYPO_METRICS: hhea wins (1854, 434, 67).
    assert_eq!((m.ascent, m.descent, m.line_gap), (1854, 434, 67));
}

#[test]
fn liberation_sans_gpos_kerning() {
    let Some(face) = load(Path::new(LIBERATION_SANS)) else {
        return;
    };
    let size = face.units_per_em() as f32;
    let kerned = shape(&face, "AV", size, &ShapeOptions::default());
    let plain = shape(
        &face,
        "AV",
        size,
        &ShapeOptions {
            kerning: Kerning::None,
            ..ShapeOptions::default()
        },
    );
    assert_eq!(plain[0].x_advance, 1366.0);
    assert!(
        kerned[0].x_advance < plain[0].x_advance,
        "A–V is kerned by GPOS: {kerned:?}"
    );
    assert_eq!(kerned[1].x_advance, plain[1].x_advance);
    // The legacy table of the same font has the pair as −152.
    let (ga, gv) = (kerned[0].glyph, kerned[1].glyph);
    assert_eq!(face.kern_pair(ga, gv), -152);
    // font-feature-settings: "kern" 0 turns it off as well.
    let off = shape(
        &face,
        "AV",
        size,
        &ShapeOptions {
            feature_settings: vec![(vgfx::font::read::Tag::new(b"kern"), 0)],
            ..ShapeOptions::default()
        },
    );
    assert_eq!(off[0].x_advance, 1366.0);
}

#[test]
fn system_index_follows_d15() {
    if !Path::new(LIBERATION_SANS).exists() {
        eprintln!("skipped: Liberation fonts are not installed");
        return;
    }
    let idx = FontIndex::system();
    let file = |i: usize| {
        idx.faces[i]
            .path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned()
    };
    let sans = idx
        .match_family(&Family::Generic(Generic::SansSerif), &Request::default())
        .unwrap();
    assert_eq!(file(sans), "LiberationSans-Regular.ttf");
    let bold_italic = Request {
        weight: 700.0,
        style: Style::Italic,
        ..Request::default()
    };
    let serif = idx
        .match_family(&Family::Generic(Generic::Serif), &bold_italic)
        .unwrap();
    assert_eq!(file(serif), "LiberationSerif-BoldItalic.ttf");
    // 600 rounds up to bold; 300 falls back to regular.
    let mono = idx
        .match_family(
            &Family::Named("liberation mono".into()),
            &Request {
                weight: 600.0,
                ..Request::default()
            },
        )
        .unwrap();
    assert_eq!(file(mono), "LiberationMono-Bold.ttf");
    let light = idx
        .match_family(
            &Family::Generic(Generic::Cursive),
            &Request {
                weight: 300.0,
                ..Request::default()
            },
        )
        .unwrap();
    assert_eq!(file(light), "LiberationSans-Regular.ttf");
    // local() by full and PostScript name.
    assert_eq!(
        idx.local("Liberation Sans").map(file).as_deref(),
        Some("LiberationSans-Regular.ttf")
    );
    assert_eq!(
        idx.local("LiberationSans-Bold").map(file).as_deref(),
        Some("LiberationSans-Bold.ttf")
    );
    // Devanagari is not in Liberation: the fallback is a Noto font.
    let families = [Family::Generic(Generic::SansSerif)];
    if let Some(i) = idx.select(&families, &Request::default(), 0x0915) {
        let f = idx.faces[i].names.css_family().unwrap().to_string();
        assert!(f.starts_with("Noto"), "fallback family {f}");
        assert!(idx.load(i).unwrap().has_char(0x0915));
    } else {
        eprintln!("skipped fallback check: no installed font maps U+0915");
    }
}
