//! DOM core: a per-document arena of nodes (vault/CONTRACT.md §3.3).
//!
//! A [`Document`] owns a `Vec` of [`Node`]s addressed by [`NodeId`]; node 0
//! is the document itself. Each node links to its parent, first and last
//! child and both siblings. Names are [`Atom`]s and each element keeps its
//! attributes in a vector. Nodes are never freed: a removed node stays in
//! the arena, detached, until the document goes away (the garbage collector
//! of K3 decides what a wrapper still reaches).
//!
//! The node kinds are those of WHATWG DOM §4: `Document`, `DocumentType`,
//! `DocumentFragment`, `Element`, `Attr`, `Text`, `CDATASection`,
//! `Comment` and `ProcessingInstruction`. [`implementation`] holds the
//! document creation steps of `DOMImplementation` and name validation.

mod atom;
pub mod implementation;

pub use atom::Atom;

use crate::encoding::Encoding;
use crate::url::{Origin, Url};

/// Index of a node in its document's arena.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(pub u32);

impl NodeId {
    /// The document node of every arena.
    pub const DOCUMENT: NodeId = NodeId(0);

    fn index(self) -> usize {
        self.0 as usize
    }
}

/// A namespace (Infra §8 and DOM §1.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Namespace {
    Html,
    MathMl,
    Svg,
    XLink,
    Xml,
    Xmlns,
    Other(Atom),
}

impl Namespace {
    pub const HTML_URL: &'static str = "http://www.w3.org/1999/xhtml";
    pub const MATHML_URL: &'static str = "http://www.w3.org/1998/Math/MathML";
    pub const SVG_URL: &'static str = "http://www.w3.org/2000/svg";
    pub const XLINK_URL: &'static str = "http://www.w3.org/1999/xlink";
    pub const XML_URL: &'static str = "http://www.w3.org/XML/1998/namespace";
    pub const XMLNS_URL: &'static str = "http://www.w3.org/2000/xmlns/";

    /// The namespace named by `url`.
    pub fn from_url(url: &str) -> Namespace {
        match url {
            Self::HTML_URL => Namespace::Html,
            Self::MATHML_URL => Namespace::MathMl,
            Self::SVG_URL => Namespace::Svg,
            Self::XLINK_URL => Namespace::XLink,
            Self::XML_URL => Namespace::Xml,
            Self::XMLNS_URL => Namespace::Xmlns,
            other => Namespace::Other(Atom::new(other)),
        }
    }

    /// The namespace URL.
    pub fn url(self) -> &'static str {
        match self {
            Namespace::Html => Self::HTML_URL,
            Namespace::MathMl => Self::MATHML_URL,
            Namespace::Svg => Self::SVG_URL,
            Namespace::XLink => Self::XLINK_URL,
            Namespace::Xml => Self::XML_URL,
            Namespace::Xmlns => Self::XMLNS_URL,
            Namespace::Other(atom) => atom.as_str(),
        }
    }
}

/// The name of an element: namespace, prefix and local name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct QualName {
    pub ns: Option<Namespace>,
    pub prefix: Option<Atom>,
    pub local: Atom,
}

impl QualName {
    pub fn new(ns: Option<Namespace>, prefix: Option<Atom>, local: Atom) -> QualName {
        QualName { ns, prefix, local }
    }

    /// An element name in the HTML namespace without prefix.
    pub fn html(local: &str) -> QualName {
        QualName::new(Some(Namespace::Html), None, Atom::new(local))
    }

    /// The qualified name: `prefix:local`, or the local name.
    pub fn qualified(&self) -> String {
        match self.prefix {
            Some(prefix) => format!("{prefix}:{}", self.local),
            None => self.local.to_string(),
        }
    }
}

/// An attribute of an element (DOM §4.9.2). `node` is the `Attr` node that
/// stands for it once one has been asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attribute {
    pub ns: Option<Namespace>,
    pub prefix: Option<Atom>,
    pub local: Atom,
    pub value: String,
    pub node: Option<NodeId>,
}

impl Attribute {
    /// An attribute without namespace or prefix.
    pub fn new(local: &str, value: &str) -> Attribute {
        Attribute {
            ns: None,
            prefix: None,
            local: Atom::new(local),
            value: value.to_string(),
            node: None,
        }
    }

