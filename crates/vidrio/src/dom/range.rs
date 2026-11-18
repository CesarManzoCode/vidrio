//! Ranges (DOM §5): boundary points, live ranges (`Range`) and
//! `StaticRange`.
//!
//! Live ranges are kept in the document by [`RangeId`] so that the
//! mutation algorithms can update them; the bindings release a range with
//! [`Document::release_range`] when its wrapper dies.

use std::cmp::Ordering;

use super::tree::utf16_substring;
use super::{Document, DomException, NodeData, NodeId};

/// A boundary point: a node and an offset into it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BoundaryPoint {
    pub node: NodeId,
    pub offset: usize,
}

impl BoundaryPoint {
    pub fn new(node: NodeId, offset: usize) -> BoundaryPoint {
        BoundaryPoint { node, offset }
    }
}

/// The start and end of a live range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LiveRange {
    pub start: BoundaryPoint,
    pub end: BoundaryPoint,
}

impl LiveRange {
    pub fn collapsed(&self) -> bool {
        self.start == self.end
    }
}

/// A live range of a document.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RangeId(pub u32);

/// `compareBoundaryPoints` constants.
pub mod how {
    pub const START_TO_START: u16 = 0;
    pub const START_TO_END: u16 = 1;
    pub const END_TO_END: u16 = 2;
    pub const END_TO_START: u16 = 3;
}

/// A `StaticRange`: not updated by mutations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StaticRange {
    pub start: BoundaryPoint,
    pub end: BoundaryPoint,
}

impl StaticRange {
    /// `new StaticRange(init)`: the containers cannot be doctypes or
    /// attributes; nothing else is checked.
    pub fn new(
        doc: &Document,
        start: BoundaryPoint,
        end: BoundaryPoint,
    ) -> Result<StaticRange, DomException> {
        for n in [start.node, end.node] {
            if doc.is_doctype(n) || doc.is_attr(n) {
                return Err(DomException::InvalidNodeTypeError);
            }
        }
        Ok(StaticRange { start, end })
    }

    pub fn collapsed(&self) -> bool {
        self.start == self.end
    }

    /// True if the range is valid (DOM §5.4): offsets within the nodes'
    /// lengths, one root, start not after end.
    pub fn is_valid(&self, doc: &Document) -> bool {
        self.start.offset <= doc.length(self.start.node)
            && self.end.offset <= doc.length(self.end.node)
            && doc.root(self.start.node) == doc.root(self.end.node)
            && doc.boundary_point_order(self.start, self.end) != Ordering::Greater
    }
}

impl Document {
    // ----- boundary points -------------------------------------------------

    /// The position of boundary point `a` relative to `b` (same root).
    pub fn boundary_point_order(&self, a: BoundaryPoint, b: BoundaryPoint) -> Ordering {
        if a.node == b.node {
            return a.offset.cmp(&b.offset);
        }
        if self.tree_order(a.node, b.node) == Ordering::Greater {
            return self.boundary_point_order(b, a).reverse();
        }
        if self.is_ancestor(a.node, b.node) {
            let mut child = b.node;
            while self.parent(child) != Some(a.node) {
                child = self.parent(child).expect("ancestor chain");
            }
            if self.index(child) < a.offset {
                return Ordering::Greater;
            }
        }
        Ordering::Less
    }

    // ----- the range registry ----------------------------------------------

    /// A new live range from `start` to `end` (not checked).
    pub fn new_range(&mut self, start: BoundaryPoint, end: BoundaryPoint) -> RangeId {
        let range = LiveRange { start, end };
        if let Some(i) = self.ranges.iter().position(Option::is_none) {
            self.ranges[i] = Some(range);
            return RangeId(i as u32);
        }
        self.ranges.push(Some(range));
        RangeId(u32::try_from(self.ranges.len() - 1).expect("too many ranges"))
    }

    /// `new Range()` and `createRange()`: collapsed at (document, 0).
    pub fn create_range(&mut self) -> RangeId {
        let bp = BoundaryPoint::new(NodeId::DOCUMENT, 0);
        self.new_range(bp, bp)
    }

