//! The XML serialization of DOM Parsing and Serialization §3.2 ("produce
//! an XML serialization"), with its namespace prefix map, for
//! `XMLSerializer.serializeToString`. The "require well-formed" flag is
//! always unset, as `XMLSerializer` asks: nothing fails.

use std::collections::HashMap;

use crate::dom::{Document, ElementData, Namespace, NodeData, NodeId};

/// Namespace (`None` for the null namespace) → the prefixes bound to it,
/// oldest first.
type PrefixMap = HashMap<Option<String>, Vec<String>>;

/// The elements of the HTML namespace that serialize without an end tag.
const VOID: [&str; 17] = [
    "area", "base", "basefont", "bgsound", "br", "col", "embed", "frame", "hr", "img", "input",
    "keygen", "link", "meta", "param", "source", "track",
];

/// The XML serialization of `node` (and its descendants).
pub fn serialize(doc: &Document, node: NodeId) -> String {
    let mut map: PrefixMap = HashMap::new();
    map.insert(
        Some(Namespace::XML_URL.to_string()),
        vec!["xml".to_string()],
    );
    let mut out = String::new();
    let mut index = 1;
    node_markup(doc, node, None, &mut map, &mut index, &mut out);
    out
}

fn node_markup(
    doc: &Document,
    node: NodeId,
    namespace: Option<&str>,
    map: &mut PrefixMap,
    index: &mut u32,
    out: &mut String,
) {
    match doc.data(node) {
        NodeData::Element(e) => element(doc, node, e, namespace, map, index, out),
        NodeData::Document | NodeData::DocumentFragment { .. } => {
            for child in doc.children(node) {
                node_markup(doc, child, namespace, map, index, out);
            }
        }
        NodeData::DocumentType(dt) => {
            out.push_str("<!DOCTYPE ");
            out.push_str(&dt.name);
            if !dt.public_id.is_empty() {
                out.push_str(" PUBLIC \"");
                out.push_str(&dt.public_id);
                out.push('"');
            } else if !dt.system_id.is_empty() {
                out.push_str(" SYSTEM");
            }
            if !dt.system_id.is_empty() {
                out.push_str(" \"");
                out.push_str(&dt.system_id);
                out.push('"');
            }
            out.push('>');
        }
        NodeData::Text(t) => escape_text(t, out),
        NodeData::CDataSection(t) => {
            out.push_str("<![CDATA[");
            out.push_str(t);
            out.push_str("]]>");
        }
        NodeData::Comment(t) => {
            out.push_str("<!--");
            out.push_str(t);
            out.push_str("-->");
        }
        NodeData::ProcessingInstruction { target, data } => {
            out.push_str("<?");
            out.push_str(target);
            out.push(' ');
            out.push_str(data);
            out.push_str("?>");
        }
        NodeData::Attr(_) => {}
    }
}

fn escape_text(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            c => out.push(c),
        }
    }
}

fn escape_attribute(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            c => out.push(c),
        }
    }
}

fn ns_of(n: Option<Namespace>) -> Option<String> {
    n.map(|n| n.url().to_string())
}

/// "Retrieve a preferred prefix string".
fn preferred_prefix(
    map: &PrefixMap,
    ns: &Option<String>,
    preferred: Option<&str>,
) -> Option<String> {
    let list = map.get(ns)?;
    let mut candidate = None;
    for p in list {
        candidate = Some(p.clone());
        if Some(p.as_str()) == preferred {
            return candidate;
        }
    }
    candidate
}

/// "Generate a prefix".
fn generate_prefix(
    map: &mut PrefixMap,
    locals: &mut HashMap<String, String>,
    ns: &Option<String>,
    index: &mut u32,
) -> String {
    let prefix = format!("ns{index}");
    *index += 1;
    map.entry(ns.clone()).or_default().push(prefix.clone());
    locals.insert(prefix.clone(), ns.clone().unwrap_or_default());
    prefix
}

fn map_has(map: &PrefixMap, ns: &Option<String>, prefix: &str) -> bool {
    map.get(ns).is_some_and(|l| l.iter().any(|p| p == prefix))
}

