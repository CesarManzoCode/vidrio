//! HTML fragment serialization (HTML §13.3) and the html5lib tree format
//! used by the parser tests.
//!
//! Both walks keep their own stack, so a deep tree does not exhaust the
//! native stack.

use crate::dom::{Attribute, Document, Namespace, NodeData, NodeId};

/// An element "serializes as void" (HTML §13.3).
fn serializes_as_void(d: &Document, id: NodeId) -> bool {
    d.element(id).is_some_and(|e| {
        e.name.ns == Some(Namespace::Html)
            && matches!(
                e.name.local.as_str(),
                "area"
                    | "base"
                    | "br"
                    | "col"
                    | "embed"
                    | "hr"
                    | "img"
                    | "input"
                    | "link"
                    | "meta"
                    | "source"
                    | "track"
                    | "wbr"
                    | "basefont"
                    | "bgsound"
                    | "frame"
                    | "keygen"
                    | "param"
            )
    })
}

/// "Escaping a string": `&`, U+00A0, `<` and `>`, and `"` in attribute
/// mode.
fn escape_into(out: &mut String, text: &str, attribute_mode: bool) {
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '\u{A0}' => out.push_str("&nbsp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' if attribute_mode => out.push_str("&quot;"),
            c => out.push(c),
        }
    }
}

/// An attribute's serialized name (HTML §13.3).
fn attribute_serialized_name(attr: &Attribute) -> String {
    match attr.ns {
        None => attr.local.to_string(),
        Some(Namespace::Xml) => format!("xml:{}", attr.local),
        Some(Namespace::Xmlns) if attr.local == "xmlns" => "xmlns".to_string(),
        Some(Namespace::Xmlns) => format!("xmlns:{}", attr.local),
        Some(Namespace::XLink) => format!("xlink:{}", attr.local),
        Some(_) => attr.qualified_name(),
    }
}

/// The tag name an element serializes with.
fn serialized_tag_name(d: &Document, id: NodeId) -> String {
    let Some(e) = d.element(id) else {
        return String::new();
    };
    match e.name.ns {
        Some(Namespace::Html | Namespace::MathMl | Namespace::Svg) => e.name.local.to_string(),
        _ => e.name.qualified(),
    }
}

/// Text inside these elements is written literally.
fn has_raw_text(d: &Document, parent: NodeId, scripting: bool) -> bool {
    d.element(parent).is_some_and(|e| {
        e.name.ns == Some(Namespace::Html)
            && (matches!(
                e.name.local.as_str(),
                "style" | "script" | "xmp" | "iframe" | "noembed" | "noframes" | "plaintext"
            ) || (scripting && e.name.local == "noscript"))
    })
}

fn write_start_tag(out: &mut String, d: &Document, id: NodeId, tag_name: &str) {
    out.push('<');
    out.push_str(tag_name);
    if let Some(e) = d.element(id) {
        if let Some(is) = e.is_value {
            if e.attr("is").is_none() {
                out.push_str(" is=\"");
                escape_into(out, &is, true);
                out.push('"');
            }
        }
        for attr in &e.attrs {
            out.push(' ');
            out.push_str(&attribute_serialized_name(attr));
            out.push_str("=\"");
            escape_into(out, &attr.value, true);
            out.push('"');
        }
    }
    out.push('>');
}

/// The HTML fragment serialization algorithm (HTML §13.3) for `node` (an
/// element, document or fragment): its children, serialized. `scripting`
/// is whether scripting is enabled for the node (it decides how `noscript`
/// text is written). Shadow roots are not serialized.
pub fn serialize_fragment(d: &Document, node: NodeId, scripting: bool) -> String {
    let mut out = String::new();
    if serializes_as_void(d, node) {
        return out;
    }
    struct Frame {
        parent: NodeId,
        next: Option<NodeId>,
        close: Option<String>,
    }
    let container = d.template_contents(node).unwrap_or(node);
    let mut stack = vec![Frame {
        parent: container,
        next: d.first_child(container),
        close: None,
    }];
    while let Some(top) = stack.last_mut() {
        let Some(child) = top.next else {
            if let Some(tag_name) = stack.pop().and_then(|f| f.close) {
                out.push_str("</");
                out.push_str(&tag_name);
                out.push('>');
            }
            continue;
        };
        top.next = d.next_sibling(child);
        let parent = top.parent;
        match d.data(child) {
            NodeData::Element(_) => {
                let tag_name = serialized_tag_name(d, child);
                write_start_tag(&mut out, d, child, &tag_name);
                if serializes_as_void(d, child) {
                    continue;
                }
                let inner = d.template_contents(child).unwrap_or(child);
                stack.push(Frame {
                    parent: inner,
                    next: d.first_child(inner),
                    close: Some(tag_name),
                });
            }
            NodeData::Text(data) | NodeData::CDataSection(data) => {
                if has_raw_text(d, parent, scripting) {
                    out.push_str(data);
                } else {
                    escape_into(&mut out, data, false);
                }
            }
            NodeData::Comment(data) => {
                out.push_str("<!--");
                out.push_str(data);
                out.push_str("-->");
            }
            NodeData::ProcessingInstruction { target, data } => {
                out.push_str("<?");
                out.push_str(target);
                out.push(' ');
                out.push_str(data);
                out.push_str("?>");
            }
            NodeData::DocumentType(doctype) => {
                out.push_str("<!DOCTYPE ");
                out.push_str(&doctype.name);
                out.push('>');
            }
            NodeData::Document | NodeData::DocumentFragment { .. } | NodeData::Attr(_) => {}
        }
    }
    out
}

