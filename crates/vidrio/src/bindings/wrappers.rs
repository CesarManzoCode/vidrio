//! One wrapper per node (vault/CONTRACT.md §3.4, D-03).
//!
//! Each document has a [`DocTable`]: its node→wrapper map. While the
//! document is active (it is a window's document) the table is a root set
//! of the heap. Every wrapper of a document also traces the whole table
//! when the collector reaches it, through a sentinel object marked once
//! per collection: a document's wrappers live or die together. Nodes are
//! never freed from their arena, so a wrapper reachable from script keeps
//! every other wrapper of its document (and their expandos) as long as a
//! node could be reached again through the tree. When the last wrapper of
//! an inactive document dies, its table goes with it.
//!
//! Adopting a node into another document copies it into the other arena
//! ([`crate::dom::Document::adopt_from`]); [`move_wrappers`] moves the
//! wrappers to the new nodes, so identity and expandos survive.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};

use vjs::gc::{Realm, RootSetId, Trace, Tracer};
use vjs::object::ObjectKind;
use vjs::{Agent, Object};

use super::generated::{ids, interface_by_name};
use super::meta::Id;
use super::platform::{self, Native};
use crate::dom::{DocRef, Document, Namespace, NodeData, NodeId, is_valid_custom_element_name};

/// The node→wrapper table of a document.
pub struct DocTable {
    doc: Weak<RefCell<Document>>,
    pub realm: Realm,
    sentinel: Object,
    wrappers: RefCell<HashMap<NodeId, Object>>,
    root: Cell<Option<RootSetId>>,
}

impl DocTable {
    /// Called when a wrapper of the document is traced: traces the table
    /// the first time in a collection.
    pub fn trace_from_wrapper(&self, tracer: &mut Tracer) {
        if tracer.object(self.sentinel) {
            self.trace_entries(tracer);
        }
    }

    fn trace_entries(&self, tracer: &mut Tracer) {
        for o in self.wrappers.borrow().values() {
            tracer.object(*o);
        }
    }

    pub fn get(&self, node: NodeId) -> Option<Object> {
        self.wrappers.borrow().get(&node).copied()
    }

    pub fn is_active(&self) -> bool {
        self.root.get().is_some()
    }
}

impl Trace for DocTable {
    fn trace(&self, tracer: &mut Tracer) {
        tracer.object(self.sentinel);
        self.trace_entries(tracer);
    }
}

thread_local! {
    static TABLES: RefCell<Vec<(Weak<RefCell<Document>>, Weak<DocTable>)>> =
        const { RefCell::new(Vec::new()) };
}

/// The table of `doc`, if one exists.
pub fn find_table(doc: &DocRef) -> Option<Rc<DocTable>> {
    let ptr = Rc::as_ptr(doc);
    TABLES.with(|t| {
        t.borrow()
            .iter()
            .find(|(d, _)| d.as_ptr() == ptr)
            .and_then(|(_, table)| table.upgrade())
    })
}

/// The table of `doc`, created (in the current realm) if needed.
pub fn doc_table(agent: &mut Agent, doc: &DocRef) -> Rc<DocTable> {
    if let Some(t) = find_table(doc) {
        if agent.heap.object_exists(t.sentinel) {
            return t;
        }
    }
    let realm = agent
        .running_context()
        .map(|c| c.realm)
        .or_else(|| agent.realms().first().copied())
        .expect("a realm");
    create_table(agent, doc, realm)
}

fn create_table(agent: &mut Agent, doc: &DocRef, realm: Realm) -> Rc<DocTable> {
    let sentinel = agent.heap.create_object(None, ObjectKind::Ordinary);
    let table = Rc::new(DocTable {
        doc: Rc::downgrade(doc),
        realm,
        sentinel,
        wrappers: RefCell::new(HashMap::new()),
        root: Cell::new(None),
    });
    TABLES.with(|t| {
        let mut t = t.borrow_mut();
        let ptr = Rc::as_ptr(doc);
        t.retain(|(d, table)| {
            d.strong_count() > 0 && table.strong_count() > 0 && d.as_ptr() != ptr
        });
        t.push((Rc::downgrade(doc), Rc::downgrade(&table)));
    });
    table
}

