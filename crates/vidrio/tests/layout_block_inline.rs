//! Block and inline layout end to end, measured with Ahem (read from
//! `bench/cache/wpt/fonts/Ahem.ttf`: every glyph, the space and `.notdef`
//! included, is 1em wide, with ascent 0.8em and descent 0.2em): margins
//! that collapse through empty blocks, floats with clearance and lines
//! shortened by floats, justification, mixed `vertical-align` and a
//! bidi paragraph. Tests are skipped with a message when Ahem is missing.

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use vgfx::font::Face;
use vgfx::font::matching::{Descriptor, StyleRange};
use vgfx::font::system::WebFace;
use vidrio::css::style::styles;
use vidrio::dom::{DocRef, NodeId};
use vidrio::layout::fragment::{Fragment, FragmentKind, FragmentTree, Rect, TextFragment};
use vidrio::layout::{Options, layout};

const RESET: &str = "<style>html, body { margin: 0 } body { font: 10px/1 Ahem }</style>";

fn ahem() -> Option<WebFace> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../bench/cache/wpt/fonts/Ahem.ttf");
    let Ok(bytes) = std::fs::read(&path) else {
        eprintln!(
            "skipped: {} does not exist (run `python3 bench/fetch.py`)",
            path.display()
        );
        return None;
    };
    Some(WebFace {
        family: "Ahem".to_string(),
        descriptor: Descriptor {
            weight: (400.0, 400.0),
            stretch: (100.0, 100.0),
            style: StyleRange::Normal,
        },
        unicode_range: Vec::new(),
        face: Arc::new(Face::parse(bytes, 0).expect("Ahem parses")),
    })
}

/// Lays out `RESET` + `body` in the 800×600 viewport; every image has
/// the natural size 15×25.
fn lay(body: &str) -> Option<(DocRef, FragmentTree)> {
    let font = ahem()?;
    let html = format!("<!DOCTYPE html>{RESET}{body}");
    let doc: DocRef = Rc::new(RefCell::new(vidrio::html::parse_document(&html, true)));
    let map = styles(&doc);
    let images = |_: NodeId| Some((15.0, 25.0));
    let tree = layout(
        &doc.borrow(),
        &map,
        Options {
            viewport: (800.0, 600.0),
            web_fonts: vec![font],
            system_fonts: false,
            images: &images,
        },
    );
    Some((doc, tree))
}

fn element(doc: &DocRef, id: &str) -> NodeId {
    let d = doc.borrow();
    d.inclusive_descendants(NodeId::DOCUMENT)
        .into_iter()
        .find(|&n| d.get_attribute(n, "id") == Some(id))
        .unwrap_or_else(|| panic!("no element #{id}"))
}

/// The border box of `id`'s only fragment.
fn rect(doc: &DocRef, tree: &FragmentTree, id: &str) -> Rect {
    let frags = tree.fragments_of(element(doc, id));
    assert_eq!(frags.len(), 1, "#{id} has one fragment");
    frags[0].rect
}

/// The border boxes of `id`'s fragments (one per line for inline boxes).
fn rects(doc: &DocRef, tree: &FragmentTree, id: &str) -> Vec<Rect> {
    tree.fragments_of(element(doc, id))
        .iter()
        .map(|f| f.rect)
        .collect()
}

fn texts<'a>(
    doc: &DocRef,
    tree: &'a FragmentTree,
    id: &str,
) -> Vec<(&'a Fragment, &'a TextFragment)> {
    tree.first_fragment(element(doc, id))
        .unwrap_or_else(|| panic!("#{id} has a fragment"))
        .texts()
}

fn glyph_xs(t: &TextFragment) -> Vec<f64> {
    t.glyphs.iter().map(|g| g.x).collect()
}

fn r(x: f64, y: f64, width: f64, height: f64) -> Rect {
    Rect::new(x, y, width, height)
}

#[test]
fn margins_collapse_through_empty_blocks() {
    let Some((d, t)) = lay(concat!(
        "<div id=a style='height:20px; margin-bottom:10px'></div>",
        "<div id=e1 style='margin-top:30px; margin-bottom:5px'></div>",
        "<div id=e2 style='margin-top:-8px; margin-bottom:15px'></div>",
        "<div id=b style='margin-top:12px; height:10px'></div>",
        "<div id=p style='margin-top:5px'>",
        "<div id=c1 style='margin-top:20px; margin-bottom:-4px'></div>",
        "<div id=c2 style='margin-top:8px; height:10px; margin-bottom:6px'></div>",
        "</div>",
        "<div id=after style='margin-top:3px; height:1px'></div>",
    )) else {
        return;
    };
    assert_eq!(rect(&d, &t, "a"), r(0.0, 0.0, 800.0, 20.0));
    // 10, 30, 5, -8, 15 and 12 all adjoin: 30 - 8 = 22.
    assert_eq!(rect(&d, &t, "b"), r(0.0, 42.0, 800.0, 10.0));
    // An empty box's top border edge is where it would be with a bottom
    // border: after the margins up to its own top margin (CSS 2 §8.3.1).
    assert_eq!(rect(&d, &t, "e1"), r(0.0, 50.0, 800.0, 0.0));
    assert_eq!(rect(&d, &t, "e2"), r(0.0, 42.0, 800.0, 0.0));
    // 5, 20, -4 and 8 collapse into 16 above #p and #c2; #c1's margins
    // collapse with #p's top margin, so its top is #p's.
    assert_eq!(rect(&d, &t, "p"), r(0.0, 68.0, 800.0, 10.0));
    assert_eq!(rect(&d, &t, "c1"), r(0.0, 68.0, 800.0, 0.0));
    assert_eq!(rect(&d, &t, "c2"), r(0.0, 68.0, 800.0, 10.0));
    // #c2's bottom margin (6) leaves #p through its bottom and collapses
    // with #after's top margin (3).
    assert_eq!(rect(&d, &t, "after"), r(0.0, 84.0, 800.0, 1.0));
}