    /// Forgets a live range; its id may be reused.
    pub fn release_range(&mut self, id: RangeId) {
        if let Some(slot) = self.ranges.get_mut(id.0 as usize) {
            *slot = None;
        }
    }

    /// The current boundary points of a live range.
    pub fn range(&self, id: RangeId) -> LiveRange {
        self.ranges[id.0 as usize].expect("released range")
    }

    fn range_mut(&mut self, id: RangeId) -> &mut LiveRange {
        self.ranges[id.0 as usize].as_mut().expect("released range")
    }

    /// The root of a live range: its start node's root.
    pub fn range_root(&self, id: RangeId) -> NodeId {
        self.root(self.range(id).start.node)
    }

    /// `commonAncestorContainer`.
    pub fn common_ancestor_container(&self, id: RangeId) -> NodeId {
        let r = self.range(id);
        let mut container = r.start.node;
        while !self.is_inclusive_ancestor(container, r.end.node) {
            container = self.parent(container).expect("same root");
        }
        container
    }

    /// True if `node` is contained in the range `r`.
    pub fn is_contained(&self, node: NodeId, r: &LiveRange) -> bool {
        self.root(node) == self.root(r.start.node)
            && self.boundary_point_order(BoundaryPoint::new(node, 0), r.start) == Ordering::Greater
            && self.boundary_point_order(BoundaryPoint::new(node, self.length(node)), r.end)
                == Ordering::Less
    }

    /// True if `node` is partially contained in the range `r`.
    pub fn is_partially_contained(&self, node: NodeId, r: &LiveRange) -> bool {
        self.is_inclusive_ancestor(node, r.start.node)
            != self.is_inclusive_ancestor(node, r.end.node)
    }

    // ----- setting boundaries (DOM §5.5) -----------------------------------

    fn set_boundary(
        &mut self,
        id: RangeId,
        node: NodeId,
        offset: usize,
        start: bool,
    ) -> Result<(), DomException> {
        if self.is_doctype(node) {
            return Err(DomException::InvalidNodeTypeError);
        }
        if offset > self.length(node) {
            return Err(DomException::IndexSizeError);
        }
        let bp = BoundaryPoint::new(node, offset);
        let r = self.range(id);
        let other_root = self.root(r.start.node) != self.root(node);
        if start {
            if other_root || self.boundary_point_order(bp, r.end) == Ordering::Greater {
                self.range_mut(id).end = bp;
            }
            self.range_mut(id).start = bp;
        } else {
            if other_root || self.boundary_point_order(bp, r.start) == Ordering::Less {
                self.range_mut(id).start = bp;
            }
            self.range_mut(id).end = bp;
        }
        Ok(())
    }

    /// `setStart(node, offset)`.
    pub fn range_set_start(
        &mut self,
        id: RangeId,
        node: NodeId,
        offset: usize,
    ) -> Result<(), DomException> {
        self.set_boundary(id, node, offset, true)
    }

    /// `setEnd(node, offset)`.
    pub fn range_set_end(
        &mut self,
        id: RangeId,
        node: NodeId,
        offset: usize,
    ) -> Result<(), DomException> {
        self.set_boundary(id, node, offset, false)
    }

    fn parent_and_index(&self, node: NodeId) -> Result<(NodeId, usize), DomException> {
        let parent = self
            .parent(node)
            .ok_or(DomException::InvalidNodeTypeError)?;
        Ok((parent, self.index(node)))
    }

    /// `setStartBefore(node)`.
    pub fn range_set_start_before(
        &mut self,
        id: RangeId,
        node: NodeId,
    ) -> Result<(), DomException> {
        let (parent, index) = self.parent_and_index(node)?;
        self.set_boundary(id, parent, index, true)
    }

    /// `setStartAfter(node)`.
    pub fn range_set_start_after(&mut self, id: RangeId, node: NodeId) -> Result<(), DomException> {
        let (parent, index) = self.parent_and_index(node)?;
        self.set_boundary(id, parent, index + 1, true)
    }

    /// `setEndBefore(node)`.
    pub fn range_set_end_before(&mut self, id: RangeId, node: NodeId) -> Result<(), DomException> {
        let (parent, index) = self.parent_and_index(node)?;
        self.set_boundary(id, parent, index, false)
    }

