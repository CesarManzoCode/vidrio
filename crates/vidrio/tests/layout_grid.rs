//! Grid layout end to end, measured with Ahem (read from
//! `bench/cache/wpt/fonts/Ahem.ttf`: every glyph is 1em wide, with
//! ascent 0.8em and descent 0.2em): dense automatic placement with items
//! that span several columns, the distribution of `fr` with intrinsic
//! minimums, `auto-fit` collapsing its empty tracks, a nested `subgrid`
//! whose items size the outer grid's tracks, and the resolved values of
//! `grid-template-*`. Tests are skipped with a message when Ahem is
//! missing.

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

/// The resolved `property` (`grid-template-columns` or `-rows`) of `id`.
fn template<'a>(doc: &DocRef, tree: &'a FragmentTree, id: &str, property: &str) -> &'a str {
    tree.grid_template(element(doc, id), property)
        .unwrap_or_else(|| panic!("#{id} is not a grid container"))
}

/// Three 100px columns and 20px rows; `a` and `b` span two columns, so
/// `b` wraps to the second row and leaves a hole after `a`.
fn spanning(flow: &str) -> String {
    format!(
        concat!(
            "<div id=g style='display:grid;grid-template-columns:repeat(3, 100px);",
            "grid-auto-rows:20px;grid-auto-flow:{}'>",
            "<div id=a style='grid-column:span 2'></div>",
            "<div id=b style='grid-column:span 2'></div>",
            "<div id=c></div>",
            "<div id=d></div>",
            "</div>",
        ),
        flow
    )
}

#[test]
fn dense_placement_fills_holes_left_by_spanning_items() {
    // Dense: every item searches from the start of the grid, so `c`
    // fills the hole after `a` and `d` the one after `b`.
    let Some((d, t)) = lay(&spanning("row dense")) else {
        return;
    };
    assert_eq!(rect(&d, &t, "a"), r(0.0, 0.0, 200.0, 20.0));
    assert_eq!(rect(&d, &t, "b"), r(0.0, 20.0, 200.0, 20.0));
    assert_eq!(rect(&d, &t, "c"), r(200.0, 0.0, 100.0, 20.0));
    assert_eq!(rect(&d, &t, "d"), r(200.0, 20.0, 100.0, 20.0));
    assert_eq!(rect(&d, &t, "g"), r(0.0, 0.0, 800.0, 40.0));
    assert_eq!(template(&d, &t, "g", "grid-template-rows"), "20px 20px");

    // Sparse: the cursor never goes back, so the hole stays and `d`
    // opens a third row.
    let Some((d, t)) = lay(&spanning("row")) else {
        return;
    };
    assert_eq!(rect(&d, &t, "b"), r(0.0, 20.0, 200.0, 20.0));
    assert_eq!(rect(&d, &t, "c"), r(200.0, 20.0, 100.0, 20.0));
    assert_eq!(rect(&d, &t, "d"), r(0.0, 40.0, 100.0, 20.0));
    assert_eq!(rect(&d, &t, "g"), r(0.0, 0.0, 800.0, 60.0));
}

#[test]
fn flexible_tracks_keep_their_intrinsic_minimums() {
    // `1fr` is `minmax(auto, 1fr)`: the first column cannot shrink below
    // the 200px word of its item, so it is treated as inflexible and the
    // other two share the rest (320 - 2×10 gap - 200 = 100).
    let Some((d, t)) = lay(concat!(
        "<div id=g style='display:grid;width:320px;column-gap:10px;",
        "grid-template-columns:1fr 1fr 1fr'>",
        "<div id=a>XXXXXXXXXXXXXXXXXXXX</div>",
        "<div id=b></div>",
        "<div id=c></div>",
        "</div>",
    )) else {
        return;
    };
    assert_eq!(rect(&d, &t, "a"), r(0.0, 0.0, 200.0, 10.0));
    assert_eq!(rect(&d, &t, "b"), r(210.0, 0.0, 50.0, 10.0));
    assert_eq!(rect(&d, &t, "c"), r(270.0, 0.0, 50.0, 10.0));
    assert_eq!(
        template(&d, &t, "g", "grid-template-columns"),
        "200px 50px 50px"
    );

    // With a zero minimum the word overflows and the columns are equal.
    let Some((d, t)) = lay(concat!(
        "<div style='display:grid;width:320px;column-gap:10px;",
        "grid-template-columns:repeat(3, minmax(0, 1fr))'>",
        "<div id=a>XXXXXXXXXXXXXXXXXXXX</div>",
        "<div id=b></div>",
        "<div id=c></div>",
        "</div>",
    )) else {
        return;
    };
    assert_eq!(rect(&d, &t, "a"), r(0.0, 0.0, 100.0, 10.0));
    assert_eq!(rect(&d, &t, "b"), r(110.0, 0.0, 100.0, 10.0));
    assert_eq!(rect(&d, &t, "c"), r(220.0, 0.0, 100.0, 10.0));
}

