//! Tree queries of DOM §4.2 and the read-only parts of `Node` (§4.4):
//! tree order, node length, the shadow-including relations,
//! `compareDocumentPosition`, `isEqualNode`, `getRootNode` and the
//! namespace lookups.
//!
//! Offsets into character data are counted in UTF-16 code units, as the
//! DOM counts them; the data itself is stored as UTF-8 ([`utf16_len`],
//! [`utf16_substring`]).

use std::cmp::Ordering;

use super::{Document, Namespace, NodeData, NodeId};

/// `compareDocumentPosition` bits (DOM §4.4).
pub mod position {
    pub const DISCONNECTED: u16 = 0x01;
    pub const PRECEDING: u16 = 0x02;
    pub const FOLLOWING: u16 = 0x04;
    pub const CONTAINS: u16 = 0x08;
    pub const CONTAINED_BY: u16 = 0x10;
    pub const IMPLEMENTATION_SPECIFIC: u16 = 0x20;
}

/// The length of `s` in UTF-16 code units.
pub fn utf16_len(s: &str) -> usize {
    s.chars().map(char::len_utf16).sum()
}

/// The `count` UTF-16 code units of `s` starting at code unit `start`
/// (clamped to the end). Half of a surrogate pair cut by either end comes
/// out as U+FFFD, because a Rust string cannot hold a lone surrogate.
pub fn utf16_substring(s: &str, start: usize, count: usize) -> String {
    let end = start.saturating_add(count);
    let mut out = String::new();
    let mut pos = 0;
    for c in s.chars() {
        let len = c.len_utf16();
        let next = pos + len;
        if next <= start {
            pos = next;
            continue;
        }
        if pos >= end {
            break;
        }
        if pos >= start && next <= end {
            out.push(c);
        } else {
            // Only a surrogate pair can be cut.
            out.push('\u{FFFD}');
        }
        pos = next;
    }
    out
}

impl Document {
    /// True for `Text`, `CDATASection`, `Comment` and
    /// `ProcessingInstruction` nodes.
    pub fn is_character_data(&self, id: NodeId) -> bool {
        self.character_data(id).is_some()
    }

    /// True for a `Text` or `CDATASection` node (a `Text` node in the DOM's
    /// sense: `CDATASection` inherits from `Text`).
    pub fn is_text_node(&self, id: NodeId) -> bool {
        matches!(
            self.node(id).data,
            NodeData::Text(_) | NodeData::CDataSection(_)
        )
    }

    pub fn is_doctype(&self, id: NodeId) -> bool {
        matches!(self.node(id).data, NodeData::DocumentType(_))
    }

    pub fn is_document_fragment(&self, id: NodeId) -> bool {
        matches!(self.node(id).data, NodeData::DocumentFragment { .. })
    }

    pub fn is_attr(&self, id: NodeId) -> bool {
        matches!(self.node(id).data, NodeData::Attr(_))
    }

    /// The index of `id` among its siblings.
    pub fn index(&self, id: NodeId) -> usize {
        let mut i = 0;
        let mut c = self.prev_sibling(id);
        while let Some(p) = c {
            i += 1;
            c = self.prev_sibling(p);
        }
        i
    }

    /// The node's length (DOM §4.2): 0 for doctypes and attributes, the
    /// data's length in UTF-16 code units for character data and the
    /// number of children otherwise.
    pub fn length(&self, id: NodeId) -> usize {
        match &self.node(id).data {
            NodeData::DocumentType(_) | NodeData::Attr(_) => 0,
            NodeData::Text(s) | NodeData::CDataSection(s) | NodeData::Comment(s) => utf16_len(s),
            NodeData::ProcessingInstruction { data, .. } => utf16_len(data),
            _ => self.children(id).count(),
        }
    }

    /// The child of `id` at `index`.
    pub fn child_at(&self, id: NodeId, index: usize) -> Option<NodeId> {
        self.children(id).nth(index)
    }

    /// The children of `id`, collected.
    pub fn child_nodes(&self, id: NodeId) -> Vec<NodeId> {
        self.children(id).collect()
    }

    pub fn has_child_nodes(&self, id: NodeId) -> bool {
        self.first_child(id).is_some()
    }

