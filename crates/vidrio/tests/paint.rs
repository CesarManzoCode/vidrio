//! Painting end to end: HTML through style, layout, the paint list and
//! the rasterizer into the 800×600 framebuffer. Exact pixels of a
//! pixel-aligned box with a solid border (and the screenshot PNG that
//! carries them), the Appendix E order of a negative `z-index` (under
//! the in-flow blocks of its stacking context) against a positive one,
//! and content clipped by a rounded `overflow: hidden`. No test paints
//! text, so no font is needed.

use std::cell::RefCell;
use std::rc::Rc;

use vidrio::dom::DocRef;
use vidrio::paint::{Framebuffer, NoResources, PaintOptions, paint_document};

const RESET: &str = "<style>html,body{margin:0}</style>";
const WHITE: [u8; 4] = [255, 255, 255, 255];
const RED: [u8; 4] = [255, 0, 0, 255];
const GREEN: [u8; 4] = [0, 128, 0, 255];
const BLUE: [u8; 4] = [0, 0, 255, 255];

/// Paints `RESET` + `body` in the 800×600 viewport, without fonts.
fn paint(body: &str) -> Framebuffer {
    let html = format!("<!DOCTYPE html>{RESET}{body}");
    let doc: DocRef = Rc::new(RefCell::new(vidrio::html::parse_document(&html, true)));
    let options = PaintOptions {
        viewport: (800, 600),
        system_fonts: false,
        web_fonts: Vec::new(),
    };
    paint_document(&doc, &NoResources, &options)
}

#[test]
fn aligned_box_with_solid_border_is_exact() {
    let fb = paint(
        "<div style=\"position:absolute;left:10px;top:20px;width:30px;height:20px;\
         border:5px solid rgb(0,0,255);background-color:rgb(0,128,0)\"></div>",
    );
    assert_eq!((fb.width, fb.height), (800, 600));
    // Border box (10, 20)–(50, 50), padding box (15, 25)–(45, 45).
    for y in 0..60 {
        for x in 0..60 {
            let expected = if !(10..50).contains(&x) || !(20..50).contains(&y) {
                WHITE
            } else if (15..45).contains(&x) && (25..45).contains(&y) {
                GREEN
            } else {
                BLUE
            };
            assert_eq!(fb.pixel(x, y), expected, "pixel ({x}, {y})");
        }
    }
    // The screenshot PNG carries the same pixels.
    let png = vgfx::png::decode(&fb.to_png()).expect("screenshot decodes");
    assert_eq!((png.width, png.height), (800, 600));
    assert_eq!(png.pixel(12, 22), BLUE);
    assert_eq!(png.pixel(30, 35), GREEN);
    assert_eq!(png.pixel(5, 5), WHITE);
}

#[test]
fn negative_z_index_paints_under_in_flow_blocks() {
    let page = |z: i32| {
        paint(&format!(
            "<div style=\"width:100px;height:100px;background-color:rgb(0,128,0)\"></div>\
             <div style=\"position:absolute;z-index:{z};left:50px;top:50px;width:100px;\
             height:100px;background-color:rgb(255,0,0)\"></div>"
        ))
    };
    // Appendix E: the contexts with negative z-index come right after the
    // background of the root context, before its in-flow blocks.
    let fb = page(-1);
    assert_eq!(fb.pixel(75, 75), GREEN);
    assert_eq!(fb.pixel(25, 25), GREEN);
    assert_eq!(fb.pixel(125, 125), RED);
    assert_eq!(fb.pixel(125, 75), RED);
    // With a positive z-index the same box goes over the block.
    let fb = page(1);
    assert_eq!(fb.pixel(75, 75), RED);
    assert_eq!(fb.pixel(25, 25), GREEN);
}

#[test]
fn rounded_overflow_clips_content() {
    let fb = paint(
        "<div style=\"position:absolute;left:0;top:0;width:100px;height:100px;\
         border-radius:50px;overflow:hidden;background-color:rgb(0,0,255)\">\
         <div style=\"width:100px;height:100px;background-color:rgb(255,0,0)\"></div></div>",
    );
    // Inside the circle of radius 50 around (50, 50): the child.
    assert_eq!(fb.pixel(50, 50), RED);
    assert_eq!(fb.pixel(50, 5), RED);
    assert_eq!(fb.pixel(5, 50), RED);
    // The corners are wholly outside the circle: the canvas shows.
    for (x, y) in [(5, 5), (94, 5), (94, 94), (5, 94), (0, 0), (99, 99)] {
        assert_eq!(fb.pixel(x, y), WHITE, "pixel ({x}, {y})");
    }
    // A pixel the curve crosses is antialiased: partly the child, partly
    // the canvas (and the parent's background under the child).
    let edge = fb.pixel(14, 14);
    assert_ne!(edge, WHITE);
    assert_ne!(edge, RED);
    assert!(edge[0] > edge[1], "{edge:?}");
}
