//! The URL-reflecting members of `a`, `area` and `base`: the
//! `HTMLHyperlinkElementUtils` and `HyperlinkElementUtils` mixins (HTML
//! §4.6.4) and `HTMLBaseElement.href` (HTML §4.2.3).
//!
//! The element's url is not cached: "reinitialize url" parses the `href`
//! attribute again on every access, which gives the same answers because
//! every setter ends with "update href" (it writes the serialized url back
//! to the attribute).

use vjs::{Agent, JsResult};

use crate::bindings::platform::Native;
use crate::bindings::{IdlValue, Invocation};
use crate::dom::{DocRef, NodeId};
use crate::url::Url;

type R = JsResult<IdlValue>;

fn this_node(inv: &Invocation) -> Option<(DocRef, NodeId)> {
    let p = inv.this.as_ref()?;
    match &*p.get().native.borrow() {
        Native::Node(d, n) => Some((d.clone(), *n)),
        _ => None,
    }
}

fn set_attr(agent: &mut Agent, doc: &DocRef, el: NodeId, attr: &str, value: &str) {
    doc.borrow_mut().set_attribute(el, attr, value);
    super::document::process_html_changes(agent, doc);
}

fn attr(doc: &DocRef, el: NodeId, name: &str) -> Option<String> {
    doc.borrow().get_attribute(el, name).map(str::to_string)
}

/// "Encoding-parse a URL" (HTML §2.4.2): `input` against the document base
/// URL of `doc`, with the query encoded in the document's encoding.
pub fn encoding_parse(doc: &DocRef, input: &str) -> Option<Url> {
    let base = super::document::base_url(doc);
    let encoding = doc.borrow().encoding;
    Url::parse_with_encoding(input, Some(&base), encoding).ok()
}

/// "Reinitialize url" / "set the url": the element's url, `None` when it
/// has no `href` or the attribute does not parse.
pub fn element_url(doc: &DocRef, el: NodeId) -> Option<Url> {
    let href = attr(doc, el, "href")?;
    encoding_parse(doc, &href)
}

fn host_string(url: &Url) -> String {
    match (&url.host, url.port) {
        (Some(h), Some(p)) => format!("{h}:{p}"),
        (Some(h), None) => h.to_string(),
        _ => String::new(),
    }
}

/// The getters and setters of the two hyperlink mixins.
pub fn hyperlink(agent: &mut Agent, inv: &Invocation, member: &str) -> Option<R> {
    let (doc, el) = this_node(inv)?;
    let s = |v: String| Some(Ok(IdlValue::String(v)));
    let url = element_url(&doc, el);
    match member {
        "get_href" => {
            return s(match (&url, attr(&doc, el, "href")) {
                (Some(u), _) => u.href(),
                (None, Some(h)) => h,
                (None, None) => String::new(),
            });
        }
        "set_href" => {
            set_attr(agent, &doc, el, "href", &inv.str(0));
            return Some(Ok(IdlValue::Undefined));
        }
        "get_origin" => return s(url.map(|u| u.origin().serialize()).unwrap_or_default()),
        "get_protocol" => {
            return s(match url {
                Some(u) => format!("{}:", u.scheme),
                None => ":".to_string(),
            });
        }
        "get_username" => return s(url.map(|u| u.username).unwrap_or_default()),
        "get_password" => return s(url.map(|u| u.password).unwrap_or_default()),
        "get_host" => return s(url.as_ref().map(host_string).unwrap_or_default()),
        "get_hostname" => {
            return s(url
                .and_then(|u| u.host.map(|h| h.to_string()))
                .unwrap_or_default());
        }
        "get_port" => {
            return s(url
                .and_then(|u| u.port.map(|p| p.to_string()))
                .unwrap_or_default());
        }
        "get_pathname" => return s(url.map(|u| u.path_string()).unwrap_or_default()),
        "get_search" => {
            return s(match url.and_then(|u| u.query) {
                Some(q) if !q.is_empty() => format!("?{q}"),
                _ => String::new(),
            });
        }
        "get_hash" => {
            return s(match url.and_then(|u| u.fragment) {
                Some(f) if !f.is_empty() => format!("#{f}"),
                _ => String::new(),
            });
        }
        "get_target" | "get_hreflang" | "get_type" => {
            return s(attr(&doc, el, &member[4..]).unwrap_or_default());
        }
        "set_target" | "set_hreflang" | "set_type" => {
            set_attr(agent, &doc, el, &member[4..], &inv.str(0));
            return Some(Ok(IdlValue::Undefined));
        }
        _ => {}
    }
    let value = inv.str(0);
    // The url setters: with no url there is nothing to change; otherwise
    // the URL API setter runs on a copy, then "update href".
    let Some(mut url) = url else {
        return Some(Ok(IdlValue::Undefined));
    };
    match member {
        "set_protocol" => url.set_protocol(&value),
        "set_username" => url.set_username(&value),
        "set_password" => url.set_password(&value),
        "set_host" => url.set_host(&value),
        "set_hostname" => url.set_hostname(&value),
        "set_port" => url.set_port(&value),
        "set_pathname" => url.set_pathname(&value),
        "set_search" => url.set_search(&value),
        "set_hash" => url.set_hash(&value),
        _ => return None,
    }
    set_attr(agent, &doc, el, "href", &url.href());
    Some(Ok(IdlValue::Undefined))
}

