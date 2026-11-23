//! Form submission (HTML §4.10.21) and the members of `HTMLFormElement`
//! and `HTMLInputElement` it needs: the content attributes that drive a
//! submission, the input's value with its dirty value, and `submit()`.
//!
//! A submission builds the entry list of the form's submittable elements,
//! picks the form's encoding (its `accept-charset`, else the document's),
//! and plans a navigation of the chosen navigable: `GET` puts the
//! urlencoded entries in the action URL's query, `POST` to `http(s)`
//! sends them as an entity body (`urlencoded`, `multipart/form-data` or
//! `text/plain`).

use std::cell::Cell;

use vjs::{Agent, JsResult};

use super::event_loop::{TaskSource, queue_task};
use super::loader::NetRequest;
use super::navigable::{self, NavigateParams};
use crate::bindings::exception::dom;
use crate::bindings::platform::Native;
use crate::bindings::{IdlValue, Invocation};
use crate::dom::{DocRef, Document, DomException, NodeId};
use crate::encoding::Encoding;

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

fn set_bool_attr(agent: &mut Agent, doc: &DocRef, el: NodeId, attr: &str, on: bool) {
    if on {
        set_attr(agent, doc, el, attr, "");
    } else {
        let _ = doc.borrow_mut().remove_attribute_by_name(el, attr);
        super::document::process_html_changes(agent, doc);
    }
}

fn attr(d: &Document, el: NodeId, name: &str) -> Option<String> {
    d.get_attribute(el, name).map(str::to_string)
}

/// The content attribute of a reflecting IDL attribute: its lowercase
/// name, or the one the IDL names (`acceptCharset`, `defaultValue`…).
fn content_attribute(member: &str) -> String {
    match member {
        "acceptCharset" => "accept-charset".to_string(),
        "defaultValue" => "value".to_string(),
        "defaultChecked" => "checked".to_string(),
        m => m.to_ascii_lowercase(),
    }
}

/// An enumerated attribute (HTML §2.3.3): the keyword `value` matches
/// ASCII case-insensitively, else `default`.
fn enumerated(value: Option<&str>, keywords: &[&'static str], default: &'static str) -> String {
    value
        .and_then(|v| keywords.iter().find(|k| k.eq_ignore_ascii_case(v)))
        .copied()
        .unwrap_or(default)
        .to_string()
}

const METHODS: [&str; 3] = ["get", "post", "dialog"];
const ENCTYPES: [&str; 3] = [
    "application/x-www-form-urlencoded",
    "multipart/form-data",
    "text/plain",
];

// ----- HTMLFormElement ------------------------------------------------------------

/// The members of `HTMLFormElement` form submission uses.
pub fn form_element(agent: &mut Agent, inv: &Invocation, member: &str) -> Option<R> {
    let (doc, el) = this_node(inv)?;
    let s = |v: String| Some(Ok(IdlValue::String(v)));
    match member {
        "get_acceptCharset" | "get_name" | "get_target" | "get_rel" => {
            let a = content_attribute(&member[4..]);
            return s(attr(&doc.borrow(), el, &a).unwrap_or_default());
        }
        "set_acceptCharset" | "set_name" | "set_target" | "set_rel" | "set_action"
        | "set_method" | "set_enctype" | "set_encoding" | "set_autocomplete" => {
            let a = match &member[4..] {
                "encoding" => "enctype".to_string(),
                m => content_attribute(m),
            };
            set_attr(agent, &doc, el, &a, &inv.str(0));
            return Some(Ok(IdlValue::Undefined));
        }
        "get_noValidate" => {
            let on = doc.borrow().get_attribute(el, "novalidate").is_some();
            return Some(Ok(IdlValue::Bool(on)));
        }
        "set_noValidate" => {
            set_bool_attr(agent, &doc, el, "novalidate", inv.bool(0));
            return Some(Ok(IdlValue::Undefined));
        }
        "get_action" => {
            let a = attr(&doc.borrow(), el, "action");
            return s(match a {
                None => doc.borrow().url.href(),
                Some(a) if a.is_empty() => doc.borrow().url.href(),
                Some(a) => match super::hyperlink::encoding_parse(&doc, &a) {
                    Some(u) => u.href(),
                    None => a,
                },
            });
        }
        "get_method" => {
            let d = doc.borrow();
            return s(enumerated(d.get_attribute(el, "method"), &METHODS, "get"));
        }
        "get_enctype" | "get_encoding" => {
            let d = doc.borrow();
            return s(enumerated(
                d.get_attribute(el, "enctype"),
                &ENCTYPES,
                ENCTYPES[0],
            ));
        }
        "get_autocomplete" => {
            let d = doc.borrow();
            return s(enumerated(
                d.get_attribute(el, "autocomplete"),
                &["on", "off"],
                "on",
            ));
        }
        "submit" => {
            submit(agent, &doc, el);
            return Some(Ok(IdlValue::Undefined));
        }
        _ => None,
    }
}

// ----- HTMLInputElement -----------------------------------------------------------

const INPUT_TYPES: [&str; 22] = [
    "hidden",
    "text",
    "search",
    "tel",
    "url",
    "email",
    "password",
    "date",
    "month",
    "week",
    "time",
    "datetime-local",
    "number",
    "range",
    "color",
    "checkbox",
    "radio",
    "file",
    "submit",
    "image",
    "reset",
    "button",
];

/// The `type` state of `input` element `el`.
pub fn input_type(d: &Document, el: NodeId) -> String {
    enumerated(d.get_attribute(el, "type"), &INPUT_TYPES, "text")
}

/// The input value modes (HTML §4.10.5.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ValueMode {
    Value,
    Default,
    DefaultOn,
    Filename,
}