#[test]
fn floats_clearance_and_shortened_lines() {
    let Some((d, t)) = lay(concat!(
        "<div id=w style='display:flow-root; width:200px'>",
        "<div id=fl style='float:left; width:30px; height:40px'></div>",
        "<div id=fr style='float:right; width:20px; height:60px'></div>",
        "<div id=cl style='clear:left; margin-top:10px; height:10px'></div>",
        "<div id=cb style='clear:both; height:10px'></div>",
        "</div>",
        "<div id=w2 style='display:flow-root; width:100px'>",
        "<div id=f3 style='float:left; width:30px; height:10px'></div>",
        "<span id=s>XXXX XXXX XXXX</span>",
        "</div>",
        "<div id=w3 style='display:flow-root; width:100px'>",
        "<div id=sf style='float:right'>XX XXX</div>",
        "</div>",
        "<div id=w4 style='display:flow-root; width:100px'>",
        "<div id=f4 style='float:left; width:30px; height:20px'></div>",
        "<div id=ov style='overflow:hidden; height:10px'></div>",
        "</div>",
    )) else {
        return;
    };
    assert_eq!(rect(&d, &t, "fl"), r(0.0, 0.0, 30.0, 40.0));
    assert_eq!(rect(&d, &t, "fr"), r(180.0, 0.0, 20.0, 60.0));
    // Clearance puts the border edge at the bottom of the cleared floats;
    // the margin above does not count.
    assert_eq!(rect(&d, &t, "cl"), r(0.0, 40.0, 200.0, 10.0));
    assert_eq!(rect(&d, &t, "cb"), r(0.0, 60.0, 200.0, 10.0));
    assert_eq!(rect(&d, &t, "w"), r(0.0, 0.0, 200.0, 70.0));

    // The first line is beside the float (70px: one word), the second
    // below it (100px: two words).
    assert_eq!(rect(&d, &t, "f3"), r(0.0, 70.0, 30.0, 10.0));
    assert_eq!(
        rects(&d, &t, "s"),
        vec![r(30.0, 70.0, 40.0, 10.0), r(0.0, 80.0, 90.0, 10.0)]
    );
    assert_eq!(rect(&d, &t, "w2"), r(0.0, 70.0, 100.0, 20.0));

    // Shrink-to-fit: the max-content width fits.
    assert_eq!(rect(&d, &t, "sf"), r(40.0, 90.0, 60.0, 10.0));
    assert_eq!(rect(&d, &t, "w3"), r(0.0, 90.0, 100.0, 10.0));

    // A formatting context root goes beside the float; its parent, a
    // formatting context root too, contains the float.
    assert_eq!(rect(&d, &t, "ov"), r(30.0, 100.0, 70.0, 10.0));
    assert_eq!(rect(&d, &t, "w4"), r(0.0, 100.0, 100.0, 20.0));
}

#[test]
fn justification_and_text_align_last() {
    let Some((d, t)) = lay(concat!(
        "<div id=j style='width:100px; text-align:justify'>XX XX XXX XXXX</div>",
        "<div id=jl style='width:100px; text-align:justify; text-align-last:center'>XX XX XXX XXXX</div>",
    )) else {
        return;
    };
    let j = texts(&d, &t, "j");
    assert_eq!(j.len(), 2);
    // "XX XX XXX" is 90px wide: the 10px left go to its two spaces.
    let (f, first) = j[0];
    assert_eq!(first.text, "XX XX XXX");
    assert_eq!(f.rect, r(0.0, 0.0, 100.0, 10.0));
    assert_eq!(
        glyph_xs(first),
        vec![0.0, 10.0, 20.0, 35.0, 45.0, 55.0, 70.0, 80.0, 90.0]
    );
    assert_eq!(first.baseline, 8.0);
    // The last line is not justified.
    let (f, last) = j[1];
    assert_eq!(last.text, "XXXX");
    assert_eq!(f.rect, r(0.0, 10.0, 40.0, 10.0));

    let jl = texts(&d, &t, "jl");
    assert_eq!(jl.len(), 2);
    assert_eq!(jl[0].0.rect, r(0.0, 20.0, 100.0, 10.0));
    assert_eq!(jl[1].0.rect, r(30.0, 30.0, 40.0, 10.0));
    assert_eq!(rect(&d, &t, "jl"), r(0.0, 20.0, 100.0, 20.0));
}

