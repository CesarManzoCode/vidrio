//! The laid out document seen from outside layout: the viewport (the
//! window size, 800×600 by default; an iframe's document gets the
//! content box of its container), one cached layout per document
//! (recomputed when the nodes, the style sheets, the viewport, the loaded
//! images or the loaded web fonts change), scroll positions of the
//! viewport and of scroll containers, hit testing in painting order, and
//! the geometry CSSOM View exposes (client rects, `offset*`, `client*`,
//! `scroll*`, carets, layout-dependent resolved values). Painting, the
//! window and WebDriver screenshots read the same layout.
//!
//! Scrolling does not change the layout: a fragment keeps its position at
//! scroll position 0, 0 and the readers here subtract the offsets of the
//! scroll containers above it (a fixed-position box ignores them). The
//! tree that is painted is a copy with those offsets applied
//! ([`painted_tree`]).

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use vjs::Agent;

use crate::css::om;
use crate::css::style;
use crate::dom::events::{EventInit, EventInterface};
use crate::dom::{DocRef, Document, NodeData, NodeId, QuirksMode};
use crate::layout::fragment::{Fragment, FragmentKind, FragmentTree, Rect, Stacking};
use crate::paint::{VIEWPORT_HEIGHT, VIEWPORT_WIDTH};

/// What a cached layout depends on besides the document: the style and
/// sheet generations, the viewport, the image store generation and the
/// number of loaded web fonts.
pub type LayoutKey = (u64, u64, (u32, u32), u64, usize);

struct LayoutEntry {
    doc: Weak<RefCell<Document>>,
    key: LayoutKey,
    tree: Rc<FragmentTree>,
}

struct ScrollEntry {
    doc: Weak<RefCell<Document>>,
    /// The scroll container, or [`NodeId::DOCUMENT`] for the viewport.
    node: NodeId,
    offset: (f64, f64),
}

thread_local! {
    static VIEWPORT: Cell<(u32, u32)> = const { Cell::new((VIEWPORT_WIDTH, VIEWPORT_HEIGHT)) };
    static LAYOUTS: RefCell<Vec<LayoutEntry>> = const { RefCell::new(Vec::new()) };
    static SCROLLS: RefCell<Vec<ScrollEntry>> = const { RefCell::new(Vec::new()) };
    static SCROLL_GENERATION: Cell<u64> = const { Cell::new(0) };
    /// The viewport each document had at its last resize steps.
    static SIZES: RefCell<Vec<(Weak<RefCell<Document>>, (u32, u32))>> =
        const { RefCell::new(Vec::new()) };
}

// ----- viewport and layout ----------------------------------------------------------

/// The size of the top-level viewport (the window's).
pub fn viewport() -> (u32, u32) {
    VIEWPORT.with(Cell::get)
}

/// Sets the size of the top-level viewport (the window was resized).
pub fn set_viewport(width: u32, height: u32) {
    VIEWPORT.with(|v| v.set((width.max(1), height.max(1))));
}

/// The viewport of `doc`: the top-level one, or the content box of the
/// iframe that contains it (300×150 while that has no box).
pub fn viewport_of(doc: &DocRef) -> (u32, u32) {
    let container = crate::browser::ua()
        .and_then(|ua| {
            ua.doc_info(doc)
                .and_then(|i| i.context.get())
                .and_then(|c| ua.context(c))
        })
        .and_then(|ctx| ctx.container_element());
    let Some((parent, element)) = container else {
        return viewport();
    };
    let tree = layout(&parent);
    match tree.first_fragment(element) {
        Some(f) => {
            let r = f.content_rect();
            (
                r.width.round().max(0.0) as u32,
                r.height.round().max(0.0) as u32,
            )
        }
        None => (300, 150),
    }
}

fn same(doc: &Weak<RefCell<Document>>, d: &DocRef) -> bool {
    doc.as_ptr() == Rc::as_ptr(d)
}

fn layout_key(doc: &DocRef) -> LayoutKey {
    (
        doc.borrow().style_generation(),
        om::sheets_generation(),
        viewport_of(doc),
        crate::paint::images::generation(),
        crate::bindings::fonts::web_faces(doc).len(),
    )
}

/// What a rendering of `doc` depends on: its layout and its scroll
/// positions. A new value means a new frame.
pub fn render_key(doc: &DocRef) -> (usize, LayoutKey, u64) {
    (
        crate::browser::doc_key(doc),
        layout_key(doc),
        SCROLL_GENERATION.with(Cell::get),
    )
}

