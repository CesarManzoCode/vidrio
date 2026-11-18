//! Shadow trees (DOM §4.2.2, §4.8 and `attachShadow` in §4.9): shadow
//! roots, slots and slottables, and the slot assignment algorithms.
//!
//! A shadow root is a `DocumentFragment` node whose host is the shadow
//! host, plus an entry in [`ShadowState::roots`]. Slot assignment lives in
//! side tables keyed by node.

use std::collections::HashMap;

use super::{Document, DomException, Namespace, NodeData, NodeId, is_valid_custom_element_name};

/// A shadow root's mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShadowRootMode {
    Open,
    Closed,
}

impl ShadowRootMode {
    pub fn as_str(self) -> &'static str {
        match self {
            ShadowRootMode::Open => "open",
            ShadowRootMode::Closed => "closed",
        }
    }
}

/// A shadow root's slot assignment.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SlotAssignmentMode {
    #[default]
    Named,
    Manual,
}

/// The state of a shadow root.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShadowRootData {
    pub mode: ShadowRootMode,
    pub delegates_focus: bool,
    pub slot_assignment: SlotAssignmentMode,
    pub clonable: bool,
    pub serializable: bool,
    pub declarative: bool,
    pub available_to_element_internals: bool,
}

/// `ShadowRootInit`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShadowRootInit {
    pub mode: ShadowRootMode,
    pub delegates_focus: bool,
    pub slot_assignment: SlotAssignmentMode,
    pub clonable: bool,
    pub serializable: bool,
}

impl ShadowRootInit {
    pub fn new(mode: ShadowRootMode) -> ShadowRootInit {
        ShadowRootInit {
            mode,
            delegates_focus: false,
            slot_assignment: SlotAssignmentMode::Named,
            clonable: false,
            serializable: false,
        }
    }
}

/// Shadow roots and slot assignment of a document's arena.
#[derive(Clone, Debug, Default)]
pub struct ShadowState {
    pub(crate) roots: HashMap<NodeId, ShadowRootData>,
    /// A slot's assigned nodes.
    pub(crate) assigned_nodes: HashMap<NodeId, Vec<NodeId>>,
    /// A slottable's assigned slot.
    pub(crate) assigned_slot: HashMap<NodeId, NodeId>,
    /// A slot's manually assigned nodes.
    pub(crate) manually_assigned: HashMap<NodeId, Vec<NodeId>>,
    /// A slottable's manual slot assignment.
    pub(crate) manual_slot: HashMap<NodeId, NodeId>,
}

/// Element local names that can host a shadow root (DOM §4.9, "valid
/// shadow host name").
const SHADOW_HOST_NAMES: [&str; 18] = [
    "article",
    "aside",
    "blockquote",
    "body",
    "div",
    "footer",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "header",
    "main",
    "nav",
    "p",
    "section",
    "span",
];

impl Document {
    /// True if the arena has any shadow root; slot work is skipped
    /// otherwise.
    pub(crate) fn has_shadow_roots(&self) -> bool {
        !self.shadow.roots.is_empty()
    }

    /// The state of the shadow root `root`.
    pub fn shadow_root_data(&self, root: NodeId) -> Option<&ShadowRootData> {
        self.shadow.roots.get(&root)
    }

    /// Mutable state of the shadow root `root` (the `declarative` flag and
    /// the like, set by the HTML parser of a later sprint).
    pub fn shadow_root_data_mut(&mut self, root: NodeId) -> Option<&mut ShadowRootData> {
        self.shadow.roots.get_mut(&root)
    }