/// Makes `doc` active in `realm`: its table becomes a root set.
pub fn activate_document(agent: &mut Agent, doc: &DocRef, realm: Realm) {
    let table = match find_table(doc) {
        Some(t) if agent.heap.object_exists(t.sentinel) => t,
        _ => create_table(agent, doc, realm),
    };
    if table.root.get().is_none() {
        let id = agent.heap.add_root_set(table.clone() as Rc<dyn Trace>);
        table.root.set(Some(id));
    }
}

/// Stops rooting the table of `doc` (the document was discarded).
pub fn deactivate_document(agent: &mut Agent, doc: &DocRef) {
    if let Some(table) = find_table(doc) {
        if let Some(id) = table.root.take() {
            agent.heap.remove_root_set(id);
        }
    }
}

/// The existing wrapper of a node.
pub fn existing_wrapper(doc: &DocRef, node: NodeId) -> Option<Object> {
    find_table(doc).and_then(|t| t.get(node))
}

/// The interface of a node's wrapper.
pub fn node_interface(doc: &Document, node: NodeId) -> Id {
    let by_name = |name: &str| interface_by_name(name);
    let id = match &doc.node(node).data {
        NodeData::Document => {
            if doc.is_html() {
                Some(ids::Document)
            } else {
                by_name("XMLDocument")
            }
        }
        NodeData::DocumentType(_) => Some(ids::DocumentType),
        NodeData::DocumentFragment { .. } => {
            if doc.is_shadow_root(node) {
                Some(ids::ShadowRoot)
            } else {
                Some(ids::DocumentFragment)
            }
        }
        NodeData::Element(e) => match e.name.ns {
            Some(Namespace::Html) => {
                let local = e.name.local.as_str();
                let name = html_interface(local);
                let name = if name == "HTMLUnknownElement" && is_valid_custom_element_name(local) {
                    "HTMLElement"
                } else {
                    name
                };
                by_name(name).or_else(|| by_name("HTMLElement"))
            }
            Some(Namespace::Svg) => by_name("SVGElement"),
            Some(Namespace::MathMl) => by_name("MathMLElement"),
            _ => None,
        },
        NodeData::Attr(_) => Some(ids::Attr),
        NodeData::Text(_) => Some(ids::Text),
        NodeData::CDataSection(_) => Some(ids::CDATASection),
        NodeData::Comment(_) => Some(ids::Comment),
        NodeData::ProcessingInstruction { .. } => Some(ids::ProcessingInstruction),
    };
    id.unwrap_or(match &doc.node(node).data {
        NodeData::Document => ids::Document,
        _ => ids::Element,
    })
}