fn value_mode(kind: &str) -> ValueMode {
    match kind {
        "hidden" | "submit" | "image" | "reset" | "button" => ValueMode::Default,
        "checkbox" | "radio" => ValueMode::DefaultOn,
        "file" => ValueMode::Filename,
        _ => ValueMode::Value,
    }
}

/// The value sanitization algorithm of the text-like types: `text`,
/// `search`, `tel` and `password` strip newlines, `url` and `email` also
/// leading and trailing ASCII whitespace.
pub fn sanitize(kind: &str, value: &str) -> String {
    let stripped: String = value.chars().filter(|&c| c != '\n' && c != '\r').collect();
    match kind {
        "url" | "email" => stripped
            .trim_matches(|c: char| matches!(c, '\t' | '\n' | '\x0c' | '\r' | ' '))
            .to_string(),
        _ => stripped,
    }
}

/// The value of `input` element `el` (the `value` IDL attribute getter).
pub fn input_value(d: &Document, el: NodeId) -> String {
    let kind = input_type(d, el);
    match value_mode(&kind) {
        ValueMode::Value => match d.element(el).and_then(|e| e.control_value.clone()) {
            Some(v) => v,
            None => sanitize(&kind, d.get_attribute(el, "value").unwrap_or("")),
        },
        ValueMode::Default => attr(d, el, "value").unwrap_or_default(),
        ValueMode::DefaultOn => attr(d, el, "value").unwrap_or_else(|| "on".to_string()),
        // No file is ever selected.
        ValueMode::Filename => String::new(),
    }
}

const INPUT_STRINGS: [&str; 12] = [
    "accept",
    "alt",
    "dirName",
    "formTarget",
    "max",
    "min",
    "name",
    "pattern",
    "placeholder",
    "step",
    "defaultValue",
    "colorSpace",
];
const INPUT_BOOLEANS: [&str; 7] = [
    "alpha",
    "defaultChecked",
    "disabled",
    "formNoValidate",
    "multiple",
    "readOnly",
    "required",
];

