//! Style sheets that load (HTML §4.2.4 and §4.6.7.3, CSS Cascade §6):
//! a `link rel=stylesheet` fetches its sheet, delays the load event,
//! blocks scripts while the parser runs, and fires `load` or `error`;
//! the `@import` rules of a fetched or inline sheet fetch theirs.

use std::rc::Rc;

use vjs::Agent;

use crate::css::om::{self, RuleKind, SheetRef};
use crate::css::sheets;
use crate::dom::events::{EventInit, Target};
use crate::dom::{DocRef, NodeId};
use crate::url::Url;

/// The deepest `@import` chain that is followed.
const MAX_IMPORT_DEPTH: usize = 16;

/// Decodes a sheet's bytes: a BOM or the `charset` of its content type
/// (CSS Syntax §3.2), else UTF-8.
fn decode(body: &[u8], content_type: Option<&str>) -> String {
    super::loader::decode_script(body, content_type, crate::encoding::Encoding::Utf8)
}

/// Runs when a `link` element is inserted, or its `rel`, `href`,
/// `media` or `type` change: (re)fetches its sheet if it is a connected
/// stylesheet link with a URL, and drops the old sheet otherwise.
pub fn link_changed(doc: &DocRef, node: NodeId) {
    let (is_link, href) = {
        let d = doc.borrow();
        (
            sheets::is_stylesheet_link(&d, node) && d.is_connected(node),
            d.get_attribute(node, "href").map(str::to_string),
        )
    };
    sheets::set_link_sheet(doc, node, None);
    if !is_link {
        return;
    }
    let Some(ua) = super::ua() else {
        return;
    };
    let Some(info) = ua.doc_info(doc) else {
        return;
    };
    let base = super::document::base_url(doc);
    let url = match href.as_deref().filter(|h| !h.is_empty()) {
        Some(h) => Url::parse(h, Some(&base)).ok(),
        None => None,
    };
    let Some(url) = url else {
        // No URL: nothing loads, and nothing fires (HTML §4.6.7.3).
        return;
    };
    let blocking = info.active_parser().is_some();
    if blocking {
        sheets::add_blocking(doc, node);
    }
    info.load_delay.set(info.load_delay.get() + 1);
    let doc2 = doc.clone();
    super::loader::fetch(&ua, url, Some(doc.clone()), move |agent, result| {
        let sheet = match result {
            Ok(r) if (200..300).contains(&r.status) => {
                let css_type = r.content_type.as_deref().is_none_or(|t| {
                    let essence = t.split(';').next().unwrap_or("").trim();
                    essence.eq_ignore_ascii_case("text/css") || essence.is_empty()
                });
                css_type.then(|| {
                    let text = decode(&r.body, r.content_type.as_deref());
                    let sheet = om::parse_sheet(&text);
                    {
                        let mut s = sheet.borrow_mut();
                        s.href = Some(r.url.href());
                        s.base_url = Some(r.url.href());
                    }
                    sheet
                })
            }
            _ => None,
        };
        let still_link = {
            let d = doc2.borrow();
            sheets::is_stylesheet_link(&d, node) && d.is_connected(node)
        };
        let ok = sheet.is_some();
        if still_link {
            if let Some(sheet) = &sheet {
                sheets::set_link_sheet(&doc2, node, Some(sheet.clone()));
                load_imports(&doc2, sheet, 0);
            }
            let event = if ok { "load" } else { "error" };
            super::navigable::fire_simple_event(
                agent,
                &doc2,
                Target::Node(node),
                event,
                EventInit::default(),
            );
        }
        finish(agent, &doc2, node);
    });
}

/// The sheet of `node` is done: it stops blocking scripts and delaying
/// the load event, and the parser and "the end" go on.
fn finish(agent: &mut Agent, doc: &DocRef, node: NodeId) {
    sheets::remove_blocking(doc, node);
    if let Some(info) = super::ua().and_then(|ua| ua.doc_info(doc)) {
        info.load_delay.set(info.load_delay.get().saturating_sub(1));
    }
    super::document::resume_parser_soon(doc);
    super::document::continue_the_end(agent, doc);
}

/// Fetches the sheets of the `@import` rules of `sheet` (whose rules may
/// be read before they arrive: `styleSheet` is `null` until then).
pub fn load_imports(doc: &DocRef, sheet: &SheetRef, depth: usize) {
    if depth >= MAX_IMPORT_DEPTH {
        return;
    }
    let Some(ua) = super::ua() else {
        return;
    };
    let base = {
        let s = sheet.borrow();
        s.base_url
            .as_deref()
            .and_then(|b| Url::parse(b, None).ok())
            .unwrap_or_else(|| super::document::base_url(doc))
    };
    let rules: Vec<_> = sheet.borrow().rules.clone();
    for rule in rules {
        let href = match &rule.borrow().kind {
            RuleKind::Import { href, supports, .. } => {
                if supports
                    .as_deref()
                    .is_some_and(|c| !om::css_supports_condition(c))
                {
                    continue;
                }
                href.clone()
            }
            _ => continue,
        };
        let Ok(url) = Url::parse(&href, Some(&base)) else {
            continue;
        };
        let info = ua.doc_info(doc);
        if let Some(info) = &info {
            info.load_delay.set(info.load_delay.get() + 1);
        }
        let doc2 = doc.clone();
        let parent = Rc::downgrade(sheet);
        let rule2 = Rc::downgrade(&rule);
        super::loader::fetch(&ua, url, Some(doc.clone()), move |agent, result| {
            if let (Ok(r), Some(rule), Some(parent)) = (result, rule2.upgrade(), parent.upgrade()) {
                if (200..300).contains(&r.status) {
                    let text = decode(&r.body, r.content_type.as_deref());
                    let child = om::parse_sheet(&text);
                    {
                        let mut c = child.borrow_mut();
                        c.href = Some(r.url.href());
                        c.base_url = Some(r.url.href());
                        c.owner_rule = Some(Rc::downgrade(&rule));
                        c.parent_sheet = Some(Rc::downgrade(&parent));
                    }
                    if let RuleKind::Import { sheet, media, .. } = &mut rule.borrow_mut().kind {
                        child.borrow_mut().media = media.clone();
                        *sheet = Some(child.clone());
                    }
                    load_imports(&doc2, &child, depth + 1);
                }
            }
            if let Some(info) = super::ua().and_then(|ua| ua.doc_info(&doc2)) {
                info.load_delay.set(info.load_delay.get().saturating_sub(1));
            }
            super::document::continue_the_end(agent, &doc2);
        });
    }
}

/// Runs when the text of a connected `style` element may have changed:
/// its sheet is updated and its imports load.
pub fn style_changed(doc: &DocRef, node: NodeId) {
    if let Some(sheet) = sheets::sheet_of(doc, node) {
        let has_imports = sheet
            .borrow()
            .rules
            .iter()
            .any(|r| matches!(&r.borrow().kind, RuleKind::Import { sheet: None, .. }));
        if has_imports {
            load_imports(doc, &sheet, 0);
        }
    }
}
