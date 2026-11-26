//! The first frame test of vault/CONTRACT.md §5.8, headless: the page of
//! `tests/first-frame` loads from its `file:` URL in a browser without a
//! window, and once its `load` event fired (the moment the window shows
//! its first frame) the painter the window and WebDriver share renders
//! it. Every pixel must equal `expected.png`, which
//! `tests/first-frame/make_expected.py` writes from the same rectangles
//! without vidrio, decoded here by the PNG decoder of vgfx.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use vidrio::browser::{Browser, BrowserOptions};
use vidrio::paint::{DocumentPainter, Painter};

fn first_frame_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/first-frame")
}

#[test]
fn first_frame_matches_expected_png() {
    let dir = first_frame_dir().canonicalize().expect("tests/first-frame");
    let url = format!("file://{}", dir.join("page.html").display());
    let mut browser = Browser::new(BrowserOptions::default());
    let id = browser.open(&url).expect("the page opens");

    // Wait for the `load` event of the page's document.
    let start = Instant::now();
    let doc = loop {
        browser.run_until_idle(Duration::from_millis(100));
        if let Some(doc) = browser.document(id) {
            let loaded = doc.borrow().url.href() == url
                && browser
                    .ua
                    .doc_info(&doc)
                    .is_some_and(|i| i.completely_loaded.get());
            if loaded {
                break doc;
            }
        }
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "the page did not load"
        );
    };

    let fb = DocumentPainter::new().paint(&mut browser.agent, &doc);
    assert_eq!((fb.width, fb.height), (800, 600));
    let rgb = fb.to_rgb();

    let bytes = std::fs::read(dir.join("expected.png")).expect("expected.png");
    let expected = vgfx::png::decode(&bytes).expect("expected.png decodes");
    assert_eq!((expected.width, expected.height), (800, 600));

    let mut mismatches = 0;
    let mut first = None;
    for y in 0..600u32 {
        for x in 0..800u32 {
            let e = expected.pixel(x, y);
            let i = (y as usize * 800 + x as usize) * 3;
            let got = [rgb[i], rgb[i + 1], rgb[i + 2]];
            if got != [e[0], e[1], e[2]] {
                mismatches += 1;
                first.get_or_insert((x, y, got, e));
            }
        }
    }
    assert_eq!(
        mismatches, 0,
        "{mismatches} pixels differ; first (x, y, vidrio, expected): {first:?}"
    );
}
