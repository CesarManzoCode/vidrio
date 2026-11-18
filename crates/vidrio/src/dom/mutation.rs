//! Mutation algorithms (DOM §4.2.3 to §4.2.9, §4.4, §4.5, §4.9, §4.10,
//! §4.11): pre-insertion validity, insert, append, replace, remove,
//! replace all, move, adopt, clone (with `template` contents and clonable
//! shadow roots), character data, `normalize`, `textContent`, attributes,
//! the `ParentNode`, `ChildNode` and `NonElementParentNode` mixins and
//! `insertAdjacent*`.
//!
//! Every algorithm keeps the live state current: live ranges, node
//! iterators, slot assignment and mutation records. None of them runs
//! script (custom element reactions do not exist yet), so they work on a
//! plain `&mut Document`.

use super::events::ListenerRef;
use super::implementation::{NameContext, is_valid_attribute_local_name, validate_and_extract};
use super::observer::RecordType;
use super::range::BoundaryPoint;
use super::shadow::{ShadowRootData, SlotAssignmentMode};
use super::tree::{utf16_len, utf16_substring};
use super::{
    Atom, AttrData, Attribute, Document, DomException, HtmlChange, Namespace, NodeData, NodeId,
};

/// A `(Node or DOMString)` argument of the mixin methods.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NodeOrString {
    Node(NodeId),
    String(String),
}

/// A copy of a subtree, read from one arena and built into another (or
/// the same): the shared part of clone, import and cross-document adopt.
#[derive(Clone, Debug)]
struct Snapshot {
    source: NodeId,
    data: NodeData,
    children: Vec<Snapshot>,
    template_contents: Option<(NodeId, Vec<Snapshot>)>,
    shadow: Option<(NodeId, ShadowRootData, Vec<Snapshot>)>,
    listeners: Vec<ListenerRef>,
}

/// True if `name` matches the qualified name of `a` (`prefix:local`).
fn qualified_name_is(a: &Attribute, name: &str) -> bool {
    match a.prefix {
        None => a.local == *name,
        Some(p) => {
            let p = p.as_str();
            name.len() == p.len() + 1 + a.local.len()
                && name.starts_with(p)
                && name[p.len()..].starts_with(':')
                && name[p.len() + 1..] == *a.local.as_str()
        }
    }
}

impl Document {
    // ----- node kinds ------------------------------------------------------

    fn can_have_children(&self, id: NodeId) -> bool {
        matches!(
            self.node(id).data,
            NodeData::Document | NodeData::DocumentFragment { .. } | NodeData::Element(_)
        )
    }

    fn is_insertable(&self, id: NodeId) -> bool {
        !matches!(self.node(id).data, NodeData::Document | NodeData::Attr(_))
    }

    fn is_document_node(&self, id: NodeId) -> bool {
        matches!(self.node(id).data, NodeData::Document)
    }

    /// True if a doctype follows `child` among its siblings.
    fn doctype_following(&self, child: NodeId) -> bool {
        let mut c = self.next_sibling(child);
        while let Some(n) = c {
            if self.is_doctype(n) {
                return true;
            }
            c = self.next_sibling(n);
        }
        false
    }

    /// True if an element precedes `child` among its siblings.
    fn element_preceding(&self, child: NodeId) -> bool {
        self.previous_element_sibling(child).is_some()
    }

    fn doctype_child(&self, parent: NodeId) -> Option<NodeId> {
        self.children(parent).find(|&c| self.is_doctype(c))
    }

    // ----- insertion (DOM §4.2.3) ------------------------------------------