    /// The qualified name: `prefix:local`, or the local name.
    pub fn qualified_name(&self) -> String {
        match self.prefix {
            Some(prefix) => format!("{prefix}:{}", self.local),
            None => self.local.to_string(),
        }
    }
}

/// A document's mode (DOM §4.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuirksMode {
    NoQuirks,
    Quirks,
    LimitedQuirks,
}

/// A document's type (DOM §4.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DocumentKind {
    Xml,
    Html,
}

/// The data of a `DocumentType` node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Doctype {
    pub name: String,
    pub public_id: String,
    pub system_id: String,
}

/// State of the `script` element processing model kept on the element
/// (HTML §4.12.1.1): set by the parser, read by script preparation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScriptFlags {
    /// The element's parser document is its node document.
    pub parser_inserted: bool,
    pub already_started: bool,
    pub force_async: bool,
}

/// The data of an element.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ElementData {
    pub name: QualName,
    pub attrs: Vec<Attribute>,
    /// The `is` value of a customized built-in element.
    pub is_value: Option<Atom>,
    /// The template contents of an HTML `template` element.
    pub template_contents: Option<NodeId>,
    pub script: ScriptFlags,
    /// The form owner the parser associated (HTML §4.10.17.3).
    pub form_owner: Option<NodeId>,
}

impl ElementData {
    pub fn new(name: QualName) -> ElementData {
        ElementData {
            name,
            attrs: Vec::new(),
            is_value: None,
            template_contents: None,
            script: ScriptFlags::default(),
            form_owner: None,
        }
    }

    /// True if the element is in the HTML namespace with local name `local`.
    pub fn is_html(&self, local: &str) -> bool {
        self.name.ns == Some(Namespace::Html) && self.name.local == *local
    }

    /// The value of the attribute without namespace named `local`.
    pub fn attr(&self, local: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|a| a.ns.is_none() && a.local == *local)
            .map(|a| a.value.as_str())
    }

    /// The value of the attribute in `ns` named `local`.
    pub fn attr_ns(&self, ns: Option<Namespace>, local: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|a| a.ns == ns && a.local == *local)
            .map(|a| a.value.as_str())
    }
}

/// The data of an `Attr` node. While `owner` is set the value lives in the
/// owner's attribute list and `value` is not used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttrData {
    pub ns: Option<Namespace>,
    pub prefix: Option<Atom>,
    pub local: Atom,
    pub value: String,
    pub owner: Option<NodeId>,
}

/// What a node is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NodeData {
    Document,
    DocumentType(Doctype),
    /// `host` is the template element for template contents.
    DocumentFragment {
        host: Option<NodeId>,
    },
    Element(ElementData),
    Attr(AttrData),
    Text(String),
    CDataSection(String),
    Comment(String),
    ProcessingInstruction {
        target: String,
        data: String,
    },
}

/// A node with its tree links.
#[derive(Clone, Debug)]
pub struct Node {
    pub parent: Option<NodeId>,
    pub first_child: Option<NodeId>,
    pub last_child: Option<NodeId>,
    pub prev_sibling: Option<NodeId>,
    pub next_sibling: Option<NodeId>,
    pub data: NodeData,
}

impl Node {
    fn new(data: NodeData) -> Node {
        Node {
            parent: None,
            first_child: None,
            last_child: None,
            prev_sibling: None,
            next_sibling: None,
            data,
        }
    }
}

/// `nodeType` values (DOM §4.4).
pub mod node_type {
    pub const ELEMENT: u16 = 1;
    pub const ATTRIBUTE: u16 = 2;
    pub const TEXT: u16 = 3;
    pub const CDATA_SECTION: u16 = 4;
    pub const PROCESSING_INSTRUCTION: u16 = 7;
    pub const COMMENT: u16 = 8;
    pub const DOCUMENT: u16 = 9;
    pub const DOCUMENT_TYPE: u16 = 10;
    pub const DOCUMENT_FRAGMENT: u16 = 11;
}

