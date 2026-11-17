//! `DOMImplementation` document creation and name validation (DOM §1.4
//! and §4.5.1).

use super::{Atom, Doctype, Document, DocumentKind, Namespace, NodeId, QualName};
use crate::url::Origin;

/// The `DOMException` names these steps throw.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DomException {
    InvalidCharacterError,
    NamespaceError,
}

impl DomException {
    /// The exception's name.
    pub fn name(self) -> &'static str {
        match self {
            DomException::InvalidCharacterError => "InvalidCharacterError",
            DomException::NamespaceError => "NamespaceError",
        }
    }
}

/// Which local name rule "validate and extract" applies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NameContext {
    Element,
    Attribute,
}

fn is_ascii_whitespace(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\u{C}' | '\r' | ' ')
}

/// A valid namespace prefix (DOM §1.4).
pub fn is_valid_namespace_prefix(name: &str) -> bool {
    !name.is_empty()
        && !name
            .chars()
            .any(|c| is_ascii_whitespace(c) || matches!(c, '\0' | '/' | '>'))
}

/// A valid attribute local name (DOM §1.4).
pub fn is_valid_attribute_local_name(name: &str) -> bool {
    !name.is_empty()
        && !name
            .chars()
            .any(|c| is_ascii_whitespace(c) || matches!(c, '\0' | '/' | '=' | '>'))
}

/// A valid element local name (DOM §1.4).
pub fn is_valid_element_local_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if first.is_ascii_alphabetic() {
        return !name
            .chars()
            .any(|c| is_ascii_whitespace(c) || matches!(c, '\0' | '/' | '>'));
    }
    if !(first == ':' || first == '_' || first >= '\u{80}') {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | ':' | '_') || c >= '\u{80}')
}

/// A valid doctype name (DOM §1.4); the empty string is one.
pub fn is_valid_doctype_name(name: &str) -> bool {
    !name
        .chars()
        .any(|c| is_ascii_whitespace(c) || matches!(c, '\0' | '>'))
}

/// "Validate and extract" (DOM §1.4): the namespace, prefix and local name
/// of `qualified_name`.
pub fn validate_and_extract(
    namespace: Option<&str>,
    qualified_name: &str,
    context: NameContext,
) -> Result<(Option<Namespace>, Option<Atom>, Atom), DomException> {
    let namespace = namespace
        .filter(|ns| !ns.is_empty())
        .map(Namespace::from_url);
    let mut prefix = None;
    let mut local = qualified_name;
    if let Some(colon) = qualified_name.find(':') {
        let before = &qualified_name[..colon];
        local = &qualified_name[colon + 1..];
        if !is_valid_namespace_prefix(before) {
            return Err(DomException::InvalidCharacterError);
        }
        prefix = Some(before);
    }
    let valid_local = match context {
        NameContext::Attribute => is_valid_attribute_local_name(local),
        NameContext::Element => is_valid_element_local_name(local),
    };
    if !valid_local {
        return Err(DomException::InvalidCharacterError);
    }
    if prefix.is_some() && namespace.is_none() {
        return Err(DomException::NamespaceError);
    }
    if prefix == Some("xml") && namespace != Some(Namespace::Xml) {
        return Err(DomException::NamespaceError);
    }
    if (qualified_name == "xmlns" || prefix == Some("xmlns")) && namespace != Some(Namespace::Xmlns)
    {
        return Err(DomException::NamespaceError);
    }
    if namespace == Some(Namespace::Xmlns) && qualified_name != "xmlns" && prefix != Some("xmlns") {
        return Err(DomException::NamespaceError);
    }
    Ok((namespace, prefix.map(Atom::new), Atom::new(local)))
}

/// `createDocumentType(name, publicId, systemId)` on `doc`'s
/// implementation.
pub fn create_document_type(
    doc: &mut Document,
    name: &str,
    public_id: &str,
    system_id: &str,
) -> Result<NodeId, DomException> {
    if !is_valid_doctype_name(name) {
        return Err(DomException::InvalidCharacterError);
    }
    Ok(doc.create_document_type(name, public_id, system_id))
}