/// The members of `HTMLInputElement` that hold what a form submits.
pub fn input_element(agent: &mut Agent, inv: &Invocation, member: &str) -> Option<R> {
    let (doc, el) = this_node(inv)?;
    let (kind, name) = member.split_once('_').unwrap_or(("", member));
    if INPUT_STRINGS.contains(&name) {
        let a = content_attribute(name);
        return Some(Ok(match kind {
            "get" => IdlValue::String(attr(&doc.borrow(), el, &a).unwrap_or_default()),
            _ => {
                set_attr(agent, &doc, el, &a, &inv.str(0));
                IdlValue::Undefined
            }
        }));
    }
    if INPUT_BOOLEANS.contains(&name) {
        let a = content_attribute(name);
        return Some(Ok(match kind {
            "get" => IdlValue::Bool(doc.borrow().get_attribute(el, &a).is_some()),
            _ => {
                set_bool_attr(agent, &doc, el, &a, inv.bool(0));
                IdlValue::Undefined
            }
        }));
    }
    match member {
        "get_type" => Some(Ok(IdlValue::String(input_type(&doc.borrow(), el)))),
        "set_type" => {
            set_attr(agent, &doc, el, "type", &inv.str(0));
            Some(Ok(IdlValue::Undefined))
        }
        "get_value" => Some(Ok(IdlValue::String(input_value(&doc.borrow(), el)))),
        "set_value" => {
            let value = inv.str(0);
            let kind = input_type(&doc.borrow(), el);
            match value_mode(&kind) {
                ValueMode::Value => {
                    if let Some(e) = doc.borrow_mut().element_mut(el) {
                        e.control_value = Some(sanitize(&kind, &value));
                    }
                }
                ValueMode::Default | ValueMode::DefaultOn => {
                    set_attr(agent, &doc, el, "value", &value);
                }
                ValueMode::Filename if !value.is_empty() => {
                    return Some(dom(agent, DomException::InvalidStateError));
                }
                ValueMode::Filename => {}
            }
            Some(Ok(IdlValue::Undefined))
        }
        _ => None,
    }
}

// ----- the entry list -------------------------------------------------------------

/// The value of an entry: a string, or a file input with no file
/// selected (an empty `File` with no name).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EntryValue {
    Text(String),
    EmptyFile,
}

fn is_html(d: &Document, el: NodeId, names: &[&str]) -> bool {
    names.iter().any(|n| d.is_html_element(el, n))
}

/// The form owner of form-associated element `el` (HTML §4.10.17.3): the
/// form its `form` attribute names, else the one the parser associated,
/// else its nearest ancestor `form`.
pub fn form_owner(d: &Document, el: NodeId) -> Option<NodeId> {
    if let Some(id) = d.get_attribute(el, "form") {
        let root = d.root(el);
        return d
            .get_element_by_id(root, id)
            .filter(|&f| d.is_html_element(f, "form"));
    }
    if let Some(f) = d.element(el).and_then(|e| e.form_owner) {
        if d.root(f) == d.root(el) {
            return Some(f);
        }
    }
    let mut p = d.parent_element(el);
    while let Some(n) = p {
        if d.is_html_element(n, "form") {
            return Some(n);
        }
        p = d.parent_element(n);
    }
    None
}

/// Whether control `el` is disabled: it has `disabled`, or it descends
/// from a disabled `fieldset` other than through that fieldset's first
/// `legend` child.
fn is_disabled(d: &Document, el: NodeId) -> bool {
    if d.get_attribute(el, "disabled").is_some() {
        return true;
    }
    let mut child = el;
    let mut p = d.parent_element(el);
    while let Some(n) = p {
        if d.is_html_element(n, "fieldset") && d.get_attribute(n, "disabled").is_some() {
            let first_legend = d
                .element_children(n)
                .into_iter()
                .find(|&c| d.is_html_element(c, "legend"));
            if first_legend != Some(child) {
                return true;
            }
        }
        child = n;
        p = d.parent_element(n);
    }
    false
}