/// A document and the arena of every node whose node document it is.
#[derive(Clone, Debug)]
pub struct Document {
    nodes: Vec<Node>,
    pub url: Url,
    pub origin: Origin,
    pub content_type: String,
    pub encoding: Encoding,
    pub mode: QuirksMode,
    pub kind: DocumentKind,
    /// The document is an iframe `srcdoc` document (HTML §4.8.5).
    pub is_srcdoc: bool,
    /// The parser cannot change the mode flag (HTML §13.2.6.4.1).
    pub parser_cannot_change_mode: bool,
}

fn about_blank() -> Url {
    Url::parse("about:blank", None).expect("about:blank parses")
}

impl Document {
    /// A new document with only its document node: URL `about:blank`,
    /// encoding UTF-8, no-quirks mode and an opaque origin (DOM §4.5).
    pub fn new(kind: DocumentKind, content_type: &str) -> Document {
        Document {
            nodes: vec![Node::new(NodeData::Document)],
            url: about_blank(),
            origin: Origin::new_opaque(),
            content_type: content_type.to_string(),
            encoding: Encoding::Utf8,
            mode: QuirksMode::NoQuirks,
            kind,
            is_srcdoc: false,
            parser_cannot_change_mode: false,
        }
    }

    /// A new empty HTML document (`text/html`).
    pub fn new_html() -> Document {
        Document::new(DocumentKind::Html, "text/html")
    }

    /// True for an HTML document.
    pub fn is_html(&self) -> bool {
        self.kind == DocumentKind::Html
    }