/// `createDocument(namespace, qualifiedName, doctype)`: a new XML document
/// with `origin`. The doctype is passed by value because it moves into the
/// new document's arena; the caller removes the original node from its
/// tree (that is what adopting it does).
pub fn create_document(
    origin: &Origin,
    namespace: Option<&str>,
    qualified_name: &str,
    doctype: Option<Doctype>,
) -> Result<Document, DomException> {
    let element = if qualified_name.is_empty() {
        None
    } else {
        Some(validate_and_extract(
            namespace,
            qualified_name,
            NameContext::Element,
        )?)
    };
    let ns = namespace
        .filter(|ns| !ns.is_empty())
        .map(Namespace::from_url);
    let content_type = match ns {
        Some(Namespace::Html) => "application/xhtml+xml",
        Some(Namespace::Svg) => "image/svg+xml",
        _ => "application/xml",
    };
    let mut doc = Document::new(DocumentKind::Xml, content_type);
    doc.origin = origin.clone();
    if let Some(d) = doctype {
        let node = doc.create_document_type(&d.name, &d.public_id, &d.system_id);
        doc.append_child(NodeId::DOCUMENT, node);
    }
    if let Some((ns, prefix, local)) = element {
        let node = doc.create_element(QualName::new(ns, prefix, local));
        doc.append_child(NodeId::DOCUMENT, node);
    }
    Ok(doc)
}

/// `createHTMLDocument(title)`: a new HTML document with a doctype,
/// `html`, `head`, an optional `title` and `body`.
pub fn create_html_document(origin: &Origin, title: Option<&str>) -> Document {
    let mut doc = Document::new_html();
    doc.origin = origin.clone();
    let doctype = doc.create_document_type("html", "", "");
    doc.append_child(NodeId::DOCUMENT, doctype);
    let html = doc.create_html_element("html");
    doc.append_child(NodeId::DOCUMENT, html);
    let head = doc.create_html_element("head");
    doc.append_child(html, head);
    if let Some(title) = title {
        let element = doc.create_html_element("title");
        doc.append_child(head, element);
        let text = doc.create_text(title);
        doc.append_child(element, text);
    }
    let body = doc.create_html_element("body");
    doc.append_child(html, body);
    doc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_rules() {
        assert!(is_valid_element_local_name("div"));
        assert!(is_valid_element_local_name("a\u{1}b"));
        assert!(!is_valid_element_local_name("a b"));
        assert!(is_valid_element_local_name("_x.y"));
        assert!(!is_valid_element_local_name("1x"));
        assert!(!is_valid_element_local_name("_x!"));
        assert!(is_valid_element_local_name("\u{E9}t\u{E9}"));
        assert!(!is_valid_attribute_local_name("a=b"));
        assert!(is_valid_doctype_name(""));
        assert!(!is_valid_doctype_name("a>"));
    }

    #[test]
    fn validate_and_extract_namespaces() {
        let svg = Some(Namespace::SVG_URL);
        let (ns, prefix, local) =
            validate_and_extract(svg, "s:path", NameContext::Element).expect("valid");
        assert_eq!(ns, Some(Namespace::Svg));
        assert_eq!(prefix.map(|p| p.as_str()), Some("s"));
        assert_eq!(local, "path");
        assert_eq!(
            validate_and_extract(None, "a:b", NameContext::Element),
            Err(DomException::NamespaceError)
        );
        assert_eq!(
            validate_and_extract(svg, "xml:a", NameContext::Element),
            Err(DomException::NamespaceError)
        );
        assert_eq!(
            validate_and_extract(Some(Namespace::XMLNS_URL), "a", NameContext::Attribute),
            Err(DomException::NamespaceError)
        );
        assert!(
            validate_and_extract(Some(Namespace::XMLNS_URL), "xmlns", NameContext::Attribute)
                .is_ok()
        );
        assert_eq!(
            validate_and_extract(svg, ":a", NameContext::Element),
            Err(DomException::InvalidCharacterError)
        );
    }

    #[test]
    fn created_documents() {
        let origin = Origin::new_opaque();
        let doc = create_html_document(&origin, Some("T"));
        let html = doc.document_element().expect("html");
        assert!(doc.doctype().is_some());
        assert_eq!(doc.children(html).count(), 2);
        assert_eq!(doc.content_type, "text/html");
        let head = doc.head().expect("head");
        let title = doc.first_child(head).expect("title");
        assert_eq!(doc.text_content(title).as_deref(), Some("T"));

        let xml = create_document(&origin, Some(Namespace::SVG_URL), "svg", None).expect("doc");
        assert_eq!(xml.content_type, "image/svg+xml");
        assert_eq!(xml.kind, DocumentKind::Xml);
        let root = xml.document_element().expect("root");
        assert_eq!(
            xml.element(root).map(|e| e.name.ns),
            Some(Some(Namespace::Svg))
        );
        assert!(
            create_document(&origin, None, "", None)
                .expect("empty")
                .document_element()
                .is_none()
        );
    }
}