    /// The parent of `id` if it is an element.
    pub fn parent_element(&self, id: NodeId) -> Option<NodeId> {
        self.parent(id).filter(|&p| self.is_element(p))
    }

    /// True if `ancestor` is an ancestor (not `node` itself) of `node`.
    pub fn is_ancestor(&self, ancestor: NodeId, node: NodeId) -> bool {
        ancestor != node && self.is_inclusive_ancestor(ancestor, node)
    }

    /// The ancestors of `id`, nearest first.
    pub fn ancestors(&self, id: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut c = self.parent(id);
        while let Some(p) = c {
            out.push(p);
            c = self.parent(p);
        }
        out
    }

    /// The last inclusive descendant of `id` in tree order.
    pub fn last_inclusive_descendant(&self, id: NodeId) -> NodeId {
        let mut n = id;
        while let Some(c) = self.last_child(n) {
            n = c;
        }
        n
    }

    /// The node following `id` in tree order, staying inside `root`'s
    /// inclusive descendants when `root` is given.
    pub fn following(&self, id: NodeId, root: Option<NodeId>) -> Option<NodeId> {
        if let Some(c) = self.first_child(id) {
            return Some(c);
        }
        self.following_skipping_children(id, root)
    }

    /// The node following `id` in tree order that is not a descendant of
    /// `id`, inside `root` when given.
    pub fn following_skipping_children(&self, id: NodeId, root: Option<NodeId>) -> Option<NodeId> {
        let mut n = id;
        loop {
            if Some(n) == root {
                return None;
            }
            if let Some(s) = self.next_sibling(n) {
                return Some(s);
            }
            n = self.parent(n)?;
        }
    }

    /// The node preceding `id` in tree order, inside `root` when given.
    pub fn preceding(&self, id: NodeId, root: Option<NodeId>) -> Option<NodeId> {
        if Some(id) == root {
            return None;
        }
        match self.prev_sibling(id) {
            Some(s) => Some(self.last_inclusive_descendant(s)),
            None => self.parent(id),
        }
    }

    /// The inclusive ancestors of `id`, root first.
    fn ancestor_chain(&self, id: NodeId) -> Vec<NodeId> {
        let mut chain = vec![id];
        let mut c = self.parent(id);
        while let Some(p) = c {
            chain.push(p);
            c = self.parent(p);
        }
        chain.reverse();
        chain
    }

    /// Compares two nodes in tree order. Nodes in different trees compare
    /// by the order of their roots' ids, which is arbitrary but stable.
    pub fn tree_order(&self, a: NodeId, b: NodeId) -> Ordering {
        if a == b {
            return Ordering::Equal;
        }
        let ca = self.ancestor_chain(a);
        let cb = self.ancestor_chain(b);
        if ca[0] != cb[0] {
            return ca[0].cmp(&cb[0]);
        }
        let mut i = 0;
        while i < ca.len() && i < cb.len() && ca[i] == cb[i] {
            i += 1;
        }
        if i == ca.len() {
            // `a` is an ancestor of `b`.
            return Ordering::Less;
        }
        if i == cb.len() {
            return Ordering::Greater;
        }
        // ca[i] and cb[i] are siblings.
        let mut c = self.next_sibling(ca[i]);
        while let Some(s) = c {
            if s == cb[i] {
                return Ordering::Less;
            }
            c = self.next_sibling(s);
        }
        Ordering::Greater
    }

    /// True if `a` precedes `b` in tree order (same tree).
    pub fn is_preceding(&self, a: NodeId, b: NodeId) -> bool {
        self.root(a) == self.root(b) && self.tree_order(a, b) == Ordering::Less
    }

    /// The element children of `id`.
    pub fn element_children(&self, id: NodeId) -> Vec<NodeId> {
        self.children(id).filter(|&c| self.is_element(c)).collect()
    }

    pub fn first_element_child(&self, id: NodeId) -> Option<NodeId> {
        self.children(id).find(|&c| self.is_element(c))
    }

    pub fn last_element_child(&self, id: NodeId) -> Option<NodeId> {
        let mut c = self.last_child(id);
        while let Some(n) = c {
            if self.is_element(n) {
                return Some(n);
            }
            c = self.prev_sibling(n);
        }
        None
    }

    pub fn child_element_count(&self, id: NodeId) -> usize {
        self.children(id).filter(|&c| self.is_element(c)).count()
    }