/// The layout of `doc` in its viewport, with the system fonts and the
/// document's web fonts and images.
pub fn layout(doc: &DocRef) -> Rc<FragmentTree> {
    let key = layout_key(doc);
    let cached = LAYOUTS.with(|l| {
        l.borrow()
            .iter()
            .find(|e| same(&e.doc, doc) && e.key == key)
            .map(|e| e.tree.clone())
    });
    if let Some(tree) = cached {
        return tree;
    }
    let styles = style::styles(doc);
    let fonts = crate::bindings::fonts::web_faces(doc);
    let images = crate::paint::images::DocumentImages::new(doc);
    let natural = |n: NodeId| images.natural_size(n);
    let (w, h) = key.2;
    let tree = {
        let d = doc.borrow();
        Rc::new(crate::layout::layout(
            &d,
            &styles,
            crate::layout::Options {
                viewport: (f64::from(w), f64::from(h)),
                web_fonts: fonts,
                system_fonts: true,
                images: &natural,
            },
        ))
    };
    LAYOUTS.with(|l| {
        let mut l = l.borrow_mut();
        l.retain(|e| e.doc.strong_count() > 0 && !same(&e.doc, doc));
        l.push(LayoutEntry {
            doc: Rc::downgrade(doc),
            key,
            tree: tree.clone(),
        });
    });
    tree
}

// ----- scrolling ----------------------------------------------------------------------

fn is_fixed(f: &Fragment) -> bool {
    f.style.text("position") == "fixed"
        && !matches!(
            f.kind,
            FragmentKind::Viewport | FragmentKind::Line { .. } | FragmentKind::Text(_)
        )
}

/// The node a scroll position of `f` is kept under.
fn scroll_node(f: &Fragment) -> Option<NodeId> {
    match f.kind {
        FragmentKind::Viewport => Some(NodeId::DOCUMENT),
        _ => f.node.filter(|_| f.pseudo.is_none()),
    }
}

/// The largest scroll offsets of scroll container `f`.
fn max_scroll(f: &Fragment) -> (f64, f64) {
    match &f.scroll {
        Some(s) => (
            (s.scrollable.width - s.scrollport.width).max(0.0),
            (s.scrollable.height - s.scrollport.height).max(0.0),
        ),
        None => (0.0, 0.0),
    }
}

fn stored_scroll(doc: &DocRef, node: NodeId) -> (f64, f64) {
    SCROLLS.with(|s| {
        s.borrow()
            .iter()
            .find(|e| same(&e.doc, doc) && e.node == node)
            .map_or((0.0, 0.0), |e| e.offset)
    })
}

/// The scroll offset of `f` (0, 0 if it is not a scroll container),
/// clamped to its scrollable overflow area.
pub fn scroll_offset(doc: &DocRef, f: &Fragment) -> (f64, f64) {
    if f.scroll.is_none() {
        return (0.0, 0.0);
    }
    let Some(node) = scroll_node(f) else {
        return (0.0, 0.0);
    };
    let (x, y) = stored_scroll(doc, node);
    let (mx, my) = max_scroll(f);
    (x.clamp(0.0, mx), y.clamp(0.0, my))
}

/// The fragment that scrolls for `node` ([`NodeId::DOCUMENT`]: the
/// viewport), if it is a scroll container.
fn scroller<'a>(tree: &'a FragmentTree, node: NodeId) -> Option<&'a Fragment> {
    let f = if node == NodeId::DOCUMENT {
        &tree.root
    } else {
        tree.first_fragment(node)?
    };
    f.scroll.as_ref().map(|_| f)
}

/// Scrolls `node` ([`NodeId::DOCUMENT`]: the viewport) to (`x`, `y`),
/// clamped to what it can scroll; non-finite values count as 0. Returns
/// whether the position changed.
pub fn scroll_to(doc: &DocRef, node: NodeId, x: f64, y: f64) -> bool {
    let tree = layout(doc);
    let Some(f) = scroller(&tree, node) else {
        return false;
    };
    let finite = |v: f64| if v.is_finite() { v } else { 0.0 };
    let (mx, my) = max_scroll(f);
    let offset = (finite(x).clamp(0.0, mx), finite(y).clamp(0.0, my));
    let old = scroll_offset(doc, f);
    SCROLLS.with(|s| {
        let mut s = s.borrow_mut();
        s.retain(|e| e.doc.strong_count() > 0 && !(same(&e.doc, doc) && e.node == node));
        s.push(ScrollEntry {
            doc: Rc::downgrade(doc),
            node,
            offset,
        });
    });
    if old == offset {
        return false;
    }
    SCROLL_GENERATION.with(|g| g.set(g.get().wrapping_add(1)));
    true
}

/// The scroll position of `node` ([`NodeId::DOCUMENT`]: the viewport).
pub fn scroll_position(doc: &DocRef, node: NodeId) -> (f64, f64) {
    let tree = layout(doc);
    scroller(&tree, node).map_or((0.0, 0.0), |f| scroll_offset(doc, f))
}

/// Whether `node` is a scroll container that can still move by
/// (`dx`, `dy`).
pub fn can_scroll(doc: &DocRef, node: NodeId, dx: f64, dy: f64) -> bool {
    let tree = layout(doc);
    let Some(f) = scroller(&tree, node) else {
        return false;
    };
    let (x, y) = scroll_offset(doc, f);
    let (mx, my) = max_scroll(f);
    (dx < 0.0 && x > 0.0) || (dx > 0.0 && x < mx) || (dy < 0.0 && y > 0.0) || (dy > 0.0 && y < my)
}