    /// The number of nodes in the arena, attached or not.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Always false: the arena holds at least the document node.
    pub fn is_empty(&self) -> bool {
        false
    }

    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id.index()]
    }

    pub fn node_mut(&mut self, id: NodeId) -> &mut Node {
        &mut self.nodes[id.index()]
    }

    fn push(&mut self, data: NodeData) -> NodeId {
        let id = NodeId(u32::try_from(self.nodes.len()).expect("arena overflow"));
        self.nodes.push(Node::new(data));
        id
    }

    // ----- creation -------------------------------------------------------

    /// Creates an element (DOM "create an element" without custom
    /// elements). An HTML `template` gets its template contents fragment.
    pub fn create_element(&mut self, name: QualName) -> NodeId {
        let id = self.push(NodeData::Element(ElementData::new(name)));
        if name.ns == Some(Namespace::Html) && name.local == "template" {
            let contents = self.push(NodeData::DocumentFragment { host: Some(id) });
            if let NodeData::Element(data) = &mut self.node_mut(id).data {
                data.template_contents = Some(contents);
            }
        }
        id
    }

    /// Creates an element in the HTML namespace without prefix.
    pub fn create_html_element(&mut self, local: &str) -> NodeId {
        self.create_element(QualName::html(local))
    }

    pub fn create_text(&mut self, data: &str) -> NodeId {
        self.push(NodeData::Text(data.to_string()))
    }

    pub fn create_cdata_section(&mut self, data: &str) -> NodeId {
        self.push(NodeData::CDataSection(data.to_string()))
    }

    pub fn create_comment(&mut self, data: &str) -> NodeId {
        self.push(NodeData::Comment(data.to_string()))
    }

    pub fn create_processing_instruction(&mut self, target: &str, data: &str) -> NodeId {
        self.push(NodeData::ProcessingInstruction {
            target: target.to_string(),
            data: data.to_string(),
        })
    }

    /// Creates a doctype (DOM "create a doctype"), not validated.
    pub fn create_document_type(&mut self, name: &str, public_id: &str, system_id: &str) -> NodeId {
        self.push(NodeData::DocumentType(Doctype {
            name: name.to_string(),
            public_id: public_id.to_string(),
            system_id: system_id.to_string(),
        }))
    }

    pub fn create_document_fragment(&mut self) -> NodeId {
        self.push(NodeData::DocumentFragment { host: None })
    }

    /// Creates an `Attr` node without owner element.
    pub fn create_attribute(
        &mut self,
        ns: Option<Namespace>,
        prefix: Option<Atom>,
        local: &str,
        value: &str,
    ) -> NodeId {
        self.push(NodeData::Attr(AttrData {
            ns,
            prefix,
            local: Atom::new(local),
            value: value.to_string(),
            owner: None,
        }))
    }

    // ----- node accessors --------------------------------------------------

    pub fn parent(&self, id: NodeId) -> Option<NodeId> {
        self.node(id).parent
    }

    pub fn first_child(&self, id: NodeId) -> Option<NodeId> {
        self.node(id).first_child
    }

    pub fn last_child(&self, id: NodeId) -> Option<NodeId> {
        self.node(id).last_child
    }

    pub fn next_sibling(&self, id: NodeId) -> Option<NodeId> {
        self.node(id).next_sibling
    }

    pub fn prev_sibling(&self, id: NodeId) -> Option<NodeId> {
        self.node(id).prev_sibling
    }

    /// The children of `id` in tree order.
    pub fn children(&self, id: NodeId) -> Children<'_> {
        Children {
            doc: self,
            next: self.first_child(id),
        }
    }

    /// `id` and its descendants in tree order (not entering template
    /// contents).
    pub fn inclusive_descendants(&self, id: NodeId) -> Vec<NodeId> {
        let mut out = vec![id];
        // Pre-order walk without recursion: children are pushed last first.
        let mut stack: Vec<NodeId> = Vec::new();
        let mut c = self.last_child(id);
        while let Some(n) = c {
            stack.push(n);
            c = self.prev_sibling(n);
        }
        while let Some(n) = stack.pop() {
            out.push(n);
            let mut c = self.last_child(n);
            while let Some(k) = c {
                stack.push(k);
                c = self.prev_sibling(k);
            }
        }
        out
    }

    pub fn data(&self, id: NodeId) -> &NodeData {
        &self.node(id).data
    }

    pub fn element(&self, id: NodeId) -> Option<&ElementData> {
        match &self.node(id).data {
            NodeData::Element(data) => Some(data),
            _ => None,
        }
    }

    pub fn element_mut(&mut self, id: NodeId) -> Option<&mut ElementData> {
        match &mut self.node_mut(id).data {
            NodeData::Element(data) => Some(data),
            _ => None,
        }
    }

    pub fn is_element(&self, id: NodeId) -> bool {
        matches!(self.node(id).data, NodeData::Element(_))
    }

    /// True if `id` is an element in the HTML namespace named `local`.
    pub fn is_html_element(&self, id: NodeId, local: &str) -> bool {
        self.element(id).is_some_and(|e| e.is_html(local))
    }

    /// The data of a `Text`, `CDATASection`, `Comment` or
    /// `ProcessingInstruction` node.
    pub fn character_data(&self, id: NodeId) -> Option<&str> {
        match &self.node(id).data {
            NodeData::Text(s) | NodeData::CDataSection(s) | NodeData::Comment(s) => Some(s),
            NodeData::ProcessingInstruction { data, .. } => Some(data),
            _ => None,
        }
    }

    /// Mutable data of a character data node.
    pub fn character_data_mut(&mut self, id: NodeId) -> Option<&mut String> {
        match &mut self.node_mut(id).data {
            NodeData::Text(s) | NodeData::CDataSection(s) | NodeData::Comment(s) => Some(s),
            NodeData::ProcessingInstruction { data, .. } => Some(data),
            _ => None,
        }
    }

    pub fn is_text(&self, id: NodeId) -> bool {
        matches!(self.node(id).data, NodeData::Text(_))
    }

    /// `nodeType` (DOM §4.4).
    pub fn node_type(&self, id: NodeId) -> u16 {
        match self.node(id).data {
            NodeData::Element(_) => node_type::ELEMENT,
            NodeData::Attr(_) => node_type::ATTRIBUTE,
            NodeData::Text(_) => node_type::TEXT,
            NodeData::CDataSection(_) => node_type::CDATA_SECTION,
            NodeData::ProcessingInstruction { .. } => node_type::PROCESSING_INSTRUCTION,
            NodeData::Comment(_) => node_type::COMMENT,
            NodeData::Document => node_type::DOCUMENT,
            NodeData::DocumentType(_) => node_type::DOCUMENT_TYPE,
            NodeData::DocumentFragment { .. } => node_type::DOCUMENT_FRAGMENT,
        }
    }

    /// `nodeName` (DOM §4.4).
    pub fn node_name(&self, id: NodeId) -> String {
        match &self.node(id).data {
            NodeData::Element(_) => self.tag_name(id),
            NodeData::Attr(a) => match a.prefix {
                Some(prefix) => format!("{prefix}:{}", a.local),
                None => a.local.to_string(),
            },
            NodeData::Text(_) => "#text".to_string(),
            NodeData::CDataSection(_) => "#cdata-section".to_string(),
            NodeData::ProcessingInstruction { target, .. } => target.clone(),
            NodeData::Comment(_) => "#comment".to_string(),
            NodeData::Document => "#document".to_string(),
            NodeData::DocumentType(d) => d.name.clone(),
            NodeData::DocumentFragment { .. } => "#document-fragment".to_string(),
        }
    }

    /// An element's HTML-uppercased qualified name (`tagName`, DOM §4.9).
    pub fn tag_name(&self, id: NodeId) -> String {
        let Some(e) = self.element(id) else {
            return String::new();
        };
        let qualified = e.name.qualified();
        if e.name.ns == Some(Namespace::Html) && self.is_html() {
            qualified.to_ascii_uppercase()
        } else {
            qualified
        }
    }

    /// The template contents of an HTML `template` element.
    pub fn template_contents(&self, id: NodeId) -> Option<NodeId> {
        self.element(id).and_then(|e| e.template_contents)
    }

    /// The document element: the first element child of the document.
    pub fn document_element(&self) -> Option<NodeId> {
        self.children(NodeId::DOCUMENT)
            .find(|&c| self.is_element(c))
    }

    /// The doctype child of the document.
    pub fn doctype(&self) -> Option<NodeId> {
        self.children(NodeId::DOCUMENT)
            .find(|&c| matches!(self.node(c).data, NodeData::DocumentType(_)))
    }

    /// The first child of the document element that is an HTML element
    /// named `local` (used for `head` and `body`).
    fn html_child(&self, local: &str) -> Option<NodeId> {
        let root = self.document_element()?;
        if !self.is_html_element(root, "html") {
            return None;
        }
        self.children(root)
            .find(|&c| self.is_html_element(c, local))
    }

    /// The `head` element (HTML §3.1.4).
    pub fn head(&self) -> Option<NodeId> {
        self.html_child("head")
    }

    /// The `body` element (HTML §3.1.4), without frameset handling.
    pub fn body(&self) -> Option<NodeId> {
        let root = self.document_element()?;
        if !self.is_html_element(root, "html") {
            return None;
        }
        self.children(root)
            .find(|&c| self.is_html_element(c, "body") || self.is_html_element(c, "frameset"))
    }

    /// The root of `id`: the furthest inclusive ancestor.
    pub fn root(&self, id: NodeId) -> NodeId {
        let mut node = id;
        while let Some(parent) = self.parent(node) {
            node = parent;
        }
        node
    }

    /// True if `ancestor` is an inclusive ancestor of `node`.
    pub fn is_inclusive_ancestor(&self, ancestor: NodeId, node: NodeId) -> bool {
        let mut current = Some(node);
        while let Some(n) = current {
            if n == ancestor {
                return true;
            }
            current = self.parent(n);
        }
        false
    }

    /// True if `ancestor` is a host-including inclusive ancestor of `node`
    /// (DOM §4.2.2): the walk continues from a fragment to its host.
    pub fn is_host_including_inclusive_ancestor(&self, ancestor: NodeId, node: NodeId) -> bool {
        let mut current = Some(node);
        while let Some(n) = current {
            if n == ancestor {
                return true;
            }
            current = match (&self.node(n).data, self.parent(n)) {
                (_, Some(parent)) => Some(parent),
                (NodeData::DocumentFragment { host }, None) => *host,
                _ => None,
            };
        }
        false
    }

    /// `textContent` getter (DOM §4.4) for elements and fragments: the
    /// concatenated data of the descendant Text nodes.
    pub fn text_content(&self, id: NodeId) -> Option<String> {
        match &self.node(id).data {
            NodeData::Document | NodeData::DocumentType(_) => None,
            NodeData::Element(_) | NodeData::DocumentFragment { .. } => {
                let mut out = String::new();
                for n in self.inclusive_descendants(id) {
                    if let NodeData::Text(s) | NodeData::CDataSection(s) = &self.node(n).data {
                        out.push_str(s);
                    }
                }
                Some(out)
            }
            NodeData::Attr(_) => self.attr_node_value(id),
            NodeData::Text(s) | NodeData::CDataSection(s) | NodeData::Comment(s) => Some(s.clone()),
            NodeData::ProcessingInstruction { data, .. } => Some(data.clone()),
        }
    }

    // ----- attributes ------------------------------------------------------

    /// The value of `id`'s attribute without namespace named `local`.
    pub fn get_attribute(&self, id: NodeId, local: &str) -> Option<&str> {
        self.element(id)?.attr(local)
    }

    /// Sets (or appends) `id`'s attribute without namespace named `local`.
    pub fn set_attribute(&mut self, id: NodeId, local: &str, value: &str) {
        if let Some(e) = self.element_mut(id) {
            if let Some(a) = e
                .attrs
                .iter_mut()
                .find(|a| a.ns.is_none() && a.local == *local)
            {
                a.value = value.to_string();
            } else {
                e.attrs.push(Attribute::new(local, value));
            }
        }
    }

    /// Appends an attribute to `id` (DOM "append an attribute").
    pub fn append_attribute(&mut self, id: NodeId, attr: Attribute) {
        if let Some(e) = self.element_mut(id) {
            e.attrs.push(attr);
        }
    }

    /// The `Attr` node for the attribute at `index` of `element`, creating it
    /// the first time.
    pub fn attribute_node(&mut self, element: NodeId, index: usize) -> Option<NodeId> {
        let attr = self.element(element)?.attrs.get(index)?.clone();
        if let Some(node) = attr.node {
            return Some(node);
        }
        let node = self.push(NodeData::Attr(AttrData {
            ns: attr.ns,
            prefix: attr.prefix,
            local: attr.local,
            value: String::new(),
            owner: Some(element),
        }));
        if let Some(e) = self.element_mut(element) {
            e.attrs[index].node = Some(node);
        }
        Some(node)
    }

    /// The value of an `Attr` node, read from its owner while it has one.
    pub fn attr_node_value(&self, attr: NodeId) -> Option<String> {
        let NodeData::Attr(data) = &self.node(attr).data else {
            return None;
        };
        match data.owner {
            Some(owner) => self
                .element(owner)?
                .attrs
                .iter()
                .find(|a| a.node == Some(attr))
                .map(|a| a.value.clone()),
            None => Some(data.value.clone()),
        }
    }

    // ----- tree mutation ---------------------------------------------------

    /// Detaches `child` from its parent, if any (the tree part of DOM
    /// "remove", without mutation records or live range updates).
    pub fn remove(&mut self, child: NodeId) {
        let Some(parent) = self.parent(child) else {
            return;
        };
        let prev = self.prev_sibling(child);
        let next = self.next_sibling(child);
        match prev {
            Some(p) => self.node_mut(p).next_sibling = next,
            None => self.node_mut(parent).first_child = next,
        }
        match next {
            Some(n) => self.node_mut(n).prev_sibling = prev,
            None => self.node_mut(parent).last_child = prev,
        }
        let node = self.node_mut(child);
        node.parent = None;
        node.prev_sibling = None;
        node.next_sibling = None;
    }

    /// Inserts `child` into `parent` before `reference` (or last), first
    /// removing it from its old parent (the tree part of DOM "insert"; a
    /// fragment `child` inserts its children). The caller checks validity.
    pub fn insert_before(&mut self, parent: NodeId, child: NodeId, reference: Option<NodeId>) {
        if matches!(self.node(child).data, NodeData::DocumentFragment { .. }) {
            let kids: Vec<NodeId> = self.children(child).collect();
            for kid in kids {
                self.insert_before(parent, kid, reference);
            }
            return;
        }
        // Inserting a node before itself leaves it where it is.
        let reference = if reference == Some(child) {
            self.next_sibling(child)
        } else {
            reference
        };
        self.remove(child);
        let prev = match reference {
            Some(r) => self.prev_sibling(r),
            None => self.last_child(parent),
        };
        {
            let node = self.node_mut(child);
            node.parent = Some(parent);
            node.prev_sibling = prev;
            node.next_sibling = reference;
        }
        match prev {
            Some(p) => self.node_mut(p).next_sibling = Some(child),
            None => self.node_mut(parent).first_child = Some(child),
        }
        match reference {
            Some(r) => self.node_mut(r).prev_sibling = Some(child),
            None => self.node_mut(parent).last_child = Some(child),
        }
    }

    /// Appends `child` as the last child of `parent`.
    pub fn append_child(&mut self, parent: NodeId, child: NodeId) {
        self.insert_before(parent, child, None);
    }

    /// Moves every child of `from` to the end of `to`, in order.
    pub fn reparent_children(&mut self, from: NodeId, to: NodeId) {
        while let Some(child) = self.first_child(from) {
            self.append_child(to, child);
        }
    }

    /// Appends `text` to the Text node `id`.
    pub fn append_text(&mut self, id: NodeId, text: &str) {
        if let NodeData::Text(s) = &mut self.node_mut(id).data {
            s.push_str(text);
        }
    }
}

