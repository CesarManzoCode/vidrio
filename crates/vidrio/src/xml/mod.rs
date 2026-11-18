//! A non-validating XML 1.0 parser with namespaces (XML 1.0 Fifth
//! Edition, Namespaces in XML 1.0 Third Edition) that builds a DOM tree,
//! for `application/xhtml+xml`, `text/xml` and the other XML documents of
//! navigation, `DOMParser` and `XMLHttpRequest` ([`parse_into`]), and the
//! XML serialization of DOM Parsing for `XMLSerializer` ([`serialize`]).
//!
//! The parser checks well-formedness and namespace well-formedness and
//! stops at the first error. Of the document type declaration it keeps the
//! name and identifiers and the internal general entities of the internal
//! subset; it reads no external subset or external entity, and attribute
//! defaults declared in `ATTLIST` are not applied (LIMITATIONS.md).

pub mod serialize;

use std::collections::HashMap;

use crate::dom::{Atom, Attribute, Document, Namespace, NodeId, QualName};
use crate::encoding::{self, Encoding};

pub use serialize::serialize;

/// The namespace of the error document of [`replace_with_error`] (the one
/// Gecko and WPT use).
pub const PARSERERROR_NS: &str = "http://www.mozilla.org/newlayout/xml/parsererror.xml";

/// A well-formedness error: where it was found (1-based) and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct XmlError {
    pub line: usize,
    pub column: usize,
    pub message: String,
}

impl std::fmt::Display for XmlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}: {}", self.line, self.column, self.message)
    }
}

/// Decodes the bytes of an XML document: a byte order mark wins, then
/// the `charset` of `content_type`, then the `encoding` of the XML
/// declaration, then UTF-8 (XML 1.0 Appendix F).
pub fn decode(bytes: &[u8], content_type: Option<&str>) -> (String, Encoding) {
    if encoding::bom_sniff(bytes).is_some() {
        return encoding::decode(bytes, Encoding::Utf8);
    }
    let declared = crate::browser::loader::charset_of(content_type)
        .or_else(|| declaration_encoding(bytes))
        .unwrap_or(Encoding::Utf8);
    // An XML declaration in an ASCII-compatible encoding cannot name a
    // UTF-16 one.
    let declared = match declared {
        Encoding::Utf16Le | Encoding::Utf16Be => Encoding::Utf8,
        e => e,
    };
    encoding::decode(bytes, declared)
}

/// The `encoding` pseudo-attribute of a leading XML declaration.
fn declaration_encoding(bytes: &[u8]) -> Option<Encoding> {
    let head = &bytes[..bytes.len().min(1024)];
    if !head.starts_with(b"<?xml") {
        return None;
    }
    let end = head.windows(2).position(|w| w == b"?>")?;
    let decl = String::from_utf8_lossy(&head[..end]).into_owned();
    let at = decl.find("encoding")?;
    let rest = decl[at + 8..].trim_start().strip_prefix('=')?.trim_start();
    let quote = rest.chars().next().filter(|c| *c == '"' || *c == '\'')?;
    let value = rest[1..].split(quote).next()?;
    Encoding::for_label(value)
}

/// Replaces the children of `doc` with the error document of a parse
/// that failed: a `parsererror` element with the message.
pub fn replace_with_error(doc: &mut Document, error: &XmlError) {
    while let Some(child) = doc.first_child(NodeId::DOCUMENT) {
        doc.remove(child);
    }
    let name = QualName::new(
        Some(Namespace::from_url(PARSERERROR_NS)),
        None,
        Atom::new("parsererror"),
    );
    let root = doc.create_element(name);
    doc.append_child(NodeId::DOCUMENT, root);
    let text = doc.create_text(&format!("XML Parsing Error: {error}"));
    doc.append_child(root, text);
}

/// Parses `input` (already decoded) into the document node of `doc`. On
/// error the nodes built so far stay in place.
pub fn parse_into(doc: &mut Document, input: &str) -> Result<(), XmlError> {
    let chars = normalize_newlines(input);
    let mut p = Parser {
        input: chars,
        pos: 0,
        doc,
        open: Vec::new(),
        scopes: vec![HashMap::new()],
        entities: HashMap::new(),
        text: String::new(),
        seen_root: false,
    };
    p.document()
}