/// Moves fragment `f` alone (not its children) by (`dx`, `dy`).
fn shift_one(f: &mut Fragment, dx: f64, dy: f64) {
    f.rect.x += dx;
    f.rect.y += dy;
    if let Some(o) = &mut f.object {
        o.x += dx;
        o.y += dy;
    }
    if let Some(sc) = &mut f.scroll {
        sc.scrollport.x += dx;
        sc.scrollport.y += dy;
        sc.scrollable.x += dx;
        sc.scrollable.y += dy;
    }
    if let Some(c) = &mut f.clip {
        if let Some((a, b)) = &mut c.x {
            *a += dx;
            *b += dx;
        }
        if let Some((a, b)) = &mut c.y {
            *a += dy;
            *b += dy;
        }
    }
    match &mut f.kind {
        FragmentKind::Line { baseline } => *baseline += dy,
        FragmentKind::Text(t) => {
            t.baseline += dy;
            for g in &mut t.glyphs {
                g.x += dx;
            }
        }
        _ => {}
    }
}

/// Moves `f` and its descendants, except fixed-position subtrees.
fn shift_scrolled(f: &mut Fragment, dx: f64, dy: f64) {
    if is_fixed(f) {
        return;
    }
    shift_one(f, dx, dy);
    for c in &mut f.children {
        shift_scrolled(c, dx, dy);
    }
}

fn apply_scroll(doc: &DocRef, f: &mut Fragment) {
    let (sx, sy) = scroll_offset(doc, f);
    if let Some(sc) = &mut f.scroll {
        sc.offset = (sx, sy);
    }
    if sx != 0.0 || sy != 0.0 {
        for c in &mut f.children {
            shift_scrolled(c, -sx, -sy);
        }
    }
    for c in &mut f.children {
        apply_scroll(doc, c);
    }
}

/// A copy of `tree` with the scroll positions of `doc` applied: what is
/// painted.
pub fn painted_tree(doc: &DocRef, tree: &FragmentTree) -> FragmentTree {
    let mut t = tree.clone();
    apply_scroll(doc, &mut t.root);
    t
}

// ----- walking the tree in client coordinates -----------------------------------------

/// A fragment with what is above it: the translation from its layout
/// position to client coordinates, the clip of its ancestors in client
/// coordinates (left, top, right, bottom) and the nearest node that
/// generated it or an ancestor.
#[derive(Clone, Copy)]
pub struct Placed<'a> {
    pub f: &'a Fragment,
    pub dx: f64,
    pub dy: f64,
    pub clip: (f64, f64, f64, f64),
    pub owner: Option<NodeId>,
}

impl Placed<'_> {
    /// The border box in client coordinates.
    pub fn rect(&self) -> Rect {
        Rect::new(
            self.f.rect.x + self.dx,
            self.f.rect.y + self.dy,
            self.f.rect.width,
            self.f.rect.height,
        )
    }

    fn clip_contains(&self, x: f64, y: f64) -> bool {
        let (l, t, r, b) = self.clip;
        x >= l && x < r && y >= t && y < b
    }
}

const NO_CLIP: (f64, f64, f64, f64) = (
    f64::NEG_INFINITY,
    f64::NEG_INFINITY,
    f64::INFINITY,
    f64::INFINITY,
);

/// The placement of `parent`'s children.
fn child_frame<'a>(doc: &DocRef, parent: &Placed<'a>, child: &'a Fragment) -> Placed<'a> {
    if is_fixed(child) {
        return Placed {
            f: child,
            dx: 0.0,
            dy: 0.0,
            clip: NO_CLIP,
            owner: child.node.or(parent.owner),
        };
    }
    let p = parent.f;
    let (sx, sy) = scroll_offset(doc, p);
    let mut clip = parent.clip;
    if let Some(c) = &p.clip {
        if let Some((a, b)) = c.x {
            clip.0 = clip.0.max(a + parent.dx);
            clip.2 = clip.2.min(b + parent.dx);
        }
        if let Some((a, b)) = c.y {
            clip.1 = clip.1.max(a + parent.dy);
            clip.3 = clip.3.min(b + parent.dy);
        }
    }
    Placed {
        f: child,
        dx: parent.dx - sx,
        dy: parent.dy - sy,
        clip,
        owner: child.node.or(parent.owner),
    }
}

fn root_placed<'a>(tree: &'a FragmentTree) -> Placed<'a> {
    Placed {
        f: &tree.root,
        dx: 0.0,
        dy: 0.0,
        clip: NO_CLIP,
        owner: None,
    }
}

