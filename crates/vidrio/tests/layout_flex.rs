//! Flex layout end to end, measured with Ahem (read from
//! `bench/cache/wpt/fonts/Ahem.ttf`: every glyph is 1em wide, with
//! ascent 0.8em and descent 0.2em): `flex-wrap` with `align-content`,
//! the automatic minimum size of an item with long content, baseline
//! alignment, the intrinsic sizes of a column container, `auto` margins,
//! `safe` alignment, `order`, `visibility: collapse` and the static
//! position of an absolutely positioned child. Tests are skipped with a
//! message when Ahem is missing.

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use vgfx::font::Face;
use vgfx::font::matching::{Descriptor, StyleRange};
use vgfx::font::system::WebFace;
use vidrio::css::style::styles;
use vidrio::dom::{DocRef, NodeId};
use vidrio::layout::fragment::{FragmentTree, Rect};
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

/// Lays out `RESET` + `body` in the 800×600 viewport.
fn lay(body: &str) -> Option<(DocRef, FragmentTree)> {
    let font = ahem()?;
    let html = format!("<!DOCTYPE html>{RESET}{body}");
    let doc: DocRef = Rc::new(RefCell::new(vidrio::html::parse_document(&html, true)));
    let map = styles(&doc);
    let images = |_: NodeId| None;
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

/// The border box of the only fragment of `id`.
fn rect(doc: &DocRef, tree: &FragmentTree, id: &str) -> Rect {
    let frags = tree.fragments_of(element(doc, id));
    assert_eq!(frags.len(), 1, "#{id} has one fragment");
    frags[0].rect
}

fn r(x: f64, y: f64, width: f64, height: f64) -> Rect {
    Rect::new(x, y, width, height)
}

#[test]
fn wrapped_lines_with_align_content() {
    // Two lines (40 + 10 + 40 fit in 100; the third item does not), 30
    // and 20 high with a 5px row gap: 45px are left between them.
    let Some((d, t)) = lay(concat!(
        "<div id=c style='display:flex;flex-wrap:wrap;width:100px;height:100px;",
        "column-gap:10px;row-gap:5px;align-content:space-between'>",
        "<div id=a style='width:40px;height:20px'></div>",
        "<div id=b style='width:40px;height:30px'></div>",
        "<div id=e style='width:40px;height:20px'></div>",
        "</div>",
    )) else {
        return;
    };
    assert_eq!(rect(&d, &t, "c"), r(0.0, 0.0, 100.0, 100.0));
    assert_eq!(rect(&d, &t, "a"), r(0.0, 0.0, 40.0, 20.0));
    assert_eq!(rect(&d, &t, "b"), r(50.0, 0.0, 40.0, 30.0));
    assert_eq!(rect(&d, &t, "e"), r(0.0, 80.0, 40.0, 20.0));

    // `space-evenly`: 60px of free space in three equal parts.
    let Some((d, t)) = lay(concat!(
        "<div style='display:flex;flex-wrap:wrap;width:100px;height:100px;",
        "align-content:space-evenly'>",
        "<div id=a style='width:60px;height:20px'></div>",
        "<div id=b style='width:60px;height:20px'></div>",
        "</div>",
    )) else {
        return;
    };
    assert_eq!(rect(&d, &t, "a"), r(0.0, 20.0, 60.0, 20.0));
    assert_eq!(rect(&d, &t, "b"), r(0.0, 60.0, 60.0, 20.0));

    // `wrap-reverse`: the first line is at the bottom, and `flex-start`
    // packs the lines there.
    let Some((d, t)) = lay(concat!(
        "<div style='display:flex;flex-wrap:wrap-reverse;width:100px;height:100px;",
        "align-content:flex-start'>",
        "<div id=a style='width:60px;height:20px'></div>",
        "<div id=b style='width:60px;height:20px'></div>",
        "</div>",
    )) else {
        return;
    };
    assert_eq!(rect(&d, &t, "a"), r(0.0, 80.0, 60.0, 20.0));
    assert_eq!(rect(&d, &t, "b"), r(0.0, 60.0, 60.0, 20.0));

    // Without a definite height, `align-content: stretch` has nothing to
    // share and the lines stretch their items.
    let Some((d, t)) = lay(concat!(
        "<div id=c style='display:flex;flex-wrap:wrap;width:100px'>",
        "<div id=a style='width:60px'>X</div>",
        "<div id=b style='width:60px;height:30px'></div>",
        "</div>",
    )) else {
        return;
    };
    assert_eq!(rect(&d, &t, "c"), r(0.0, 0.0, 100.0, 40.0));
    assert_eq!(rect(&d, &t, "a"), r(0.0, 0.0, 60.0, 10.0));
    assert_eq!(rect(&d, &t, "b"), r(0.0, 10.0, 60.0, 30.0));
}

#[test]
fn automatic_minimum_size_with_long_content() {
    // The word is 150px wide: `min-width: auto` keeps the first item at
    // its min-content width and the second item shrinks to nothing.
    let Some((d, t)) = lay(concat!(
        "<div id=c style='display:flex;width:100px'>",
        "<div id=a>XXXXXXXXXXXXXXX</div>",
        "<div id=b style='width:50px'></div>",
        "</div>",
    )) else {
        return;
    };
    assert_eq!(rect(&d, &t, "a"), r(0.0, 0.0, 150.0, 10.0));
    assert_eq!(rect(&d, &t, "b"), r(150.0, 0.0, 0.0, 10.0));
    assert_eq!(rect(&d, &t, "c"), r(0.0, 0.0, 100.0, 10.0));

    // `min-width: 0` and a scroll container have no automatic minimum:
    // both items shrink in proportion to their flex base sizes (150 and
    // 50 share the 100px of negative free space).
    for a_style in ["min-width:0", "overflow:hidden"] {
        let Some((d, t)) = lay(&format!(
            "<div style='display:flex;width:100px'>\
             <div id=a style='{a_style}'>XXXXXXXXXXXXXXX</div>\
             <div id=b style='width:50px'></div></div>"
        )) else {
            return;
        };
        assert_eq!(rect(&d, &t, "a"), r(0.0, 0.0, 75.0, 10.0), "{a_style}");
        assert_eq!(rect(&d, &t, "b"), r(75.0, 0.0, 25.0, 10.0), "{a_style}");
    }
}

#[test]
fn baseline_alignment() {
    // #a's baseline is 16px below its top, #b's 15 + 8 = 23px: #a moves
    // down 7px and the line is 23 + max(4, 2) = 27px high.
    let Some((d, t)) = lay(concat!(
        "<div id=c style='display:flex;align-items:baseline'>",
        "<div id=a style='font-size:20px'>X</div>",
        "<div id=b style='padding-top:15px'>X</div>",
        "</div>",
    )) else {
        return;
    };
    assert_eq!(rect(&d, &t, "c"), r(0.0, 0.0, 800.0, 27.0));
    assert_eq!(rect(&d, &t, "a"), r(0.0, 7.0, 20.0, 20.0));
    assert_eq!(rect(&d, &t, "b"), r(20.0, 0.0, 10.0, 25.0));

    // `last baseline`: #a's last line is its second; the last baselines
    // are 10 + 8 = 18px (#a) and 8px (#b) below the tops, so #b moves
    // down 10px.
    let Some((d, t)) = lay(concat!(
        "<div style='display:flex;align-items:last baseline'>",
        "<div id=a style='width:10px'>X X</div>",
        "<div id=b>X</div>",
        "</div>",
    )) else {
        return;
    };
    assert_eq!(rect(&d, &t, "a"), r(0.0, 0.0, 10.0, 20.0));
    assert_eq!(rect(&d, &t, "b"), r(10.0, 10.0, 10.0, 10.0));
}

#[test]
fn intrinsic_sizes_of_a_column_container() {
    // A floated column container shrinks to its widest item (the
    // 50px word); its height is the sum of the items' heights.
    let Some((d, t)) = lay(concat!(
        "<div id=c style='float:left;display:flex;flex-direction:column;padding:5px'>",
        "<div id=a style='width:30px;height:10px'></div>",
        "<div id=b>XXXXX</div>",
        "</div>",
    )) else {
        return;
    };
    assert_eq!(rect(&d, &t, "c"), r(0.0, 0.0, 60.0, 30.0));
    assert_eq!(rect(&d, &t, "a"), r(5.0, 5.0, 30.0, 10.0));
    assert_eq!(rect(&d, &t, "b"), r(5.0, 15.0, 50.0, 10.0));

    // `min-content`: the largest min-content contribution is #e's 30px;
    // #f is stretched to it and wraps onto two lines.
    let Some((d, t)) = lay(concat!(
        "<div id=c style='display:flex;flex-direction:column;width:min-content'>",
        "<div id=e style='width:30px;height:10px'></div>",
        "<div id=f>XX XX</div>",
        "</div>",
    )) else {
        return;
    };
    assert_eq!(rect(&d, &t, "c"), r(0.0, 0.0, 30.0, 30.0));
    assert_eq!(rect(&d, &t, "e"), r(0.0, 0.0, 30.0, 10.0));
    assert_eq!(rect(&d, &t, "f"), r(0.0, 10.0, 30.0, 20.0));

    // `max-content`: the largest max-content contribution, #f's 50px.
    let Some((d, t)) = lay(concat!(
        "<div id=c style='display:flex;flex-direction:column;width:max-content'>",
        "<div id=e style='width:30px;height:10px'></div>",
        "<div id=f>XX XX</div>",
        "</div>",
    )) else {
        return;
    };
    assert_eq!(rect(&d, &t, "c"), r(0.0, 0.0, 50.0, 20.0));
    assert_eq!(rect(&d, &t, "f"), r(0.0, 10.0, 50.0, 10.0));
}

#[test]
fn auto_margins_safe_alignment_and_order() {
    let Some((d, t)) = lay(concat!(
        "<div style='display:flex;width:100px;height:50px'>",
        "<div id=a style='width:20px;height:10px;margin:auto'></div>",
        "</div>",
    )) else {
        return;
    };
    assert_eq!(rect(&d, &t, "a"), r(40.0, 20.0, 20.0, 10.0));

    // A 150px item in a 100px container: `center` overflows both sides,
    // `safe center` aligns it to the start.
    for (justify, x) in [("center", -25.0), ("safe center", 0.0)] {
        let Some((d, t)) = lay(&format!(
            "<div style='display:flex;width:100px;justify-content:{justify}'>\
             <div id=a style='width:150px;height:10px;flex-shrink:0'></div></div>"
        )) else {
            return;
        };
        assert_eq!(rect(&d, &t, "a"), r(x, 0.0, 150.0, 10.0), "{justify}");
    }

    // `order` puts #b first; `row-reverse` starts at the right.
    let Some((d, t)) = lay(concat!(
        "<div style='display:flex;flex-direction:row-reverse;width:100px;height:10px'>",
        "<div id=a style='width:10px;order:2'></div>",
        "<div id=b style='width:20px'></div>",
        "</div>",
    )) else {
        return;
    };
    assert_eq!(rect(&d, &t, "b"), r(80.0, 0.0, 20.0, 10.0));
    assert_eq!(rect(&d, &t, "a"), r(70.0, 0.0, 10.0, 10.0));
}

#[test]
fn collapsed_items_and_absolute_children() {
    // The collapsed item takes no main space but keeps the line 40px
    // high (its strut).
    let Some((d, t)) = lay(concat!(
        "<div id=c style='display:flex;width:100px'>",
        "<div id=a style='width:20px'>X</div>",
        "<div id=b style='visibility:collapse;width:30px;height:40px'></div>",
        "<div id=e style='width:10px;height:5px'></div>",
        "</div>",
    )) else {
        return;
    };
    assert_eq!(rect(&d, &t, "c"), r(0.0, 0.0, 100.0, 40.0));
    assert_eq!(rect(&d, &t, "a"), r(0.0, 0.0, 20.0, 40.0));
    assert_eq!(rect(&d, &t, "e"), r(20.0, 0.0, 10.0, 5.0));

    // The static position of an absolutely positioned child is that of
    // a sole item aligned by `justify-content` and `align-items`.
    let Some((d, t)) = lay(concat!(
        "<div style='display:flex;position:relative;width:100px;height:100px;",
        "justify-content:center;align-items:flex-end'>",
        "<div id=p style='position:absolute;width:20px;height:10px'></div>",
        "<div id=a style='width:10px;height:10px'></div>",
        "</div>",
    )) else {
        return;
    };
    assert_eq!(rect(&d, &t, "p"), r(40.0, 90.0, 20.0, 10.0));
    assert_eq!(rect(&d, &t, "a"), r(45.0, 90.0, 10.0, 10.0));
}

#[test]
fn nested_percentages_resolve_against_flexed_sizes() {
    // The column container has a definite height, so #a's flexed height
    // (100 - 20 = 80px) is definite and #inner's 50% is 40px.
    let Some((d, t)) = lay(concat!(
        "<div style='display:flex;flex-direction:column;width:100px;height:100px'>",
        "<div id=a style='flex:1'><div id=inner style='height:50%'></div></div>",
        "<div id=b style='height:20px'></div>",
        "</div>",
    )) else {
        return;
    };
    assert_eq!(rect(&d, &t, "a"), r(0.0, 0.0, 100.0, 80.0));
    assert_eq!(rect(&d, &t, "inner"), r(0.0, 0.0, 100.0, 40.0));
    assert_eq!(rect(&d, &t, "b"), r(0.0, 80.0, 100.0, 20.0));
}
