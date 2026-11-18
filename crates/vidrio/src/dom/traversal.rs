//! Traversal (DOM §6): `NodeIterator`, `TreeWalker` and `NodeFilter`.
//!
//! Filters are script callbacks, so the traversal methods take the
//! [`DocRef`] and the [`DomHost`] and borrow the document only between
//! filter calls. Node iterators live in the document, which runs their
//! pre-removing steps; tree walkers live behind a [`WalkerRef`].

use std::cell::RefCell;
use std::rc::Rc;

use super::host::DomHost;
use super::{DocRef, Document, DomError, DomException, HostRef, NodeId};

/// `NodeFilter` constants.
pub mod filter {
    pub const FILTER_ACCEPT: u16 = 1;
    pub const FILTER_REJECT: u16 = 2;
    pub const FILTER_SKIP: u16 = 3;
    pub const SHOW_ALL: u32 = 0xFFFF_FFFF;
    pub const SHOW_ELEMENT: u32 = 0x1;
    pub const SHOW_ATTRIBUTE: u32 = 0x2;
    pub const SHOW_TEXT: u32 = 0x4;
    pub const SHOW_CDATA_SECTION: u32 = 0x8;
    pub const SHOW_ENTITY_REFERENCE: u32 = 0x10;
    pub const SHOW_ENTITY: u32 = 0x20;
    pub const SHOW_PROCESSING_INSTRUCTION: u32 = 0x40;
    pub const SHOW_COMMENT: u32 = 0x80;
    pub const SHOW_DOCUMENT: u32 = 0x100;
    pub const SHOW_DOCUMENT_TYPE: u32 = 0x200;
    pub const SHOW_DOCUMENT_FRAGMENT: u32 = 0x400;
    pub const SHOW_NOTATION: u32 = 0x800;
}

use filter::{FILTER_ACCEPT, FILTER_REJECT, FILTER_SKIP};

/// A `NodeIterator` of a document.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct IteratorId(pub u32);

/// The state of a `NodeIterator`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeIterator {
    pub root: NodeId,
    pub reference: NodeId,
    pub pointer_before_reference: bool,
    pub what_to_show: u32,
    pub filter: Option<HostRef>,
    active: bool,
}

/// The state of a `TreeWalker`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeWalker {
    pub root: NodeId,
    pub current: NodeId,
    pub what_to_show: u32,
    pub filter: Option<HostRef>,
    active: bool,
}

/// A tree walker shared with its wrapper.
pub type WalkerRef = Rc<RefCell<TreeWalker>>;

impl TreeWalker {
    /// `createTreeWalker(root, whatToShow, filter)`.
    pub fn new(root: NodeId, what_to_show: u32, filter: Option<HostRef>) -> WalkerRef {
        Rc::new(RefCell::new(TreeWalker {
            root,
            current: root,
            what_to_show,
            filter,
            active: false,
        }))
    }
}

/// "Filter" (DOM §6.1): the node type mask, then the callback with the
/// traverser's active flag set around it.
fn run_filter(
    doc: &DocRef,
    host: &mut dyn DomHost,
    node: NodeId,
    what_to_show: u32,
    filter_cb: Option<HostRef>,
    active: &mut dyn FnMut(Option<bool>) -> bool,
) -> Result<u16, DomError> {
    if active(None) {
        return Err(DomException::InvalidStateError.into());
    }
    let n = doc.borrow().node_type(node) - 1;
    if what_to_show & (1u32 << n) == 0 {
        return Ok(FILTER_SKIP);
    }
    let Some(cb) = filter_cb else {
        return Ok(FILTER_ACCEPT);
    };
    active(Some(true));
    let result = host.call_node_filter(cb, node);
    active(Some(false));
    result.map_err(DomError::Thrown)
}

impl Document {
    /// `createNodeIterator(root, whatToShow, filter)`.
    pub fn create_node_iterator(
        &mut self,
        root: NodeId,
        what_to_show: u32,
        filter: Option<HostRef>,
    ) -> IteratorId {
        let it = NodeIterator {
            root,
            reference: root,
            pointer_before_reference: true,
            what_to_show,
            filter,
            active: false,
        };
        if let Some(i) = self.iterators.iter().position(Option::is_none) {
            self.iterators[i] = Some(it);
            return IteratorId(i as u32);
        }
        self.iterators.push(Some(it));
        IteratorId(u32::try_from(self.iterators.len() - 1).expect("too many iterators"))
    }

    /// The state of a node iterator.
    pub fn node_iterator(&self, id: IteratorId) -> &NodeIterator {
        self.iterators[id.0 as usize]
            .as_ref()
            .expect("released iterator")
    }