fn strip_and_collapse(s: &str) -> String {
    s.split(|c: char| matches!(c, '\t' | '\n' | '\x0c' | '\r' | ' '))
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// The submitted options of `select` element `el`: the selected ones that
/// are not disabled. With none selected, a single-line `select` without
/// `multiple` selects its first enabled option.
fn select_values(d: &Document, el: NodeId) -> Vec<String> {
    let mut options = Vec::new();
    for c in d.element_children(el) {
        if d.is_html_element(c, "option") {
            options.push((c, false));
        } else if d.is_html_element(c, "optgroup") {
            let group_disabled = d.get_attribute(c, "disabled").is_some();
            for o in d.element_children(c) {
                if d.is_html_element(o, "option") {
                    options.push((o, group_disabled));
                }
            }
        }
    }
    let disabled = |&(o, g): &(NodeId, bool)| g || d.get_attribute(o, "disabled").is_some();
    let multiple = d.get_attribute(el, "multiple").is_some();
    let size_one = d
        .get_attribute(el, "size")
        .and_then(|s| s.trim().parse::<u32>().ok())
        .is_none_or(|s| s <= 1);
    let mut selected: Vec<(NodeId, bool)> = options
        .iter()
        .copied()
        .filter(|&(o, _)| d.get_attribute(o, "selected").is_some())
        .collect();
    if !multiple && selected.len() > 1 {
        selected = selected.split_off(selected.len() - 1);
    }
    if selected.is_empty() && !multiple && size_one {
        selected.extend(options.iter().copied().find(|o| !disabled(o)));
    }
    selected
        .into_iter()
        .filter(|o| !disabled(o))
        .map(|(o, _)| match d.get_attribute(o, "value") {
            Some(v) => v.to_string(),
            None => strip_and_collapse(&d.text_content(o).unwrap_or_default()),
        })
        .collect()
}

/// Constructs the entry list of `form` (HTML §4.10.21.4) with `form` as
/// the submitter (`submit()`): no button is submitted.
pub fn entry_list(d: &Document, form: NodeId, encoding: Encoding) -> Vec<(String, EntryValue)> {
    let root = d.root(form);
    let mut entries = Vec::new();
    for field in d.inclusive_descendants(root) {
        if !is_html(
            d,
            field,
            &["button", "input", "object", "select", "textarea"],
        ) {
            continue;
        }
        if form_owner(d, field) != Some(form) {
            continue;
        }
        let in_datalist = {
            let mut p = d.parent_element(field);
            let mut found = false;
            while let Some(n) = p {
                found |= d.is_html_element(n, "datalist");
                p = d.parent_element(n);
            }
            found
        };
        if in_datalist || is_disabled(d, field) {
            continue;
        }
        if is_html(d, field, &["button", "object"]) {
            continue;
        }
        let kind = if d.is_html_element(field, "input") {
            input_type(d, field)
        } else {
            String::new()
        };
        if matches!(kind.as_str(), "submit" | "image" | "reset" | "button") {
            continue;
        }
        if matches!(kind.as_str(), "checkbox" | "radio")
            && d.get_attribute(field, "checked").is_none()
        {
            continue;
        }
        let Some(name) = d.get_attribute(field, "name").filter(|n| !n.is_empty()) else {
            continue;
        };
        let name = name.to_string();
        if d.is_html_element(field, "select") {
            for v in select_values(d, field) {
                entries.push((name.clone(), EntryValue::Text(v)));
            }
            continue;
        }
        let value = if d.is_html_element(field, "textarea") {
            match d.element(field).and_then(|e| e.control_value.clone()) {
                Some(v) => v,
                None => d.child_text_content(field),
            }
        } else if kind == "file" {
            entries.push((name, EntryValue::EmptyFile));
            continue;
        } else if kind == "hidden"
            && name.eq_ignore_ascii_case("_charset_")
            && d.get_attribute(field, "value").is_none()
        {
            encoding.output_encoding().name().to_string()
        } else {
            input_value(d, field)
        };
        entries.push((name, EntryValue::Text(value)));
        let dirname =
            matches!(kind.as_str(), "text" | "search") || d.is_html_element(field, "textarea");
        if dirname {
            if let Some(dn) = d.get_attribute(field, "dirname").filter(|v| !v.is_empty()) {
                entries.push((dn.to_string(), EntryValue::Text("ltr".to_string())));
            }
        }
    }
    entries
}

/// Picks the encoding of `form` (HTML §4.10.21.5): the first label of its
/// `accept-charset` an encoding answers to, else the document's encoding,
/// through get an output encoding.
pub fn form_encoding(d: &Document, form: NodeId) -> Encoding {
    let chosen = d.get_attribute(form, "accept-charset").and_then(|v| {
        v.split(|c: char| matches!(c, '\t' | '\n' | '\x0c' | '\r' | ' '))
            .filter(|t| !t.is_empty())
            .find_map(Encoding::for_label)
    });
    chosen.unwrap_or(d.encoding).output_encoding()
}

/// Every lone CR and lone LF becomes CRLF.
fn normalize_newlines(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push_str("\r\n");
            }
            '\n' => out.push_str("\r\n"),
            c => out.push(c),
        }
    }
    out
}