/// Every fragment of `tree` in tree order, placed.
fn all_placed<'a>(doc: &DocRef, tree: &'a FragmentTree) -> Vec<Placed<'a>> {
    let mut out = Vec::new();
    let mut stack = vec![root_placed(tree)];
    while let Some(p) = stack.pop() {
        for c in p.f.children.iter().rev() {
            stack.push(child_frame(doc, &p, c));
        }
        out.push(p);
    }
    out
}

/// The fragments generated by `node` itself, placed, in tree order.
fn boxes_of<'a>(doc: &DocRef, tree: &'a FragmentTree, node: NodeId) -> Vec<Placed<'a>> {
    all_placed(doc, tree)
        .into_iter()
        .filter(|p| p.f.node == Some(node) && p.f.pseudo.is_none())
        .collect()
}

/// The stacking layer of a fragment within its parent context: negative
/// z-index, `z-index: auto`/0 positioned, or positive z-index.
fn layer(f: &Fragment) -> Option<i32> {
    match f.stacking {
        Stacking::None => None,
        Stacking::Positioned => Some(0),
        Stacking::Context(z) => Some(z),
    }
}

/// Appends the fragments of the stacking context (or positioned box)
/// `ctx` to `out`, bottom to top, in a simplified CSS 2 Appendix E order:
/// the box itself, the layers with negative z-index, the boxes in normal
/// flow in tree order, then the layers with z-index 0 or `auto` and the
/// positive ones.
fn paint_order<'a>(doc: &DocRef, ctx: Placed<'a>, out: &mut Vec<Placed<'a>>) {
    out.push(ctx);
    let mut flow = Vec::new();
    let mut layers: Vec<(i32, Placed<'a>)> = Vec::new();
    let mut stack: Vec<Placed<'a>> = ctx
        .f
        .children
        .iter()
        .rev()
        .map(|c| child_frame(doc, &ctx, c))
        .collect();
    while let Some(p) = stack.pop() {
        if let Some(z) = layer(p.f) {
            layers.push((z, p));
            continue;
        }
        for c in p.f.children.iter().rev() {
            stack.push(child_frame(doc, &p, c));
        }
        flow.push(p);
    }
    // A stable sort keeps tree order within a z-index.
    layers.sort_by_key(|(z, _)| *z);
    for (_, l) in layers.iter().filter(|(z, _)| *z < 0) {
        paint_order(doc, *l, out);
    }
    out.extend(flow);
    for (_, l) in layers.iter().filter(|(z, _)| *z >= 0) {
        paint_order(doc, *l, out);
    }
}

// ----- hit testing --------------------------------------------------------------------

/// The element that `node` stands for in hit testing: the nearest
/// element at or above it.
fn element_at_or_above(d: &Document, mut node: NodeId) -> Option<NodeId> {
    loop {
        if d.is_element(node) {
            return Some(node);
        }
        node = d.parent(node)?;
    }
}

/// `node`, or the shadow host whose tree holds it, repeatedly, until it is
/// in the document tree.
fn retarget(d: &Document, mut node: NodeId) -> NodeId {
    loop {
        let root = d.root(node);
        match d.data(root) {
            NodeData::DocumentFragment { host: Some(h) } if d.shadow_root_data(root).is_some() => {
                node = *h;
            }
            _ => return node,
        }
    }
}

fn hit_candidates<'a>(doc: &DocRef, tree: &'a FragmentTree, x: f64, y: f64) -> Vec<Placed<'a>> {
    let mut order = Vec::new();
    paint_order(doc, root_placed(tree), &mut order);
    order
        .into_iter()
        .rev()
        .filter(|p| {
            !matches!(
                p.f.kind,
                FragmentKind::Viewport | FragmentKind::Line { .. } | FragmentKind::Placeholder
            ) && p.f.is_visible()
                && p.f.style.text("pointer-events") != "none"
        })
        .filter(|p| {
            let r = p.rect();
            x >= r.x && x < r.right() && y >= r.y && y < r.bottom() && p.clip_contains(x, y)
        })
        .collect()
}

fn inside_viewport(doc: &DocRef, x: f64, y: f64) -> bool {
    let (w, h) = viewport_of(doc);
    x >= 0.0 && y >= 0.0 && x <= f64::from(w) && y <= f64::from(h)
}

/// The elements under the client point (`x`, `y`), topmost first,
/// retargeted to the document tree; the root element comes last.
/// Empty outside the viewport (`elementsFromPoint`).
pub fn elements_from_point(doc: &DocRef, x: f64, y: f64) -> Vec<NodeId> {
    if !inside_viewport(doc, x, y) {
        return Vec::new();
    }
    let tree = layout(doc);
    let hits = hit_candidates(doc, &tree, x, y);
    let d = doc.borrow();
    let mut out = Vec::new();
    for p in hits {
        let Some(n) = p.owner.and_then(|n| element_at_or_above(&d, n)) else {
            continue;
        };
        let n = retarget(&d, n);
        if !out.contains(&n) {
            out.push(n);
        }
    }
    if let Some(root) = d.document_element() {
        if !out.contains(&root) {
            out.push(root);
        }
    }
    out
}