    /// `attachShadow(init)` ("attach a shadow root", DOM §4.9). Custom
    /// element definitions do not exist yet, so none disables shadow.
    pub fn attach_shadow(
        &mut self,
        element: NodeId,
        init: ShadowRootInit,
    ) -> Result<NodeId, DomException> {
        let e = self
            .element(element)
            .ok_or(DomException::NotSupportedError)?;
        if e.name.ns != Some(Namespace::Html) {
            return Err(DomException::NotSupportedError);
        }
        let local = e.name.local;
        if !SHADOW_HOST_NAMES.contains(&local.as_str()) && !is_valid_custom_element_name(&local) {
            return Err(DomException::NotSupportedError);
        }
        if let Some(current) = e.shadow_root {
            let data = self.shadow.roots[&current];
            if !data.declarative || data.mode != init.mode {
                return Err(DomException::NotSupportedError);
            }
            let kids = self.child_nodes(current);
            for kid in kids {
                self.remove_node(kid, false);
            }
            if let Some(d) = self.shadow.roots.get_mut(&current) {
                d.declarative = false;
            }
            return Ok(current);
        }
        let root = self.push(NodeData::DocumentFragment {
            host: Some(element),
        });
        self.shadow.roots.insert(
            root,
            ShadowRootData {
                mode: init.mode,
                delegates_focus: init.delegates_focus,
                slot_assignment: init.slot_assignment,
                clonable: init.clonable,
                serializable: init.serializable,
                declarative: false,
                available_to_element_internals: is_valid_custom_element_name(&local),
            },
        );
        if let Some(e) = self.element_mut(element) {
            e.shadow_root = Some(root);
        }
        // The host's children may now have slots to go to.
        Ok(root)
    }

    /// The `shadowRoot` getter: the shadow root if it is open.
    pub fn open_shadow_root(&self, element: NodeId) -> Option<NodeId> {
        let root = self.shadow_root_of(element)?;
        (self.shadow.roots.get(&root)?.mode == ShadowRootMode::Open).then_some(root)
    }

    // ----- slots and slottables (DOM §4.2.2) -------------------------------

    /// True if `id` is a slot: an HTML `slot` element.
    pub fn is_slot(&self, id: NodeId) -> bool {
        self.is_html_element(id, "slot")
    }

    /// True if `id` is a slottable: an element or a Text node.
    pub fn is_slottable(&self, id: NodeId) -> bool {
        matches!(self.node(id).data, NodeData::Element(_) | NodeData::Text(_))
    }

    /// A slot's name: its `name` attribute, or the empty string.
    pub fn slot_name(&self, slot: NodeId) -> String {
        self.get_attribute(slot, "name").unwrap_or("").to_string()
    }

    /// A slottable's name: an element's `slot` attribute, or the empty
    /// string.
    pub fn slottable_name(&self, slottable: NodeId) -> String {
        self.get_attribute(slottable, "slot")
            .unwrap_or("")
            .to_string()
    }

    /// True if `slottable` is assigned (has an assigned slot).
    pub fn is_assigned(&self, slottable: NodeId) -> bool {
        self.shadow.assigned_slot.contains_key(&slottable)
    }

    /// The internal assigned slot of `slottable`.
    pub fn assigned_slot_internal(&self, slottable: NodeId) -> Option<NodeId> {
        self.shadow.assigned_slot.get(&slottable).copied()
    }