/// "Convert to a list of name-value pairs": a file is its name (empty)
/// and newlines are normalized.
fn name_value_pairs(entries: &[(String, EntryValue)]) -> Vec<(String, String)> {
    entries
        .iter()
        .map(|(n, v)| {
            let v = match v {
                EntryValue::Text(t) => normalize_newlines(t),
                EntryValue::EmptyFile => String::new(),
            };
            (normalize_newlines(n), v)
        })
        .collect()
}

/// The urlencoded serialization of `entries` in `encoding`.
pub fn urlencoded(entries: &[(String, EntryValue)], encoding: Encoding) -> String {
    let pairs = name_value_pairs(entries);
    crate::url::form::serialize_with_encoding(
        pairs.iter().map(|(n, v)| (n.as_str(), v.as_str())),
        encoding,
    )
}

/// The `text/plain` encoding algorithm: `name=value` CRLF per entry,
/// encoded with `encoding` (unmappable code points as `&#N;`).
pub fn text_plain(entries: &[(String, EntryValue)], encoding: Encoding) -> Vec<u8> {
    let mut text = String::new();
    for (n, v) in name_value_pairs(entries) {
        text.push_str(&n);
        text.push('=');
        text.push_str(&v);
        text.push_str("\r\n");
    }
    crate::encoding::encode(&text, encoding)
}

/// The `multipart/form-data` encoding algorithm with `boundary`.
pub fn multipart(entries: &[(String, EntryValue)], encoding: Encoding, boundary: &str) -> Vec<u8> {
    let escape = |s: &str| {
        s.replace('\n', "%0A")
            .replace('\r', "%0D")
            .replace('"', "%22")
    };
    let mut text = String::new();
    for (n, v) in entries {
        text.push_str("--");
        text.push_str(boundary);
        text.push_str("\r\nContent-Disposition: form-data; name=\"");
        text.push_str(&escape(&normalize_newlines(n)));
        text.push('"');
        match v {
            EntryValue::Text(t) => {
                text.push_str("\r\n\r\n");
                text.push_str(&normalize_newlines(t));
            }
            EntryValue::EmptyFile => {
                text.push_str("; filename=\"\"\r\nContent-Type: application/octet-stream\r\n\r\n");
            }
        }
        text.push_str("\r\n");
    }
    text.push_str("--");
    text.push_str(boundary);
    text.push_str("--\r\n");
    crate::encoding::encode(&text, encoding)
}

// ----- submission -----------------------------------------------------------------

thread_local! {
    static NEXT_PLAN: Cell<u64> = const { Cell::new(1) };
}

/// The target of `form`: its `target`, else the first `base` element's
/// `target`, else `""`.
fn target(d: &Document, form: NodeId) -> String {
    if let Some(t) = d.get_attribute(form, "target") {
        return t.to_string();
    }
    d.inclusive_descendants(NodeId::DOCUMENT)
        .into_iter()
        .filter(|&n| d.is_html_element(n, "base"))
        .find_map(|n| d.get_attribute(n, "target").map(str::to_string))
        .unwrap_or_default()
}