    pub fn previous_element_sibling(&self, id: NodeId) -> Option<NodeId> {
        let mut c = self.prev_sibling(id);
        while let Some(n) = c {
            if self.is_element(n) {
                return Some(n);
            }
            c = self.prev_sibling(n);
        }
        None
    }

    pub fn next_element_sibling(&self, id: NodeId) -> Option<NodeId> {
        let mut c = self.next_sibling(id);
        while let Some(n) = c {
            if self.is_element(n) {
                return Some(n);
            }
            c = self.next_sibling(n);
        }
        None
    }

    /// The descendant elements of `id` in tree order (not `id` itself).
    pub fn descendant_elements(&self, id: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut n = self.first_child(id);
        while let Some(c) = n {
            if self.is_element(c) {
                out.push(c);
            }
            n = self.following(c, Some(id));
        }
        out
    }

    /// The child text content: the data of the Text children, in order.
    pub fn child_text_content(&self, id: NodeId) -> String {
        let mut out = String::new();
        for c in self.children(id) {
            if let NodeData::Text(s) | NodeData::CDataSection(s) = &self.node(c).data {
                out.push_str(s);
            }
        }
        out
    }

    /// The first element in tree order among the descendants of `root`
    /// whose ID is `id` (`getElementById`).
    pub fn get_element_by_id(&self, root: NodeId, id: &str) -> Option<NodeId> {
        if id.is_empty() {
            return None;
        }
        let mut n = self.first_child(root);
        while let Some(c) = n {
            if self.element(c).is_some_and(|e| e.attr("id") == Some(id)) {
                return Some(c);
            }
            n = self.following(c, Some(root));
        }
        None
    }

    /// `nodeValue` (DOM §4.4).
    pub fn node_value(&self, id: NodeId) -> Option<String> {
        match &self.node(id).data {
            NodeData::Attr(_) => self.attr_node_value(id),
            _ => self.character_data(id).map(str::to_string),
        }
    }

    /// The element an `Attr` node belongs to.
    pub fn owner_element(&self, attr: NodeId) -> Option<NodeId> {
        match &self.node(attr).data {
            NodeData::Attr(a) => a.owner,
            _ => None,
        }
    }

    /// The namespace of an element or attribute node.
    pub fn namespace_of(&self, id: NodeId) -> Option<Namespace> {
        match &self.node(id).data {
            NodeData::Element(e) => e.name.ns,
            NodeData::Attr(a) => a.ns,
            _ => None,
        }
    }

    // ----- shadow-including relations (DOM §4.2.2.1, §4.8) ---------------

    /// True if `id` is a shadow root.
    pub fn is_shadow_root(&self, id: NodeId) -> bool {
        self.shadow.roots.contains_key(&id)
    }

    /// The host of the shadow root `id`.
    pub fn shadow_root_host(&self, id: NodeId) -> Option<NodeId> {
        if !self.is_shadow_root(id) {
            return None;
        }
        match &self.node(id).data {
            NodeData::DocumentFragment { host } => *host,
            _ => None,
        }
    }

    /// The shadow root of `element`, whatever its mode.
    pub fn shadow_root_of(&self, element: NodeId) -> Option<NodeId> {
        self.element(element)?.shadow_root
    }

    /// True if `id` is a shadow host.
    pub fn is_shadow_host(&self, id: NodeId) -> bool {
        self.shadow_root_of(id).is_some()
    }

    /// The shadow-including root of `id`.
    pub fn shadow_including_root(&self, id: NodeId) -> NodeId {
        let mut root = self.root(id);
        while let Some(host) = self.shadow_root_host(root) {
            root = self.root(host);
        }
        root
    }

    /// True if `id` is connected: its shadow-including root is the
    /// document.
    pub fn is_connected(&self, id: NodeId) -> bool {
        self.shadow_including_root(id) == NodeId::DOCUMENT
    }

    /// True if `id` is in a document tree: its root is the document.
    pub fn is_in_document_tree(&self, id: NodeId) -> bool {
        self.root(id) == NodeId::DOCUMENT
    }