/// The topmost element under the client point (`x`, `y`)
/// (`elementFromPoint`).
pub fn element_from_point(doc: &DocRef, x: f64, y: f64) -> Option<NodeId> {
    elements_from_point(doc, x, y).into_iter().next()
}

/// The caret position at the client point (`x`, `y`)
/// (`caretPositionFromPoint`): a text node, a UTF-16 offset into it and
/// the caret's rectangle (zero wide, as tall as the text) when the point
/// is over text, else the element hit, offset 0 and no rectangle. The
/// text of a fragment is found in its element's text children by
/// content, so collapsed white space may shift the offset.
pub fn caret_position_from_point(
    doc: &DocRef,
    x: f64,
    y: f64,
) -> Option<(NodeId, usize, Option<Rect>)> {
    if !inside_viewport(doc, x, y) {
        return None;
    }
    let tree = layout(doc);
    let hits = hit_candidates(doc, &tree, x, y);
    for p in &hits {
        let FragmentKind::Text(t) = &p.f.kind else {
            continue;
        };
        let Some(owner) = p.owner else {
            continue;
        };
        // The character before which the point falls.
        let chars: Vec<char> = t.text.chars().collect();
        let mut index = chars.len();
        if t.glyphs.len() == chars.len() {
            for (i, g) in t.glyphs.iter().enumerate() {
                let next = t.glyphs.get(i + 1).map_or(p.f.rect.right(), |n| n.x);
                if x - p.dx < (g.x + next) / 2.0 {
                    index = i;
                    break;
                }
            }
        } else if p.f.rect.width > 0.0 {
            let ratio = ((x - p.rect().x) / p.f.rect.width).clamp(0.0, 1.0);
            index = (ratio * chars.len() as f64).round() as usize;
        }
        let d = doc.borrow();
        let texts: Vec<NodeId> = d.children(owner).filter(|&c| d.is_text(c)).collect();
        let found = texts.iter().find_map(|&c| {
            let data = d.character_data(c)?;
            data.find(t.text.as_str())
                .map(|at| (c, data[..at].encode_utf16().count()))
        });
        let (node, start) = match found.or_else(|| texts.first().map(|&c| (c, 0))) {
            Some(f) => f,
            None => continue,
        };
        let prefix: String = chars[..index].iter().collect();
        let len = d
            .character_data(node)
            .map_or(0, |s| s.encode_utf16().count());
        let caret_x = if t.glyphs.len() == chars.len() {
            t.glyphs.get(index).map_or(p.f.rect.right(), |g| g.x) + p.dx
        } else {
            p.rect().x + p.f.rect.width * index as f64 / chars.len().max(1) as f64
        };
        let caret = Rect::new(caret_x, p.rect().y, 0.0, p.f.rect.height);
        return Some((
            node,
            (start + prefix.encode_utf16().count()).min(len),
            Some(caret),
        ));
    }
    drop(hits);
    element_from_point(doc, x, y).map(|e| (e, 0, None))
}

// ----- element geometry ---------------------------------------------------------------

/// The border boxes of `node`'s fragments in client coordinates
/// (`getClientRects`).
pub fn client_rects(doc: &DocRef, node: NodeId) -> Vec<Rect> {
    let tree = layout(doc);
    boxes_of(doc, &tree, node)
        .iter()
        .filter(|p| !matches!(p.f.kind, FragmentKind::Line { .. } | FragmentKind::Text(_)))
        .map(Placed::rect)
        .collect()
}

/// The union of [`client_rects`], or a zero rectangle at the origin
/// when there is none (`getBoundingClientRect`).
pub fn bounding_client_rect(doc: &DocRef, node: NodeId) -> Rect {
    let rects = client_rects(doc, node);
    let Some(first) = rects.first() else {
        return Rect::default();
    };
    let (mut x0, mut y0, mut x1, mut y1) = (first.x, first.y, first.right(), first.bottom());
    for r in &rects[1..] {
        x0 = x0.min(r.x);
        y0 = y0.min(r.y);
        x1 = x1.max(r.right());
        y1 = y1.max(r.bottom());
    }
    Rect::new(x0, y0, x1 - x0, y1 - y0)
}

/// Whether `node` is the element whose scrolling is the viewport's: the
/// root element in no-quirks mode, the body in quirks mode.
pub fn is_scrolling_element(d: &Document, node: NodeId) -> bool {
    if d.mode == QuirksMode::Quirks {
        d.body() == Some(node)
    } else {
        d.document_element() == Some(node)
    }
}

/// The scrolling element of the document (`document.scrollingElement`).
pub fn scrolling_element(d: &Document) -> Option<NodeId> {
    if d.mode == QuirksMode::Quirks {
        d.body()
    } else {
        d.document_element()
    }
}