    fn node_iterator_mut(&mut self, id: IteratorId) -> &mut NodeIterator {
        self.iterators[id.0 as usize]
            .as_mut()
            .expect("released iterator")
    }

    /// Forgets a node iterator whose wrapper died.
    pub fn release_node_iterator(&mut self, id: IteratorId) {
        if let Some(slot) = self.iterators.get_mut(id.0 as usize) {
            *slot = None;
        }
    }

    /// The `NodeIterator` pre-removing steps (DOM §6.1) of every iterator
    /// for `to_be_removed`.
    pub(crate) fn node_iterators_pre_remove(&mut self, to_be_removed: NodeId) {
        if self.iterators.iter().all(Option::is_none) {
            return;
        }
        let mut iterators = std::mem::take(&mut self.iterators);
        for it in iterators.iter_mut().flatten() {
            if !self.is_inclusive_ancestor(to_be_removed, it.reference) || to_be_removed == it.root
            {
                continue;
            }
            if it.pointer_before_reference {
                // The first following node that is inside root but not
                // inside the removed node.
                let next = self.following_skipping_children(to_be_removed, Some(it.root));
                if let Some(next) = next {
                    it.reference = next;
                    continue;
                }
                it.pointer_before_reference = false;
            }
            it.reference = match self.prev_sibling(to_be_removed) {
                None => self
                    .parent(to_be_removed)
                    .expect("removed nodes have a parent"),
                Some(prev) => self.last_inclusive_descendant(prev),
            };
        }
        self.iterators = iterators;
    }
}

/// "Traverse" a node iterator (DOM §6.1) forward or backward.
fn traverse_iterator(
    doc: &DocRef,
    host: &mut dyn DomHost,
    id: IteratorId,
    next: bool,
) -> Result<Option<NodeId>, DomError> {
    let (mut node, mut before, root, what_to_show, filter_cb) = {
        let d = doc.borrow();
        let it = d.node_iterator(id);
        (
            it.reference,
            it.pointer_before_reference,
            it.root,
            it.what_to_show,
            it.filter,
        )
    };
    loop {
        {
            let d = doc.borrow();
            if next {
                if !before {
                    match d.following(node, Some(root)) {
                        Some(n) => node = n,
                        None => return Ok(None),
                    }
                } else {
                    before = false;
                }
            } else if before {
                match d.preceding(node, Some(root)) {
                    Some(n) => node = n,
                    None => return Ok(None),
                }
            } else {
                before = true;
            }
        }
        let mut active = |set: Option<bool>| {
            let mut d = doc.borrow_mut();
            let it = d.node_iterator_mut(id);
            if let Some(v) = set {
                it.active = v;
            }
            it.active
        };
        let result = run_filter(doc, host, node, what_to_show, filter_cb, &mut active)?;
        if result == FILTER_ACCEPT {
            break;
        }
    }
    let mut d = doc.borrow_mut();
    let it = d.node_iterator_mut(id);
    it.reference = node;
    it.pointer_before_reference = before;
    Ok(Some(node))
}

/// `NodeIterator.nextNode()`.
pub fn iterator_next_node(
    doc: &DocRef,
    host: &mut dyn DomHost,
    id: IteratorId,
) -> Result<Option<NodeId>, DomError> {
    traverse_iterator(doc, host, id, true)
}

/// `NodeIterator.previousNode()`.
pub fn iterator_previous_node(
    doc: &DocRef,
    host: &mut dyn DomHost,
    id: IteratorId,
) -> Result<Option<NodeId>, DomError> {
    traverse_iterator(doc, host, id, false)
}

// ----- TreeWalker (DOM §6.2) -----------------------------------------------

fn walker_filter(
    doc: &DocRef,
    host: &mut dyn DomHost,
    walker: &WalkerRef,
    node: NodeId,
) -> Result<u16, DomError> {
    let (what_to_show, filter_cb) = {
        let w = walker.borrow();
        (w.what_to_show, w.filter)
    };
    let mut active = |set: Option<bool>| {
        let mut w = walker.borrow_mut();
        if let Some(v) = set {
            w.active = v;
        }
        w.active
    };
    run_filter(doc, host, node, what_to_show, filter_cb, &mut active)
}

fn set_current(walker: &WalkerRef, node: NodeId) -> Option<NodeId> {
    walker.borrow_mut().current = node;
    Some(node)
}