    /// True if `ancestor` is a shadow-including inclusive ancestor of
    /// `node`.
    pub fn is_shadow_including_inclusive_ancestor(&self, ancestor: NodeId, node: NodeId) -> bool {
        let mut current = Some(node);
        while let Some(n) = current {
            if n == ancestor {
                return true;
            }
            current = match self.parent(n) {
                Some(p) => Some(p),
                None => self.shadow_root_host(n),
            };
        }
        false
    }

    /// The shadow-including inclusive descendants of `id`, in
    /// shadow-including tree order (a host's shadow root comes right after
    /// the host, before its children).
    pub fn shadow_including_inclusive_descendants(&self, id: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut stack = vec![id];
        while let Some(n) = stack.pop() {
            out.push(n);
            let mut c = self.last_child(n);
            while let Some(k) = c {
                stack.push(k);
                c = self.prev_sibling(k);
            }
            if let Some(root) = self.shadow_root_of(n) {
                stack.push(root);
            }
        }
        out
    }

    /// `getRootNode({composed})`.
    pub fn get_root_node(&self, id: NodeId, composed: bool) -> NodeId {
        if composed {
            self.shadow_including_root(id)
        } else {
            self.root(id)
        }
    }

    /// "Retarget" `a` against `b` (DOM §4.8) for nodes.
    pub fn retarget(&self, a: NodeId, b: NodeId) -> NodeId {
        let mut a = a;
        loop {
            let root = self.root(a);
            match self.shadow_root_host(root) {
                Some(host) if !self.is_shadow_including_inclusive_ancestor(root, b) => a = host,
                _ => return a,
            }
        }
    }

    /// True if `a` is closed-shadow-hidden from `b` (DOM §4.8).
    pub fn is_closed_shadow_hidden(&self, a: NodeId, b: NodeId) -> bool {
        let mut a = a;
        loop {
            let root = self.root(a);
            let Some(host) = self.shadow_root_host(root) else {
                return false;
            };
            if self.is_shadow_including_inclusive_ancestor(root, b) {
                return false;
            }
            if self
                .shadow
                .roots
                .get(&root)
                .is_some_and(|s| s.mode == super::shadow::ShadowRootMode::Closed)
            {
                return true;
            }
            a = host;
        }
    }

    // ----- Node methods --------------------------------------------------

    /// `compareDocumentPosition(other)` on `this` (DOM §4.4).
    pub fn compare_document_position(&self, this: NodeId, other: NodeId) -> u16 {
        use position::*;
        if this == other {
            return 0;
        }
        let mut node1 = Some(other);
        let mut node2 = Some(this);
        let mut attr1 = None;
        let mut attr2 = None;
        if self.is_attr(other) {
            attr1 = Some(other);
            node1 = self.owner_element(other);
        }
        if self.is_attr(this) {
            attr2 = Some(this);
            node2 = self.owner_element(this);
            if let (Some(a1), Some(n1)) = (attr1, node1) {
                if node2 == Some(n1) {
                    for attr in &self
                        .element(n1)
                        .map(|e| e.attrs.clone())
                        .unwrap_or_default()
                    {
                        if attr.node == Some(a1) {
                            return IMPLEMENTATION_SPECIFIC | PRECEDING;
                        }
                        if attr.node == Some(this) {
                            return IMPLEMENTATION_SPECIFIC | FOLLOWING;
                        }
                    }
                }
            }
        }
        let (Some(n1), Some(n2)) = (node1, node2) else {
            let order = if other < this { PRECEDING } else { FOLLOWING };
            return DISCONNECTED | IMPLEMENTATION_SPECIFIC | order;
        };
        if self.root(n1) != self.root(n2) {
            let order = if self.root(n1) < self.root(n2) {
                PRECEDING
            } else {
                FOLLOWING
            };
            return DISCONNECTED | IMPLEMENTATION_SPECIFIC | order;
        }
        if (self.is_ancestor(n1, n2) && attr1.is_none()) || (n1 == n2 && attr2.is_some()) {
            return CONTAINS | PRECEDING;
        }
        if (self.is_ancestor(n2, n1) && attr2.is_none()) || (n1 == n2 && attr1.is_some()) {
            return CONTAINED_BY | FOLLOWING;
        }
        if self.tree_order(n1, n2) == Ordering::Less {
            return PRECEDING;
        }
        FOLLOWING
    }