    /// `setEndAfter(node)`.
    pub fn range_set_end_after(&mut self, id: RangeId, node: NodeId) -> Result<(), DomException> {
        let (parent, index) = self.parent_and_index(node)?;
        self.set_boundary(id, parent, index + 1, false)
    }

    /// `collapse(toStart)`.
    pub fn range_collapse(&mut self, id: RangeId, to_start: bool) {
        let r = self.range_mut(id);
        if to_start {
            r.end = r.start;
        } else {
            r.start = r.end;
        }
    }

    /// "Select" `node` within the range (`selectNode`).
    pub fn range_select_node(&mut self, id: RangeId, node: NodeId) -> Result<(), DomException> {
        let (parent, index) = self.parent_and_index(node)?;
        let r = self.range_mut(id);
        r.start = BoundaryPoint::new(parent, index);
        r.end = BoundaryPoint::new(parent, index + 1);
        Ok(())
    }

    /// `selectNodeContents(node)`.
    pub fn range_select_node_contents(
        &mut self,
        id: RangeId,
        node: NodeId,
    ) -> Result<(), DomException> {
        if self.is_doctype(node) {
            return Err(DomException::InvalidNodeTypeError);
        }
        let length = self.length(node);
        let r = self.range_mut(id);
        r.start = BoundaryPoint::new(node, 0);
        r.end = BoundaryPoint::new(node, length);
        Ok(())
    }

    /// `compareBoundaryPoints(how, sourceRange)`.
    pub fn range_compare_boundary_points(
        &self,
        id: RangeId,
        how_: u16,
        source: RangeId,
    ) -> Result<i16, DomException> {
        if how_ > how::END_TO_START {
            return Err(DomException::NotSupportedError);
        }
        if self.range_root(id) != self.range_root(source) {
            return Err(DomException::WrongDocumentError);
        }
        let this = self.range(id);
        let other = self.range(source);
        let (a, b) = match how_ {
            how::START_TO_START => (this.start, other.start),
            how::START_TO_END => (this.end, other.start),
            how::END_TO_END => (this.end, other.end),
            _ => (this.start, other.end),
        };
        Ok(match self.boundary_point_order(a, b) {
            Ordering::Less => -1,
            Ordering::Equal => 0,
            Ordering::Greater => 1,
        })
    }

    /// `cloneRange()`.
    pub fn clone_range(&mut self, id: RangeId) -> RangeId {
        let r = self.range(id);
        self.new_range(r.start, r.end)
    }

    /// `isPointInRange(node, offset)`.
    pub fn range_is_point_in_range(
        &self,
        id: RangeId,
        node: NodeId,
        offset: usize,
    ) -> Result<bool, DomException> {
        if self.root(node) != self.range_root(id) {
            return Ok(false);
        }
        if self.is_doctype(node) {
            return Err(DomException::InvalidNodeTypeError);
        }
        if offset > self.length(node) {
            return Err(DomException::IndexSizeError);
        }
        let r = self.range(id);
        let bp = BoundaryPoint::new(node, offset);
        Ok(self.boundary_point_order(bp, r.start) != Ordering::Less
            && self.boundary_point_order(bp, r.end) != Ordering::Greater)
    }

    /// `comparePoint(node, offset)`.
    pub fn range_compare_point(
        &self,
        id: RangeId,
        node: NodeId,
        offset: usize,
    ) -> Result<i16, DomException> {
        if self.root(node) != self.range_root(id) {
            return Err(DomException::WrongDocumentError);
        }
        if self.is_doctype(node) {
            return Err(DomException::InvalidNodeTypeError);
        }
        if offset > self.length(node) {
            return Err(DomException::IndexSizeError);
        }
        let r = self.range(id);
        let bp = BoundaryPoint::new(node, offset);
        if self.boundary_point_order(bp, r.start) == Ordering::Less {
            return Ok(-1);
        }
        if self.boundary_point_order(bp, r.end) == Ordering::Greater {
            return Ok(1);
        }
        Ok(0)
    }