/// "Record the namespace information": the prefix definitions of `e` go
/// into `map` and `locals`; returns the default namespace it declares.
fn record_namespaces(
    e: &ElementData,
    map: &mut PrefixMap,
    locals: &mut HashMap<String, String>,
) -> Option<String> {
    let mut default_ns = None;
    for a in &e.attrs {
        if a.ns != Some(Namespace::Xmlns) {
            continue;
        }
        match a.prefix {
            None => default_ns = Some(a.value.clone()),
            Some(_) => {
                let prefix = a.local.as_str().to_string();
                if a.value == Namespace::XML_URL {
                    continue;
                }
                let ns = (!a.value.is_empty()).then(|| a.value.clone());
                if map_has(map, &ns, &prefix) {
                    continue;
                }
                map.entry(ns.clone()).or_default().push(prefix.clone());
                locals.insert(prefix, ns.unwrap_or_default());
            }
        }
    }
    default_ns
}

fn element(
    doc: &Document,
    node: NodeId,
    e: &ElementData,
    inherited: Option<&str>,
    parent_map: &mut PrefixMap,
    index: &mut u32,
    out: &mut String,
) {
    let mut map = parent_map.clone();
    let mut locals: HashMap<String, String> = HashMap::new();
    let local_default = record_namespaces(e, &mut map, &mut locals);
    let mut inherited_ns: Option<String> = inherited.map(str::to_string);
    let ns = ns_of(e.name.ns);
    let local = e.name.local.as_str();
    let mut ignore_ns_definition = false;
    let qualified;
    out.push('<');
    if inherited_ns == ns {
        if local_default.is_some() {
            ignore_ns_definition = true;
        }
        qualified = if ns.as_deref() == Some(Namespace::XML_URL) {
            format!("xml:{local}")
        } else {
            local.to_string()
        };
        out.push_str(&qualified);
    } else {
        let mut prefix = e.name.prefix.map(|p| p.as_str().to_string());
        let candidate = if prefix.as_deref() == Some("xmlns") {
            prefix.clone()
        } else {
            preferred_prefix(&map, &ns, prefix.as_deref())
        };
        if let Some(candidate) = candidate {
            qualified = format!("{candidate}:{local}");
            if let Some(d) = &local_default {
                if d != Namespace::XML_URL {
                    inherited_ns = (!d.is_empty()).then(|| d.clone());
                }
            }
            out.push_str(&qualified);
        } else if let Some(p) = prefix.take() {
            let p = if locals.contains_key(&p) {
                generate_prefix(&mut map, &mut locals, &ns, index)
            } else {
                map.entry(ns.clone()).or_default().push(p.clone());
                p
            };
            qualified = format!("{p}:{local}");
            out.push_str(&qualified);
            out.push_str(" xmlns:");
            out.push_str(&p);
            out.push_str("=\"");
            escape_attribute(ns.as_deref().unwrap_or(""), out);
            out.push('"');
            if let Some(d) = &local_default {
                inherited_ns = (!d.is_empty()).then(|| d.clone());
            }
        } else if local_default.is_none() || local_default != ns {
            ignore_ns_definition = true;
            qualified = local.to_string();
            inherited_ns = ns.clone();
            out.push_str(&qualified);
            out.push_str(" xmlns=\"");
            escape_attribute(ns.as_deref().unwrap_or(""), out);
            out.push('"');
        } else {
            qualified = local.to_string();
            inherited_ns = ns.clone();
            out.push_str(&qualified);
        }
    }
    attributes(e, &mut map, &mut locals, index, ignore_ns_definition, out);

    let is_html = ns.as_deref() == Some(Namespace::HTML_URL);
    let template = is_html && local == "template";
    let has_children = doc.first_child(node).is_some()
        || (template
            && e.template_contents
                .is_some_and(|t| doc.first_child(t).is_some()));
    if !has_children && is_html && VOID.contains(&local) {
        out.push_str(" />");
        return;
    }
    if !has_children && !is_html {
        out.push_str("/>");
        return;
    }
    out.push('>');
    let content = if template {
        e.template_contents.unwrap_or(node)
    } else {
        node
    };
    for child in doc.children(content) {
        node_markup(doc, child, inherited_ns.as_deref(), &mut map, index, out);
    }
    out.push_str("</");
    out.push_str(&qualified);
    out.push('>');
}