/// Line ends become U+000A (XML 1.0 §2.11).
fn normalize_newlines(input: &str) -> Vec<char> {
    let mut out = Vec::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    // A leading byte order mark is not part of the document.
    if chars.peek() == Some(&'\u{FEFF}') {
        chars.next();
    }
    while let Some(c) = chars.next() {
        if c == '\r' {
            if chars.peek() == Some(&'\n') {
                chars.next();
            }
            out.push('\n');
        } else {
            out.push(c);
        }
    }
    out
}

/// `Char` (XML 1.0 §2.2).
fn is_xml_char(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\r')
        || ('\u{20}'..='\u{D7FF}').contains(&c)
        || ('\u{E000}'..='\u{FFFD}').contains(&c)
        || ('\u{10000}'..='\u{10FFFF}').contains(&c)
}

/// `NameStartChar` (XML 1.0 §2.3).
pub fn is_name_start_char(c: char) -> bool {
    matches!(c, ':' | 'A'..='Z' | '_' | 'a'..='z')
        || ('\u{C0}'..='\u{D6}').contains(&c)
        || ('\u{D8}'..='\u{F6}').contains(&c)
        || ('\u{F8}'..='\u{2FF}').contains(&c)
        || ('\u{370}'..='\u{37D}').contains(&c)
        || ('\u{37F}'..='\u{1FFF}').contains(&c)
        || ('\u{200C}'..='\u{200D}').contains(&c)
        || ('\u{2070}'..='\u{218F}').contains(&c)
        || ('\u{2C00}'..='\u{2FEF}').contains(&c)
        || ('\u{3001}'..='\u{D7FF}').contains(&c)
        || ('\u{F900}'..='\u{FDCF}').contains(&c)
        || ('\u{FDF0}'..='\u{FFFD}').contains(&c)
        || ('\u{10000}'..='\u{EFFFF}').contains(&c)
}

/// `NameChar` (XML 1.0 §2.3).
pub fn is_name_char(c: char) -> bool {
    is_name_start_char(c)
        || matches!(c, '-' | '.' | '0'..='9' | '\u{B7}')
        || ('\u{300}'..='\u{36F}').contains(&c)
        || ('\u{203F}'..='\u{2040}').contains(&c)
}

/// `Name` (XML 1.0 §2.3).
pub fn is_name(s: &str) -> bool {
    let mut chars = s.chars();
    chars.next().is_some_and(is_name_start_char) && chars.all(is_name_char)
}

/// `QName` of Namespaces in XML: a name with at most one colon, not at
/// either end.
fn split_qname(name: &str) -> Option<(Option<&str>, &str)> {
    match name.split_once(':') {
        None => Some((None, name)),
        Some((p, l)) if !p.is_empty() && !l.is_empty() && !l.contains(':') => Some((Some(p), l)),
        Some(_) => None,
    }
}

/// An attribute as written in a start tag.
struct RawAttr {
    name: String,
    value: String,
}

/// The depth past which entity references are not expanded (protection
/// against entity expansion bombs).
const MAX_ENTITY_DEPTH: usize = 16;

struct Parser<'a> {
    input: Vec<char>,
    pos: usize,
    doc: &'a mut Document,
    /// The open elements with their qualified names.
    open: Vec<(NodeId, String)>,
    /// The in-scope namespace bindings: one map per open element (and one
    /// for the document); `""` is the default namespace, an empty value
    /// unbinds it.
    scopes: Vec<HashMap<String, String>>,
    /// The internal general entities of the document type declaration.
    entities: HashMap<String, String>,
    /// Character data not yet flushed into a Text node.
    text: String,
    seen_root: bool,
}

