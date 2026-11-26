//! Image resources for painting: the images of `<img>` elements (their
//! `src`) and of the `url()` values of `background-image` and
//! `border-image-source` (of elements and of their `::before` and
//! `::after`), fetched once per absolute URL, decoded by vgfx and kept
//! for the life of the thread. A fetch delays its document's load event
//! like a style sheet (HTML §4.8.4.3.4 for images), so a screenshot taken
//! after `load` sees them.
//!
//! URLs in style sheets resolve against the document's base URL.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use vgfx::image::Image;
use vjs::Agent;

use super::build::Resources;
use crate::css::cascade::Pseudo;
use crate::css::style::ComputedStyle;
use crate::css::values::Value;
use crate::dom::{DocRef, Document, NodeId};
use crate::url::Url;

#[derive(Clone)]
enum Entry {
    Loading,
    Ready(Rc<Image>),
    Broken,
}

thread_local! {
    static STORE: RefCell<HashMap<String, Entry>> = RefCell::new(HashMap::new());
    /// Grows whenever an image arrives or breaks (layout depends on
    /// natural sizes).
    static GENERATION: Cell<u64> = const { Cell::new(0) };
}

/// The generation of the image store.
pub fn generation() -> u64 {
    GENERATION.with(Cell::get)
}

fn bump() {
    GENERATION.with(|g| g.set(g.get().wrapping_add(1)));
}

/// The decoded image of an absolute URL, once it has arrived.
pub fn get(url: &str) -> Option<Rc<Image>> {
    STORE.with(|s| match s.borrow().get(url) {
        Some(Entry::Ready(i)) => Some(i.clone()),
        _ => None,
    })
}

/// Stores a decoded image for `url` (as if it had been fetched).
pub fn insert(url: &str, image: Image) {
    STORE.with(|s| {
        s.borrow_mut()
            .insert(url.to_string(), Entry::Ready(Rc::new(image)))
    });
    bump();
}

/// Decodes and stores `bytes` for `url`; false if they do not decode.
pub fn insert_bytes(url: &str, bytes: &[u8]) -> bool {
    let entry = match vgfx::image::decode(bytes) {
        Ok(i) => Entry::Ready(Rc::new(i)),
        Err(_) => Entry::Broken,
    };
    let ok = matches!(entry, Entry::Ready(_));
    STORE.with(|s| s.borrow_mut().insert(url.to_string(), entry));
    bump();
    ok
}

/// The URL an element shows as its image, resolved against `base`.
pub fn element_url(doc: &Document, node: NodeId, base: &Url) -> Option<String> {
    let e = doc.element(node)?;
    let src = if e.is_html("img")
        || (e.is_html("input")
            && e.attr("type")
                .is_some_and(|t| t.eq_ignore_ascii_case("image")))
    {
        e.attr("src")?
    } else if e.is_html("video") {
        e.attr("poster")?
    } else {
        return None;
    };
    let src = src.trim();
    if src.is_empty() {
        return None;
    }
    Url::parse(src, Some(base)).ok().map(|u| u.href())
}

/// The `url()` images of a computed style.
fn style_urls(style: &ComputedStyle, out: &mut Vec<String>) {
    for name in ["background-image", "border-image-source"] {
        if let Some(values) = style.get(name).and_then(|v| v.values.as_deref()) {
            for v in values {
                if let Value::Url(u) = v {
                    out.push(u.clone());
                }
            }
        }
    }
}

/// Starts fetching every image `doc` uses that is not known yet.
pub fn update_document(agent: &mut Agent, doc: &DocRef) {
    let _ = agent;
    let Some(ua) = crate::browser::ua() else {
        return;
    };
    let base = crate::browser::document::base_url(doc);
    let styles = crate::css::style::styles(doc);
    let mut urls = Vec::new();
    {
        let d = doc.borrow();
        for n in d.inclusive_descendants(NodeId::DOCUMENT) {
            if !d.is_element(n) {
                continue;
            }
            if let Some(u) = element_url(&d, n, &base) {
                urls.push(u);
            }
            let mut css = Vec::new();
            if let Some(s) = styles.get(n) {
                style_urls(s, &mut css);
            }
            for p in [Pseudo::Before, Pseudo::After] {
                if let Some(s) = styles.pseudo(n, p) {
                    style_urls(s, &mut css);
                }
            }
            for u in css {
                if let Ok(abs) = Url::parse(&u, Some(&base)) {
                    urls.push(abs.href());
                }
            }
        }
    }
    for href in urls {
        let known = STORE.with(|s| s.borrow().contains_key(&href));
        if known {
            continue;
        }
        let Ok(url) = Url::parse(&href, None) else {
            continue;
        };
        STORE.with(|s| s.borrow_mut().insert(href.clone(), Entry::Loading));
        let info = ua.doc_info(doc);
        if let Some(info) = &info {
            info.load_delay.set(info.load_delay.get() + 1);
        }
        let doc2 = doc.clone();
        crate::browser::loader::fetch(&ua, url, Some(doc.clone()), move |agent, result| {
            let entry = match result {
                Ok(r) if (200..300).contains(&r.status) || r.status == 0 => {
                    match vgfx::image::decode(&r.body) {
                        Ok(i) => Entry::Ready(Rc::new(i)),
                        Err(_) => Entry::Broken,
                    }
                }
                _ => Entry::Broken,
            };
            STORE.with(|s| s.borrow_mut().insert(href, entry));
            bump();
            if let Some(info) = crate::browser::ua().and_then(|ua| ua.doc_info(&doc2)) {
                info.load_delay.set(info.load_delay.get().saturating_sub(1));
            }
            crate::browser::document::continue_the_end(agent, &doc2);
        });
    }
}

/// The images of one document: CSS URLs resolve against its base URL.
pub struct DocumentImages {
    base: Url,
    /// The image URL of each element that shows one.
    elements: HashMap<NodeId, String>,
}

impl DocumentImages {
    pub fn new(doc: &DocRef) -> DocumentImages {
        let base = crate::browser::document::base_url(doc);
        let mut elements = HashMap::new();
        let d = doc.borrow();
        for n in d.inclusive_descendants(NodeId::DOCUMENT) {
            if let Some(u) = element_url(&d, n, &base) {
                elements.insert(n, u);
            }
        }
        DocumentImages { base, elements }
    }

    /// The natural size of the image of `node`, once decoded.
    pub fn natural_size(&self, node: NodeId) -> Option<(f64, f64)> {
        self.element_image(node)
            .map(|i| (f64::from(i.width), f64::from(i.height)))
    }
}

impl Resources for DocumentImages {
    fn css_image(&self, url: &str) -> Option<Rc<Image>> {
        let abs = Url::parse(url, Some(&self.base)).ok()?;
        get(&abs.href())
    }

    fn element_image(&self, node: NodeId) -> Option<Rc<Image>> {
        get(self.elements.get(&node)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_images_are_found_by_url() {
        insert("http://example.test/a.png", Image::transparent(3, 2));
        assert_eq!(get("http://example.test/a.png").map(|i| i.width), Some(3));
        assert!(get("http://example.test/b.png").is_none());
        assert!(!insert_bytes("http://example.test/c.png", b"not an image"));
        assert!(get("http://example.test/c.png").is_none());
    }
}