/// The `outerHTML`-style serialization of an element: its start tag, its
/// serialized children and its end tag.
pub fn serialize_outer(d: &Document, element: NodeId, scripting: bool) -> String {
    let tag_name = serialized_tag_name(d, element);
    let mut out = String::new();
    write_start_tag(&mut out, d, element, &tag_name);
    if serializes_as_void(d, element) {
        return out;
    }
    out.push_str(&serialize_fragment(d, element, scripting));
    out.push_str("</");
    out.push_str(&tag_name);
    out.push('>');
    out
}

fn tree_attribute_name(attr: &Attribute) -> String {
    match attr.ns {
        None => attr.local.to_string(),
        Some(Namespace::XLink) => format!("xlink {}", attr.local),
        Some(Namespace::Xml) => format!("xml {}", attr.local),
        Some(Namespace::Xmlns) => format!("xmlns {}", attr.local),
        Some(_) => attr.qualified_name(),
    }
}

/// The children of `node` in the tree format of html5lib's
/// tree-construction tests (`| <html>` lines, two spaces per level,
/// attributes sorted, template contents under `content`).
pub fn html5lib_tree(d: &Document, node: NodeId) -> String {
    let mut lines: Vec<String> = Vec::new();
    // (node, depth); children are pushed in reverse to come out in order.
    let mut stack: Vec<(NodeId, usize)> = Vec::new();
    let push_children = |stack: &mut Vec<(NodeId, usize)>, parent: NodeId, depth: usize| {
        let mut child = d.last_child(parent);
        while let Some(c) = child {
            stack.push((c, depth));
            child = d.prev_sibling(c);
        }
    };
    push_children(&mut stack, node, 0);
    while let Some((id, depth)) = stack.pop() {
        let indent = "  ".repeat(depth);
        match d.data(id) {
            NodeData::Element(e) => {
                let name = match e.name.ns {
                    Some(Namespace::Svg) => format!("svg {}", e.name.local),
                    Some(Namespace::MathMl) => format!("math {}", e.name.local),
                    _ => e.name.local.to_string(),
                };
                lines.push(format!("| {indent}<{name}>"));
                let mut attrs: Vec<(String, &str)> = e
                    .attrs
                    .iter()
                    .map(|a| (tree_attribute_name(a), a.value.as_str()))
                    .collect();
                attrs.sort();
                for (attr_name, value) in attrs {
                    lines.push(format!("| {indent}  {attr_name}=\"{value}\""));
                }
                match e.template_contents {
                    Some(contents) => {
                        lines.push(format!("| {indent}  content"));
                        push_children(&mut stack, contents, depth + 2);
                    }
                    None => push_children(&mut stack, id, depth + 1),
                }
            }
            NodeData::Text(data) | NodeData::CDataSection(data) => {
                lines.push(format!("| {indent}\"{data}\""));
            }
            NodeData::Comment(data) => lines.push(format!("| {indent}<!-- {data} -->")),
            NodeData::ProcessingInstruction { target, data } => {
                lines.push(format!("| {indent}<?{target} {data}>"));
            }
            NodeData::DocumentType(doctype) => {
                if doctype.public_id.is_empty() && doctype.system_id.is_empty() {
                    lines.push(format!("| {indent}<!DOCTYPE {}>", doctype.name));
                } else {
                    lines.push(format!(
                        "| {indent}<!DOCTYPE {} \"{}\" \"{}\">",
                        doctype.name, doctype.public_id, doctype.system_id
                    ));
                }
            }
            NodeData::Document | NodeData::DocumentFragment { .. } | NodeData::Attr(_) => {
                push_children(&mut stack, id, depth);
            }
        }
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dom::{Atom, QualName};

    #[test]
    fn escapes_text_and_attributes() {
        let mut d = Document::new_html();
        let div = d.create_html_element("div");
        d.set_attribute(div, "title", "a\"b<c>&\u{A0}");
        let text = d.create_text("x<y>&z\u{A0}\"");
        d.append_child(div, text);
        assert_eq!(
            serialize_outer(&d, div, true),
            "<div title=\"a&quot;b&lt;c&gt;&amp;&nbsp;\">x&lt;y&gt;&amp;z&nbsp;\"</div>"
        );
    }

    #[test]
    fn void_raw_text_and_foreign_names() {
        let mut d = Document::new_html();
        let root = d.create_html_element("div");
        let br = d.create_html_element("br");
        d.append_child(root, br);
        let style = d.create_html_element("style");
        let css = d.create_text("a > b & c");
        d.append_child(style, css);
        d.append_child(root, style);
        let svg = d.create_element(QualName::new(Some(Namespace::Svg), None, Atom::new("svg")));
        let mut attr = Attribute::new("href", "#x");
        attr.ns = Some(Namespace::XLink);
        attr.prefix = Some(Atom::new("xlink"));
        d.append_attribute(svg, attr);
        d.append_child(root, svg);
        let pi = d.create_processing_instruction("t", "d");
        d.append_child(root, pi);
        assert_eq!(
            serialize_fragment(&d, root, true),
            "<br><style>a > b & c</style><svg xlink:href=\"#x\"></svg><?t d?>"
        );
        assert_eq!(serialize_fragment(&d, br, true), "");
    }
}