    /// `intersectsNode(node)`.
    pub fn range_intersects_node(&self, id: RangeId, node: NodeId) -> bool {
        if self.root(node) != self.range_root(id) {
            return false;
        }
        let Some(parent) = self.parent(node) else {
            return true;
        };
        let offset = self.index(node);
        let r = self.range(id);
        self.boundary_point_order(BoundaryPoint::new(parent, offset), r.end) == Ordering::Less
            && self.boundary_point_order(BoundaryPoint::new(parent, offset + 1), r.start)
                == Ordering::Greater
    }

    /// The range stringifier (`toString()`).
    pub fn range_to_string(&self, id: RangeId) -> String {
        let r = self.range(id);
        let (sn, so, en, eo) = (r.start.node, r.start.offset, r.end.node, r.end.offset);
        let text = |n: NodeId| self.character_data(n).unwrap_or("");
        if sn == en && self.is_text_node(sn) {
            return utf16_substring(text(sn), so, eo.saturating_sub(so));
        }
        let mut s = String::new();
        if self.is_text_node(sn) {
            s.push_str(&utf16_substring(text(sn), so, usize::MAX));
        }
        let common = self.common_ancestor_container(id);
        let mut n = Some(common);
        while let Some(c) = n {
            if self.is_text_node(c) && self.is_contained(c, &r) {
                s.push_str(text(c));
            }
            n = self.following(c, Some(common));
        }
        if self.is_text_node(en) {
            s.push_str(&utf16_substring(text(en), 0, eo));
        }
        s
    }

    // ----- content manipulation (DOM §5.5) ---------------------------------

    /// A shallow clone of `node` for the range algorithms.
    fn range_clone(&mut self, node: NodeId) -> NodeId {
        self.clone_node(node, false)
            .expect("range clones are never documents")
    }

    /// Replaces the data of a fresh character data node, without records.
    fn set_data_raw(&mut self, node: NodeId, data: String) {
        if let Some(d) = self.character_data_mut(node) {
            *d = data;
        }
    }

    fn data_substring(&self, node: NodeId, offset: usize, count: usize) -> String {
        utf16_substring(self.character_data(node).unwrap_or(""), offset, count)
    }

    /// The (node, offset) a range collapses to after its contents go
    /// (steps shared by delete and extract).
    fn collapse_point_after_removal(
        &self,
        start: NodeId,
        start_offset: usize,
        end: NodeId,
    ) -> (NodeId, usize) {
        if self.is_inclusive_ancestor(start, end) {
            return (start, start_offset);
        }
        let mut reference = start;
        while let Some(p) = self.parent(reference) {
            if self.is_inclusive_ancestor(p, end) {
                break;
            }
            reference = p;
        }
        let parent = self.parent(reference).expect("same root");
        (parent, self.index(reference) + 1)
    }

    /// `deleteContents()`.
    pub fn range_delete_contents(&mut self, id: RangeId) -> Result<(), DomException> {
        let r = self.range(id);
        if r.collapsed() {
            return Ok(());
        }
        let (sn, so, en, eo) = (r.start.node, r.start.offset, r.end.node, r.end.offset);
        if sn == en && self.is_character_data(sn) {
            return self.replace_data(sn, so, eo - so, "");
        }
        let common = self.common_ancestor_container(id);
        let mut to_remove = Vec::new();
        let mut n = Some(common);
        while let Some(c) = n {
            if self.is_contained(c, &r) {
                to_remove.push(c);
                n = self.following_skipping_children(c, Some(common));
            } else {
                n = self.following(c, Some(common));
            }
        }
        let (new_node, new_offset) = self.collapse_point_after_removal(sn, so, en);
        if self.is_character_data(sn) {
            let len = self.length(sn);
            self.replace_data(sn, so, len - so, "")?;
        }
        for node in to_remove {
            self.remove(node);
        }
        if self.is_character_data(en) {
            self.replace_data(en, 0, eo, "")?;
        }
        let bp = BoundaryPoint::new(new_node, new_offset);
        let r = self.range_mut(id);
        r.start = bp;
        r.end = bp;
        Ok(())
    }

    /// `extractContents()` ("extract", DOM §5.5).
    pub fn range_extract_contents(&mut self, id: RangeId) -> Result<NodeId, DomException> {
        self.extract_or_clone(id, true)
    }