/// Submits `form` from `submit()` (HTML §4.10.21.3).
pub fn submit(agent: &mut Agent, doc: &DocRef, form: NodeId) {
    let Some(ua) = super::ua() else {
        return;
    };
    let Some(info) = ua.doc_info(doc) else {
        return;
    };
    let Some(source) = info.context.get() else {
        return;
    };
    let d = doc.borrow();
    if !d.is_connected(form) {
        return;
    }
    let encoding = form_encoding(&d, form);
    let entries = entry_list(&d, form, encoding);
    let action = match d.get_attribute(form, "action") {
        Some(a) if !a.is_empty() => a.to_string(),
        _ => d.url.href(),
    };
    let method = enumerated(d.get_attribute(form, "method"), &METHODS, "get");
    let enctype = enumerated(d.get_attribute(form, "enctype"), &ENCTYPES, ENCTYPES[0]);
    let target = target(&d, form);
    let rel: Vec<String> = d
        .get_attribute(form, "rel")
        .unwrap_or("")
        .split_ascii_whitespace()
        .map(str::to_ascii_lowercase)
        .collect();
    let origin = d.origin.clone();
    drop(d);
    let Some(mut url) = super::hyperlink::encoding_parse(doc, &action) else {
        return;
    };
    // Closing a `dialog` is not implemented (LIMITATIONS.md).
    if method == "dialog" {
        return;
    }
    let post = method == "post";
    let request = match url.scheme.as_str() {
        "http" | "https" if post => {
            let boundary = format!("----vidrioFormBoundary{:016x}", NEXT_PLAN.get());
            let (content_type, body) = match enctype.as_str() {
                "multipart/form-data" => (
                    format!("multipart/form-data; boundary={boundary}"),
                    multipart(&entries, encoding, &boundary),
                ),
                "text/plain" => ("text/plain".to_string(), text_plain(&entries, encoding)),
                _ => (
                    ENCTYPES[0].to_string(),
                    urlencoded(&entries, encoding).into_bytes(),
                ),
            };
            Some(NetRequest {
                method: "POST".to_string(),
                headers: vec![("Content-Type".to_string(), content_type)],
                body,
                manual_redirects: false,
            })
        }
        // Mutate action URL.
        "http" | "https" | "ftp" | "data" | "file" if !post => {
            url.query = Some(urlencoded(&entries, encoding));
            None
        }
        // There is no mail client.
        "mailto" => return,
        // Get action URL.
        _ => None,
    };
    let noopener = rel.iter().any(|t| t == "noopener" || t == "noreferrer")
        || (target.eq_ignore_ascii_case("_blank") && !rel.iter().any(|t| t == "opener"));
    let ctx = match navigable::context_for_target(&ua, source, &target) {
        Some(c) => c,
        None => {
            let name = if target.eq_ignore_ascii_case("_blank") {
                ""
            } else {
                &target
            };
            let opener = if noopener { None } else { Some(source) };
            let id = navigable::create_top_level(agent, opener, name);
            match ua.context(id) {
                Some(c) => c,
                None => return,
            }
        }
    };
    // Plan to navigate: a later submission of the same form replaces a
    // planned navigation that has not run yet.
    let plan = NEXT_PLAN.get();
    NEXT_PLAN.set(plan + 1);
    {
        let mut planned = info.planned_navigations.borrow_mut();
        planned.retain(|(f, _)| *f != form);
        planned.push((form, plan));
    }
    let target_id = ctx.id;
    let weak = std::rc::Rc::downgrade(&info);
    queue_task(
        TaskSource::DomManipulation,
        Some(doc.clone()),
        move |agent| {
            let Some(info) = weak.upgrade() else {
                return;
            };
            {
                let mut planned = info.planned_navigations.borrow_mut();
                let Some(i) = planned.iter().position(|&(f, p)| f == form && p == plan) else {
                    return;
                };
                planned.remove(i);
            }
            let mut p = NavigateParams::new(url);
            p.source_origin = Some(origin);
            p.request = request;
            navigable::navigate(agent, target_id, p);
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::url::Url;

    fn parse(html: &str) -> Document {
        let mut d = Document::new_html();
        d.url = Url::parse("http://a.test/page.html", None).unwrap();
        crate::html::parse_into(&mut d, html, true);
        d
    }

    fn first(d: &Document, local: &str) -> NodeId {
        d.inclusive_descendants(NodeId::DOCUMENT)
            .into_iter()
            .find(|&n| d.is_html_element(n, local))
            .unwrap()
    }

    fn texts(entries: &[(String, EntryValue)]) -> Vec<(String, String)> {
        name_value_pairs(entries)
    }

    #[test]
    fn entry_list_skips_unsubmittable_controls() {
        let d = parse(
            "<form><input name=a value=1><input value=noname>\
             <input name=b disabled value=2><input type=checkbox name=c>\
             <input type=checkbox name=d checked><input type=submit name=e value=go>\
             <fieldset disabled><input name=f></fieldset>\
             <select name=g><option>x<option selected value=y>z</select>\
             <textarea name=h>t\nu</textarea></form><input name=outside>",
        );
        let form = first(&d, "form");
        let entries = entry_list(&d, form, Encoding::Utf8);
        assert_eq!(
            texts(&entries),
            vec![
                ("a".to_string(), "1".to_string()),
                ("d".to_string(), "on".to_string()),
                ("g".to_string(), "y".to_string()),
                ("h".to_string(), "t\r\nu".to_string()),
            ]
        );
    }

    #[test]
    fn dirty_value_wins_over_the_attribute() {
        let mut d = parse("<form><input name=q value=old></form>");
        let input = first(&d, "input");
        d.element_mut(input).unwrap().control_value = Some(sanitize("text", "n\new"));
        assert_eq!(input_value(&d, input), "new");
        let form = first(&d, "form");
        assert_eq!(
            urlencoded(&entry_list(&d, form, Encoding::Utf8), Encoding::Utf8),
            "q=new"
        );
    }

    #[test]
    fn accept_charset_picks_the_encoding_and_unmappable_become_references() {
        let d = parse(
            "<form accept-charset='bogus euc-jp'><input name=x value='\u{3042}\u{AC00}'></form>",
        );
        let form = first(&d, "form");
        let encoding = form_encoding(&d, form);
        assert_eq!(encoding, Encoding::for_label("euc-jp").unwrap());
        assert_eq!(
            urlencoded(&entry_list(&d, form, encoding), encoding),
            "x=%A4%A2%26%2344032%3B"
        );
        let plain = parse("<form><input name=x></form>");
        assert_eq!(form_encoding(&plain, first(&plain, "form")), Encoding::Utf8);
    }

    #[test]
    fn text_plain_and_multipart_bodies() {
        let entries = vec![
            ("a\"b".to_string(), EntryValue::Text("1 2".to_string())),
            ("f".to_string(), EntryValue::EmptyFile),
        ];
        assert_eq!(text_plain(&entries, Encoding::Utf8), b"a\"b=1 2\r\nf=\r\n");
        let body = String::from_utf8(multipart(&entries, Encoding::Utf8, "B")).unwrap();
        assert_eq!(
            body,
            "--B\r\nContent-Disposition: form-data; name=\"a%22b\"\r\n\r\n1 2\r\n\
             --B\r\nContent-Disposition: form-data; name=\"f\"; filename=\"\"\r\n\
             Content-Type: application/octet-stream\r\n\r\n\r\n--B--\r\n"
        );
    }

    #[test]
    fn enumerated_attributes_fall_back_to_their_default() {
        assert_eq!(enumerated(Some("POST"), &METHODS, "get"), "post");
        assert_eq!(enumerated(Some("put"), &METHODS, "get"), "get");
        assert_eq!(enumerated(None, &ENCTYPES, ENCTYPES[0]), ENCTYPES[0]);
    }
}