fn is_inline_box(f: &Fragment) -> bool {
    matches!(f.kind, FragmentKind::Inline { .. })
}

/// `clientTop`, `clientLeft`, `clientWidth` and `clientHeight`: the
/// border widths and the padding box size (scroll bars take no space);
/// zeros without a box or for an inline box; the viewport size for the
/// scrolling element.
pub fn client_metrics(doc: &DocRef, node: NodeId) -> (f64, f64, f64, f64) {
    let tree = layout(doc);
    let Some(f) = tree.first_fragment(node) else {
        return (0.0, 0.0, 0.0, 0.0);
    };
    if is_scrolling_element(&doc.borrow(), node) {
        let (w, h) = viewport_of(doc);
        return (f.border.top, f.border.left, f64::from(w), f64::from(h));
    }
    if is_inline_box(f) {
        return (0.0, 0.0, 0.0, 0.0);
    }
    let pad = f.padding_rect();
    (f.border.top, f.border.left, pad.width, pad.height)
}

/// The width and height of the scrolling area of `f`: its scrollable
/// overflow area if it is a scroll container, else its padding box
/// grown to the border boxes of its descendants.
fn scrolling_area(f: &Fragment) -> (f64, f64) {
    if let Some(s) = &f.scroll {
        return (s.scrollable.width, s.scrollable.height);
    }
    let pad = f.padding_rect();
    let (mut right, mut bottom) = (pad.right(), pad.bottom());
    for d in f.descendants().into_iter().skip(1) {
        if matches!(d.kind, FragmentKind::Placeholder) {
            continue;
        }
        right = right.max(d.rect.right());
        bottom = bottom.max(d.rect.bottom());
    }
    (right - pad.x, bottom - pad.y)
}

/// `scrollWidth` and `scrollHeight`.
pub fn scroll_size(doc: &DocRef, node: NodeId) -> (f64, f64) {
    let tree = layout(doc);
    if is_scrolling_element(&doc.borrow(), node) {
        let (w, h) = viewport_of(doc);
        let (aw, ah) = scrolling_area(&tree.root);
        return (aw.max(f64::from(w)), ah.max(f64::from(h)));
    }
    match tree.first_fragment(node) {
        Some(f) if !is_inline_box(f) => scrolling_area(f),
        _ => (0.0, 0.0),
    }
}

/// The node whose scroll position `element.scrollTop` and friends read:
/// the viewport for the scrolling element.
pub fn scroll_target(d: &Document, node: NodeId) -> NodeId {
    if is_scrolling_element(d, node) {
        NodeId::DOCUMENT
    } else {
        node
    }
}

fn is_positioned(f: &Fragment) -> bool {
    !matches!(f.style.text("position").as_str(), "" | "static")
}

/// `offsetParent` (CSSOM View §7): `None` for an element without a box,
/// the root, the body or a fixed-position element; else the nearest
/// ancestor that is positioned, or a `td`, `th` or `table` when the
/// element itself is not positioned, or the body.
pub fn offset_parent(doc: &DocRef, node: NodeId) -> Option<NodeId> {
    let tree = layout(doc);
    let f = tree.first_fragment(node)?;
    let d = doc.borrow();
    if d.document_element() == Some(node) || d.body() == Some(node) || is_fixed(f) {
        return None;
    }
    let positioned = is_positioned(f);
    let mut a = d.parent(node);
    while let Some(n) = a {
        if d.is_element(n) {
            let af = tree.first_fragment(n);
            if af.is_some_and(is_positioned) || d.body() == Some(n) {
                return Some(n);
            }
            if !positioned
                && af.is_some()
                && ["td", "th", "table"]
                    .iter()
                    .any(|t| d.is_html_element(n, t))
            {
                return Some(n);
            }
        }
        a = d.parent(n);
    }
    None
}

/// `offsetTop`, `offsetLeft`, `offsetWidth` and `offsetHeight`: the
/// border box of the first fragment relative to the padding edge of the
/// offset parent (to the initial containing block when there is none or
/// it is a body that is not positioned), and the size of the union of
/// the border boxes. Layout positions: scrolling does not change them.
pub fn offset_metrics(doc: &DocRef, node: NodeId) -> (f64, f64, f64, f64) {
    let tree = layout(doc);
    let Some(f) = tree.first_fragment(node) else {
        return (0.0, 0.0, 0.0, 0.0);
    };
    let bounds = tree.border_box(node).unwrap_or(f.rect);
    if doc.borrow().body() == Some(node) {
        return (f.rect.y, f.rect.x, bounds.width, bounds.height);
    }
    let parent = offset_parent(doc, node);
    let origin = match parent {
        Some(p) => match tree.first_fragment(p) {
            Some(pf) if doc.borrow().body() != Some(p) || is_positioned(pf) => {
                let r = pf.padding_rect();
                (r.y, r.x)
            }
            _ => (0.0, 0.0),
        },
        None => (0.0, 0.0),
    };
    (
        f.rect.y - origin.0,
        f.rect.x - origin.1,
        bounds.width,
        bounds.height,
    )
}