    /// A slot's assigned nodes.
    pub fn slot_assigned_nodes(&self, slot: NodeId) -> &[NodeId] {
        self.shadow
            .assigned_nodes
            .get(&slot)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// "Find a slot" for `slottable`; with `open`, slots of closed shadow
    /// roots are not found.
    pub fn find_slot(&self, slottable: NodeId, open: bool) -> Option<NodeId> {
        let parent = self.parent(slottable)?;
        let shadow = self.shadow_root_of(parent)?;
        let data = self.shadow.roots.get(&shadow)?;
        if open && data.mode != ShadowRootMode::Open {
            return None;
        }
        let mut n = self.first_child(shadow);
        if data.slot_assignment == SlotAssignmentMode::Manual {
            while let Some(c) = n {
                if self.is_slot(c)
                    && self
                        .shadow
                        .manually_assigned
                        .get(&c)
                        .is_some_and(|v| v.contains(&slottable))
                {
                    return Some(c);
                }
                n = self.following(c, Some(shadow));
            }
            return None;
        }
        let name = self.slottable_name(slottable);
        while let Some(c) = n {
            if self.is_slot(c) && self.slot_name(c) == name {
                return Some(c);
            }
            n = self.following(c, Some(shadow));
        }
        None
    }

    /// "Find slottables" for `slot`.
    pub fn find_slottables(&self, slot: NodeId) -> Vec<NodeId> {
        let root = self.root(slot);
        let Some(host) = self.shadow_root_host(root) else {
            return Vec::new();
        };
        let manual = self.shadow.roots[&root].slot_assignment == SlotAssignmentMode::Manual;
        if manual {
            return self
                .shadow
                .manually_assigned
                .get(&slot)
                .map(|v| {
                    v.iter()
                        .copied()
                        .filter(|&s| self.parent(s) == Some(host))
                        .collect()
                })
                .unwrap_or_default();
        }
        self.children(host)
            .filter(|&c| self.is_slottable(c) && self.find_slot(c, false) == Some(slot))
            .collect()
    }

    /// "Find flattened slottables" for `slot`.
    pub fn find_flattened_slottables(&self, slot: NodeId) -> Vec<NodeId> {
        let mut result = Vec::new();
        if !self.is_shadow_root(self.root(slot)) {
            return result;
        }
        let mut slottables = self.find_slottables(slot);
        if slottables.is_empty() {
            slottables = self
                .children(slot)
                .filter(|&c| self.is_slottable(c))
                .collect();
        }
        for node in slottables {
            if self.is_slot(node) && self.is_shadow_root(self.root(node)) {
                result.extend(self.find_flattened_slottables(node));
            } else {
                result.push(node);
            }
        }
        result
    }

    /// "Assign slottables" for `slot`.
    pub(crate) fn assign_slottables(&mut self, slot: NodeId) {
        let slottables = self.find_slottables(slot);
        let old = self
            .shadow
            .assigned_nodes
            .get(&slot)
            .cloned()
            .unwrap_or_default();
        if slottables != old {
            self.signal_slot_change(slot);
        }
        // Nodes that left the slot are no longer assigned to it.
        for node in &old {
            if !slottables.contains(node) && self.shadow.assigned_slot.get(node) == Some(&slot) {
                self.shadow.assigned_slot.remove(node);
            }
        }
        for &node in &slottables {
            self.shadow.assigned_slot.insert(node, slot);
        }
        if slottables.is_empty() {
            self.shadow.assigned_nodes.remove(&slot);
        } else {
            self.shadow.assigned_nodes.insert(slot, slottables);
        }
    }

    /// "Assign slottables for a tree" rooted at `root`.
    pub(crate) fn assign_slottables_for_tree(&mut self, root: NodeId) {
        if !self.has_shadow_roots() {
            return;
        }
        let slots: Vec<NodeId> = self
            .inclusive_descendants(root)
            .into_iter()
            .filter(|&n| self.is_slot(n))
            .collect();
        for slot in slots {
            self.assign_slottables(slot);
        }
    }

    /// "Assign a slot" for `slottable`.
    pub(crate) fn assign_a_slot(&mut self, slottable: NodeId) {
        if let Some(slot) = self.find_slot(slottable, false) {
            self.assign_slottables(slot);
        }
    }

    /// "Signal a slot change" for `slot`.
    pub(crate) fn signal_slot_change(&mut self, slot: NodeId) {
        if !self.observers.signal_slots.contains(&slot) {
            self.observers.signal_slots.push(slot);
        }
        self.queue_mutation_observer_microtask();
    }

    /// True if `id` has an inclusive descendant that is a slot.
    pub(crate) fn has_slot_descendant(&self, id: NodeId) -> bool {
        self.has_shadow_roots()
            && self
                .inclusive_descendants(id)
                .iter()
                .any(|&n| self.is_slot(n))
    }

    /// The `slot` and `name` attribute change steps (DOM §4.2.2.3, §4.2.2.4).
    pub(crate) fn slot_attribute_changed(
        &mut self,
        element: NodeId,
        local: &str,
        old: Option<&str>,
        value: Option<&str>,
        ns: Option<Namespace>,
    ) {
        if ns.is_some() || !self.has_shadow_roots() {
            return;
        }
        if value == old
            || (value.is_none() && old == Some(""))
            || (value == Some("") && old.is_none())
        {
            return;
        }
        if local == "slot" {
            if let Some(slot) = self.assigned_slot_internal(element) {
                self.assign_slottables(slot);
            }
            self.assign_a_slot(element);
        } else if local == "name" && self.is_slot(element) {
            let root = self.root(element);
            if self.is_shadow_root(root) {
                self.assign_slottables_for_tree(root);
            }
        }
    }

    /// `assignedSlot`: "find a slot" with the open flag.
    pub fn assigned_slot(&self, slottable: NodeId) -> Option<NodeId> {
        self.find_slot(slottable, true)
    }

    /// `assignedNodes({flatten})`.
    pub fn assigned_nodes(&self, slot: NodeId, flatten: bool) -> Vec<NodeId> {
        if flatten {
            self.find_flattened_slottables(slot)
        } else {
            self.slot_assigned_nodes(slot).to_vec()
        }
    }

    /// `assignedElements({flatten})`.
    pub fn assigned_elements(&self, slot: NodeId, flatten: bool) -> Vec<NodeId> {
        self.assigned_nodes(slot, flatten)
            .into_iter()
            .filter(|&n| self.is_element(n))
            .collect()
    }

    /// `slot.assign(...nodes)` for manual slot assignment.
    pub fn slot_assign(&mut self, slot: NodeId, nodes: &[NodeId]) {
        let previous = self
            .shadow
            .manually_assigned
            .remove(&slot)
            .unwrap_or_default();
        for node in previous {
            self.shadow.manual_slot.remove(&node);
        }
        let mut set: Vec<NodeId> = Vec::new();
        for &node in nodes {
            if let Some(old_slot) = self.shadow.manual_slot.get(&node).copied() {
                if let Some(list) = self.shadow.manually_assigned.get_mut(&old_slot) {
                    list.retain(|&n| n != node);
                }
            }
            self.shadow.manual_slot.insert(node, slot);
            if !set.contains(&node) {
                set.push(node);
            }
        }
        self.shadow.manually_assigned.insert(slot, set);
        let root = self.root(slot);
        self.assign_slottables_for_tree(root);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attach_and_assign_named_slots() {
        let mut doc = Document::new_html();
        let host = doc.create_html_element("div");
        doc.append_child(NodeId::DOCUMENT, host);
        let a = doc.create_html_element("span");
        doc.set_attribute(a, "slot", "x");
        let b = doc.create_text("b");
        doc.append_child(host, a);
        doc.append_child(host, b);
        let root = doc
            .attach_shadow(host, ShadowRootInit::new(ShadowRootMode::Open))
            .expect("attach");
        assert_eq!(doc.open_shadow_root(host), Some(root));
        assert_eq!(
            doc.attach_shadow(host, ShadowRootInit::new(ShadowRootMode::Open)),
            Err(DomException::NotSupportedError)
        );
        let named = doc.create_html_element("slot");
        doc.set_attribute(named, "name", "x");
        let default = doc.create_html_element("slot");
        doc.append_child(root, named);
        doc.append_child(root, default);
        assert_eq!(doc.assigned_nodes(named, false), vec![a]);
        assert_eq!(doc.assigned_nodes(default, false), vec![b]);
        assert_eq!(doc.assigned_slot(a), Some(named));
        doc.set_attribute(a, "slot", "");
        assert_eq!(doc.assigned_nodes(default, false), vec![a, b]);
        assert!(doc.assigned_nodes(named, false).is_empty());
        let p = doc.create_html_element("table");
        assert_eq!(
            doc.attach_shadow(p, ShadowRootInit::new(ShadowRootMode::Open)),
            Err(DomException::NotSupportedError)
        );
    }
}
