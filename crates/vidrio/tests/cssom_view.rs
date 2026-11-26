//! CSSOM View from script over known geometry: `getBoundingClientRect`
//! (and `getClientRects`) of absolutely positioned boxes, before and
//! after the viewport scrolls, and `elementFromPoint` over overlapping
//! boxes, the empty canvas and a point outside the viewport. No box
//! holds text, so no font is needed.

use std::time::Duration;

use vidrio::browser::{Browser, BrowserOptions, ClockMode, ContextId};

fn page() -> (Browser, ContextId) {
    let mut b = Browser::new(BrowserOptions {
        clock: ClockMode::Virtual,
        ..BrowserOptions::default()
    });
    let id = b.open("about:blank").expect("about:blank");
    b.run_until_idle(Duration::from_secs(5));
    let r = b.evaluate_to_string(
        id,
        r#"document.head.innerHTML = '<style>html, body { margin: 0 } div { position: absolute }</style>';
           document.body.innerHTML =
             '<div id=a style="left: 10px; top: 20px; width: 100px; height: 50px"></div>' +
             '<div id=b style="left: 60px; top: 40px; width: 100px; height: 100px; z-index: 1"></div>' +
             '<div id=tall style="left: 0; top: 0; width: 10px; height: 2000px; z-index: -1"></div>';
           'ok'"#,
    );
    assert_eq!(r, "ok");
    (b, id)
}

#[test]
fn bounding_client_rect_of_known_boxes() {
    let (mut b, id) = page();
    assert_eq!(
        b.evaluate_to_string(
            id,
            "JSON.stringify(document.getElementById('a').getBoundingClientRect())"
        ),
        r#"{"x":10,"y":20,"width":100,"height":50,"top":20,"right":110,"bottom":70,"left":10}"#
    );
    assert_eq!(
        b.evaluate_to_string(
            id,
            "var r = document.getElementById('b').getClientRects(); \
             [r.length, r[0].left, r[0].top, r[0].right, r[0].bottom].join()"
        ),
        "1,60,40,160,140"
    );
    // A box that is not rendered has no rectangles.
    assert_eq!(
        b.evaluate_to_string(
            id,
            "var e = document.createElement('div'); \
             var r = e.getBoundingClientRect(); \
             [e.getClientRects().length, r.x, r.y, r.width, r.height].join()"
        ),
        "0,0,0,0,0"
    );
    // The viewport scrolls: client rectangles move up, offsets do not.
    assert_eq!(
        b.evaluate_to_string(
            id,
            "window.scrollTo(0, 100); \
             var a = document.getElementById('a'); \
             [window.scrollY, a.getBoundingClientRect().top, a.offsetTop].join()"
        ),
        "100,-80,20"
    );
}

#[test]
fn element_from_point_follows_painting_order() {
    let (mut b, id) = page();
    let at = |b: &mut Browser, x: u32, y: u32| {
        b.evaluate_to_string(
            id,
            &format!(
                "var e = document.elementFromPoint({x}, {y}); \
                 e === null ? 'null' : (e.id || e.localName)"
            ),
        )
    };
    // Only `a` covers (20, 30).
    assert_eq!(at(&mut b, 20, 30), "a");
    // `b` (z-index 1) is painted over `a` where they overlap.
    assert_eq!(at(&mut b, 80, 50), "b");
    assert_eq!(at(&mut b, 150, 130), "b");
    // `tall` (z-index -1) is under the canvas content but over nothing.
    assert_eq!(at(&mut b, 5, 500), "tall");
    // The empty canvas is the root element; outside the viewport is null.
    assert_eq!(at(&mut b, 700, 500), "html");
    assert_eq!(at(&mut b, 900, 10), "null");
    assert_eq!(
        b.evaluate_to_string(
            id,
            "document.elementsFromPoint(80, 50).map(e => e.id || e.localName).join()"
        ),
        // The body has no height (its children are out of flow); the
        // root element always comes last.
        "b,a,html"
    );
}