/// How `scrollIntoView` aligns a box in a scrollport.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Start,
    Center,
    End,
    Nearest,
}

/// The scroll position along one axis that shows `[lo, hi)` (client
/// coordinates) in the scrollport `[port_lo, port_hi)` scrolled to
/// `current`.
fn align(a: Align, current: f64, lo: f64, hi: f64, port_lo: f64, port_hi: f64) -> f64 {
    let size = port_hi - port_lo;
    match a {
        Align::Start => current + lo - port_lo,
        Align::End => current + hi - port_hi,
        Align::Center => current + (lo + hi) / 2.0 - (port_lo + port_hi) / 2.0,
        Align::Nearest => {
            if lo < port_lo && hi - lo <= size || hi > port_hi && hi - lo > size {
                current + lo - port_lo
            } else if hi > port_hi && hi - lo <= size || lo < port_lo && hi - lo > size {
                current + hi - port_hi
            } else {
                current
            }
        }
    }
}

/// `scrollIntoView`: every scroll container above `node`, innermost
/// first, then the viewport, scrolls so that its border box is aligned
/// as asked.
pub fn scroll_into_view(doc: &DocRef, node: NodeId, block: Align, inline: Align) {
    if client_rects(doc, node).is_empty() {
        return;
    }
    let ancestors: Vec<NodeId> = {
        let d = doc.borrow();
        let mut v = Vec::new();
        let mut a = d.parent(node);
        while let Some(n) = a {
            if d.is_element(n) && !is_scrolling_element(&d, n) {
                v.push(n);
            }
            a = d.parent(n);
        }
        v.push(NodeId::DOCUMENT);
        v
    };
    for s in ancestors {
        let tree = layout(doc);
        let port = if s == NodeId::DOCUMENT {
            let (w, h) = viewport_of(doc);
            Rect::new(0.0, 0.0, f64::from(w), f64::from(h))
        } else {
            let Some(p) = boxes_of(doc, &tree, s).into_iter().next() else {
                continue;
            };
            let Some(sc) = &p.f.scroll else {
                continue;
            };
            Rect::new(
                sc.scrollport.x + p.dx,
                sc.scrollport.y + p.dy,
                sc.scrollport.width,
                sc.scrollport.height,
            )
        };
        let r = bounding_client_rect(doc, node);
        let (cx, cy) = scroll_position(doc, s);
        let x = align(inline, cx, r.x, r.right(), port.x, port.right());
        let y = align(block, cy, r.y, r.bottom(), port.y, port.bottom());
        scroll_to(doc, s, x, y);
    }
}

fn px(v: f64) -> String {
    format!("{}px", crate::css::values::number(v))
}

/// The resolved value of `property` of `node` where it depends on
/// layout (CSSOM §9): the used `width`, `height`, margins and paddings
/// of an element with a box (sizes of a non-replaced inline box keep
/// their computed value), the used insets of a positioned element and
/// the resolved grid tracks of a grid container. `None` where the
/// computed value is the resolved one.
pub fn resolved_layout_value(doc: &DocRef, node: NodeId, property: &str) -> Option<String> {
    let tree = layout(doc);
    if property.starts_with("grid-template-") {
        return tree.grid_template(node, property).map(str::to_string);
    }
    let f = tree.first_fragment(node)?;
    let side = |s: &crate::layout::fragment::Sides, name: &str| match name {
        "top" => s.top,
        "right" => s.right,
        "bottom" => s.bottom,
        _ => s.left,
    };
    if let Some(name) = property.strip_prefix("margin-") {
        return ["top", "right", "bottom", "left"]
            .contains(&name)
            .then(|| px(side(&f.margin, name)));
    }
    if let Some(name) = property.strip_prefix("padding-") {
        return ["top", "right", "bottom", "left"]
            .contains(&name)
            .then(|| px(side(&f.padding, name)));
    }
    match property {
        "width" | "height" => {
            if is_inline_box(f) {
                return None;
            }
            let border_box = f.style.text("box-sizing") == "border-box";
            let c = f.content_rect();
            let v = if property == "width" {
                c.width
                    + if border_box {
                        f.padding.horizontal() + f.border.horizontal()
                    } else {
                        0.0
                    }
            } else {
                c.height
                    + if border_box {
                        f.padding.vertical() + f.border.vertical()
                    } else {
                        0.0
                    }
            };
            Some(px(v))
        }
        "top" | "right" | "bottom" | "left" => {
            let position = f.style.text("position");
            match position.as_str() {
                "absolute" | "fixed" => {
                    let cb = containing_block(doc, &tree, node, position == "fixed");
                    let m = f.margin_rect();
                    Some(px(match property {
                        "top" => m.y - cb.y,
                        "left" => m.x - cb.x,
                        "right" => cb.right() - m.right(),
                        _ => cb.bottom() - m.bottom(),
                    }))
                }
                "relative" | "sticky" => {
                    let opposite = match property {
                        "top" => "bottom",
                        "bottom" => "top",
                        "left" => "right",
                        _ => "left",
                    };
                    let own = style::resolved_value(&f.style, property);
                    if own != "auto" {
                        return None;
                    }
                    let other = style::resolved_value(&f.style, opposite);
                    if other == "auto" {
                        return Some("0px".to_string());
                    }
                    let v = other.strip_suffix("px")?.parse::<f64>().ok()?;
                    Some(px(-v))
                }
                _ => None,
            }
        }
        _ => None,
    }
}