/// `TreeWalker.parentNode()`.
pub fn walker_parent_node(
    doc: &DocRef,
    host: &mut dyn DomHost,
    walker: &WalkerRef,
) -> Result<Option<NodeId>, DomError> {
    let (mut node, root) = {
        let w = walker.borrow();
        (Some(w.current), w.root)
    };
    while let Some(n) = node {
        if n == root {
            break;
        }
        node = doc.borrow().parent(n);
        if let Some(p) = node {
            if walker_filter(doc, host, walker, p)? == FILTER_ACCEPT {
                return Ok(set_current(walker, p));
            }
        }
    }
    Ok(None)
}

/// "Traverse children" (first or last).
fn traverse_children(
    doc: &DocRef,
    host: &mut dyn DomHost,
    walker: &WalkerRef,
    first: bool,
) -> Result<Option<NodeId>, DomError> {
    let (current, root) = {
        let w = walker.borrow();
        (w.current, w.root)
    };
    let child_of = |n: NodeId| {
        let d = doc.borrow();
        if first {
            d.first_child(n)
        } else {
            d.last_child(n)
        }
    };
    let sibling_of = |n: NodeId| {
        let d = doc.borrow();
        if first {
            d.next_sibling(n)
        } else {
            d.prev_sibling(n)
        }
    };
    let mut node = child_of(current);
    'outer: while let Some(mut n) = node {
        let result = walker_filter(doc, host, walker, n)?;
        if result == FILTER_ACCEPT {
            return Ok(set_current(walker, n));
        }
        if result == FILTER_SKIP {
            if let Some(child) = child_of(n) {
                node = Some(child);
                continue;
            }
        }
        loop {
            if let Some(sibling) = sibling_of(n) {
                node = Some(sibling);
                continue 'outer;
            }
            let parent = doc.borrow().parent(n);
            match parent {
                None => return Ok(None),
                Some(p) if p == root || p == current => return Ok(None),
                Some(p) => n = p,
            }
        }
    }
    Ok(None)
}

/// `TreeWalker.firstChild()`.
pub fn walker_first_child(
    doc: &DocRef,
    host: &mut dyn DomHost,
    walker: &WalkerRef,
) -> Result<Option<NodeId>, DomError> {
    traverse_children(doc, host, walker, true)
}

/// `TreeWalker.lastChild()`.
pub fn walker_last_child(
    doc: &DocRef,
    host: &mut dyn DomHost,
    walker: &WalkerRef,
) -> Result<Option<NodeId>, DomError> {
    traverse_children(doc, host, walker, false)
}

/// "Traverse siblings" (next or previous).
fn traverse_siblings(
    doc: &DocRef,
    host: &mut dyn DomHost,
    walker: &WalkerRef,
    next: bool,
) -> Result<Option<NodeId>, DomError> {
    let (mut node, root) = {
        let w = walker.borrow();
        (w.current, w.root)
    };
    if node == root {
        return Ok(None);
    }
    let sibling_of = |n: NodeId| {
        let d = doc.borrow();
        if next {
            d.next_sibling(n)
        } else {
            d.prev_sibling(n)
        }
    };
    let child_of = |n: NodeId| {
        let d = doc.borrow();
        if next {
            d.first_child(n)
        } else {
            d.last_child(n)
        }
    };
    loop {
        let mut sibling = sibling_of(node);
        while let Some(s) = sibling {
            node = s;
            let result = walker_filter(doc, host, walker, node)?;
            if result == FILTER_ACCEPT {
                return Ok(set_current(walker, node));
            }
            sibling = child_of(node);
            if result == FILTER_REJECT || sibling.is_none() {
                sibling = sibling_of(node);
            }
        }
        let parent = doc.borrow().parent(node);
        match parent {
            None => return Ok(None),
            Some(p) if p == root => return Ok(None),
            Some(p) => node = p,
        }
        if walker_filter(doc, host, walker, node)? == FILTER_ACCEPT {
            return Ok(None);
        }
    }
}

/// `TreeWalker.nextSibling()`.
pub fn walker_next_sibling(
    doc: &DocRef,
    host: &mut dyn DomHost,
    walker: &WalkerRef,
) -> Result<Option<NodeId>, DomError> {
    traverse_siblings(doc, host, walker, true)
}

/// `TreeWalker.previousSibling()`.
pub fn walker_previous_sibling(
    doc: &DocRef,
    host: &mut dyn DomHost,
    walker: &WalkerRef,
) -> Result<Option<NodeId>, DomError> {
    traverse_siblings(doc, host, walker, false)
}