/// The interface of an HTML element by local name (HTML §4, the element
/// index).
pub fn html_interface(local: &str) -> &'static str {
    match local {
        "a" => "HTMLAnchorElement",
        "area" => "HTMLAreaElement",
        "audio" => "HTMLAudioElement",
        "base" => "HTMLBaseElement",
        "blockquote" | "q" => "HTMLQuoteElement",
        "body" => "HTMLBodyElement",
        "br" => "HTMLBRElement",
        "button" => "HTMLButtonElement",
        "canvas" => "HTMLCanvasElement",
        "caption" => "HTMLTableCaptionElement",
        "col" | "colgroup" => "HTMLTableColElement",
        "data" => "HTMLDataElement",
        "datalist" => "HTMLDataListElement",
        "del" | "ins" => "HTMLModElement",
        "details" => "HTMLDetailsElement",
        "dialog" => "HTMLDialogElement",
        "dir" => "HTMLDirectoryElement",
        "div" => "HTMLDivElement",
        "dl" => "HTMLDListElement",
        "embed" => "HTMLEmbedElement",
        "fieldset" => "HTMLFieldSetElement",
        "font" => "HTMLFontElement",
        "form" => "HTMLFormElement",
        "frame" => "HTMLFrameElement",
        "frameset" => "HTMLFrameSetElement",
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => "HTMLHeadingElement",
        "head" => "HTMLHeadElement",
        "hr" => "HTMLHRElement",
        "html" => "HTMLHtmlElement",
        "iframe" => "HTMLIFrameElement",
        "img" => "HTMLImageElement",
        "input" => "HTMLInputElement",
        "label" => "HTMLLabelElement",
        "legend" => "HTMLLegendElement",
        "li" => "HTMLLIElement",
        "link" => "HTMLLinkElement",
        "map" => "HTMLMapElement",
        "marquee" => "HTMLMarqueeElement",
        "menu" => "HTMLMenuElement",
        "meta" => "HTMLMetaElement",
        "meter" => "HTMLMeterElement",
        "object" => "HTMLObjectElement",
        "ol" => "HTMLOListElement",
        "optgroup" => "HTMLOptGroupElement",
        "option" => "HTMLOptionElement",
        "output" => "HTMLOutputElement",
        "p" => "HTMLParagraphElement",
        "param" => "HTMLParamElement",
        "picture" => "HTMLPictureElement",
        "pre" | "listing" | "xmp" => "HTMLPreElement",
        "progress" => "HTMLProgressElement",
        "script" => "HTMLScriptElement",
        "select" => "HTMLSelectElement",
        "slot" => "HTMLSlotElement",
        "source" => "HTMLSourceElement",
        "span" => "HTMLSpanElement",
        "style" => "HTMLStyleElement",
        "table" => "HTMLTableElement",
        "tbody" | "thead" | "tfoot" => "HTMLTableSectionElement",
        "td" | "th" => "HTMLTableCellElement",
        "template" => "HTMLTemplateElement",
        "textarea" => "HTMLTextAreaElement",
        "time" => "HTMLTimeElement",
        "title" => "HTMLTitleElement",
        "tr" => "HTMLTableRowElement",
        "track" => "HTMLTrackElement",
        "ul" => "HTMLUListElement",
        "video" => "HTMLVideoElement",
        "abbr" | "acronym" | "address" | "article" | "aside" | "b" | "basefont" | "bdi" | "bdo"
        | "big" | "center" | "cite" | "code" | "dd" | "dfn" | "dt" | "em" | "figcaption"
        | "figure" | "footer" | "header" | "hgroup" | "i" | "kbd" | "main" | "mark" | "nav"
        | "nobr" | "noembed" | "noframes" | "noscript" | "plaintext" | "rb" | "rp" | "rt"
        | "rtc" | "ruby" | "s" | "samp" | "search" | "section" | "small" | "strike" | "strong"
        | "sub" | "summary" | "sup" | "tt" | "u" | "var" | "wbr" => "HTMLElement",
        _ => "HTMLUnknownElement",
    }
}

/// The wrapper of `node` of `doc`, created on first use.
pub fn wrap_node(agent: &mut Agent, doc: &DocRef, node: NodeId) -> Object {
    wrap_node_with(agent, doc, node, None, None)
}

/// The wrapper of `node` of `doc`; if it does not exist yet it is created
/// with interface `iface` (the node's own if `None`) and prototype
/// `proto` (constructors called through a subclass).
pub fn wrap_node_with(
    agent: &mut Agent,
    doc: &DocRef,
    node: NodeId,
    iface: Option<Id>,
    proto: Option<Object>,
) -> Object {
    let table = doc_table(agent, doc);
    if let Some(o) = table.get(node) {
        if agent.heap.object_exists(o) {
            return o;
        }
    }
    let iface = iface.unwrap_or_else(|| node_interface(&doc.borrow(), node));
    let state = super::state_of(agent, table.realm)
        .or_else(|| super::current(agent))
        .expect("a window realm");
    let o = platform::create(agent, &state, iface, Native::Node(doc.clone(), node), proto);
    if let Some(p) = platform::platform_ref(agent, vjs::Value::Object(o)) {
        *p.get().table.borrow_mut() = Some(table.clone());
    }
    table.wrappers.borrow_mut().insert(node, o);
    o
}

/// Moves the wrappers of adopted nodes from `from` to `to`; `mapping`
/// pairs old and new node ids.
pub fn move_wrappers(agent: &mut Agent, from: &DocRef, to: &DocRef, mapping: &[(NodeId, NodeId)]) {
    let Some(source) = find_table(from) else {
        return;
    };
    let target = doc_table(agent, to);
    for &(old, new) in mapping {
        let Some(o) = source.wrappers.borrow_mut().remove(&old) else {
            continue;
        };
        if let Some(p) = platform::platform_ref(agent, vjs::Value::Object(o)) {
            *p.get().native.borrow_mut() = Native::Node(to.clone(), new);
            *p.get().table.borrow_mut() = Some(target.clone());
        }
        target.wrappers.borrow_mut().insert(new, o);
    }
}