/// The padding box of the containing block of an absolutely (or fixed)
/// positioned `node`: the nearest positioned ancestor's, or the
/// viewport.
fn containing_block(doc: &DocRef, tree: &FragmentTree, node: NodeId, fixed: bool) -> Rect {
    if !fixed {
        let d = doc.borrow();
        let mut a = d.parent(node);
        while let Some(n) = a {
            if let Some(f) = tree.first_fragment(n).filter(|f| is_positioned(f)) {
                return f.padding_rect();
            }
            a = d.parent(n);
        }
    }
    tree.root.rect
}

// ----- resize -------------------------------------------------------------------------

fn last_size(doc: &DocRef) -> Option<(u32, u32)> {
    SIZES.with(|s| {
        s.borrow()
            .iter()
            .find(|(d, _)| same(d, doc))
            .map(|(_, size)| *size)
    })
}

/// Whether the viewport of `doc` changed since its last resize steps.
pub fn resize_pending(doc: &DocRef) -> bool {
    last_size(doc).is_some_and(|s| s != viewport_of(doc))
}

/// "Run the resize steps" (CSSOM View §13.1): if the viewport of `doc`
/// changed since the last time, a `resize` event fires at its window.
pub fn run_resize_steps(agent: &mut Agent, doc: &DocRef) {
    let now = viewport_of(doc);
    let before = last_size(doc);
    SIZES.with(|s| {
        let mut s = s.borrow_mut();
        s.retain(|(d, _)| d.strong_count() > 0 && !same(d, doc));
        s.push((Rc::downgrade(doc), now));
    });
    if before.is_none_or(|b| b == now) {
        return;
    }
    let Some(ua) = crate::browser::ua() else {
        return;
    };
    let Some(state) = crate::browser::window::state_of_document(agent, &ua, doc) else {
        return;
    };
    crate::browser::navigable::fire_window_event(
        agent,
        &state,
        "resize",
        EventInterface::Event,
        EventInit::default(),
        &[],
        false,
    );
    crate::browser::event_loop::clean_up_after_running_script(agent);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc_of(html: &str) -> DocRef {
        Rc::new(RefCell::new(crate::html::parse_document(html, true)))
    }

    fn by_id(doc: &DocRef, id: &str) -> NodeId {
        let d = doc.borrow();
        d.inclusive_descendants(NodeId::DOCUMENT)
            .into_iter()
            .find(|&n| d.get_attribute(n, "id") == Some(id))
            .expect("an element with that id")
    }

    #[test]
    fn hit_testing_follows_z_index() {
        let doc = doc_of(
            "<!DOCTYPE html><style>body{margin:0}div{position:absolute;width:50px;height:50px}</style>\
             <div id=a style='left:0;top:0;z-index:2'></div>\
             <div id=b style='left:25px;top:25px;z-index:1'></div>",
        );
        let (a, b) = (by_id(&doc, "a"), by_id(&doc, "b"));
        assert_eq!(element_from_point(&doc, 30.0, 30.0), Some(a));
        assert_eq!(element_from_point(&doc, 60.0, 60.0), Some(b));
        let all = elements_from_point(&doc, 30.0, 30.0);
        assert_eq!(&all[..2], &[a, b]);
        assert_eq!(element_from_point(&doc, -1.0, 10.0), None);
    }

    #[test]
    fn scrolling_moves_client_rects_but_not_offsets() {
        let doc = doc_of(
            "<!DOCTYPE html><style>body{margin:0}</style>\
             <div id=s style='width:100px;height:100px;overflow:auto'>\
             <div id=c style='height:300px'></div></div>",
        );
        let (s, c) = (by_id(&doc, "s"), by_id(&doc, "c"));
        assert_eq!(scroll_size(&doc, s), (100.0, 300.0));
        assert!(scroll_to(&doc, s, 0.0, 50.0));
        assert_eq!(scroll_position(&doc, s), (0.0, 50.0));
        assert_eq!(bounding_client_rect(&doc, c).y, -50.0);
        assert_eq!(offset_metrics(&doc, c).0, 0.0);
        // Clamped to the scrollable range.
        scroll_to(&doc, s, 0.0, 1000.0);
        assert_eq!(scroll_position(&doc, s), (0.0, 200.0));
    }
}
