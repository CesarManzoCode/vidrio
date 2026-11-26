//! Positioned layout end to end, measured with Ahem (read from
//! `bench/cache/wpt/fonts/Ahem.ttf`: every glyph is 1em wide, with
//! ascent 0.8em and descent 0.2em): absolutely positioned boxes whose
//! containing block is an inline box, over-constrained equations in an
//! `rtl` containing block (with logical margins), sticky boxes in a
//! scroll container, and `aspect-ratio` with `min-height`. Tests are
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
use vidrio::layout::fragment::{Fragment, FragmentKind, FragmentTree, Overflow, Rect, Stacking};
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

/// The only fragment of `id`.
fn frag<'a>(doc: &DocRef, tree: &'a FragmentTree, id: &str) -> &'a Fragment {
    let frags = tree.fragments_of(element(doc, id));
    assert_eq!(frags.len(), 1, "#{id} has one fragment");
    frags[0]
}

fn rect(doc: &DocRef, tree: &FragmentTree, id: &str) -> Rect {
    frag(doc, tree, id).rect
}

fn r(x: f64, y: f64, width: f64, height: f64) -> Rect {
    Rect::new(x, y, width, height)
}

#[test]
fn absolute_boxes_in_an_inline_containing_block() {
    let Some((d, t)) = lay(concat!(
        "<div style='padding-left:50px'>xxxx",
        "<span id=cb style='position:relative; padding:5px'>abc",
        "<b id=a style='position:absolute; left:10px; top:7px; width:20px; height:20px'></b>",
        "<b id=b style='position:absolute; right:0; bottom:0; width:4px; height:4px'></b>",
        "</span></div>",
        "<div id=icb style='position:absolute; right:0; bottom:0; width:10px; height:10px'></div>",
    )) else {
        return;
    };
    // The span starts after "xxxx" (50 + 40) and its padding box is 5px
    // around the content area of "abc" (ascent 8 + descent 2 at the top
    // of the line).
    assert_eq!(rect(&d, &t, "cb"), r(90.0, -5.0, 40.0, 20.0));
    // Its padding box is the containing block of the absolute boxes.
    assert_eq!(rect(&d, &t, "a"), r(100.0, 2.0, 20.0, 20.0));
    assert_eq!(rect(&d, &t, "b"), r(126.0, 11.0, 4.0, 4.0));
    assert!(matches!(frag(&d, &t, "a").kind, FragmentKind::OutOfFlow));
    // Without a positioned ancestor, the initial containing block.
    assert_eq!(rect(&d, &t, "icb"), r(790.0, 590.0, 10.0, 10.0));
    // No placeholder is left behind.
    assert!(
        t.root
            .descendants()
            .iter()
            .all(|f| !matches!(f.kind, FragmentKind::Placeholder))
    );
    // A positioned box with `z-index: auto` does not create a stacking
    // context; the root element does.
    assert_eq!(frag(&d, &t, "a").stacking, Stacking::Positioned);
    assert_eq!(t.root.children[0].stacking, Stacking::Context(0));
}

#[test]
fn over_constrained_absolute_boxes_in_rtl() {
    let Some((d, t)) = lay(concat!(
        "<div style='margin-left:100px; width:200px'>",
        "<div style='position:relative; direction:rtl; height:100px'>",
        // margin-inline-start is the right margin in rtl.
        "<div id=o style='position:absolute; left:10px; right:30px; width:50px; ",
        "margin-inline-start:7px; margin-inline-end:5px; height:10px'></div>",
        "<div id=neg style='position:absolute; left:0; right:0; top:20px; width:300px; ",
        "margin:0 auto; height:10px'></div>",
        "<div id=st style='position:absolute; width:20px; height:10px'></div>",
        "</div></div>",
        "<div style='position:relative; width:200px; height:100px; margin-left:100px'>",
        "<div id=ltr style='position:absolute; left:10px; right:30px; width:50px; ",
        "margin-inline-start:5px; margin-inline-end:7px; height:10px'></div>",
        "</div>",
    )) else {
        return;
    };
    // `left` is ignored: the right margin edge is 30px from the right
    // padding edge (300), with a 7px right margin.
    let o = frag(&d, &t, "o");
    assert_eq!(o.rect, r(213.0, 0.0, 50.0, 10.0));
    assert_eq!((o.margin.left, o.margin.right), (5.0, 7.0));
    // In ltr, `right` is ignored instead.
    assert_eq!(rect(&d, &t, "ltr"), r(115.0, 100.0, 50.0, 10.0));
    // Equal auto margins would be negative: in rtl the left one takes
    // all of it, so the box ends at the right edge.
    let neg = frag(&d, &t, "neg");
    assert_eq!(neg.rect, r(0.0, 20.0, 300.0, 10.0));
    assert_eq!((neg.margin.left, neg.margin.right), (-100.0, 0.0));
    // All of `left`, `width` and `right` auto in rtl: the right static
    // position.
    assert_eq!(rect(&d, &t, "st"), r(280.0, 0.0, 20.0, 10.0));
}