impl Parser<'_> {
    fn error<T>(&self, message: &str) -> Result<T, XmlError> {
        let mut line = 1;
        let mut column = 1;
        for c in &self.input[..self.pos.min(self.input.len())] {
            if *c == '\n' {
                line += 1;
                column = 1;
            } else {
                column += 1;
            }
        }
        Err(XmlError {
            line,
            column,
            message: message.to_string(),
        })
    }

    fn peek(&self) -> Option<char> {
        self.input.get(self.pos).copied()
    }

    fn starts_with(&self, s: &str) -> bool {
        let mut i = self.pos;
        for c in s.chars() {
            if self.input.get(i) != Some(&c) {
                return false;
            }
            i += 1;
        }
        true
    }

    fn eat(&mut self, s: &str) -> bool {
        if self.starts_with(s) {
            self.pos += s.chars().count();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, s: &str) -> Result<(), XmlError> {
        if self.eat(s) {
            Ok(())
        } else {
            self.error(&format!("expected '{s}'"))
        }
    }

    fn skip_space(&mut self) -> bool {
        let start = self.pos;
        while matches!(self.peek(), Some(' ' | '\t' | '\n' | '\r')) {
            self.pos += 1;
        }
        self.pos > start
    }

    fn name(&mut self) -> Result<String, XmlError> {
        let start = self.pos;
        match self.peek() {
            Some(c) if is_name_start_char(c) => self.pos += 1,
            _ => return self.error("expected a name"),
        }
        while self.peek().is_some_and(is_name_char) {
            self.pos += 1;
        }
        Ok(self.input[start..self.pos].iter().collect())
    }

    /// Text up to `end` (consumed), checking every character.
    fn until(&mut self, end: &str) -> Result<String, XmlError> {
        let start = self.pos;
        while !self.starts_with(end) {
            match self.peek() {
                None => return self.error(&format!("expected '{end}' before the end")),
                Some(c) if !is_xml_char(c) => return self.error("invalid character"),
                Some(_) => self.pos += 1,
            }
        }
        let s: String = self.input[start..self.pos].iter().collect();
        self.pos += end.chars().count();
        Ok(s)
    }

    fn quoted(&mut self) -> Result<String, XmlError> {
        let quote = match self.peek() {
            Some(q @ ('"' | '\'')) => q,
            _ => return self.error("expected a quoted value"),
        };
        self.pos += 1;
        let start = self.pos;
        while self.peek() != Some(quote) {
            if self.peek().is_none() {
                return self.error("unterminated quoted value");
            }
            self.pos += 1;
        }
        let s: String = self.input[start..self.pos].iter().collect();
        self.pos += 1;
        Ok(s)
    }

    /// The node new content goes into.
    fn parent(&self) -> NodeId {
        self.open.last().map_or(NodeId::DOCUMENT, |(id, _)| *id)
    }

    fn flush_text(&mut self) {
        if self.text.is_empty() {
            return;
        }
        let text = std::mem::take(&mut self.text);
        if self.open.is_empty() {
            // Only white space reaches here outside the root element, and
            // the document cannot hold text.
            return;
        }
        let parent = self.parent();
        match self.doc.last_child(parent) {
            Some(last) if matches!(self.doc.data(last), crate::dom::NodeData::Text(_)) => {
                self.doc.append_text(last, &text)
            }
            _ => {
                let node = self.doc.create_text(&text);
                self.doc.append_child(parent, node);
            }
        }
    }

    fn append(&mut self, node: NodeId) {
        self.flush_text();
        let parent = self.parent();
        self.doc.append_child(parent, node);
    }

    // ----- the document ------------------------------------------------------------

    fn document(&mut self) -> Result<(), XmlError> {
        if self.starts_with("<?xml")
            && self
                .input
                .get(self.pos + 5)
                .is_some_and(|c| c.is_whitespace())
        {
            self.xml_declaration()?;
        }
        loop {
            if self.peek().is_none() {
                break;
            }
            if !self.open.is_empty() {
                self.content()?;
                continue;
            }
            if self.skip_space() {
                continue;
            }
            if self.starts_with("<!--") {
                self.comment()?;
            } else if self.starts_with("<?") {
                self.processing_instruction()?;
            } else if self.starts_with("<!DOCTYPE") {
                if self.seen_root || self.doc.doctype().is_some() {
                    return self.error("misplaced document type declaration");
                }
                self.doctype()?;
            } else if self.starts_with("<") && !self.starts_with("</") {
                if self.seen_root {
                    return self.error("content after the root element");
                }
                self.seen_root = true;
                self.start_tag()?;
            } else {
                return self.error("text outside the root element");
            }
        }
        if let Some((_, name)) = self.open.last() {
            let name = name.clone();
            return self.error(&format!("unclosed element '{name}'"));
        }
        if !self.seen_root {
            return self.error("no root element");
        }
        Ok(())
    }

    fn xml_declaration(&mut self) -> Result<(), XmlError> {
        self.expect("<?xml")?;
        let body = self.until("?>")?;
        if !body.trim_start().starts_with("version") {
            return self.error("the XML declaration has no version");
        }
        Ok(())
    }

    fn comment(&mut self) -> Result<(), XmlError> {
        self.expect("<!--")?;
        let data = self.until("--")?;
        if !self.eat(">") {
            return self.error("'--' inside a comment");
        }
        let node = self.doc.create_comment(&data);
        self.append(node);
        Ok(())
    }

    fn processing_instruction(&mut self) -> Result<(), XmlError> {
        self.expect("<?")?;
        let target = self.name()?;
        if target.eq_ignore_ascii_case("xml") {
            return self.error("misplaced XML declaration");
        }
        if target.contains(':') {
            return self.error("a colon in a processing instruction target");
        }
        let data = if self.eat("?>") {
            String::new()
        } else {
            if !self.skip_space() {
                return self.error("expected white space after the target");
            }
            self.until("?>")?
        };
        let node = self.doc.create_processing_instruction(&target, &data);
        self.append(node);
        Ok(())
    }

    fn doctype(&mut self) -> Result<(), XmlError> {
        self.expect("<!DOCTYPE")?;
        if !self.skip_space() {
            return self.error("expected white space after DOCTYPE");
        }
        let name = self.name()?;
        self.skip_space();
        let mut public_id = String::new();
        let mut system_id = String::new();
        if self.eat("PUBLIC") {
            self.skip_space();
            public_id = self.quoted()?;
            self.skip_space();
            system_id = self.quoted()?;
        } else if self.eat("SYSTEM") {
            self.skip_space();
            system_id = self.quoted()?;
        }
        self.skip_space();
        if self.eat("[") {
            self.internal_subset()?;
            self.skip_space();
        }
        self.expect(">")?;
        let node = self.doc.create_document_type(&name, &public_id, &system_id);
        self.append(node);
        Ok(())
    }

    /// The internal subset: entity declarations are kept, the rest of the
    /// markup declarations are checked for their end and skipped.
    fn internal_subset(&mut self) -> Result<(), XmlError> {
        loop {
            self.skip_space();
            if self.eat("]") {
                return Ok(());
            }
            if self.starts_with("<!--") {
                self.expect("<!--")?;
                self.until("-->")?;
            } else if self.starts_with("<?") {
                self.expect("<?")?;
                self.until("?>")?;
            } else if self.eat("<!ENTITY") {
                self.entity_declaration()?;
            } else if self.starts_with("<!") {
                self.skip_declaration()?;
            } else if self.eat("%") {
                // A parameter entity reference: not read.
                self.name()?;
                self.expect(";")?;
            } else {
                return self.error("invalid internal subset");
            }
        }
    }

    fn skip_declaration(&mut self) -> Result<(), XmlError> {
        loop {
            match self.peek() {
                None => return self.error("unterminated markup declaration"),
                Some('>') => {
                    self.pos += 1;
                    return Ok(());
                }
                Some('"' | '\'') => {
                    self.quoted()?;
                }
                Some(_) => self.pos += 1,
            }
        }
    }

    fn entity_declaration(&mut self) -> Result<(), XmlError> {
        self.skip_space();
        let parameter = self.eat("%");
        self.skip_space();
        let name = self.name()?;
        self.skip_space();
        if matches!(self.peek(), Some('"' | '\'')) {
            let value = self.quoted()?;
            if !parameter && !self.entities.contains_key(&name) {
                self.entities.insert(name, value);
            }
        }
        // External entities (SYSTEM/PUBLIC) are declared but not read.
        self.skip_declaration()
    }

    // ----- elements ----------------------------------------------------------------

    fn content(&mut self) -> Result<(), XmlError> {
        match self.peek() {
            Some('<') => {
                if self.starts_with("</") {
                    self.end_tag()
                } else if self.starts_with("<!--") {
                    self.comment()
                } else if self.starts_with("<![CDATA[") {
                    self.expect("<![CDATA[")?;
                    let data = self.until("]]>")?;
                    let node = self.doc.create_cdata_section(&data);
                    self.append(node);
                    Ok(())
                } else if self.starts_with("<?") {
                    self.processing_instruction()
                } else if self.starts_with("<!") {
                    self.error("markup declaration inside an element")
                } else {
                    self.start_tag()
                }
            }
            Some('&') => {
                let text = self.reference(0)?;
                self.text.push_str(&text);
                Ok(())
            }
            Some(_) => {
                if self.starts_with("]]>") {
                    return self.error("']]>' in content");
                }
                let c = self.peek().expect("a character");
                if !is_xml_char(c) {
                    return self.error("invalid character");
                }
                self.text.push(c);
                self.pos += 1;
                Ok(())
            }
            None => Ok(()),
        }
    }

    /// A character or entity reference at `&`, expanded.
    fn reference(&mut self, depth: usize) -> Result<String, XmlError> {
        self.expect("&")?;
        if self.eat("#") {
            let hex = self.eat("x");
            let start = self.pos;
            while self.peek().is_some_and(|c| c.is_ascii_hexdigit()) {
                self.pos += 1;
            }
            let digits: String = self.input[start..self.pos].iter().collect();
            self.expect(";")?;
            let value = if hex {
                u32::from_str_radix(&digits, 16).ok()
            } else if digits.bytes().all(|b| b.is_ascii_digit()) {
                digits.parse::<u32>().ok()
            } else {
                None
            };
            return match value.and_then(char::from_u32).filter(|c| is_xml_char(*c)) {
                Some(c) => Ok(c.to_string()),
                None => self.error("invalid character reference"),
            };
        }
        let name = self.name()?;
        self.expect(";")?;
        self.expand_entity(&name, depth)
    }

    fn expand_entity(&self, name: &str, depth: usize) -> Result<String, XmlError> {
        let predefined = match name {
            "lt" => Some("<"),
            "gt" => Some(">"),
            "amp" => Some("&"),
            "apos" => Some("'"),
            "quot" => Some("\""),
            _ => None,
        };
        if let Some(p) = predefined {
            return Ok(p.to_string());
        }
        if depth >= MAX_ENTITY_DEPTH {
            return self.error("entity references nest too deeply");
        }
        let Some(value) = self.entities.get(name) else {
            return self.error(&format!("undefined entity '{name}'"));
        };
        // The replacement text may hold references of its own.
        let mut out = String::new();
        let mut rest = value.as_str();
        while let Some(i) = rest.find('&') {
            out.push_str(&rest[..i]);
            let after = &rest[i + 1..];
            let Some(end) = after.find(';') else {
                return self.error("unterminated reference in an entity");
            };
            let r = &after[..end];
            if let Some(num) = r.strip_prefix('#') {
                let v = match num.strip_prefix('x') {
                    Some(h) => u32::from_str_radix(h, 16).ok(),
                    None => num.parse::<u32>().ok(),
                };
                match v.and_then(char::from_u32) {
                    Some(c) => out.push(c),
                    None => return self.error("invalid character reference"),
                }
            } else {
                out.push_str(&self.expand_entity(r, depth + 1)?);
            }
            rest = &after[end + 1..];
        }
        out.push_str(rest);
        Ok(out)
    }

    fn attribute_value(&mut self) -> Result<String, XmlError> {
        let quote = match self.peek() {
            Some(q @ ('"' | '\'')) => q,
            _ => return self.error("expected a quoted attribute value"),
        };
        self.pos += 1;
        let mut value = String::new();
        loop {
            match self.peek() {
                None => return self.error("unterminated attribute value"),
                Some(c) if c == quote => {
                    self.pos += 1;
                    return Ok(value);
                }
                Some('<') => return self.error("'<' in an attribute value"),
                Some('&') => {
                    let text = self.reference(0)?;
                    if text.contains('<') && text.len() > 1 {
                        return self.error("'<' in an attribute value");
                    }
                    value.push_str(&text);
                }
                // Attribute-value normalization (XML 1.0 §3.3.3).
                Some('\t' | '\n' | '\r') => {
                    value.push(' ');
                    self.pos += 1;
                }
                Some(c) if !is_xml_char(c) => return self.error("invalid character"),
                Some(c) => {
                    value.push(c);
                    self.pos += 1;
                }
            }
        }
    }

    fn lookup(&self, prefix: &str) -> Option<String> {
        match prefix {
            "xml" => return Some(Namespace::XML_URL.to_string()),
            "xmlns" => return Some(Namespace::XMLNS_URL.to_string()),
            _ => {}
        }
        for scope in self.scopes.iter().rev() {
            if let Some(ns) = scope.get(prefix) {
                return if ns.is_empty() {
                    None
                } else {
                    Some(ns.clone())
                };
            }
        }
        None
    }

    fn start_tag(&mut self) -> Result<(), XmlError> {
        self.expect("<")?;
        let name = self.name()?;
        let mut attrs: Vec<RawAttr> = Vec::new();
        let empty;
        loop {
            let spaced = self.skip_space();
            if self.eat("/>") {
                empty = true;
                break;
            }
            if self.eat(">") {
                empty = false;
                break;
            }
            if !spaced {
                return self.error("expected white space between attributes");
            }
            let attr_name = self.name()?;
            self.skip_space();
            self.expect("=")?;
            self.skip_space();
            let value = self.attribute_value()?;
            if attrs.iter().any(|a| a.name == attr_name) {
                return self.error(&format!("duplicate attribute '{attr_name}'"));
            }
            attrs.push(RawAttr {
                name: attr_name,
                value,
            });
        }

        // The namespace declarations of this element.
        let mut scope = HashMap::new();
        for a in &attrs {
            if a.name == "xmlns" {
                if a.value == Namespace::XML_URL || a.value == Namespace::XMLNS_URL {
                    return self.error("reserved namespace as the default namespace");
                }
                scope.insert(String::new(), a.value.clone());
            } else if let Some(prefix) = a.name.strip_prefix("xmlns:") {
                if prefix == "xmlns" {
                    return self.error("the 'xmlns' prefix cannot be declared");
                }
                if a.value.is_empty() {
                    return self.error(&format!("empty namespace for prefix '{prefix}'"));
                }
                if (prefix == "xml") != (a.value == Namespace::XML_URL) {
                    return self.error("the 'xml' prefix is bound to its namespace only");
                }
                if a.value == Namespace::XMLNS_URL {
                    return self.error("the xmlns namespace cannot be declared");
                }
                scope.insert(prefix.to_string(), a.value.clone());
            }
        }
        self.scopes.push(scope);

        let Some((prefix, local)) = split_qname(&name) else {
            return self.error(&format!("invalid qualified name '{name}'"));
        };
        let ns = match prefix {
            Some(p) => match self.lookup(p) {
                Some(ns) => Some(ns),
                None => return self.error(&format!("unbound prefix '{p}'")),
            },
            None => self.lookup(""),
        };
        let qual = QualName::new(
            ns.as_deref().map(Namespace::from_url),
            prefix.map(Atom::new),
            Atom::new(local),
        );
        let element = self.doc.create_element(qual);
        let mut seen: Vec<(Option<String>, String)> = Vec::new();
        for a in attrs {
            let Some((prefix, local)) = split_qname(&a.name) else {
                return self.error(&format!("invalid attribute name '{}'", a.name));
            };
            let ns = if a.name == "xmlns" {
                Some(Namespace::XMLNS_URL.to_string())
            } else {
                match prefix {
                    Some(p) => match self.lookup(p) {
                        Some(ns) => Some(ns),
                        None => return self.error(&format!("unbound prefix '{p}'")),
                    },
                    None => None,
                }
            };
            let key = (ns.clone(), local.to_string());
            if seen.contains(&key) {
                return self.error(&format!("duplicate attribute '{}'", a.name));
            }
            seen.push(key);
            self.doc.append_attribute(
                element,
                Attribute {
                    ns: ns.as_deref().map(Namespace::from_url),
                    prefix: prefix.map(Atom::new),
                    local: Atom::new(local),
                    value: a.value,
                    node: None,
                },
            );
        }
        self.append(element);
        if empty {
            self.scopes.pop();
        } else {
            self.open.push((element, name));
        }
        Ok(())
    }

    fn end_tag(&mut self) -> Result<(), XmlError> {
        self.expect("</")?;
        let name = self.name()?;
        self.skip_space();
        self.expect(">")?;
        match self.open.last() {
            Some((_, open)) if *open == name => {}
            Some((_, open)) => {
                let open = open.clone();
                return self.error(&format!(
                    "end tag '{name}' does not match the start tag '{open}'"
                ));
            }
            None => return self.error(&format!("end tag '{name}' without a start tag")),
        }
        self.flush_text();
        self.open.pop();
        self.scopes.pop();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dom::{DocumentKind, NodeData};

    fn parse(s: &str) -> Result<Document, XmlError> {
        let mut d = Document::new(DocumentKind::Xml, "application/xml");
        parse_into(&mut d, s).map(|_| d)
    }

    #[test]
    fn namespaces_resolve_on_elements_and_attributes() {
        let d = parse(
            "<?xml version='1.0'?>\n<r xmlns='urn:a' xmlns:b='urn:b'><b:c b:x='1' y='2'/><d xmlns=''/></r>",
        )
        .unwrap();
        let r = d.document_element().unwrap();
        let e = d.element(r).unwrap();
        assert_eq!(e.name.ns.map(|n| n.url()), Some("urn:a"));
        let c = d.first_child(r).unwrap();
        let ce = d.element(c).unwrap();
        assert_eq!(ce.name.ns.map(|n| n.url()), Some("urn:b"));
        assert_eq!(ce.name.prefix.map(|p| p.as_str()), Some("b"));
        assert_eq!(
            ce.attr_ns(Some(Namespace::from_url("urn:b")), "x"),
            Some("1")
        );
        assert_eq!(ce.attr("y"), Some("2"));
        let dd = d.next_sibling(c).unwrap();
        assert_eq!(d.element(dd).unwrap().name.ns, None);
    }

    #[test]
    fn entities_cdata_comments_and_pis() {
        let d = parse(
            "<!DOCTYPE r [<!ENTITY e 'v&#65;'>]><r a='&lt;&e;'>x&amp;&e;<![CDATA[<y>]]><!--c--><?p d?></r>",
        )
        .unwrap();
        let r = d.document_element().unwrap();
        assert_eq!(d.element(r).unwrap().attr("a"), Some("<vA"));
        let kids: Vec<_> = d.children(r).map(|k| d.data(k).clone()).collect();
        assert_eq!(kids[0], NodeData::Text("x&vA".to_string()));
        assert_eq!(kids[1], NodeData::CDataSection("<y>".to_string()));
        assert_eq!(kids[2], NodeData::Comment("c".to_string()));
        assert!(
            matches!(&kids[3], NodeData::ProcessingInstruction { target, data } if target == "p" && data == "d")
        );
        assert!(d.doctype().is_some());
    }

    #[test]
    fn well_formedness_errors() {
        for bad in [
            "<a><b></a>",
            "<a>",
            "<a/><b/>",
            "<p:a/>",
            "<a x='1' x='2'/>",
            "<a>&nope;</a>",
            "text",
            "<a x='<'/>",
            "<a><!-- a -- b --></a>",
            "<a xmlns:p=''/>",
            "",
        ] {
            assert!(parse(bad).is_err(), "{bad:?} should not parse");
        }
    }

    #[test]
    fn newlines_and_attribute_normalization() {
        let d = parse("<a v='x\r\ny\tz'>1\r2</a>").unwrap();
        let a = d.document_element().unwrap();
        assert_eq!(d.element(a).unwrap().attr("v"), Some("x y z"));
        assert_eq!(d.text_content(a).as_deref(), Some("1\n2"));
    }

    #[test]
    fn declared_encoding_is_read() {
        let (s, e) = decode(
            b"<?xml version='1.0' encoding='windows-1252'?><a>\xe9</a>",
            None,
        );
        assert_eq!(e, Encoding::Windows1252);
        assert!(s.contains('\u{e9}'));
        let (_, e) = decode(b"<a/>", Some("text/xml; charset=iso-8859-2"));
        assert_eq!(e, Encoding::Iso8859_2);
    }
}