/// `HTMLBaseElement`: `href` is the attribute parsed against the
/// document's fallback base URL (not against itself); `target` reflects.
pub fn base_element(agent: &mut Agent, inv: &Invocation, member: &str) -> Option<R> {
    let (doc, el) = this_node(inv)?;
    Some(Ok(match member {
        "get_href" => {
            let fallback = super::document::fallback_base_url(&doc);
            let encoding = doc.borrow().encoding;
            IdlValue::String(match attr(&doc, el, "href") {
                None => fallback.href(),
                Some(h) => match Url::parse_with_encoding(&h, Some(&fallback), encoding) {
                    Ok(u) => u.href(),
                    Err(_) => h,
                },
            })
        }
        "set_href" | "set_target" => {
            set_attr(agent, &doc, el, &member[4..], &inv.str(0));
            IdlValue::Undefined
        }
        "get_target" => IdlValue::String(attr(&doc, el, "target").unwrap_or_default()),
        _ => return None,
    }))
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use super::*;
    use crate::dom::Document;
    use crate::encoding::Encoding;

    fn doc(html: &str, encoding: &str) -> DocRef {
        let mut d = Document::new_html();
        d.url = Url::parse("http://a.test/dir/page.html", None).unwrap();
        d.encoding = Encoding::for_label(encoding).unwrap();
        crate::html::parse_into(&mut d, html, true);
        Rc::new(RefCell::new(d))
    }

    fn first_a(doc: &DocRef) -> NodeId {
        let d = doc.borrow();
        d.inclusive_descendants(NodeId::DOCUMENT)
            .into_iter()
            .find(|&n| d.is_html_element(n, "a"))
            .unwrap()
    }

    #[test]
    fn element_url_encodes_the_query_in_the_document_encoding() {
        let doc = doc("<a href='x?\u{3042}#\u{3042}'>", "euc-jp");
        let url = element_url(&doc, first_a(&doc)).unwrap();
        assert_eq!(url.href(), "http://a.test/dir/x?%A4%A2#%E3%81%82");
        let utf8 = self::doc("<a href='x?\u{3042}'>", "utf-8");
        let url = element_url(&utf8, first_a(&utf8)).unwrap();
        assert_eq!(url.query.as_deref(), Some("%E3%81%82"));
    }

    #[test]
    fn element_url_resolves_against_the_base_element() {
        let doc = doc("<base href='/b/'><a href='c'>", "utf-8");
        let url = element_url(&doc, first_a(&doc)).unwrap();
        assert_eq!(url.href(), "http://a.test/b/c");
        assert_eq!(host_string(&url), "a.test");
    }

    #[test]
    fn element_url_is_none_without_a_parsable_href() {
        let none = doc("<a>", "utf-8");
        assert!(element_url(&none, first_a(&none)).is_none());
        let bad = doc("<a href='http://[::'>", "utf-8");
        assert!(element_url(&bad, first_a(&bad)).is_none());
        let port = doc("<a href='http://h.test:81/'>", "utf-8");
        let url = element_url(&port, first_a(&port)).unwrap();
        assert_eq!(host_string(&url), "h.test:81");
    }
}