#[test]
fn mixed_vertical_align() {
    let Some((d, t)) = lay(concat!(
        "<div id=v>X",
        "<span id=big style='font-size:20px'>X</span>",
        "<span id=up style='vertical-align:5px'>X</span>",
        "<span id=down style='vertical-align:-4px'>X</span>",
        "<img id=im style='vertical-align:middle'>",
        "<span id=top style='vertical-align:top; font-size:30px'>X</span>",
        "</div>",
    )) else {
        return;
    };
    // Around the baseline: the strut -8..2, #big -16..4, #up -13..-3,
    // #down -4..6 and the image, whose middle is half the x-height (8px)
    // above the baseline, -16.5..8.5. They span 25px; #top's 30px makes
    // the line taller, and the rest stays at the top.
    let v = t.first_fragment(element(&d, "v")).unwrap();
    let line = v
        .children
        .iter()
        .find(|f| matches!(f.kind, FragmentKind::Line { .. }))
        .expect("a line box");
    assert_eq!(line.rect, r(0.0, 0.0, 800.0, 30.0));
    assert!(matches!(line.kind, FragmentKind::Line { baseline } if baseline == 16.5));
    assert_eq!(rect(&d, &t, "v"), r(0.0, 0.0, 800.0, 30.0));
    // Inline boxes are as tall as their content area.
    assert_eq!(rect(&d, &t, "big"), r(10.0, 0.5, 20.0, 20.0));
    assert_eq!(rect(&d, &t, "up"), r(30.0, 3.5, 10.0, 10.0));
    assert_eq!(rect(&d, &t, "down"), r(40.0, 12.5, 10.0, 10.0));
    assert_eq!(rect(&d, &t, "im"), r(50.0, 0.0, 15.0, 25.0));
    assert_eq!(rect(&d, &t, "top"), r(65.0, 0.0, 30.0, 30.0));
    let first = texts(&d, &t, "v")[0].1;
    assert_eq!((first.text.as_str(), first.baseline), ("X", 16.5));
}

#[test]
fn bidi_paragraphs() {
    let Some((d, t)) = lay(concat!(
        "<div id=ltr style='width:200px'>AB \u{5D0}\u{5D1}\u{5D2} \u{5D3} CD</div>",
        "<div id=rtl style='width:200px; direction:rtl'>AB CD</div>",
        "<div id=ovr style='width:200px'>XY\u{C9}<span style='unicode-bidi:bidi-override; direction:rtl'>XY\u{C9}</span></div>",
    )) else {
        return;
    };
    // Levels: "AB " 0, "אבג ד" 1 (the space between two R is R), " CD" 0.
    let ltr = texts(&d, &t, "ltr");
    let found: Vec<(&str, u8, Rect)> = ltr
        .iter()
        .map(|(f, t)| (t.text.as_str(), t.level, f.rect))
        .collect();
    assert_eq!(
        found,
        vec![
            ("AB ", 0, r(0.0, 0.0, 30.0, 10.0)),
            ("\u{5D0}\u{5D1}\u{5D2} \u{5D3}", 1, r(30.0, 0.0, 50.0, 10.0)),
            (" CD", 0, r(80.0, 0.0, 30.0, 10.0)),
        ]
    );
    // The right-to-left run is reversed: ד, the space, ג, ב, א.
    let space = ltr[0].1.glyphs[2].id;
    let hebrew = ltr[1].1;
    assert_eq!(glyph_xs(hebrew), vec![30.0, 40.0, 50.0, 60.0, 70.0]);
    let ids: Vec<u16> = hebrew.glyphs.iter().map(|g| g.id).collect();
    assert_eq!(ids[1], space);
    assert!(ids[3] != space);

    // A right-to-left paragraph of left-to-right text: one run at level
    // 2, aligned to the right (`text-align: start`).
    let rtl = texts(&d, &t, "rtl");
    assert_eq!(rtl.len(), 1);
    assert_eq!((rtl[0].1.text.as_str(), rtl[0].1.level), ("AB CD", 2));
    assert_eq!(rtl[0].0.rect, r(150.0, 10.0, 50.0, 10.0));

    // `bidi-override` reverses Latin text: the same letters, the other
    // way round.
    let ovr = texts(&d, &t, "ovr");
    assert_eq!(ovr.len(), 2);
    let (f, plain) = ovr[0];
    assert_eq!((plain.text.as_str(), plain.level), ("XY\u{C9}", 0));
    assert_eq!(f.rect, r(0.0, 20.0, 30.0, 10.0));
    let (f, over) = ovr[1];
    assert_eq!((over.text.as_str(), over.level), ("XY\u{C9}", 1));
    assert_eq!(f.rect, r(30.0, 20.0, 30.0, 10.0));
    assert_eq!(glyph_xs(over), vec![30.0, 40.0, 50.0]);
    let mut ids: Vec<u16> = plain.glyphs.iter().map(|g| g.id).collect();
    ids.reverse();
    assert_eq!(over.glyphs.iter().map(|g| g.id).collect::<Vec<u16>>(), ids);
}
