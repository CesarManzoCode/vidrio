//! The style sheets of documents (CSSOM §6.2, HTML §4.2.6): the sheet of
//! each `style` element, kept while the element's text does not change
//! ("update a style block" runs lazily, when the sheet is asked for), the
//! sheet each `link rel=stylesheet` fetched, `document.styleSheets` in
//! tree order, and the adopted style sheets of documents and shadow
//! roots.

use std::cell::RefCell;
use std::rc::{Rc, Weak};

use super::media;
use super::om::{self, SheetRef};
use crate::dom::{DocRef, Document, NodeId};

/// The sheet of one owner node.
struct OwnerEntry {
    node: NodeId,
    /// The text the sheet was parsed from (`style` elements).
    text: Option<String>,
    /// The `media` attribute the sheet's media list was set from.
    media: Option<String>,
    sheet: SheetRef,
}

/// The sheets of one document.
struct DocSheets {
    doc: Weak<RefCell<Document>>,
    owners: Vec<OwnerEntry>,
    /// Adopted style sheets, by the document or shadow root node.
    adopted: Vec<(NodeId, Vec<SheetRef>)>,
    /// `link` elements whose sheet is loading and blocks scripts.
    blocking: Vec<NodeId>,
}

thread_local! {
    static STORE: RefCell<Vec<DocSheets>> = const { RefCell::new(Vec::new()) };
}

fn with_doc<T>(doc: &DocRef, f: impl FnOnce(&mut DocSheets) -> T) -> T {
    STORE.with(|s| {
        let mut s = s.borrow_mut();
        s.retain(|d| d.doc.strong_count() > 0);
        let i = match s.iter().position(|d| d.doc.as_ptr() == Rc::as_ptr(doc)) {
            Some(i) => i,
            None => {
                s.push(DocSheets {
                    doc: Rc::downgrade(doc),
                    owners: Vec::new(),
                    adopted: Vec::new(),
                    blocking: Vec::new(),
                });
                s.len() - 1
            }
        };
        f(&mut s[i])
    })
}

/// Whether `node` is a `link` element whose sheet is for `rel=stylesheet`.
pub fn is_stylesheet_link(d: &Document, node: NodeId) -> bool {
    d.is_html_element(node, "link")
        && d.get_attribute(node, "rel").is_some_and(|rel| {
            rel.split_ascii_whitespace()
                .any(|t| t.eq_ignore_ascii_case("stylesheet"))
        })
}

/// Whether the `rel` of a stylesheet link also has `alternate`.
fn is_alternate(d: &Document, node: NodeId) -> bool {
    d.get_attribute(node, "rel").is_some_and(|rel| {
        rel.split_ascii_whitespace()
            .any(|t| t.eq_ignore_ascii_case("alternate"))
    })
}

/// Whether a `style` element's type allows a CSS sheet (HTML §4.2.6).
fn style_type_ok(d: &Document, node: NodeId) -> bool {
    match d.get_attribute(node, "type") {
        None => true,
        Some(t) => t.is_empty() || t.eq_ignore_ascii_case("text/css"),
    }
}

/// Whether `node` is in a document tree or a shadow tree that is connected.
fn in_tree(d: &Document, node: NodeId) -> bool {
    d.is_connected(node)
}

/// The sheet of a `style` element, created or replaced if its text
/// changed; `None` if the element has no sheet.
fn style_element_sheet(doc: &DocRef, node: NodeId) -> Option<SheetRef> {
    let (text, media_attr, title) = {
        let d = doc.borrow();
        if !d.is_html_element(node, "style") || !in_tree(&d, node) || !style_type_ok(&d, node) {
            return None;
        }
        (
            d.child_text_content(node),
            d.get_attribute(node, "media").map(str::to_string),
            d.get_attribute(node, "title").map(str::to_string),
        )
    };
    with_doc(doc, |s| {
        if let Some(e) = s.owners.iter_mut().find(|e| e.node == node) {
            if e.text.as_deref() == Some(text.as_str()) {
                if e.media != media_attr {
                    e.sheet.borrow_mut().media = media::parse(media_attr.as_deref().unwrap_or(""));
                    e.media = media_attr;
                }
                e.sheet.borrow_mut().title = title.filter(|t| !t.is_empty());
                return Some(e.sheet.clone());
            }
        }
        let sheet = om::parse_sheet(&text);
        {
            let mut sh = sheet.borrow_mut();
            sh.media = media::parse(media_attr.as_deref().unwrap_or(""));
            sh.title = title.filter(|t| !t.is_empty());
        }
        s.owners.retain(|e| e.node != node);
        s.owners.push(OwnerEntry {
            node,
            text: Some(text),
            media: media_attr,
            sheet: sheet.clone(),
        });
        Some(sheet)
    })
}

/// The sheet a `link` element fetched, if it is still a connected
/// stylesheet link.
fn link_sheet(doc: &DocRef, node: NodeId) -> Option<SheetRef> {
    {
        let d = doc.borrow();
        if !is_stylesheet_link(&d, node) || !in_tree(&d, node) {
            return None;
        }
    }
    with_doc(doc, |s| {
        s.owners
            .iter()
            .find(|e| e.node == node && e.text.is_none())
            .map(|e| e.sheet.clone())
    })
}