    /// `cloneContents()` ("clone the contents", DOM §5.5).
    pub fn range_clone_contents(&mut self, id: RangeId) -> Result<NodeId, DomException> {
        self.extract_or_clone(id, false)
    }

    /// Extract (`extract` true) or clone the contents of a range: the two
    /// algorithms share their structure.
    fn extract_or_clone(&mut self, id: RangeId, extract: bool) -> Result<NodeId, DomException> {
        let fragment = self.create_document_fragment();
        let r = self.range(id);
        if r.collapsed() {
            return Ok(fragment);
        }
        let (sn, so, en, eo) = (r.start.node, r.start.offset, r.end.node, r.end.offset);
        if sn == en && self.is_character_data(sn) {
            let clone = self.range_clone(sn);
            let data = self.data_substring(sn, so, eo - so);
            self.set_data_raw(clone, data);
            self.append_child(fragment, clone);
            if extract {
                self.replace_data(sn, so, eo - so, "")?;
            }
            return Ok(fragment);
        }
        let common = self.common_ancestor_container(id);
        let first_partial = if self.is_inclusive_ancestor(sn, en) {
            None
        } else {
            self.children(common)
                .find(|&c| self.is_partially_contained(c, &r))
        };
        let last_partial = if self.is_inclusive_ancestor(en, sn) {
            None
        } else {
            self.child_nodes(common)
                .into_iter()
                .rev()
                .find(|&c| self.is_partially_contained(c, &r))
        };
        let contained: Vec<NodeId> = self
            .children(common)
            .filter(|&c| self.is_contained(c, &r))
            .collect();
        if contained.iter().any(|&c| self.is_doctype(c)) {
            return Err(DomException::HierarchyRequestError);
        }
        let collapse_to = extract.then(|| self.collapse_point_after_removal(sn, so, en));
        if let Some(fp) = first_partial {
            if self.is_character_data(fp) {
                let clone = self.range_clone(sn);
                let len = self.length(sn);
                let data = self.data_substring(sn, so, len - so);
                self.set_data_raw(clone, data);
                self.append_child(fragment, clone);
                if extract {
                    self.replace_data(sn, so, len - so, "")?;
                }
            } else {
                let clone = self.range_clone(fp);
                self.append_child(fragment, clone);
                let len = self.length(fp);
                let sub = self.new_range(BoundaryPoint::new(sn, so), BoundaryPoint::new(fp, len));
                let subfragment = self.extract_or_clone(sub, extract);
                self.release_range(sub);
                let subfragment = subfragment?;
                self.append_child(clone, subfragment);
            }
        }
        for child in contained {
            if extract {
                self.append_child(fragment, child);
            } else {
                let clone = self.clone_node(child, true).expect("not a document");
                self.append_child(fragment, clone);
            }
        }
        if let Some(lp) = last_partial {
            if self.is_character_data(lp) {
                let clone = self.range_clone(en);
                let data = self.data_substring(en, 0, eo);
                self.set_data_raw(clone, data);
                self.append_child(fragment, clone);
                if extract {
                    self.replace_data(en, 0, eo, "")?;
                }
            } else {
                let clone = self.range_clone(lp);
                self.append_child(fragment, clone);
                let sub = self.new_range(BoundaryPoint::new(lp, 0), BoundaryPoint::new(en, eo));
                let subfragment = self.extract_or_clone(sub, extract);
                self.release_range(sub);
                let subfragment = subfragment?;
                self.append_child(clone, subfragment);
            }
        }
        if let Some((node, offset)) = collapse_to {
            let bp = BoundaryPoint::new(node, offset);
            let r = self.range_mut(id);
            r.start = bp;
            r.end = bp;
        }
        Ok(fragment)
    }