#[test]
fn sticky_boxes_in_a_scroll_container() {
    let Some((d, t)) = lay(concat!(
        "<div id=sc style='overflow:auto; width:200px; height:100px; margin-top:50px'>",
        "<div style='height:20px'></div>",
        "<div id=st style='position:sticky; top:30px; height:10px'></div>",
        "<div id=par style='height:15px'>",
        "<div id=st2 style='position:sticky; top:60px; height:10px'></div>",
        "</div>",
        "<div style='height:300px'></div>",
        "</div>",
    )) else {
        return;
    };
    // In flow at 70, it sticks 30px below the scrollport top (50).
    assert_eq!(rect(&d, &t, "st"), r(0.0, 80.0, 200.0, 10.0));
    // It would stick at 110, but stays inside its containing block
    // (#par, 80 to 95).
    assert_eq!(rect(&d, &t, "par"), r(0.0, 80.0, 200.0, 15.0));
    assert_eq!(rect(&d, &t, "st2"), r(0.0, 85.0, 200.0, 10.0));
    // Sticky boxes create stacking contexts.
    assert_eq!(frag(&d, &t, "st").stacking, Stacking::Context(0));
    // The scroll container: scroll bars take no space, so the scrollport
    // is the padding box; the scrollable area holds all the content.
    let sc = frag(&d, &t, "sc");
    let scroll = sc.scroll.as_ref().expect("#sc is a scroll container");
    assert_eq!(scroll.overflow_x, Overflow::Auto);
    assert_eq!(scroll.overflow_y, Overflow::Auto);
    assert_eq!(scroll.scrollport, r(0.0, 50.0, 200.0, 100.0));
    assert_eq!(scroll.scrollable, r(0.0, 50.0, 200.0, 345.0));
    let clip = sc.clip.expect("#sc clips");
    assert_eq!(clip.y, Some((50.0, 150.0)));
    // The viewport is a scroll container too.
    assert!(t.root.scroll.is_some());
}

#[test]
fn aspect_ratio_with_min_height() {
    let Some((d, t)) = lay(concat!(
        "<div id=plain style='width:100px; aspect-ratio:2/1'></div>",
        "<div id=min style='width:100px; aspect-ratio:2/1; min-height:80px'></div>",
        "<div id=max style='width:100px; aspect-ratio:2/1; min-height:10px; max-height:30px'></div>",
        "<div id=tall style='width:100px; aspect-ratio:2/1'><div style='height:70px'></div></div>",
        "<div id=zero style='width:100px; aspect-ratio:2/1; min-height:0'>",
        "<div style='height:70px'></div></div>",
        "<div id=wide style='height:30px; aspect-ratio:2/1'></div>",
        "<div style='position:relative'>",
        "<div id=abs style='position:absolute; width:100px; aspect-ratio:2/1; min-height:80px'>",
        "</div></div>",
    )) else {
        return;
    };
    assert_eq!(rect(&d, &t, "plain"), r(0.0, 0.0, 100.0, 50.0));
    // `min-height` wins over the ratio.
    assert_eq!(rect(&d, &t, "min"), r(0.0, 50.0, 100.0, 80.0));
    assert_eq!(rect(&d, &t, "max"), r(0.0, 130.0, 100.0, 30.0));
    // With `min-height: auto` the content can make the box taller; with
    // `min-height: 0` it overflows.
    assert_eq!(rect(&d, &t, "tall"), r(0.0, 160.0, 100.0, 70.0));
    assert_eq!(rect(&d, &t, "zero"), r(0.0, 230.0, 100.0, 50.0));
    // A definite height gives the width.
    assert_eq!(rect(&d, &t, "wide"), r(0.0, 280.0, 60.0, 30.0));
    // The same for an absolutely positioned box.
    assert_eq!(rect(&d, &t, "abs"), r(0.0, 310.0, 100.0, 80.0));
}