/// Iterator over the children of a node.
pub struct Children<'a> {
    doc: &'a Document,
    next: Option<NodeId>,
}

impl Iterator for Children<'_> {
    type Item = NodeId;

    fn next(&mut self) -> Option<NodeId> {
        let current = self.next?;
        self.next = self.doc.next_sibling(current);
        Some(current)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_remove_and_reparent_keep_links_consistent() {
        let mut doc = Document::new_html();
        let html = doc.create_html_element("html");
        doc.append_child(NodeId::DOCUMENT, html);
        let a = doc.create_text("a");
        let b = doc.create_comment("b");
        let c = doc.create_html_element("p");
        doc.append_child(html, a);
        doc.append_child(html, c);
        doc.insert_before(html, b, Some(c));
        assert_eq!(doc.children(html).collect::<Vec<_>>(), vec![a, b, c]);
        doc.remove(b);
        assert_eq!(doc.children(html).collect::<Vec<_>>(), vec![a, c]);
        assert_eq!(doc.parent(b), None);
        assert_eq!(doc.prev_sibling(c), Some(a));
        doc.append_child(c, a);
        assert_eq!(doc.children(html).collect::<Vec<_>>(), vec![c]);
        assert_eq!(doc.children(c).collect::<Vec<_>>(), vec![a]);
        assert_eq!(doc.document_element(), Some(html));
        assert_eq!(doc.inclusive_descendants(html), vec![html, c, a]);
    }

    #[test]
    fn template_elements_get_contents_with_host() {
        let mut doc = Document::new_html();
        let t = doc.create_html_element("template");
        let contents = doc.template_contents(t).expect("contents");
        assert_eq!(
            doc.data(contents),
            &NodeData::DocumentFragment { host: Some(t) }
        );
        assert!(doc.is_host_including_inclusive_ancestor(t, contents));
        assert!(!doc.is_inclusive_ancestor(t, contents));
    }

    #[test]
    fn names_and_types() {
        let mut doc = Document::new_html();
        let div = doc.create_html_element("div");
        let svg = doc.create_element(QualName::new(
            Some(Namespace::Svg),
            None,
            Atom::new("foreignObject"),
        ));
        assert_eq!(doc.tag_name(div), "DIV");
        assert_eq!(doc.node_name(svg), "foreignObject");
        assert_eq!(doc.node_type(NodeId::DOCUMENT), node_type::DOCUMENT);
        assert_eq!(Namespace::from_url(Namespace::SVG_URL), Namespace::Svg);
        doc.set_attribute(div, "id", "x");
        doc.set_attribute(div, "id", "y");
        assert_eq!(doc.get_attribute(div, "id"), Some("y"));
        let attr = doc.attribute_node(div, 0).expect("attr");
        assert_eq!(doc.attr_node_value(attr).as_deref(), Some("y"));
        assert_eq!(doc.node_name(attr), "id");
    }
}