    /// `insertNode(node)` ("insert" into a range).
    pub fn range_insert_node(&mut self, id: RangeId, node: NodeId) -> Result<(), DomException> {
        let r = self.range(id);
        let sn = r.start.node;
        let bad_start = matches!(
            self.data(sn),
            NodeData::ProcessingInstruction { .. } | NodeData::Comment(_)
        ) || (self.is_text_node(sn) && self.parent(sn).is_none())
            || sn == node;
        if bad_start {
            return Err(DomException::HierarchyRequestError);
        }
        let mut reference = if self.is_text_node(sn) {
            Some(sn)
        } else {
            self.child_at(sn, r.start.offset)
        };
        let parent = match reference {
            None => sn,
            Some(rn) => self.parent(rn).ok_or(DomException::HierarchyRequestError)?,
        };
        self.ensure_pre_insert_validity(node, parent, reference)?;
        if self.is_text_node(sn) {
            reference = Some(self.split_text(sn, r.start.offset)?);
        }
        if reference == Some(node) {
            reference = self.next_sibling(node);
        }
        if self.parent(node).is_some() {
            self.remove(node);
        }
        let mut new_offset = match reference {
            None => self.length(parent),
            Some(rn) => self.index(rn),
        };
        new_offset += if self.is_document_fragment(node) {
            self.length(node)
        } else {
            1
        };
        self.pre_insert(node, parent, reference)?;
        if self.range(id).collapsed() {
            self.range_mut(id).end = BoundaryPoint::new(parent, new_offset);
        }
        Ok(())
    }

    /// `surroundContents(newParent)`.
    pub fn range_surround_contents(
        &mut self,
        id: RangeId,
        new_parent: NodeId,
    ) -> Result<(), DomException> {
        let r = self.range(id);
        let mut start_chain = vec![r.start.node];
        start_chain.extend(self.ancestors(r.start.node));
        let mut end_chain = vec![r.end.node];
        end_chain.extend(self.ancestors(r.end.node));
        let partially = start_chain
            .iter()
            .filter(|n| !end_chain.contains(n))
            .chain(end_chain.iter().filter(|n| !start_chain.contains(n)));
        for &n in partially {
            if !self.is_text_node(n) {
                return Err(DomException::InvalidStateError);
            }
        }
        if matches!(
            self.data(new_parent),
            NodeData::Document | NodeData::DocumentType(_) | NodeData::DocumentFragment { .. }
        ) {
            return Err(DomException::InvalidNodeTypeError);
        }
        let fragment = self.range_extract_contents(id)?;
        if self.has_child_nodes(new_parent) {
            self.replace_all(None, new_parent);
        }
        self.range_insert_node(id, new_parent)?;
        self.pre_insert(fragment, new_parent, None)?;
        self.range_select_node(id, new_parent)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (Document, NodeId, NodeId, NodeId) {
        let mut doc = Document::new_html();
        let p = doc.create_html_element("p");
        doc.append_child(NodeId::DOCUMENT, p);
        let a = doc.create_text("hello");
        let b = doc.create_text("world");
        doc.append_child(p, a);
        doc.append_child(p, b);
        (doc, p, a, b)
    }

    #[test]
    fn set_start_after_end_collapses() {
        let (mut doc, p, a, _) = setup();
        let r = doc.create_range();
        doc.range_set_start(r, a, 2).expect("start");
        assert_eq!(doc.range(r).end, BoundaryPoint::new(a, 2));
        doc.range_set_end(r, p, 2).expect("end");
        assert_eq!(doc.range_to_string(r), "lloworld");
        assert_eq!(
            doc.range_set_start(r, a, 9),
            Err(DomException::IndexSizeError)
        );
    }

    #[test]
    fn extract_across_text_nodes() {
        let (mut doc, p, a, b) = setup();
        let r = doc.create_range();
        doc.range_set_start(r, a, 3).expect("start");
        doc.range_set_end(r, b, 2).expect("end");
        let frag = doc.range_extract_contents(r).expect("extract");
        assert_eq!(doc.text_content(frag).as_deref(), Some("lowo"));
        assert_eq!(doc.text_content(p).as_deref(), Some("helrld"));
        assert_eq!(doc.range(r).start, BoundaryPoint::new(p, 1));
        assert!(doc.range(r).collapsed());
    }

    #[test]
    fn static_range_rejects_doctypes() {
        let mut doc = Document::new_html();
        let dt = doc.create_document_type("html", "", "");
        let bp = BoundaryPoint::new(dt, 0);
        assert_eq!(
            StaticRange::new(&doc, bp, bp),
            Err(DomException::InvalidNodeTypeError)
        );
    }
}