/// "XML serialization of the attributes".
fn attributes(
    e: &ElementData,
    map: &mut PrefixMap,
    locals: &mut HashMap<String, String>,
    index: &mut u32,
    ignore_ns_definition: bool,
    out: &mut String,
) {
    for a in &e.attrs {
        let mut candidate = None;
        if let Some(ns) = a.ns {
            let ns_s = Some(ns.url().to_string());
            let prefix = a.prefix.map(|p| p.as_str().to_string());
            candidate = preferred_prefix(map, &ns_s, prefix.as_deref());
            if ns == Namespace::Xmlns {
                let local = a.local.as_str();
                let value_ns = (!a.value.is_empty()).then(|| a.value.clone());
                let redundant = a.prefix.is_some()
                    && locals.get(local).is_none_or(|v| *v != a.value)
                    && map_has(map, &value_ns, local);
                if a.value == Namespace::XML_URL
                    || (a.prefix.is_none() && ignore_ns_definition)
                    || redundant
                {
                    continue;
                }
                if prefix.as_deref() == Some("xmlns") {
                    candidate = Some("xmlns".to_string());
                }
            } else if candidate.is_none() {
                let p = generate_prefix(map, locals, &ns_s, index);
                out.push_str(" xmlns:");
                out.push_str(&p);
                out.push_str("=\"");
                escape_attribute(ns.url(), out);
                out.push('"');
                candidate = Some(p);
            }
        }
        out.push(' ');
        if let Some(c) = candidate {
            out.push_str(&c);
            out.push(':');
        }
        out.push_str(a.local.as_str());
        out.push_str("=\"");
        escape_attribute(&a.value, out);
        out.push('"');
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dom::DocumentKind;

    fn round_trip(s: &str) -> String {
        let mut d = Document::new(DocumentKind::Xml, "application/xml");
        super::super::parse_into(&mut d, s).unwrap();
        serialize(&d, NodeId::DOCUMENT)
    }

    #[test]
    fn namespaces_round_trip() {
        assert_eq!(
            round_trip("<r xmlns='urn:a' xmlns:b='urn:b'><b:c b:x='1'/><d xmlns=''>t&amp;</d></r>"),
            "<r xmlns=\"urn:a\" xmlns:b=\"urn:b\"><b:c b:x=\"1\"/><d xmlns=\"\">t&amp;</d></r>"
        );
    }

    #[test]
    fn missing_declarations_are_added() {
        let mut d = Document::new(DocumentKind::Xml, "application/xml");
        let root = d.create_element(crate::dom::QualName::new(
            Some(Namespace::from_url("urn:x")),
            Some(crate::dom::Atom::new("p")),
            crate::dom::Atom::new("root"),
        ));
        d.append_child(NodeId::DOCUMENT, root);
        let child = d.create_element(crate::dom::QualName::new(
            Some(Namespace::from_url("urn:y")),
            None,
            crate::dom::Atom::new("c"),
        ));
        d.append_child(root, child);
        d.append_attribute(
            child,
            crate::dom::Attribute {
                ns: Some(Namespace::from_url("urn:z")),
                prefix: None,
                local: crate::dom::Atom::new("a"),
                value: "v\"".to_string(),
                node: None,
            },
        );
        assert_eq!(
            serialize(&d, NodeId::DOCUMENT),
            "<p:root xmlns:p=\"urn:x\"><c xmlns=\"urn:y\" xmlns:ns1=\"urn:z\" ns1:a=\"v&quot;\"/></p:root>"
        );
    }

    #[test]
    fn doctype_cdata_comment_pi() {
        assert_eq!(
            round_trip("<!DOCTYPE r SYSTEM 's'><r><![CDATA[<x>]]><!--c--><?p d?></r>"),
            "<!DOCTYPE r SYSTEM \"s\"><r><![CDATA[<x>]]><!--c--><?p d?></r>"
        );
    }
}