    /// `contains(other)`: `other` is an inclusive descendant of `this`.
    pub fn contains(&self, this: NodeId, other: Option<NodeId>) -> bool {
        other.is_some_and(|o| self.is_inclusive_ancestor(this, o))
    }

    /// `isEqualNode(other)` for two nodes of this document.
    pub fn is_equal_node(&self, a: NodeId, b: NodeId) -> bool {
        nodes_equal(self, a, self, b)
    }

    /// "Locate a namespace" (DOM §4.4) for `prefix` from `id`.
    pub fn locate_namespace(&self, id: NodeId, prefix: Option<&str>) -> Option<String> {
        match &self.node(id).data {
            NodeData::Element(e) => {
                if prefix == Some("xml") {
                    return Some(Namespace::XML_URL.to_string());
                }
                if prefix == Some("xmlns") {
                    return Some(Namespace::XMLNS_URL.to_string());
                }
                if let Some(ns) = e.name.ns {
                    if e.name.prefix.map(|p| p.as_str()) == prefix {
                        return Some(ns.url().to_string());
                    }
                }
                let declared = e.attrs.iter().find(|a| {
                    a.ns == Some(Namespace::Xmlns)
                        && match prefix {
                            Some(p) => a.prefix.is_some_and(|ap| ap == "xmlns") && a.local == *p,
                            None => a.prefix.is_none() && a.local == "xmlns",
                        }
                });
                if let Some(a) = declared {
                    return if a.value.is_empty() {
                        None
                    } else {
                        Some(a.value.clone())
                    };
                }
                let parent = self.parent_element(id)?;
                self.locate_namespace(parent, prefix)
            }
            NodeData::Document => {
                let root = self.document_element()?;
                self.locate_namespace(root, prefix)
            }
            NodeData::DocumentType(_) | NodeData::DocumentFragment { .. } => None,
            NodeData::Attr(a) => {
                let owner = a.owner?;
                self.locate_namespace(owner, prefix)
            }
            _ => {
                let parent = self.parent_element(id)?;
                self.locate_namespace(parent, prefix)
            }
        }
    }

    /// "Locate a namespace prefix" (DOM §4.4) for `namespace` from the
    /// element `id`.
    fn locate_namespace_prefix(&self, id: NodeId, namespace: &str) -> Option<String> {
        let e = self.element(id)?;
        if e.name.ns.is_some_and(|ns| ns.url() == namespace) {
            if let Some(prefix) = e.name.prefix {
                return Some(prefix.to_string());
            }
        }
        if let Some(a) = e
            .attrs
            .iter()
            .find(|a| a.prefix.is_some_and(|p| p == "xmlns") && a.value == namespace)
        {
            return Some(a.local.to_string());
        }
        let parent = self.parent_element(id)?;
        self.locate_namespace_prefix(parent, namespace)
    }

    /// `lookupPrefix(namespace)`.
    pub fn lookup_prefix(&self, id: NodeId, namespace: Option<&str>) -> Option<String> {
        let namespace = namespace.filter(|ns| !ns.is_empty())?;
        match &self.node(id).data {
            NodeData::Element(_) => self.locate_namespace_prefix(id, namespace),
            NodeData::Document => {
                let root = self.document_element()?;
                self.locate_namespace_prefix(root, namespace)
            }
            NodeData::DocumentType(_) | NodeData::DocumentFragment { .. } => None,
            NodeData::Attr(a) => {
                let owner = a.owner?;
                self.locate_namespace_prefix(owner, namespace)
            }
            _ => {
                let parent = self.parent_element(id)?;
                self.locate_namespace_prefix(parent, namespace)
            }
        }
    }

    /// `lookupNamespaceURI(prefix)`.
    pub fn lookup_namespace_uri(&self, id: NodeId, prefix: Option<&str>) -> Option<String> {
        let prefix = prefix.filter(|p| !p.is_empty());
        self.locate_namespace(id, prefix)
    }

    /// `isDefaultNamespace(namespace)`.
    pub fn is_default_namespace(&self, id: NodeId, namespace: Option<&str>) -> bool {
        let namespace = namespace.filter(|ns| !ns.is_empty());
        self.locate_namespace(id, None).as_deref() == namespace
    }
}