/// `TreeWalker.previousNode()`.
pub fn walker_previous_node(
    doc: &DocRef,
    host: &mut dyn DomHost,
    walker: &WalkerRef,
) -> Result<Option<NodeId>, DomError> {
    let (mut node, root) = {
        let w = walker.borrow();
        (w.current, w.root)
    };
    while node != root {
        let mut sibling = doc.borrow().prev_sibling(node);
        while let Some(s) = sibling {
            node = s;
            let mut result = walker_filter(doc, host, walker, node)?;
            loop {
                let last = doc.borrow().last_child(node);
                match last {
                    Some(child) if result != FILTER_REJECT => {
                        node = child;
                        result = walker_filter(doc, host, walker, node)?;
                    }
                    _ => break,
                }
            }
            if result == FILTER_ACCEPT {
                return Ok(set_current(walker, node));
            }
            sibling = doc.borrow().prev_sibling(node);
        }
        let parent = doc.borrow().parent(node);
        if node == root {
            return Ok(None);
        }
        let Some(p) = parent else {
            return Ok(None);
        };
        node = p;
        if walker_filter(doc, host, walker, node)? == FILTER_ACCEPT {
            return Ok(set_current(walker, node));
        }
    }
    Ok(None)
}

/// `TreeWalker.nextNode()`.
pub fn walker_next_node(
    doc: &DocRef,
    host: &mut dyn DomHost,
    walker: &WalkerRef,
) -> Result<Option<NodeId>, DomError> {
    let (mut node, root) = {
        let w = walker.borrow();
        (w.current, w.root)
    };
    let mut result = FILTER_ACCEPT;
    loop {
        loop {
            let first = doc.borrow().first_child(node);
            match first {
                Some(child) if result != FILTER_REJECT => {
                    node = child;
                    result = walker_filter(doc, host, walker, node)?;
                    if result == FILTER_ACCEPT {
                        return Ok(set_current(walker, node));
                    }
                }
                _ => break,
            }
        }
        let mut temporary = Some(node);
        let mut found = false;
        while let Some(t) = temporary {
            if t == root {
                return Ok(None);
            }
            let d = doc.borrow();
            if let Some(sibling) = d.next_sibling(t) {
                node = sibling;
                found = true;
                break;
            }
            temporary = d.parent(t);
        }
        if !found {
            // The current node was outside root's subtree.
            return Ok(None);
        }
        result = walker_filter(doc, host, walker, node)?;
        if result == FILTER_ACCEPT {
            return Ok(set_current(walker, node));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dom::host::NullHost;

    #[test]
    fn iterator_reference_moves_on_removal() {
        let mut d = Document::new_html();
        let root = d.create_html_element("div");
        d.append_child(NodeId::DOCUMENT, root);
        let a = d.create_html_element("a");
        let b = d.create_html_element("b");
        let c = d.create_html_element("c");
        d.append_child(root, a);
        d.append_child(a, b);
        d.append_child(root, c);
        let it = d.create_node_iterator(root, filter::SHOW_ELEMENT, None);
        let doc: DocRef = Rc::new(RefCell::new(d));
        let mut host = NullHost::default();
        assert_eq!(iterator_next_node(&doc, &mut host, it), Ok(Some(root)));
        assert_eq!(iterator_next_node(&doc, &mut host, it), Ok(Some(a)));
        assert_eq!(iterator_next_node(&doc, &mut host, it), Ok(Some(b)));
        doc.borrow_mut().remove(a);
        // The reference was inside the removed subtree, after the pointer:
        // it moves back to the node before `a`.
        assert_eq!(doc.borrow().node_iterator(it).reference, root);
        assert_eq!(iterator_next_node(&doc, &mut host, it), Ok(Some(c)));
    }

    #[test]
    fn walker_visits_in_tree_order() {
        let mut d = Document::new_html();
        let root = d.create_html_element("div");
        let a = d.create_html_element("a");
        let t = d.create_text("x");
        let b = d.create_html_element("b");
        d.append_child(root, a);
        d.append_child(a, t);
        d.append_child(root, b);
        let doc: DocRef = Rc::new(RefCell::new(d));
        let mut host = NullHost::default();
        let w = TreeWalker::new(root, filter::SHOW_ELEMENT, None);
        assert_eq!(walker_next_node(&doc, &mut host, &w), Ok(Some(a)));
        assert_eq!(walker_next_node(&doc, &mut host, &w), Ok(Some(b)));
        assert_eq!(walker_next_node(&doc, &mut host, &w), Ok(None));
        assert_eq!(walker_previous_node(&doc, &mut host, &w), Ok(Some(a)));
        assert_eq!(walker_parent_node(&doc, &mut host, &w), Ok(Some(root)));
        assert_eq!(walker_last_child(&doc, &mut host, &w), Ok(Some(b)));
        assert_eq!(walker_previous_sibling(&doc, &mut host, &w), Ok(Some(a)));
    }
}