/// The associated CSS style sheet of `node` (`sheet` of `LinkStyle`).
pub fn sheet_of(doc: &DocRef, node: NodeId) -> Option<SheetRef> {
    let is_style = doc.borrow().is_html_element(node, "style");
    if is_style {
        style_element_sheet(doc, node)
    } else {
        link_sheet(doc, node)
    }
}

/// Sets (or, with `None`, removes) the sheet a `link` element fetched.
pub fn set_link_sheet(doc: &DocRef, node: NodeId, sheet: Option<SheetRef>) {
    let (media_attr, title, alternate) = {
        let d = doc.borrow();
        (
            d.get_attribute(node, "media").map(str::to_string),
            d.get_attribute(node, "title").map(str::to_string),
            is_alternate(&d, node),
        )
    };
    om::sheets_changed();
    with_doc(doc, |s| {
        s.owners.retain(|e| e.node != node);
        if let Some(sheet) = sheet {
            {
                let mut sh = sheet.borrow_mut();
                sh.media = media::parse(media_attr.as_deref().unwrap_or(""));
                sh.title = title.filter(|t| !t.is_empty());
                sh.alternate = alternate;
            }
            s.owners.push(OwnerEntry {
                node,
                text: None,
                media: media_attr,
                sheet,
            });
        }
    });
}

/// The sheets of the document or shadow root `root` in tree order with
/// their owner nodes (`styleSheets`).
pub fn style_sheets(doc: &DocRef, root: NodeId) -> Vec<(NodeId, SheetRef)> {
    let nodes: Vec<NodeId> = {
        let d = doc.borrow();
        d.inclusive_descendants(root)
            .into_iter()
            .filter(|&n| d.is_html_element(n, "style") || is_stylesheet_link(&d, n))
            .collect()
    };
    nodes
        .into_iter()
        .filter_map(|n| sheet_of(doc, n).map(|s| (n, s)))
        .collect()
}

/// The owner node of `sheet`, if a `style` or `link` element of `doc`
/// owns it.
pub fn owner_node(doc: &DocRef, sheet: &SheetRef) -> Option<NodeId> {
    with_doc(doc, |s| {
        s.owners
            .iter()
            .find(|e| Rc::ptr_eq(&e.sheet, sheet))
            .map(|e| e.node)
    })
}

/// The adopted style sheets of the document or shadow root `root`.
pub fn adopted(doc: &DocRef, root: NodeId) -> Vec<SheetRef> {
    with_doc(doc, |s| {
        s.adopted
            .iter()
            .find(|(n, _)| *n == root)
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    })
}

/// Sets the adopted style sheets of `root`.
pub fn set_adopted(doc: &DocRef, root: NodeId, sheets: Vec<SheetRef>) {
    om::sheets_changed();
    with_doc(doc, |s| {
        s.adopted.retain(|(n, _)| *n != root);
        s.adopted.push((root, sheets));
    });
}

/// Every sheet that applies to `doc`'s document tree, in cascade order:
/// the owner sheets, then the adopted ones (without disabled sheets and
/// sheets whose media does not match).
pub fn applicable_sheets(doc: &DocRef, env: &media::Environment) -> Vec<SheetRef> {
    let mut out: Vec<SheetRef> = style_sheets(doc, NodeId::DOCUMENT)
        .into_iter()
        .map(|(_, s)| s)
        .collect();
    out.extend(adopted(doc, NodeId::DOCUMENT));
    out.retain(|s| {
        let s = s.borrow();
        !s.disabled && !s.alternate && s.media.matches(env)
    });
    out
}

/// A `link` element starts blocking scripts.
pub fn add_blocking(doc: &DocRef, node: NodeId) {
    with_doc(doc, |s| {
        if !s.blocking.contains(&node) {
            s.blocking.push(node);
        }
    });
}

/// A `link` element stops blocking scripts.
pub fn remove_blocking(doc: &DocRef, node: NodeId) {
    with_doc(doc, |s| s.blocking.retain(|n| *n != node));
}

/// "Has a style sheet that is blocking scripts".
pub fn has_blocking(doc: &DocRef) -> bool {
    with_doc(doc, |s| !s.blocking.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn style_element_sheet_follows_its_text() {
        let doc: DocRef = Rc::new(RefCell::new(Document::new_html()));
        let style = {
            let mut d = doc.borrow_mut();
            let s = d.create_html_element("style");
            let t = d.create_text("a { color: red }");
            d.append_child(s, t);
            d.append_child(NodeId::DOCUMENT, s);
            s
        };
        let first = sheet_of(&doc, style).expect("a sheet");
        assert_eq!(first.borrow().rules.len(), 1);
        let again = sheet_of(&doc, style).expect("a sheet");
        assert!(Rc::ptr_eq(&first, &again));
        assert_eq!(style_sheets(&doc, NodeId::DOCUMENT).len(), 1);
        assert_eq!(owner_node(&doc, &first), Some(style));
    }
}