#[test]
fn auto_fit_collapses_empty_tracks() {
    // Four 100px columns fit in 500px (4×100 + 3×10 = 430); two of them
    // stay empty and collapse with their gaps, so `center` works on the
    // 210px the other two take.
    let Some((d, t)) = lay(concat!(
        "<div id=g style='display:grid;width:500px;column-gap:10px;",
        "grid-template-columns:repeat(auto-fit, 100px);justify-content:center'>",
        "<div id=a></div>",
        "<div id=b></div>",
        "</div>",
    )) else {
        return;
    };
    assert_eq!(rect(&d, &t, "a"), r(145.0, 0.0, 100.0, 0.0));
    assert_eq!(rect(&d, &t, "b"), r(255.0, 0.0, 100.0, 0.0));
    assert_eq!(
        template(&d, &t, "g", "grid-template-columns"),
        "100px 100px 0px 0px"
    );

    // `auto-fill` keeps them: the four tracks and their gaps are 430px.
    let Some((d, t)) = lay(concat!(
        "<div id=g style='display:grid;width:500px;column-gap:10px;",
        "grid-template-columns:repeat(auto-fill, 100px);justify-content:center'>",
        "<div id=a></div>",
        "<div id=b></div>",
        "</div>",
    )) else {
        return;
    };
    assert_eq!(rect(&d, &t, "a"), r(35.0, 0.0, 100.0, 0.0));
    assert_eq!(rect(&d, &t, "b"), r(145.0, 0.0, 100.0, 0.0));
    assert_eq!(
        template(&d, &t, "g", "grid-template-columns"),
        "100px 100px 100px 100px"
    );
}

#[test]
fn nested_subgrid_items_size_the_outer_tracks() {
    // `s` subgrids both axes of `g`; `t`, inside `s`, subgrids only its
    // columns (the last two of `g`). `x` sizes the `auto` column of `g`
    // through two subgrids, and `y`, in a row of `t`'s own, sizes the
    // `auto` row of `g`.
    let Some((d, t)) = lay(concat!(
        "<div id=g style='display:grid;justify-content:start;",
        "grid-template-columns:50px auto 150px;grid-template-rows:20px auto'>",
        "<div id=s style='display:grid;grid-column:1 / 4;grid-row:1 / 3;",
        "grid-template-columns:subgrid;grid-template-rows:subgrid'>",
        "<div id=t style='display:grid;grid-column:2 / 4;grid-row:2;",
        "grid-template-columns:subgrid'>",
        "<div id=x style='width:120px'></div>",
        "<div id=y style='height:40px'></div>",
        "</div>",
        "<div id=u style='grid-column:1;grid-row:1'></div>",
        "</div>",
        "</div>",
    )) else {
        return;
    };
    assert_eq!(rect(&d, &t, "g"), r(0.0, 0.0, 800.0, 60.0));
    assert_eq!(rect(&d, &t, "s"), r(0.0, 0.0, 320.0, 60.0));
    assert_eq!(rect(&d, &t, "t"), r(50.0, 20.0, 270.0, 40.0));
    assert_eq!(rect(&d, &t, "u"), r(0.0, 0.0, 50.0, 20.0));
    assert_eq!(rect(&d, &t, "x"), r(50.0, 20.0, 120.0, 40.0));
    assert_eq!(rect(&d, &t, "y"), r(170.0, 20.0, 150.0, 40.0));
    assert_eq!(
        template(&d, &t, "g", "grid-template-columns"),
        "50px 120px 150px"
    );
    assert_eq!(template(&d, &t, "g", "grid-template-rows"), "20px 40px");
    // css-grid-2 §7.2.6.2: one line name set per line of the subgrid,
    // empty ones included.
    assert_eq!(
        template(&d, &t, "s", "grid-template-columns"),
        "subgrid [] [] [] []"
    );
    assert_eq!(template(&d, &t, "t", "grid-template-rows"), "40px");
}
