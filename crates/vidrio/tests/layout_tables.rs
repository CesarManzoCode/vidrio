//! Table layout end to end, measured with Ahem (read from
//! `bench/cache/wpt/fonts/Ahem.ttf`: every glyph is 1em wide, with
//! ascent 0.8em and descent 0.2em): automatic widths with a cell that
//! spans two columns, fixed layout with percentage columns, the
//! resolution of collapsed border conflicts and the baselines of cells,
//! plus captions below the table with `border-spacing`. Tests are
//! skipped with a message when Ahem is missing.

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use vgfx::font::Face;
use vgfx::font::matching::{Descriptor, StyleRange};
use vgfx::font::system::WebFace;
use vidrio::css::style::styles;
use vidrio::dom::{DocRef, NodeId};
use vidrio::layout::fragment::{Fragment, FragmentTree, Rect, Side, Sides};
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

/// The only fragment of `id`.
fn fragment<'a>(doc: &DocRef, tree: &'a FragmentTree, id: &str) -> &'a Fragment {
    let frags = tree.fragments_of(element(doc, id));
    assert_eq!(frags.len(), 1, "#{id} has one fragment");
    frags[0]
}

fn rect(doc: &DocRef, tree: &FragmentTree, id: &str) -> Rect {
    fragment(doc, tree, id).rect
}

/// The border box of a table: its wrapper box and its table grid box.
fn table_box(doc: &DocRef, tree: &FragmentTree, id: &str) -> Rect {
    tree.border_box(element(doc, id))
        .unwrap_or_else(|| panic!("#{id} has no box"))
}

/// The baseline of the first text run inside `id`.
fn text_baseline(doc: &DocRef, tree: &FragmentTree, id: &str) -> f64 {
    let f = fragment(doc, tree, id);
    let texts = f.texts();
    assert!(!texts.is_empty(), "#{id} has text");
    texts[0].1.baseline
}

fn r(x: f64, y: f64, width: f64, height: f64) -> Rect {
    Rect::new(x, y, width, height)
}

#[test]
fn automatic_widths_give_a_spanning_cell_its_width() {
    // The 100px word of `c` spans both columns, whose own cells only need
    // 30px and 10px: the 60px left over go to the columns in proportion
    // to their max-content widths (45 and 15).
    let cells = concat!(
        "<tr><td id=a>XXX</td><td id=b>X</td></tr>",
        "<tr><td id=c colspan=2>XXXXXXXXXX</td></tr>",
    );
    let style = "<style>table { border-spacing: 0 } td { padding: 0 }</style>";
    let Some((d, t)) = lay(&format!("{style}<table id=t>{cells}</table>")) else {
        return;
    };
    assert_eq!(rect(&d, &t, "a"), r(0.0, 0.0, 75.0, 10.0));
    assert_eq!(rect(&d, &t, "b"), r(75.0, 0.0, 25.0, 10.0));
    assert_eq!(rect(&d, &t, "c"), r(0.0, 10.0, 100.0, 10.0));
    assert_eq!(table_box(&d, &t, "t"), r(0.0, 0.0, 100.0, 20.0));

    // A wider table: the space past max-content goes to the automatic
    // columns in proportion to their max-content widths too.
    let Some((d, t)) = lay(&format!(
        "{style}<table id=t style='width: 300px'>{cells}</table>"
    )) else {
        return;
    };
    assert_eq!(rect(&d, &t, "a"), r(0.0, 0.0, 225.0, 10.0));
    assert_eq!(rect(&d, &t, "b"), r(225.0, 0.0, 75.0, 10.0));
    assert_eq!(rect(&d, &t, "c"), r(0.0, 10.0, 300.0, 10.0));
}

#[test]
fn fixed_layout_resolves_percentage_columns_against_the_table() {
    // Fixed layout: 25% and 50% of 400px, the automatic column gets the
    // rest, and the long word of `c` does not widen it.
    let Some((d, t)) = lay(concat!(
        "<style>table { border-spacing: 0 } td { padding: 0 }</style>",
        "<table id=t style='table-layout: fixed; width: 400px'>",
        "<col style='width: 25%'><col style='width: 50%'><col>",
        "<tr><td id=a>X</td><td id=b>X</td>",
        "<td id=c>XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX</td></tr>",
        "</table>",
    )) else {
        return;
    };
    assert_eq!(rect(&d, &t, "a"), r(0.0, 0.0, 100.0, 10.0));
    assert_eq!(rect(&d, &t, "b"), r(100.0, 0.0, 200.0, 10.0));
    assert_eq!(rect(&d, &t, "c"), r(300.0, 0.0, 100.0, 10.0));
    assert_eq!(table_box(&d, &t, "t"), r(0.0, 0.0, 400.0, 10.0));
}