    /// "Ensure pre-insertion validity" of `node` into `parent` before
    /// `child`.
    pub fn ensure_pre_insert_validity(
        &self,
        node: NodeId,
        parent: NodeId,
        child: Option<NodeId>,
    ) -> Result<(), DomException> {
        use DomException::{HierarchyRequestError as Hre, NotFoundError};
        if !self.can_have_children(parent) {
            return Err(Hre);
        }
        if self.is_host_including_inclusive_ancestor(node, parent) {
            return Err(Hre);
        }
        if let Some(c) = child {
            if self.parent(c) != Some(parent) {
                return Err(NotFoundError);
            }
        }
        if !self.is_insertable(node) {
            return Err(Hre);
        }
        let parent_is_document = self.is_document_node(parent);
        if (self.is_text_node(node) && parent_is_document)
            || (self.is_doctype(node) && !parent_is_document)
        {
            return Err(Hre);
        }
        if !parent_is_document {
            return Ok(());
        }
        let child_is_doctype = child.is_some_and(|c| self.is_doctype(c));
        let doctype_after = child.is_some_and(|c| self.doctype_following(c));
        match &self.node(node).data {
            NodeData::DocumentFragment { .. } => {
                let elements = self.child_element_count(node);
                if elements > 1 || self.children(node).any(|c| self.is_text_node(c)) {
                    return Err(Hre);
                }
                if elements == 1
                    && (self.first_element_child(parent).is_some()
                        || child_is_doctype
                        || doctype_after)
                {
                    return Err(Hre);
                }
            }
            NodeData::Element(_) => {
                if self.first_element_child(parent).is_some() || child_is_doctype || doctype_after {
                    return Err(Hre);
                }
            }
            NodeData::DocumentType(_) => {
                if self.doctype_child(parent).is_some()
                    || child.is_some_and(|c| self.element_preceding(c))
                    || (child.is_none() && self.first_element_child(parent).is_some())
                {
                    return Err(Hre);
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// "Pre-insert" `node` into `parent` before `child` (`insertBefore`).
    pub fn pre_insert(
        &mut self,
        node: NodeId,
        parent: NodeId,
        child: Option<NodeId>,
    ) -> Result<NodeId, DomException> {
        self.ensure_pre_insert_validity(node, parent, child)?;
        let reference = if child == Some(node) {
            self.next_sibling(node)
        } else {
            child
        };
        self.insert(node, parent, reference, false);
        Ok(node)
    }

    /// `appendChild(node)`.
    pub fn append_child_checked(
        &mut self,
        parent: NodeId,
        node: NodeId,
    ) -> Result<NodeId, DomException> {
        self.pre_insert(node, parent, None)
    }

    /// Inserts `child` into `parent` before `reference` (or last) with the
    /// DOM "insert" algorithm, without validity checks (the parser and the
    /// algorithms that checked already). Inserting a node before itself
    /// leaves it in place.
    pub fn insert_before(&mut self, parent: NodeId, child: NodeId, reference: Option<NodeId>) {
        let reference = if reference == Some(child) {
            self.next_sibling(child)
        } else {
            reference
        };
        self.insert(child, parent, reference, false);
    }

    /// Appends `child` to `parent` ("append" without validity checks).
    pub fn append_child(&mut self, parent: NodeId, child: NodeId) {
        self.insert(child, parent, None, false);
    }

    /// "Insert" `node` into `parent` before `child` (DOM §4.2.3).
    pub fn insert(
        &mut self,
        node: NodeId,
        parent: NodeId,
        child: Option<NodeId>,
        suppress_observers: bool,
    ) {
        let is_fragment = self.is_document_fragment(node);
        let nodes: Vec<NodeId> = if is_fragment {
            self.child_nodes(node)
        } else {
            vec![node]
        };
        let count = nodes.len();
        if count == 0 {
            return;
        }
        if is_fragment {
            for &n in &nodes {
                self.remove_node(n, true);
            }
            if self.has_registered_observers() {
                self.queue_tree_mutation_record(node, Vec::new(), nodes.clone(), None, None);
            }
        }
        if let Some(child) = child {
            if self.ranges.iter().any(Option::is_some) {
                let index = self.index(child);
                for r in self.ranges.iter_mut().flatten() {
                    if r.start.node == parent && r.start.offset > index {
                        r.start.offset += count;
                    }
                    if r.end.node == parent && r.end.offset > index {
                        r.end.offset += count;
                    }
                }
            }
        }
        let previous_sibling = match child {
            Some(c) => self.prev_sibling(c),
            None => self.last_child(parent),
        };
        for &n in &nodes {
            // Adopting within one arena only removes the node from its
            // old parent.
            if self.parent(n).is_some() {
                self.remove_node(n, false);
            }
            self.attach(n, parent, child);
        }
        if self.html_tracking {
            for &n in &nodes {
                self.html_changes.push(HtmlChange::Inserted(n));
            }
        }
        if !suppress_observers && self.has_registered_observers() {
            self.queue_tree_mutation_record(parent, nodes, Vec::new(), previous_sibling, child);
        }
    }

    /// Links `node` into `parent` and runs the slot steps of insert (and
    /// move).
    fn attach(&mut self, node: NodeId, parent: NodeId, child: Option<NodeId>) {
        self.tree_insert(parent, node, child);
        if !self.has_shadow_roots() {
            return;
        }
        if let Some(root) = self.shadow_root_of(parent) {
            if self.shadow.roots[&root].slot_assignment == SlotAssignmentMode::Named
                && self.is_slottable(node)
            {
                self.assign_a_slot(node);
            }
        }
        let parent_root = self.root(parent);
        if self.is_shadow_root(parent_root)
            && self.is_slot(parent)
            && self.slot_assigned_nodes(parent).is_empty()
        {
            self.signal_slot_change(parent);
        }
        let root = self.root(node);
        if self.is_shadow_root(root) {
            self.assign_slottables_for_tree(root);
        }
    }

    // ----- replacement (DOM §4.2.3) ----------------------------------------

    /// "Replace" `child` with `node` within `parent` (`replaceChild(node,
    /// child)`); returns `child`.
    pub fn replace_child(
        &mut self,
        parent: NodeId,
        node: NodeId,
        child: NodeId,
    ) -> Result<NodeId, DomException> {
        use DomException::{HierarchyRequestError as Hre, NotFoundError};
        if !self.can_have_children(parent) {
            return Err(Hre);
        }
        if self.is_host_including_inclusive_ancestor(node, parent) {
            return Err(Hre);
        }
        if self.parent(child) != Some(parent) {
            return Err(NotFoundError);
        }
        if !self.is_insertable(node) {
            return Err(Hre);
        }
        let parent_is_document = self.is_document_node(parent);
        if (self.is_text_node(node) && parent_is_document)
            || (self.is_doctype(node) && !parent_is_document)
        {
            return Err(Hre);
        }
        if parent_is_document {
            let other_element = self
                .children(parent)
                .any(|c| c != child && self.is_element(c));
            match &self.node(node).data {
                NodeData::DocumentFragment { .. } => {
                    let elements = self.child_element_count(node);
                    if elements > 1 || self.children(node).any(|c| self.is_text_node(c)) {
                        return Err(Hre);
                    }
                    if elements == 1 && (other_element || self.doctype_following(child)) {
                        return Err(Hre);
                    }
                }
                NodeData::Element(_) => {
                    if other_element || self.doctype_following(child) {
                        return Err(Hre);
                    }
                }
                NodeData::DocumentType(_) => {
                    let other_doctype = self
                        .children(parent)
                        .any(|c| c != child && self.is_doctype(c));
                    if other_doctype || self.element_preceding(child) {
                        return Err(Hre);
                    }
                }
                _ => {}
            }
        }
        let mut reference = self.next_sibling(child);
        if reference == Some(node) {
            reference = self.next_sibling(node);
        }
        let previous = self.prev_sibling(child);
        let mut removed = Vec::new();
        if self.parent(child).is_some() {
            removed.push(child);
            self.remove_node(child, true);
        }
        let nodes = if self.is_document_fragment(node) {
            self.child_nodes(node)
        } else {
            vec![node]
        };
        self.insert(node, parent, reference, true);
        if self.has_registered_observers() {
            self.queue_tree_mutation_record(parent, nodes, removed, previous, reference);
        }
        Ok(child)
    }

    /// "Replace all" of `parent`'s children with `node`.
    pub fn replace_all(&mut self, node: Option<NodeId>, parent: NodeId) {
        let removed = self.child_nodes(parent);
        let added = match node {
            Some(n) if self.is_document_fragment(n) => self.child_nodes(n),
            Some(n) => vec![n],
            None => Vec::new(),
        };
        for &c in &removed {
            self.remove_node(c, true);
        }
        if let Some(n) = node {
            self.insert(n, parent, None, true);
        }
        if (!added.is_empty() || !removed.is_empty()) && self.has_registered_observers() {
            self.queue_tree_mutation_record(parent, added, removed, None, None);
        }
    }

    // ----- removal (DOM §4.2.3) ---------------------------------------------

    /// "Pre-remove" `child` from `parent` (`removeChild`).
    pub fn pre_remove(&mut self, child: NodeId, parent: NodeId) -> Result<NodeId, DomException> {
        if self.parent(child) != Some(parent) {
            return Err(DomException::NotFoundError);
        }
        self.remove_node(child, false);
        Ok(child)
    }

    /// "Remove" `node` from its parent, if it has one.
    pub fn remove(&mut self, node: NodeId) {
        self.remove_node(node, false);
    }

    /// "Remove" with the suppress observers flag.
    pub(crate) fn remove_node(&mut self, node: NodeId, suppress_observers: bool) {
        let Some(parent) = self.parent(node) else {
            return;
        };
        let (previous, next) = self.detach(node, parent);
        if self.html_tracking {
            self.html_changes.push(HtmlChange::Removed(parent));
        }
        self.add_transient_observers(parent, node);
        if !suppress_observers && self.has_registered_observers() {
            self.queue_tree_mutation_record(parent, Vec::new(), vec![node], previous, next);
        }
    }

    /// The live range pre-remove steps (DOM §5.5) for `node`.
    fn live_range_pre_remove(&mut self, node: NodeId, parent: NodeId) {
        if self.ranges.iter().all(Option::is_none) {
            return;
        }
        let index = self.index(node);
        let mut ranges = std::mem::take(&mut self.ranges);
        for r in ranges.iter_mut().flatten() {
            if self.is_inclusive_ancestor(node, r.start.node) {
                r.start = BoundaryPoint::new(parent, index);
            }
            if self.is_inclusive_ancestor(node, r.end.node) {
                r.end = BoundaryPoint::new(parent, index);
            }
            if r.start.node == parent && r.start.offset > index {
                r.start.offset -= 1;
            }
            if r.end.node == parent && r.end.offset > index {
                r.end.offset -= 1;
            }
        }
        self.ranges = ranges;
    }

    /// The steps remove and move share: live ranges, node iterators,
    /// unlinking and slots. Returns the old siblings.
    fn detach(&mut self, node: NodeId, parent: NodeId) -> (Option<NodeId>, Option<NodeId>) {
        self.live_range_pre_remove(node, parent);
        self.node_iterators_pre_remove(node);
        let previous = self.prev_sibling(node);
        let next = self.next_sibling(node);
        self.tree_remove(node);
        if self.has_shadow_roots() {
            if let Some(slot) = self.assigned_slot_internal(node) {
                self.assign_slottables(slot);
            }
            let parent_root = self.root(parent);
            if self.is_shadow_root(parent_root)
                && self.is_slot(parent)
                && self.slot_assigned_nodes(parent).is_empty()
            {
                self.signal_slot_change(parent);
            }
            if self.has_slot_descendant(node) {
                self.assign_slottables_for_tree(parent_root);
                self.assign_slottables_for_tree(node);
            }
        }
        (previous, next)
    }

    // ----- move (DOM §4.2.3, `moveBefore`) ----------------------------------

    /// `moveBefore(node, child)` on `parent`: moves `node` keeping its
    /// state ("move").
    pub fn move_before(
        &mut self,
        parent: NodeId,
        node: NodeId,
        child: Option<NodeId>,
    ) -> Result<(), DomException> {
        use DomException::{HierarchyRequestError as Hre, NotFoundError};
        let child = if child == Some(node) {
            self.next_sibling(node)
        } else {
            child
        };
        if self.shadow_including_root(parent) != self.shadow_including_root(node) {
            return Err(Hre);
        }
        if self.is_host_including_inclusive_ancestor(node, parent) {
            return Err(Hre);
        }
        if let Some(c) = child {
            if self.parent(c) != Some(parent) {
                return Err(NotFoundError);
            }
        }
        if !self.is_element(node) && !self.is_character_data(node) {
            return Err(Hre);
        }
        let parent_is_document = self.is_document_node(parent);
        if self.is_text_node(node) && parent_is_document {
            return Err(Hre);
        }
        if parent_is_document
            && self.is_element(node)
            && (self.first_element_child(parent).is_some()
                || child.is_some_and(|c| self.is_doctype(c) || self.doctype_following(c)))
        {
            return Err(Hre);
        }
        let Some(old_parent) = self.parent(node) else {
            return Err(Hre);
        };
        let (old_previous, old_next) = self.detach(node, old_parent);
        if let Some(c) = child {
            if self.ranges.iter().any(Option::is_some) {
                let index = self.index(c);
                for r in self.ranges.iter_mut().flatten() {
                    if r.start.node == parent && r.start.offset > index {
                        r.start.offset += 1;
                    }
                    if r.end.node == parent && r.end.offset > index {
                        r.end.offset += 1;
                    }
                }
            }
        }
        let new_previous = match child {
            Some(c) => self.prev_sibling(c),
            None => self.last_child(parent),
        };
        self.attach(node, parent, child);
        if self.has_registered_observers() {
            self.queue_tree_mutation_record(
                old_parent,
                Vec::new(),
                vec![node],
                old_previous,
                old_next,
            );
            self.queue_tree_mutation_record(parent, vec![node], Vec::new(), new_previous, child);
        }
        Ok(())
    }

    // ----- adoption and cloning (DOM §4.2.4, §4.5) ----------------------------

    /// `adoptNode(node)` for a node of this document: removes it from its
    /// parent.
    pub fn adopt_node(&mut self, node: NodeId) -> Result<NodeId, DomException> {
        if self.is_document_node(node) {
            return Err(DomException::NotSupportedError);
        }
        if self.is_shadow_root(node) {
            return Err(DomException::HierarchyRequestError);
        }
        if matches!(
            self.node(node).data,
            NodeData::DocumentFragment { host: Some(_) }
        ) {
            return Ok(node);
        }
        self.remove(node);
        Ok(node)
    }

    /// `adoptNode(node)` for a node of another document `src`: the subtree
    /// (with template contents, shadow roots, attribute nodes and event
    /// listeners) moves into this arena. Returns the (old, new) id of every
    /// moved node, `node` first, so that the bindings can move the
    /// wrappers; empty if nothing moves (template contents).
    pub fn adopt_from(
        &mut self,
        src: &mut Document,
        node: NodeId,
    ) -> Result<Vec<(NodeId, NodeId)>, DomException> {
        if src.is_document_node(node) {
            return Err(DomException::NotSupportedError);
        }
        if src.is_shadow_root(node) {
            return Err(DomException::HierarchyRequestError);
        }
        if matches!(
            src.node(node).data,
            NodeData::DocumentFragment { host: Some(_) }
        ) {
            return Ok(Vec::new());
        }
        src.remove(node);
        let snap = src.snapshot(node, true, true);
        let mut map = Vec::new();
        self.build(&snap, true, &mut map);
        for (old, _) in &map {
            src.listeners.remove(old);
        }
        Ok(map)
    }

    /// `importNode(node, deep)`: a clone of a node of `src` in this
    /// document.
    pub fn import_node(
        &mut self,
        src: &Document,
        node: NodeId,
        deep: bool,
    ) -> Result<NodeId, DomException> {
        if src.is_document_node(node) || src.is_shadow_root(node) {
            return Err(DomException::NotSupportedError);
        }
        let snap = src.snapshot(node, deep, false);
        Ok(self.build(&snap, false, &mut Vec::new()))
    }

    /// `cloneNode(deep)` for a node other than the document ("clone a
    /// node"; [`Document::clone_document`] clones a document).
    pub fn clone_node(&mut self, node: NodeId, deep: bool) -> Result<NodeId, DomException> {
        if self.is_document_node(node) || self.is_shadow_root(node) {
            return Err(DomException::NotSupportedError);
        }
        let snap = self.snapshot(node, deep, false);
        Ok(self.build(&snap, false, &mut Vec::new()))
    }

    /// `cloneNode(deep)` on the document: a new document with the same
    /// type, URL, origin, content type, encoding and mode.
    pub fn clone_document(&self, deep: bool) -> Document {
        let mut copy = Document::new(self.kind, &self.content_type);
        copy.url = self.url.clone();
        copy.origin = self.origin.clone();
        copy.encoding = self.encoding;
        copy.mode = self.mode;
        if deep {
            for child in self.child_nodes(NodeId::DOCUMENT) {
                let snap = self.snapshot(child, true, false);
                let id = copy.build(&snap, false, &mut Vec::new());
                copy.tree_insert(NodeId::DOCUMENT, id, None);
            }
        }
        copy
    }

    /// Reads `node` (and its subtree when `deep`) for clone or adopt.
    fn snapshot(&self, node: NodeId, deep: bool, adopt: bool) -> Snapshot {
        let mut data = self.node(node).data.clone();
        match &mut data {
            NodeData::Element(e) if !adopt => {
                for a in &mut e.attrs {
                    a.node = None;
                }
            }
            NodeData::Attr(a) => {
                a.value = self.attr_node_value(node).unwrap_or_default();
                a.owner = None;
            }
            _ => {}
        }
        let subtree = |root: NodeId| -> Vec<Snapshot> {
            self.children(root)
                .map(|c| self.snapshot(c, true, adopt))
                .collect()
        };
        let children = if deep { subtree(node) } else { Vec::new() };
        let template_contents = match self.template_contents(node) {
            Some(tc) if deep => Some((tc, subtree(tc))),
            _ => None,
        };
        let shadow = self.shadow_root_of(node).and_then(|root| {
            let d = self.shadow.roots[&root];
            (adopt || d.clonable).then(|| (root, d, if deep { subtree(root) } else { Vec::new() }))
        });
        let listeners = if adopt {
            self.listeners.get(&node).cloned().unwrap_or_default()
        } else {
            Vec::new()
        };
        Snapshot {
            source: node,
            data,
            children,
            template_contents,
            shadow,
            listeners,
        }
    }

    /// Creates the nodes of `snap` in this arena; `map` gets each (old,
    /// new) pair.
    fn build(&mut self, snap: &Snapshot, adopt: bool, map: &mut Vec<(NodeId, NodeId)>) -> NodeId {
        let id = match &snap.data {
            NodeData::Element(e) => {
                let id = self.create_element(e.name);
                let mut attrs = Vec::with_capacity(e.attrs.len());
                for a in &e.attrs {
                    let mut a = a.clone();
                    if let Some(old) = a.node {
                        let new = self.push(NodeData::Attr(AttrData {
                            ns: a.ns,
                            prefix: a.prefix,
                            local: a.local,
                            value: String::new(),
                            owner: Some(id),
                        }));
                        map.push((old, new));
                        a.node = Some(new);
                    }
                    attrs.push(a);
                }
                if let Some(el) = self.element_mut(id) {
                    el.attrs = attrs;
                    el.is_value = e.is_value;
                    // HTML's cloning steps for `script` copy "already
                    // started".
                    el.script.already_started = e.script.already_started;
                    if adopt {
                        el.script = e.script;
                        el.custom_state = e.custom_state;
                    }
                }
                id
            }
            NodeData::Document | NodeData::DocumentFragment { .. } => {
                self.create_document_fragment()
            }
            other => self.push(other.clone()),
        };
        map.push((snap.source, id));
        for c in &snap.children {
            let k = self.build(c, adopt, map);
            self.tree_insert(id, k, None);
        }
        if let Some((old, kids)) = &snap.template_contents {
            if let Some(contents) = self.template_contents(id) {
                map.push((*old, contents));
                for c in kids {
                    let k = self.build(c, adopt, map);
                    self.tree_insert(contents, k, None);
                }
            }
        }
        if let Some((old, data, kids)) = &snap.shadow {
            let root = self.push(NodeData::DocumentFragment { host: Some(id) });
            let mut data = *data;
            if !adopt {
                data.clonable = true;
            }
            self.shadow.roots.insert(root, data);
            if let Some(el) = self.element_mut(id) {
                el.shadow_root = Some(root);
            }
            map.push((*old, root));
            for c in kids {
                let k = self.build(c, adopt, map);
                self.tree_insert(root, k, None);
            }
            self.assign_slottables_for_tree(root);
        }
        if adopt && !snap.listeners.is_empty() {
            self.listeners.insert(id, snap.listeners.clone());
        }
        id
    }

    // ----- character data (DOM §4.10, §4.11) ---------------------------------

    /// "Replace data" of `node`: `count` code units at `offset` become
    /// `data`.
    pub fn replace_data(
        &mut self,
        node: NodeId,
        offset: usize,
        count: usize,
        data: &str,
    ) -> Result<(), DomException> {
        let length = self.length(node);
        if offset > length {
            return Err(DomException::IndexSizeError);
        }
        let count = count.min(length - offset);
        let old = self.character_data(node).unwrap_or("").to_string();
        self.queue_mutation_record(
            RecordType::CharacterData,
            node,
            None,
            None,
            Some(&old),
            Vec::new(),
            Vec::new(),
            None,
            None,
        );
        let mut new = utf16_substring(&old, 0, offset);
        new.push_str(data);
        new.push_str(&utf16_substring(&old, offset + count, usize::MAX));
        if let Some(d) = self.character_data_mut(node) {
            *d = new;
        }
        let data_len = utf16_len(data);
        for r in self.ranges.iter_mut().flatten() {
            for bp in [&mut r.start, &mut r.end] {
                if bp.node != node {
                    continue;
                }
                if bp.offset > offset && bp.offset <= offset + count {
                    bp.offset = offset;
                } else if bp.offset > offset + count {
                    bp.offset = bp.offset + data_len - count;
                }
            }
        }
        Ok(())
    }

    /// `substringData(offset, count)`.
    pub fn substring_data(
        &self,
        node: NodeId,
        offset: usize,
        count: usize,
    ) -> Result<String, DomException> {
        if offset > self.length(node) {
            return Err(DomException::IndexSizeError);
        }
        Ok(utf16_substring(
            self.character_data(node).unwrap_or(""),
            offset,
            count,
        ))
    }

    /// `data` setter of character data.
    pub fn set_data(&mut self, node: NodeId, data: &str) {
        let length = self.length(node);
        let _ = self.replace_data(node, 0, length, data);
    }

    /// `appendData(data)`.
    pub fn append_data(&mut self, node: NodeId, data: &str) {
        let length = self.length(node);
        let _ = self.replace_data(node, length, 0, data);
    }

    /// `insertData(offset, data)`.
    pub fn insert_data(
        &mut self,
        node: NodeId,
        offset: usize,
        data: &str,
    ) -> Result<(), DomException> {
        self.replace_data(node, offset, 0, data)
    }

    /// `deleteData(offset, count)`.
    pub fn delete_data(
        &mut self,
        node: NodeId,
        offset: usize,
        count: usize,
    ) -> Result<(), DomException> {
        self.replace_data(node, offset, count, "")
    }

    /// "Split a Text node" (`splitText(offset)`).
    pub fn split_text(&mut self, node: NodeId, offset: usize) -> Result<NodeId, DomException> {
        let length = self.length(node);
        if offset > length {
            return Err(DomException::IndexSizeError);
        }
        let count = length - offset;
        let new_data = utf16_substring(self.character_data(node).unwrap_or(""), offset, count);
        let new_node = self.create_text(&new_data);
        if let Some(parent) = self.parent(node) {
            let next = self.next_sibling(node);
            self.insert(new_node, parent, next, false);
            let node_index = self.index(node);
            for r in self.ranges.iter_mut().flatten() {
                for bp in [&mut r.start, &mut r.end] {
                    if bp.node == node && bp.offset > offset {
                        bp.node = new_node;
                        bp.offset -= offset;
                    } else if bp.node == parent && bp.offset == node_index + 1 {
                        bp.offset += 1;
                    }
                }
            }
        }
        self.replace_data(node, offset, count, "")?;
        Ok(new_node)
    }

    /// `wholeText`: the data of the contiguous Text nodes around `node`.
    pub fn whole_text(&self, node: NodeId) -> String {
        let mut first = node;
        while let Some(p) = self.prev_sibling(first).filter(|&p| self.is_text_node(p)) {
            first = p;
        }
        let mut out = String::new();
        let mut c = Some(first);
        while let Some(n) = c.filter(|&n| self.is_text_node(n)) {
            out.push_str(self.character_data(n).unwrap_or(""));
            c = self.next_sibling(n);
        }
        out
    }

    /// `normalize()`.
    pub fn normalize(&mut self, node: NodeId) {
        let candidates: Vec<NodeId> = self
            .inclusive_descendants(node)
            .into_iter()
            .skip(1)
            .filter(|&n| self.is_text(n))
            .collect();
        for text in candidates {
            // An earlier step may have merged and removed it.
            if !self.is_ancestor(node, text) {
                continue;
            }
            let mut length = self.length(text);
            if length == 0 {
                self.remove(text);
                continue;
            }
            let mut data = String::new();
            let mut contiguous = Vec::new();
            let mut c = self.next_sibling(text);
            while let Some(s) = c.filter(|&s| self.is_text(s)) {
                data.push_str(self.character_data(s).unwrap_or(""));
                contiguous.push(s);
                c = self.next_sibling(s);
            }
            let _ = self.replace_data(text, length, 0, &data);
            for &current in &contiguous {
                let parent = self.parent(current);
                let index = self.index(current);
                for r in self.ranges.iter_mut().flatten() {
                    for bp in [&mut r.start, &mut r.end] {
                        if bp.node == current {
                            bp.offset += length;
                            bp.node = text;
                        } else if Some(bp.node) == parent && bp.offset == index {
                            bp.node = text;
                            bp.offset = length;
                        }
                    }
                }
                length += self.length(current);
            }
            for s in contiguous {
                self.remove(s);
            }
        }
    }

    /// `textContent` setter (`None` is null).
    pub fn set_text_content(&mut self, node: NodeId, value: Option<&str>) {
        let value = value.unwrap_or("");
        use super::node_type::*;
        match self.node_type(node) {
            DOCUMENT_FRAGMENT | ELEMENT => {
                let text = (!value.is_empty()).then(|| self.create_text(value));
                self.replace_all(text, node);
            }
            ATTRIBUTE => self.set_existing_attribute_value(node, value),
            TEXT | CDATA_SECTION | COMMENT | PROCESSING_INSTRUCTION => self.set_data(node, value),
            _ => {}
        }
    }

    /// `nodeValue` setter.
    pub fn set_node_value(&mut self, node: NodeId, value: Option<&str>) {
        let value = value.unwrap_or("");
        if self.is_attr(node) {
            self.set_existing_attribute_value(node, value);
        } else if self.is_character_data(node) {
            self.set_data(node, value);
        }
    }

    // ----- attributes (DOM §4.9.2) -------------------------------------------

    /// The index of the attribute named `qualified` ("get an attribute by
    /// name": lowercased for HTML elements of HTML documents).
    pub fn attribute_index_by_name(&self, element: NodeId, qualified: &str) -> Option<usize> {
        let e = self.element(element)?;
        if e.name.ns == Some(Namespace::Html) && self.is_html() {
            let lowered = qualified.to_ascii_lowercase();
            e.attrs.iter().position(|a| qualified_name_is(a, &lowered))
        } else {
            e.attrs.iter().position(|a| qualified_name_is(a, qualified))
        }
    }

    /// The index of the attribute in `ns` named `local`.
    pub fn attribute_index_ns(
        &self,
        element: NodeId,
        ns: Option<Namespace>,
        local: &str,
    ) -> Option<usize> {
        self.element(element)?
            .attrs
            .iter()
            .position(|a| a.ns == ns && a.local == *local)
    }

    fn namespace_arg(ns: Option<&str>) -> Option<Namespace> {
        ns.filter(|n| !n.is_empty()).map(Namespace::from_url)
    }

    /// "Handle attribute changes": the mutation record and the attribute
    /// change steps.
    fn handle_attribute_changes(
        &mut self,
        element: NodeId,
        ns: Option<Namespace>,
        local: Atom,
        old: Option<&str>,
        value: Option<&str>,
    ) {
        if self.html_tracking && ns.is_none() {
            self.html_changes
                .push(HtmlChange::Attribute(element, local));
        }
        self.queue_mutation_record(
            RecordType::Attributes,
            element,
            Some(local.as_str()),
            ns,
            old,
            Vec::new(),
            Vec::new(),
            None,
            None,
        );
        self.slot_attribute_changed(element, local.as_str(), old, value, ns);
    }

    /// "Change an attribute" (by index) to `value`.
    pub fn change_attribute(&mut self, element: NodeId, index: usize, value: &str) {
        let Some(e) = self.element_mut(element) else {
            return;
        };
        let a = &mut e.attrs[index];
        let old = std::mem::replace(&mut a.value, value.to_string());
        let (ns, local) = (a.ns, a.local);
        self.handle_attribute_changes(element, ns, local, Some(&old), Some(value));
    }

    /// "Append an attribute" with its change steps.
    pub fn append_attribute_with_steps(&mut self, element: NodeId, attr: Attribute) {
        let (ns, local, value, node) = (attr.ns, attr.local, attr.value.clone(), attr.node);
        let Some(e) = self.element_mut(element) else {
            return;
        };
        e.attrs.push(attr);
        if let Some(n) = node {
            if let NodeData::Attr(a) = &mut self.node_mut(n).data {
                a.owner = Some(element);
            }
        }
        self.handle_attribute_changes(element, ns, local, None, Some(&value));
    }

    /// "Remove an attribute" (by index); its `Attr` node, if any, keeps
    /// the value and loses its element.
    pub fn remove_attribute_at(&mut self, element: NodeId, index: usize) -> Option<Attribute> {
        let attr = self.element_mut(element)?.attrs.remove(index);
        if let Some(n) = attr.node {
            if let NodeData::Attr(a) = &mut self.node_mut(n).data {
                a.owner = None;
                a.value = attr.value.clone();
            }
        }
        self.handle_attribute_changes(element, attr.ns, attr.local, Some(&attr.value), None);
        Some(attr)
    }

    /// "Replace an attribute" at `index` with `new`.
    fn replace_attribute_at(&mut self, element: NodeId, index: usize, new: Attribute) {
        let (ns, local, value, node) = (new.ns, new.local, new.value.clone(), new.node);
        let Some(e) = self.element_mut(element) else {
            return;
        };
        let old = std::mem::replace(&mut e.attrs[index], new);
        if let Some(n) = old.node {
            if let NodeData::Attr(a) = &mut self.node_mut(n).data {
                a.owner = None;
                a.value = old.value.clone();
            }
        }
        if let Some(n) = node {
            if let NodeData::Attr(a) = &mut self.node_mut(n).data {
                a.owner = Some(element);
            }
        }
        self.handle_attribute_changes(element, ns, local, Some(&old.value), Some(&value));
    }

    /// `getAttribute(qualifiedName)`.
    pub fn get_attribute_by_name(&self, element: NodeId, qualified: &str) -> Option<String> {
        let i = self.attribute_index_by_name(element, qualified)?;
        Some(self.element(element)?.attrs[i].value.clone())
    }

    /// `getAttributeNS(namespace, localName)`.
    pub fn get_attribute_ns(
        &self,
        element: NodeId,
        ns: Option<&str>,
        local: &str,
    ) -> Option<String> {
        let i = self.attribute_index_ns(element, Self::namespace_arg(ns), local)?;
        Some(self.element(element)?.attrs[i].value.clone())
    }

    /// "Set an attribute value" in `ns` named `local`.
    pub fn set_attribute_value(
        &mut self,
        element: NodeId,
        local: &str,
        value: &str,
        prefix: Option<Atom>,
        ns: Option<Namespace>,
    ) {
        match self.attribute_index_ns(element, ns, local) {
            Some(i) => self.change_attribute(element, i, value),
            None => self.append_attribute_with_steps(
                element,
                Attribute {
                    ns,
                    prefix,
                    local: Atom::new(local),
                    value: value.to_string(),
                    node: None,
                },
            ),
        }
    }

    /// Sets the attribute without namespace named `local` ("set an
    /// attribute value", with mutation records).
    pub fn set_attribute(&mut self, element: NodeId, local: &str, value: &str) {
        self.set_attribute_value(element, local, value, None, None);
    }

    /// `setAttribute(qualifiedName, value)`.
    pub fn set_attribute_checked(
        &mut self,
        element: NodeId,
        qualified: &str,
        value: &str,
    ) -> Result<(), DomException> {
        if !is_valid_attribute_local_name(qualified) {
            return Err(DomException::InvalidCharacterError);
        }
        let html = self
            .element(element)
            .is_some_and(|e| e.name.ns == Some(Namespace::Html))
            && self.is_html();
        let name = if html {
            qualified.to_ascii_lowercase()
        } else {
            qualified.to_string()
        };
        match self.attribute_index_by_name(element, &name) {
            Some(i) => self.change_attribute(element, i, value),
            None => self.append_attribute_with_steps(element, Attribute::new(&name, value)),
        }
        Ok(())
    }

    /// `setAttributeNS(namespace, qualifiedName, value)`.
    pub fn set_attribute_ns(
        &mut self,
        element: NodeId,
        ns: Option<&str>,
        qualified: &str,
        value: &str,
    ) -> Result<(), DomException> {
        let (ns, prefix, local) = validate_and_extract(ns, qualified, NameContext::Attribute)?;
        self.set_attribute_value(element, &local, value, prefix, ns);
        Ok(())
    }

    /// `removeAttribute(qualifiedName)` ("remove an attribute by name").
    pub fn remove_attribute_by_name(
        &mut self,
        element: NodeId,
        qualified: &str,
    ) -> Option<Attribute> {
        let i = self.attribute_index_by_name(element, qualified)?;
        self.remove_attribute_at(element, i)
    }

    /// `removeAttributeNS(namespace, localName)`.
    pub fn remove_attribute_ns(
        &mut self,
        element: NodeId,
        ns: Option<&str>,
        local: &str,
    ) -> Option<Attribute> {
        let i = self.attribute_index_ns(element, Self::namespace_arg(ns), local)?;
        self.remove_attribute_at(element, i)
    }

    /// `toggleAttribute(qualifiedName, force)`.
    pub fn toggle_attribute(
        &mut self,
        element: NodeId,
        qualified: &str,
        force: Option<bool>,
    ) -> Result<bool, DomException> {
        if !is_valid_attribute_local_name(qualified) {
            return Err(DomException::InvalidCharacterError);
        }
        let html = self
            .element(element)
            .is_some_and(|e| e.name.ns == Some(Namespace::Html))
            && self.is_html();
        let name = if html {
            qualified.to_ascii_lowercase()
        } else {
            qualified.to_string()
        };
        match self.attribute_index_by_name(element, &name) {
            None => {
                if force != Some(false) {
                    self.append_attribute_with_steps(element, Attribute::new(&name, ""));
                    return Ok(true);
                }
                Ok(false)
            }
            Some(_) => {
                if force != Some(true) {
                    self.remove_attribute_by_name(element, &name);
                    return Ok(false);
                }
                Ok(true)
            }
        }
    }

    /// `hasAttribute(qualifiedName)`.
    pub fn has_attribute(&self, element: NodeId, qualified: &str) -> bool {
        self.attribute_index_by_name(element, qualified).is_some()
    }

    /// `hasAttributeNS(namespace, localName)`.
    pub fn has_attribute_ns(&self, element: NodeId, ns: Option<&str>, local: &str) -> bool {
        self.attribute_index_ns(element, Self::namespace_arg(ns), local)
            .is_some()
    }

    /// `hasAttributes()`.
    pub fn has_attributes(&self, element: NodeId) -> bool {
        self.element(element).is_some_and(|e| !e.attrs.is_empty())
    }

    /// `getAttributeNames()`.
    pub fn get_attribute_names(&self, element: NodeId) -> Vec<String> {
        self.element(element)
            .map(|e| e.attrs.iter().map(Attribute::qualified_name).collect())
            .unwrap_or_default()
    }

    /// `getAttributeNode(qualifiedName)`.
    pub fn get_attribute_node(&mut self, element: NodeId, qualified: &str) -> Option<NodeId> {
        let i = self.attribute_index_by_name(element, qualified)?;
        self.attribute_node(element, i)
    }

    /// `getAttributeNodeNS(namespace, localName)`.
    pub fn get_attribute_node_ns(
        &mut self,
        element: NodeId,
        ns: Option<&str>,
        local: &str,
    ) -> Option<NodeId> {
        let i = self.attribute_index_ns(element, Self::namespace_arg(ns), local)?;
        self.attribute_node(element, i)
    }

    /// `setAttributeNode(attr)` / `setAttributeNodeNS(attr)` ("set an
    /// attribute"): returns the replaced `Attr` node.
    pub fn set_attribute_node(
        &mut self,
        element: NodeId,
        attr: NodeId,
    ) -> Result<Option<NodeId>, DomException> {
        let NodeData::Attr(data) = &self.node(attr).data else {
            return Err(DomException::HierarchyRequestError);
        };
        let data = data.clone();
        if data.owner.is_some_and(|o| o != element) {
            return Err(DomException::InUseAttributeError);
        }
        let old_index = self.attribute_index_ns(element, data.ns, &data.local);
        if let Some(i) = old_index {
            if self
                .element(element)
                .is_some_and(|e| e.attrs[i].node == Some(attr))
            {
                return Ok(Some(attr));
            }
        }
        let new = Attribute {
            ns: data.ns,
            prefix: data.prefix,
            local: data.local,
            value: data.value,
            node: Some(attr),
        };
        match old_index {
            Some(i) => {
                let old_node = self.attribute_node(element, i);
                self.replace_attribute_at(element, i, new);
                Ok(old_node)
            }
            None => {
                self.append_attribute_with_steps(element, new);
                Ok(None)
            }
        }
    }

    /// `removeAttributeNode(attr)`.
    pub fn remove_attribute_node(
        &mut self,
        element: NodeId,
        attr: NodeId,
    ) -> Result<NodeId, DomException> {
        let index = self
            .element(element)
            .and_then(|e| e.attrs.iter().position(|a| a.node == Some(attr)))
            .ok_or(DomException::NotFoundError)?;
        self.remove_attribute_at(element, index);
        Ok(attr)
    }

    /// "Set an existing attribute value" (the `Attr.value` setter).
    pub fn set_existing_attribute_value(&mut self, attr: NodeId, value: &str) {
        let owner = match &mut self.node_mut(attr).data {
            NodeData::Attr(a) => match a.owner {
                Some(owner) => owner,
                None => {
                    a.value = value.to_string();
                    return;
                }
            },
            _ => return,
        };
        if let Some(i) = self
            .element(owner)
            .and_then(|e| e.attrs.iter().position(|a| a.node == Some(attr)))
        {
            self.change_attribute(owner, i, value);
        }
    }

    // ----- mixins (DOM §4.2.6 to §4.2.8) ---------------------------------------

    /// "Convert nodes into a node": strings become Text nodes; several
    /// nodes go into a new fragment.
    pub fn convert_nodes_into_node(
        &mut self,
        nodes: Vec<NodeOrString>,
    ) -> Result<NodeId, DomException> {
        let mut ids = Vec::with_capacity(nodes.len());
        for n in nodes {
            ids.push(match n {
                NodeOrString::Node(id) => id,
                NodeOrString::String(s) => self.create_text(&s),
            });
        }
        if ids.len() == 1 {
            return Ok(ids[0]);
        }
        let fragment = self.create_document_fragment();
        for id in ids {
            self.pre_insert(id, fragment, None)?;
        }
        Ok(fragment)
    }

    /// `prepend(...nodes)`.
    pub fn prepend(
        &mut self,
        parent: NodeId,
        nodes: Vec<NodeOrString>,
    ) -> Result<(), DomException> {
        let node = self.convert_nodes_into_node(nodes)?;
        let first = self.first_child(parent);
        self.pre_insert(node, parent, first)?;
        Ok(())
    }

    /// `append(...nodes)`.
    pub fn append(&mut self, parent: NodeId, nodes: Vec<NodeOrString>) -> Result<(), DomException> {
        let node = self.convert_nodes_into_node(nodes)?;
        self.pre_insert(node, parent, None)?;
        Ok(())
    }

    /// `replaceChildren(...nodes)`.
    pub fn replace_children(
        &mut self,
        parent: NodeId,
        nodes: Vec<NodeOrString>,
    ) -> Result<(), DomException> {
        let node = self.convert_nodes_into_node(nodes)?;
        self.ensure_pre_insert_validity(node, parent, None)?;
        self.replace_all(Some(node), parent);
        Ok(())
    }

    fn node_set(nodes: &[NodeOrString]) -> Vec<NodeId> {
        nodes
            .iter()
            .filter_map(|n| match n {
                NodeOrString::Node(id) => Some(*id),
                NodeOrString::String(_) => None,
            })
            .collect()
    }

    /// The first following sibling of `node` not in `set`.
    fn viable_next_sibling(&self, node: NodeId, set: &[NodeId]) -> Option<NodeId> {
        let mut c = self.next_sibling(node);
        while let Some(n) = c {
            if !set.contains(&n) {
                return Some(n);
            }
            c = self.next_sibling(n);
        }
        None
    }

    /// `before(...nodes)`.
    pub fn before(&mut self, node: NodeId, nodes: Vec<NodeOrString>) -> Result<(), DomException> {
        let Some(parent) = self.parent(node) else {
            return Ok(());
        };
        let set = Self::node_set(&nodes);
        let mut viable_previous = self.prev_sibling(node);
        while let Some(p) = viable_previous {
            if !set.contains(&p) {
                break;
            }
            viable_previous = self.prev_sibling(p);
        }
        let new = self.convert_nodes_into_node(nodes)?;
        let reference = match viable_previous {
            None => self.first_child(parent),
            Some(p) => self.next_sibling(p),
        };
        self.pre_insert(new, parent, reference)?;
        Ok(())
    }

    /// `after(...nodes)`.
    pub fn after(&mut self, node: NodeId, nodes: Vec<NodeOrString>) -> Result<(), DomException> {
        let Some(parent) = self.parent(node) else {
            return Ok(());
        };
        let set = Self::node_set(&nodes);
        let viable_next = self.viable_next_sibling(node, &set);
        let new = self.convert_nodes_into_node(nodes)?;
        self.pre_insert(new, parent, viable_next)?;
        Ok(())
    }

    /// `replaceWith(...nodes)`.
    pub fn replace_with(
        &mut self,
        node: NodeId,
        nodes: Vec<NodeOrString>,
    ) -> Result<(), DomException> {
        let Some(parent) = self.parent(node) else {
            return Ok(());
        };
        let set = Self::node_set(&nodes);
        let viable_next = self.viable_next_sibling(node, &set);
        let new = self.convert_nodes_into_node(nodes)?;
        if self.parent(node) == Some(parent) {
            self.replace_child(parent, new, node)?;
        } else {
            self.pre_insert(new, parent, viable_next)?;
        }
        Ok(())
    }

    /// `ChildNode.remove()`.
    pub fn remove_self(&mut self, node: NodeId) {
        self.remove(node);
    }

    /// "Insert adjacent" `node` at `position` relative to `element`
    /// (`insertAdjacentElement`).
    pub fn insert_adjacent_element(
        &mut self,
        element: NodeId,
        position: &str,
        node: NodeId,
    ) -> Result<Option<NodeId>, DomException> {
        match position.to_ascii_lowercase().as_str() {
            "beforebegin" => match self.parent(element) {
                None => Ok(None),
                Some(p) => self.pre_insert(node, p, Some(element)).map(Some),
            },
            "afterbegin" => {
                let first = self.first_child(element);
                self.pre_insert(node, element, first).map(Some)
            }
            "beforeend" => self.pre_insert(node, element, None).map(Some),
            "afterend" => match self.parent(element) {
                None => Ok(None),
                Some(p) => {
                    let next = self.next_sibling(element);
                    self.pre_insert(node, p, next).map(Some)
                }
            },
            _ => Err(DomException::SyntaxError),
        }
    }

    /// `insertAdjacentText(where, data)`.
    pub fn insert_adjacent_text(
        &mut self,
        element: NodeId,
        position: &str,
        data: &str,
    ) -> Result<(), DomException> {
        let text = self.create_text(data);
        self.insert_adjacent_element(element, position, text)?;
        Ok(())
    }

    /// `insertAdjacentHTML(position, string)` (HTML §8.5), with the HTML
    /// fragment parser for every document.
    pub fn insert_adjacent_html(
        &mut self,
        element: NodeId,
        position: &str,
        html: &str,
        scripting: bool,
    ) -> Result<(), DomException> {
        let position = position.to_ascii_lowercase();
        let context = match position.as_str() {
            "beforebegin" | "afterend" => match self.parent(element) {
                Some(p) if !self.is_document_node(p) => p,
                _ => return Err(DomException::NoModificationAllowedError),
            },
            "afterbegin" | "beforeend" => element,
            _ => return Err(DomException::SyntaxError),
        };
        let parent = context;
        let context = if !self.is_element(context)
            || (self.is_html() && self.is_html_element(context, "html"))
        {
            self.create_html_element("body")
        } else {
            context
        };
        let fragment = crate::html::parse_fragment(self, context, html, scripting);
        match position.as_str() {
            "beforebegin" => self.insert(fragment, parent, Some(element), false),
            "afterbegin" => {
                let first = self.first_child(element);
                self.insert(fragment, element, first, false);
            }
            "beforeend" => self.insert(fragment, element, None, false),
            _ => {
                let next = self.next_sibling(element);
                self.insert(fragment, parent, next, false);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dom::QualName;

    fn doc_with_body() -> (Document, NodeId) {
        let mut doc = Document::new_html();
        let html = doc.create_html_element("html");
        doc.append_child(NodeId::DOCUMENT, html);
        let body = doc.create_html_element("body");
        doc.append_child(html, body);
        (doc, body)
    }

    #[test]
    fn pre_insert_validity() {
        let (mut doc, body) = doc_with_body();
        let text = doc.create_text("t");
        assert_eq!(
            doc.pre_insert(text, NodeId::DOCUMENT, None),
            Err(DomException::HierarchyRequestError)
        );
        let second = doc.create_html_element("html");
        assert_eq!(
            doc.pre_insert(second, NodeId::DOCUMENT, None),
            Err(DomException::HierarchyRequestError)
        );
        let html = doc.document_element().expect("html");
        assert_eq!(
            doc.pre_insert(html, body, None),
            Err(DomException::HierarchyRequestError)
        );
        let other = doc.create_html_element("p");
        assert_eq!(
            doc.pre_insert(text, body, Some(other)),
            Err(DomException::NotFoundError)
        );
        let doctype = doc.create_document_type("html", "", "");
        assert_eq!(
            doc.pre_insert(doctype, NodeId::DOCUMENT, None),
            Err(DomException::HierarchyRequestError)
        );
        assert_eq!(
            doc.pre_insert(doctype, NodeId::DOCUMENT, Some(html)),
            Ok(doctype)
        );
    }

    #[test]
    fn replace_child_and_fragments() {
        let (mut doc, body) = doc_with_body();
        let a = doc.create_html_element("a");
        doc.append_child(body, a);
        let frag = doc.create_document_fragment();
        let x = doc.create_text("x");
        let y = doc.create_html_element("y");
        doc.append_child(frag, x);
        doc.append_child(frag, y);
        assert_eq!(doc.replace_child(body, frag, a), Ok(a));
        assert_eq!(doc.child_nodes(body), vec![x, y]);
        assert!(doc.child_nodes(frag).is_empty());
        assert_eq!(doc.parent(a), None);
    }

    #[test]
    fn clone_copies_template_contents() {
        let (mut doc, body) = doc_with_body();
        let t = doc.create_html_element("template");
        doc.append_child(body, t);
        let contents = doc.template_contents(t).expect("contents");
        let span = doc.create_html_element("span");
        doc.append_child(contents, span);
        doc.set_attribute(t, "id", "t");
        let copy = doc.clone_node(t, true).expect("clone");
        assert_eq!(doc.get_attribute(copy, "id"), Some("t"));
        let copied = doc.template_contents(copy).expect("contents");
        assert_ne!(copied, contents);
        assert_eq!(doc.child_nodes(copied).len(), 1);
        let shallow = doc.clone_node(t, false).expect("clone");
        assert!(
            doc.child_nodes(doc.template_contents(shallow).expect("c"))
                .is_empty()
        );
    }

    #[test]
    fn normalize_merges_text_and_moves_ranges() {
        let (mut doc, body) = doc_with_body();
        let a = doc.create_text("ab");
        let empty = doc.create_text("");
        let b = doc.create_text("cd");
        doc.append_child(body, a);
        doc.append_child(body, empty);
        doc.append_child(body, b);
        let r = doc.create_range();
        doc.range_set_start(r, b, 1).expect("start");
        doc.range_set_end(r, body, 3).expect("end");
        doc.normalize(body);
        assert_eq!(doc.child_nodes(body), vec![a]);
        assert_eq!(doc.character_data(a), Some("abcd"));
        assert_eq!(doc.range(r).start, BoundaryPoint::new(a, 3));
        assert_eq!(doc.range(r).end, BoundaryPoint::new(body, 1));
    }

    #[test]
    fn attributes_by_name_and_namespace() {
        let (mut doc, body) = doc_with_body();
        doc.set_attribute_checked(body, "DATA-X", "1").expect("set");
        assert_eq!(
            doc.get_attribute_by_name(body, "data-x").as_deref(),
            Some("1")
        );
        assert_eq!(
            doc.set_attribute_checked(body, "a b", "1"),
            Err(DomException::InvalidCharacterError)
        );
        doc.set_attribute_ns(body, Some(Namespace::XLINK_URL), "xlink:href", "u")
            .expect("ns");
        assert_eq!(
            doc.get_attribute_ns(body, Some(Namespace::XLINK_URL), "href")
                .as_deref(),
            Some("u")
        );
        assert_eq!(doc.get_attribute_names(body), vec!["data-x", "xlink:href"]);
        assert_eq!(doc.toggle_attribute(body, "hidden", None), Ok(true));
        assert_eq!(doc.toggle_attribute(body, "hidden", Some(true)), Ok(true));
        assert_eq!(doc.toggle_attribute(body, "hidden", None), Ok(false));
        let attr = doc.get_attribute_node(body, "data-x").expect("attr");
        doc.set_existing_attribute_value(attr, "2");
        assert_eq!(doc.get_attribute(body, "data-x"), Some("2"));
        doc.remove_attribute_node(body, attr).expect("remove");
        assert_eq!(doc.attr_node_value(attr).as_deref(), Some("2"));
        let svg = doc.create_element(QualName::new(Some(Namespace::Svg), None, Atom::new("svg")));
        assert_eq!(doc.set_attribute_node(svg, attr), Ok(None));
        assert_eq!(doc.get_attribute(svg, "data-x"), Some("2"));
        assert_eq!(
            doc.set_attribute_node(body, attr),
            Err(DomException::InUseAttributeError)
        );
    }

    #[test]
    fn child_node_mixins() {
        let (mut doc, body) = doc_with_body();
        let a = doc.create_html_element("a");
        let b = doc.create_html_element("b");
        doc.append_child(body, a);
        doc.append_child(body, b);
        doc.before(
            b,
            vec![NodeOrString::String("t".into()), NodeOrString::Node(b)],
        )
        .expect("before");
        assert_eq!(doc.child_nodes(body).len(), 3);
        assert_eq!(doc.child_nodes(body)[2], b);
        doc.replace_with(a, vec![NodeOrString::Node(b)])
            .expect("replaceWith");
        assert_eq!(doc.child_nodes(body)[0], b);
        assert_eq!(doc.parent(a), None);
        assert_eq!(
            doc.insert_adjacent_element(b, "nowhere", a),
            Err(DomException::SyntaxError)
        );
        assert_eq!(doc.insert_adjacent_element(b, "afterBegin", a), Ok(Some(a)));
        assert_eq!(doc.parent(a), Some(b));
        doc.insert_adjacent_html(b, "beforeend", "<i>x</i>y", false)
            .expect("html");
        assert_eq!(doc.text_content(b).as_deref(), Some("xy"));
        doc.move_before(body, a, None).expect("move");
        assert_eq!(doc.last_child(body), Some(a));
    }

    #[test]
    fn html_changes_are_recorded_only_when_tracking() {
        let (mut doc, body) = doc_with_body();
        let a = doc.create_html_element("script");
        doc.append_child(body, a);
        assert!(doc.html_changes.is_empty());
        doc.html_tracking = true;
        let b = doc.create_html_element("iframe");
        doc.append_child(body, b);
        doc.set_attribute(b, "src", "x.html");
        doc.remove(a);
        assert_eq!(
            doc.html_changes,
            vec![
                HtmlChange::Inserted(b),
                HtmlChange::Attribute(b, Atom::new("src")),
                HtmlChange::Removed(body),
            ]
        );
    }
}