/// `isEqualNode` (DOM §4.4) for nodes of possibly different documents.
pub fn nodes_equal(da: &Document, a: NodeId, db: &Document, b: NodeId) -> bool {
    let equal_here = match (&da.node(a).data, &db.node(b).data) {
        (NodeData::Document, NodeData::Document) => true,
        (NodeData::DocumentFragment { .. }, NodeData::DocumentFragment { .. }) => true,
        (NodeData::DocumentType(x), NodeData::DocumentType(y)) => x == y,
        (NodeData::Element(x), NodeData::Element(y)) => {
            x.name.ns == y.name.ns
                && x.name.prefix == y.name.prefix
                && x.name.local == y.name.local
                && x.attrs.len() == y.attrs.len()
                && x.attrs.iter().all(|ax| {
                    y.attrs
                        .iter()
                        .any(|ay| ax.ns == ay.ns && ax.local == ay.local && ax.value == ay.value)
                })
        }
        (NodeData::Attr(x), NodeData::Attr(y)) => {
            x.ns == y.ns && x.local == y.local && da.attr_node_value(a) == db.attr_node_value(b)
        }
        (
            NodeData::ProcessingInstruction {
                target: t1,
                data: d1,
            },
            NodeData::ProcessingInstruction {
                target: t2,
                data: d2,
            },
        ) => t1 == t2 && d1 == d2,
        (NodeData::Text(x), NodeData::Text(y))
        | (NodeData::CDataSection(x), NodeData::CDataSection(y))
        | (NodeData::Comment(x), NodeData::Comment(y)) => x == y,
        _ => false,
    };
    if !equal_here {
        return false;
    }
    let ka: Vec<NodeId> = da.children(a).collect();
    let kb: Vec<NodeId> = db.children(b).collect();
    ka.len() == kb.len() && ka.iter().zip(&kb).all(|(&x, &y)| nodes_equal(da, x, db, y))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16_offsets() {
        assert_eq!(utf16_len("a\u{1F600}b"), 4);
        assert_eq!(utf16_substring("a\u{1F600}b", 1, 2), "\u{1F600}");
        assert_eq!(utf16_substring("a\u{1F600}b", 2, 2), "\u{FFFD}b");
        assert_eq!(utf16_substring("abc", 1, 99), "bc");
        assert_eq!(utf16_substring("abc", 5, 1), "");
    }

    #[test]
    fn tree_order_and_positions() {
        let mut doc = Document::new_html();
        let html = doc.create_html_element("html");
        doc.append_child(NodeId::DOCUMENT, html);
        let a = doc.create_html_element("a");
        let b = doc.create_html_element("b");
        let t = doc.create_text("x");
        doc.append_child(html, a);
        doc.append_child(html, b);
        doc.append_child(a, t);
        assert_eq!(doc.tree_order(a, b), Ordering::Less);
        assert_eq!(doc.tree_order(t, b), Ordering::Less);
        assert_eq!(doc.tree_order(html, t), Ordering::Less);
        assert_eq!(doc.following(t, None), Some(b));
        assert_eq!(doc.preceding(b, None), Some(t));
        use position::*;
        assert_eq!(doc.compare_document_position(a, b), FOLLOWING);
        assert_eq!(doc.compare_document_position(t, html), CONTAINS | PRECEDING);
        assert_eq!(
            doc.compare_document_position(html, t),
            CONTAINED_BY | FOLLOWING
        );
        let loose = doc.create_html_element("i");
        assert_ne!(doc.compare_document_position(a, loose) & DISCONNECTED, 0);
        assert_eq!(doc.length(t), 1);
        assert_eq!(doc.length(html), 2);
        assert_eq!(doc.index(b), 1);
    }

    #[test]
    fn namespace_lookups() {
        let mut doc = Document::new_html();
        let html = doc.create_html_element("html");
        doc.append_child(NodeId::DOCUMENT, html);
        assert_eq!(
            doc.lookup_namespace_uri(html, None).as_deref(),
            Some(Namespace::HTML_URL)
        );
        assert!(doc.is_default_namespace(html, Some(Namespace::HTML_URL)));
        assert_eq!(doc.lookup_prefix(html, Some(Namespace::HTML_URL)), None);
        assert_eq!(
            doc.lookup_namespace_uri(html, Some("xml")).as_deref(),
            Some(Namespace::XML_URL)
        );
    }
}