#[test]
fn collapsed_borders_resolve_conflicts_and_share_their_width() {
    // Between `a` and `b` the wider border wins (6px dotted over 2px
    // solid); between `c` and `d`, of equal width, `double` wins over
    // `dashed`. Each cell takes half of the border on each side, the
    // table half of its outer borders (1px at the top and on the left).
    let Some((d, t)) = lay(concat!(
        "<style>table { border-collapse: collapse } td { padding: 0 }</style>",
        "<table id=t>",
        "<tr><td id=a style='border: 2px solid'>X</td>",
        "<td id=b style='border-left: 6px dotted'>X</td></tr>",
        "<tr><td id=c style='border-right: 4px dashed'>X</td>",
        "<td id=d style='border-left: 4px double'>X</td></tr>",
        "</table>",
    )) else {
        return;
    };
    let a = fragment(&d, &t, "a");
    assert_eq!(a.border, Sides::new(1.0, 3.0, 1.0, 1.0));
    let right = &a.collapsed_borders.as_ref().expect("collapsed borders")[1];
    assert_eq!(right.width, 6.0);
    assert_eq!(right.side, Side::Left);
    let source = right.source.as_ref().expect("a winning border");
    assert_eq!(source.text("border-left-style"), "dotted");

    let c = fragment(&d, &t, "c");
    assert_eq!(c.border, Sides::new(1.0, 2.0, 0.0, 0.0));
    let right = &c.collapsed_borders.as_ref().expect("collapsed borders")[1];
    assert_eq!(right.width, 4.0);
    assert_eq!(right.side, Side::Left);
    let source = right.source.as_ref().expect("a winning border");
    assert_eq!(source.text("border-left-style"), "double");

    // Columns of 14px (10 + 1 + 3) and 13px (3 + 10), rows of 12px
    // (1 + 10 + 1) and 11px (1 + 10), inside the table's 1px borders.
    assert_eq!(a.rect, r(1.0, 1.0, 14.0, 12.0));
    assert_eq!(rect(&d, &t, "b"), r(15.0, 1.0, 13.0, 12.0));
    assert_eq!(c.rect, r(1.0, 13.0, 14.0, 11.0));
    assert_eq!(rect(&d, &t, "d"), r(15.0, 13.0, 13.0, 11.0));
    assert_eq!(table_box(&d, &t, "t"), r(0.0, 0.0, 28.0, 24.0));
}

#[test]
fn baseline_cells_share_the_row_baseline() {
    // `a` (10px font, baseline 8px down) and `b` (30px font, baseline
    // 24px down) share the row's baseline at 24px; the row is 30px tall
    // (24 above the baseline, 6 below). `c` is centered and `d` at the
    // bottom.
    let Some((d, t)) = lay(concat!(
        "<style>table { border-spacing: 0 } td { padding: 0; vertical-align: baseline }</style>",
        "<table id=t><tr>",
        "<td id=a>X</td>",
        "<td id=b style='font-size: 30px'>X</td>",
        "<td id=c style='vertical-align: middle'>X</td>",
        "<td id=d style='vertical-align: bottom'>X</td>",
        "</tr></table>",
    )) else {
        return;
    };
    assert_eq!(text_baseline(&d, &t, "a"), 24.0);
    assert_eq!(text_baseline(&d, &t, "b"), 24.0);
    assert_eq!(text_baseline(&d, &t, "c"), 18.0);
    assert_eq!(text_baseline(&d, &t, "d"), 28.0);
    assert_eq!(rect(&d, &t, "a"), r(0.0, 0.0, 10.0, 30.0));
    assert_eq!(rect(&d, &t, "b"), r(10.0, 0.0, 30.0, 30.0));
    assert_eq!(rect(&d, &t, "d"), r(50.0, 0.0, 10.0, 30.0));
    assert_eq!(table_box(&d, &t, "t"), r(0.0, 0.0, 60.0, 30.0));
}

#[test]
fn captions_go_below_and_spacing_surrounds_the_cells() {
    // `border-spacing: 4px 6px` around and between two 10px columns:
    // the table is 32px wide and 22px tall, and the caption follows it.
    let Some((d, t)) = lay(concat!(
        "<style>td { padding: 0 }</style>",
        "<table id=t style='border-spacing: 4px 6px'>",
        "<caption id=cap style='caption-side: bottom'>XX</caption>",
        "<tr><td id=a>X</td><td id=b>X</td></tr>",
        "</table>",
    )) else {
        return;
    };
    assert_eq!(rect(&d, &t, "a"), r(4.0, 6.0, 10.0, 10.0));
    assert_eq!(rect(&d, &t, "b"), r(18.0, 6.0, 10.0, 10.0));
    assert_eq!(rect(&d, &t, "cap"), r(0.0, 22.0, 32.0, 10.0));
    assert_eq!(table_box(&d, &t, "t"), r(0.0, 0.0, 32.0, 32.0));
}
