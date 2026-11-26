//! Positioned layout (css-position-3; CSS 2 §9.3, §9.4.3, §10.1,
//! §10.3.7, §10.3.8, §10.6.4, §10.6.5), overflow (css-overflow-3) and
//! stacking (CSS 2 §9.9.1): a pass over the fragment tree once normal
//! flow has placed everything in absolute coordinates.
//!
//! 1. Relatively positioned boxes move by their insets and sticky ones
//!    by the offset that keeps them inside their scrollport insets
//!    without leaving their containing block (at scroll position 0, 0).
//! 2. Each out-of-flow placeholder, in tree order, finds its containing
//!    block among its box ancestors (the padding box of the nearest
//!    positioned or transformed ancestor, the first and last fragments
//!    of an inline one, or the initial containing block), solves the
//!    horizontal and vertical constraint equations with its static
//!    position and is laid out in its place; the boxes inside it go
//!    through 1 and 2 in turn.
//! 3. Every box gets its overflow clip, its scroll container (with its
//!    scrollable overflow area; scroll bars take no space) and its
//!    participation in stacking.

use std::rc::Rc;

use crate::css::cascade::Pseudo;
use crate::css::style::ComputedStyle;
use crate::dom::NodeId;

use super::Ctx;
use super::block::{self, Cb, Model};
use super::boxtree::{BoxKind, BoxNode, Contents, InlineItem};
use super::fragment::{Clip, Fragment, FragmentKind, Overflow, Rect, Scroll, Sides, Stacking};
use super::values::{self, Display, Inner, Position, Size};

const EPS: f64 = 1e-7;

/// What the pass needs to know about the viewport.
pub(crate) struct Viewport {
    /// The used `overflow` of the viewport (propagated from the root or
    /// the body, css-overflow-3 §3.3).
    pub overflow: (Overflow, Overflow),
    /// The elements whose `overflow` went to the viewport: they behave
    /// as `visible` themselves.
    pub propagated: Vec<NodeId>,
}

/// A laid out out-of-flow box and the box ancestors from its parent up
/// to its containing block (all of them for the initial containing
/// block).
struct Placed {
    node: NodeId,
    pseudo: Option<Pseudo>,
    initial: bool,
    inside: Vec<NodeId>,
}

/// What the boxes inside a box see while offsets are applied.
#[derive(Clone, Copy)]
struct Frame {
    /// The content box of the nearest block container.
    cb: Rect,
    cb_definite_height: bool,
    cb_rtl: bool,
    /// The scrollport of the nearest scroll container.
    scrollport: Rect,
}

/// Runs positioned layout, overflow and stacking over `root` (the
/// viewport fragment of a tree in absolute coordinates).
pub(crate) fn place(ctx: &mut Ctx, boxes: &BoxNode, view: &Viewport, root: &mut Fragment) {
    let icb = root.rect;
    let root_rtl = root.children.first().is_some_and(|r| r.style.rtl());
    let initial = Frame {
        cb: icb,
        cb_definite_height: true,
        cb_rtl: root_rtl,
        scrollport: icb,
    };
    offsets(root, initial, view);
    let mut placed = Vec::new();
    while let Some(path) = first_placeholder(root, &mut Vec::new()) {
        let ph = at(root, &path);
        let (node, pseudo, static_rect, grid_area) = (ph.node, ph.pseudo, ph.rect, ph.grid_area);
        let mut chain = Vec::new();
        let found = node.and_then(|n| find_box(boxes, n, pseudo, &mut chain));
        let Some(b) = found else {
            remove(root, &path);
            continue;
        };
        let fixed = values::position(&b.style) == Position::Fixed;
        let (mut cb, cb_rtl, inside, is_initial) =
            containing_block(root, &chain, fixed, icb, root_rtl);
        // A grid container that is the containing block of its child
        // gives it its grid area instead (css-grid-2 §9.1).
        if let Some(a) = grid_area {
            if !is_initial && inside.len() == 1 {
                cb = Rect::new(static_rect.x + a.x, static_rect.y + a.y, a.width, a.height);
            }
        }
        let mut frag = layout_absolute(ctx, b, cb, cb_rtl, static_rect);
        let frame = frame_at(root, &path, initial, view);
        offsets(&mut frag, frame, view);
        *at_mut(root, &path) = frag;
        if let Some(n) = node {
            placed.push(Placed {
                node: n,
                pseudo,
                initial: is_initial,
                inside,
            });
        }
    }
    finish(root, None, false, view, &placed);
}

// ----- the box tree ----------------------------------------------------------------------

/// The out-of-flow box of `node` and `pseudo` under `b`; `chain` gets
/// the elements of its ancestor boxes (block containers and inline
/// boxes), outermost first.
fn find_box<'a>(
    b: &'a BoxNode,
    node: NodeId,
    pseudo: Option<Pseudo>,
    chain: &mut Vec<(NodeId, Rc<ComputedStyle>)>,
) -> Option<&'a BoxNode> {
    let target =
        |x: &BoxNode| x.node == Some(node) && x.pseudo == pseudo && values::out_of_flow(&x.style);
    let base = chain.len();
    if let (Some(n), None) = (b.node, b.pseudo) {
        chain.push((n, b.style.clone()));
    }
    if let BoxKind::Container(c) = &b.kind {
        match &c.contents {
            Contents::Blocks(children) => {
                for ch in children {
                    if target(ch) {
                        return Some(ch);
                    }
                    if let Some(f) = find_box(ch, node, pseudo, chain) {
                        return Some(f);
                    }
                }
            }
            Contents::Inline(ic) => {
                let inner = chain.len();
                let mut opened = Vec::new();
                for item in &ic.items {
                    match item {
                        InlineItem::Open(i) => {
                            let ib = &ic.boxes[*i];
                            let element = ib.pseudo.is_none() && ib.node.is_some();
                            if let (Some(n), true) = (ib.node, element) {
                                chain.push((n, ib.style.clone()));
                            }
                            opened.push(element);
                        }
                        InlineItem::Close(_) => {
                            if opened.pop() == Some(true) {
                                chain.pop();
                            }
                        }
                        InlineItem::Atomic(x) | InlineItem::Float(x) | InlineItem::OutOfFlow(x) => {
                            if target(x) {
                                return Some(x);
                            }
                            if let Some(f) = find_box(x, node, pseudo, chain) {
                                return Some(f);
                            }
                        }
                        _ => {}
                    }
                }
                chain.truncate(inner);
            }
        }
    }
    chain.truncate(base);
    None
}

// ----- the fragment tree -----------------------------------------------------------------

fn first_placeholder(f: &Fragment, path: &mut Vec<usize>) -> Option<Vec<usize>> {
    for (i, c) in f.children.iter().enumerate() {
        path.push(i);
        if matches!(c.kind, FragmentKind::Placeholder) {
            return Some(path.clone());
        }
        if let Some(p) = first_placeholder(c, path) {
            return Some(p);
        }
        path.pop();
    }
    None
}

fn at<'a>(root: &'a Fragment, path: &[usize]) -> &'a Fragment {
    path.iter().fold(root, |f, &i| &f.children[i])
}

fn at_mut<'a>(root: &'a mut Fragment, path: &[usize]) -> &'a mut Fragment {
    path.iter().fold(root, |f, &i| &mut f.children[i])
}

fn remove(root: &mut Fragment, path: &[usize]) {
    if let Some((last, parents)) = path.split_last() {
        at_mut(root, parents).children.remove(*last);
    }
}

/// A fragment of a box (not a line, a text run or a placeholder).
fn is_box(f: &Fragment) -> bool {
    matches!(
        f.kind,
        FragmentKind::Block {
            anonymous: false,
            ..
        } | FragmentKind::Float
            | FragmentKind::InlineBlock
            | FragmentKind::Replaced(_)
            | FragmentKind::Inline { .. }
            | FragmentKind::OutOfFlow
    )
}

/// A fragment of a block container (or the viewport).
fn is_block_container(f: &Fragment) -> bool {
    matches!(
        f.kind,
        FragmentKind::Viewport
            | FragmentKind::Block { .. }
            | FragmentKind::Float
            | FragmentKind::InlineBlock
            | FragmentKind::OutOfFlow
    )
}

/// The used `overflow` of a box fragment (`visible` where it does not
/// apply or went to the viewport).
fn used_overflow(f: &Fragment, view: &Viewport) -> (Overflow, Overflow) {
    let applies = matches!(
        f.kind,
        FragmentKind::Block { .. }
            | FragmentKind::Float
            | FragmentKind::InlineBlock
            | FragmentKind::OutOfFlow
    );
    let propagated = f.pseudo.is_none() && f.node.is_some_and(|n| view.propagated.contains(&n));
    if !applies || propagated {
        return (Overflow::Visible, Overflow::Visible);
    }
    values::overflow(&f.style)
}

fn is_scroll_container(f: &Fragment, view: &Viewport) -> bool {
    if matches!(f.kind, FragmentKind::Viewport) {
        return true;
    }
    let (x, y) = used_overflow(f, view);
    x.scrolls() || y.scrolls()
}

/// The height of the box is definite for the percentages inside it.
fn definite_height(f: &Fragment) -> bool {
    match f.kind {
        FragmentKind::Viewport | FragmentKind::OutOfFlow => true,
        _ => matches!(values::size(&f.style, "height"), Size::Lp(l) if !l.has_percent()),
    }
}

/// The frame inside `f` given the frame `f` is in.
fn inner_frame(f: &Fragment, frame: Frame, view: &Viewport) -> Frame {
    let mut inner = frame;
    if is_block_container(f) {
        inner.cb = f.content_rect();
        inner.cb_definite_height = definite_height(f);
        inner.cb_rtl = f.style.rtl();
    }
    if is_scroll_container(f, view) {
        inner.scrollport = f.padding_rect();
    }
    inner
}

/// The frame the fragment at `path` is in.
fn frame_at(root: &Fragment, path: &[usize], initial: Frame, view: &Viewport) -> Frame {
    let mut frame = initial;
    let mut f = root;
    for &i in path {
        if !matches!(f.kind, FragmentKind::Viewport) {
            frame = inner_frame(f, frame, view);
        }
        f = &f.children[i];
    }
    frame
}

// ----- relative and sticky positioning ---------------------------------------------------

/// Moves the relatively and sticky positioned boxes of `f`'s subtree.
fn offsets(f: &mut Fragment, frame: Frame, view: &Viewport) {
    if is_box(f) {
        let (dx, dy) = match values::position(&f.style) {
            Position::Relative => relative_offset(&f.style, frame),
            Position::Sticky => sticky_offset(f, frame),
            _ => (0.0, 0.0),
        };
        if dx != 0.0 || dy != 0.0 {
            f.shift(dx, dy);
        }
    }
    let inner = if matches!(f.kind, FragmentKind::Viewport) {
        frame
    } else {
        inner_frame(f, frame, view)
    };
    for c in &mut f.children {
        offsets(c, inner, view);
    }
}

/// The offset of a relatively positioned box (CSS 2 §9.4.3): `left`
/// wins over `right` when the containing block is `ltr`, `top` over
/// `bottom` always; percentages of an indefinite height are `auto`.
fn relative_offset(s: &ComputedStyle, frame: Frame) -> (f64, f64) {
    let ins = values::insets(s);
    let w = frame.cb.width;
    let h = frame.cb.height;
    let vertical = |l: &values::Lp| {
        if l.has_percent() && !frame.cb_definite_height {
            None
        } else {
            l.resolve_opt(Some(h))
        }
    };
    let left = ins.left.map(|l| l.resolve(w));
    let right = ins.right.map(|l| l.resolve(w));
    let top = ins.top.as_ref().and_then(vertical);
    let bottom = ins.bottom.as_ref().and_then(vertical);
    let dx = match (left, right) {
        (Some(l), Some(r)) => {
            if frame.cb_rtl {
                -r
            } else {
                l
            }
        }
        (Some(l), None) => l,
        (None, Some(r)) => -r,
        (None, None) => 0.0,
    };
    let dy = match (top, bottom) {
        (Some(t), _) => t,
        (None, Some(b)) => -b,
        (None, None) => 0.0,
    };
    (dx, dy)
}

/// The offset of a sticky positioned box (css-position-3 §3.4) at
/// scroll position 0, 0: far enough to respect its insets from the
/// scrollport, never so far that its margin box leaves its containing
/// block. When opposite insets conflict, `top` and `left` win.
fn sticky_offset(f: &Fragment, frame: Frame) -> (f64, f64) {
    let ins = values::insets(&f.style);
    let sp = frame.scrollport;
    let cb = frame.cb;
    let r = f.rect;
    let mr = f.margin_rect();
    let axis = |start: Option<f64>,
                end: Option<f64>,
                (r0, r1): (f64, f64),
                (m0, m1): (f64, f64),
                (s0, s1): (f64, f64),
                (c0, c1): (f64, f64)| {
        if let Some(st) = start {
            let limit = s0 + st;
            if r0 < limit {
                return (limit - r0).min(c1 - m1).max(0.0);
            }
        }
        if let Some(en) = end {
            let limit = s1 - en;
            if r1 > limit {
                return -(r1 - limit).min(m0 - c0).max(0.0);
            }
        }
        0.0
    };
    let dx = axis(
        ins.left.map(|l| l.resolve(sp.width)),
        ins.right.map(|l| l.resolve(sp.width)),
        (r.x, r.right()),
        (mr.x, mr.right()),
        (sp.x, sp.right()),
        (cb.x, cb.right()),
    );
    let dy = axis(
        ins.top.map(|l| l.resolve(sp.height)),
        ins.bottom.map(|l| l.resolve(sp.height)),
        (r.y, r.bottom()),
        (mr.y, mr.bottom()),
        (sp.y, sp.bottom()),
        (cb.y, cb.bottom()),
    );
    (dx, dy)
}

// ----- absolute and fixed positioning ----------------------------------------------------

/// The containing block of an out-of-flow box whose box ancestors are
/// `chain` (CSS 2 §10.1): its rectangle, whether it is `rtl`, the
/// ancestors up to it and whether it is the initial containing block.
fn containing_block(
    root: &Fragment,
    chain: &[(NodeId, Rc<ComputedStyle>)],
    fixed: bool,
    icb: Rect,
    root_rtl: bool,
) -> (Rect, bool, Vec<NodeId>, bool) {
    let mut inside = Vec::new();
    for (n, style) in chain.iter().rev() {
        inside.push(*n);
        let qualifies = if fixed {
            values::contains_fixed(style)
        } else {
            values::contains_absolute(style)
        };
        if !qualifies {
            continue;
        }
        let frags: Vec<&Fragment> = root
            .fragments_of(*n)
            .into_iter()
            .filter(|f| is_box(f))
            .collect();
        let (Some(first), Some(last)) = (frags.first(), frags.last()) else {
            continue;
        };
        let rtl = style.rtl();
        let rect = if matches!(first.kind, FragmentKind::Inline { .. }) {
            // The padding edges of the first and last inline fragments
            // (CSS 2 §10.1, item 4).
            let (a, z) = (first.padding_rect(), last.padding_rect());
            let (x0, x1) = if rtl {
                (z.x, a.right())
            } else {
                (a.x, z.right())
            };
            Rect::new(x0, a.y, (x1 - x0).max(0.0), (z.bottom() - a.y).max(0.0))
        } else {
            first.padding_rect()
        };
        return (rect, rtl, inside, false);
    }
    (icb, root_rtl, inside, true)
}

/// One axis of the constraint equation of an out-of-flow box.
struct Axis {
    /// The size of the containing block.
    total: f64,
    start: Option<f64>,
    end: Option<f64>,
    margin_start: Option<f64>,
    margin_end: Option<f64>,
    /// Borders and paddings.
    bp: f64,
    /// The distances from the containing block's edges to the static
    /// position.
    static_start: f64,
    static_end: f64,
    /// The horizontal axis of an `rtl` containing block: the static
    /// position and the ignored value are at the other end.
    reverse: bool,
    /// The horizontal axis: equal auto margins never go negative.
    horizontal: bool,
}

/// Solves one axis (CSS 2 §10.3.7 and §10.6.4; with `size` given, also
/// §10.3.8 and §10.6.5): the used start inset, content size and margins.
/// `fit` gives the content size for an available space when the size is
/// `auto` (shrink-to-fit widths, content heights).
fn solve(a: &Axis, size: Option<f64>, fit: &mut dyn FnMut(f64) -> f64) -> (f64, f64, f64, f64) {
    let t = a.total;
    if let (Some(s), Some(w), Some(e)) = (a.start, size, a.end) {
        let rest = t - s - e - w - a.bp;
        return match (a.margin_start, a.margin_end) {
            (None, None) => {
                let half = rest / 2.0;
                if half < 0.0 && a.horizontal {
                    if a.reverse {
                        (s, w, rest, 0.0)
                    } else {
                        (s, w, 0.0, rest)
                    }
                } else {
                    (s, w, half, half)
                }
            }
            (None, Some(me)) => (s, w, rest - me, me),
            (Some(ms), None) => (s, w, ms, rest - ms),
            // Over-constrained: the end inset is ignored (the start one
            // in an `rtl` containing block).
            (Some(ms), Some(me)) => {
                if a.reverse {
                    (t - e - me - w - a.bp - ms, w, ms, me)
                } else {
                    (s, w, ms, me)
                }
            }
        };
    }
    let ms = a.margin_start.unwrap_or(0.0);
    let me = a.margin_end.unwrap_or(0.0);
    let room = |s: f64, e: f64| t - s - e - ms - me - a.bp;
    let from_end = |e: f64, w: f64| t - e - me - w - a.bp - ms;
    let (start, w) = match (a.start, size, a.end) {
        (None, None, None) => {
            if a.reverse {
                let w = fit(room(0.0, a.static_end));
                (from_end(a.static_end, w), w)
            } else {
                (a.static_start, fit(room(a.static_start, 0.0)))
            }
        }
        (None, None, Some(e)) => {
            let w = fit(room(0.0, e));
            (from_end(e, w), w)
        }
        (None, Some(w), None) => {
            if a.reverse {
                (from_end(a.static_end, w), w)
            } else {
                (a.static_start, w)
            }
        }
        (Some(s), None, None) => (s, fit(room(s, 0.0))),
        (None, Some(w), Some(e)) => (from_end(e, w), w),
        (Some(s), None, Some(e)) => (s, room(s, e).max(0.0)),
        (Some(s), Some(w), None) => (s, w),
        (Some(_), Some(_), Some(_)) => unreachable!(),
    };
    (start, w, ms, me)
}

/// Lays out out-of-flow box `b` in containing block `cb` (a padding box,
/// `rtl` when its direction is) with its placeholder `st`; the fragment
/// comes in absolute coordinates.
fn layout_absolute(ctx: &mut Ctx, b: &BoxNode, cb: Rect, rtl: bool, st: Rect) -> Fragment {
    let s = &b.style;
    let m = Model::of(s, cb.width);
    let bp = m.bp();
    let ins = values::insets(s);
    let ccb = Cb {
        width: cb.width,
        height: Some(cb.height),
    };
    let h_axis = Axis {
        total: cb.width,
        start: ins.left.map(|l| l.resolve(cb.width)),
        end: ins.right.map(|l| l.resolve(cb.width)),
        margin_start: m.margin.left,
        margin_end: m.margin.right,
        bp: bp.horizontal(),
        static_start: st.x - cb.x,
        static_end: cb.right() - st.right(),
        reverse: rtl,
        horizontal: true,
    };
    let v_axis = Axis {
        total: cb.height,
        start: ins.top.map(|l| l.resolve(cb.height)),
        end: ins.bottom.map(|l| l.resolve(cb.height)),
        margin_start: m.margin.top,
        margin_end: m.margin.bottom,
        bp: bp.vertical(),
        static_start: st.y - cb.y,
        static_end: cb.bottom() - st.y,
        reverse: false,
        horizontal: false,
    };
    // The height the insets give a box with `height: auto`.
    let inset_height = match (v_axis.start, v_axis.end) {
        (Some(t), Some(bt)) if values::size(s, "height").is_auto() => Some(
            (cb.height
                - t
                - bt
                - m.margin.top.unwrap_or(0.0)
                - m.margin.bottom.unwrap_or(0.0)
                - bp.vertical())
            .max(0.0),
        ),
        _ => None,
    };
    let mut frag;
    match &b.kind {
        BoxKind::Replaced(r) => {
            let (w, h) = block::replaced_size(ctx, b, r, Some(cb.width), Some(cb.height));
            let (left, w, ml, mr) = solve(&h_axis, Some(w), &mut |_| w);
            let (top, h, mt, mb) = solve(&v_axis, Some(h), &mut |_| h);
            frag = Fragment::new(FragmentKind::Replaced(r.kind), s.clone());
            frag.margin = Sides::new(mt, mr, mb, ml);
            frag.rect = Rect::new(
                cb.x + left + ml,
                cb.y + top + mt,
                w + bp.horizontal(),
                h + bp.vertical(),
            );
            frag.border = m.border;
            frag.padding = m.padding;
            block::set_object(&mut frag, b, r);
        }
        BoxKind::Container(c) => {
            let bp_h = bp.horizontal();
            let stretch = cb.width
                - h_axis.start.unwrap_or(0.0)
                - h_axis.end.unwrap_or(0.0)
                - m.margin.left.unwrap_or(0.0)
                - m.margin.right.unwrap_or(0.0)
                - bp_h;
            let mut spec = block::specified_width(ctx, b, Some(cb.width), bp_h, stretch);
            if spec.is_none() {
                let h = block::specified_height(s, ccb, bp.vertical()).or(inset_height);
                spec = h.and_then(|h| {
                    let h = block::clamp_height(s, h, Some(cb.height), bp.vertical(), h);
                    block::ratio_width(s, h, &bp)
                });
            }
            if spec.is_none() {
                if let Some(chars) = c.control.and_then(|cs| cs.chars) {
                    spec = Some(chars * ctx.fonts.metrics(s).zero);
                }
            }
            let (mut left, mut w, mut ml, mut mr) = {
                let mut fit = |avail: f64| block::shrink_to_fit(ctx, b, avail);
                solve(&h_axis, spec, &mut fit)
            };
            let clamped = block::clamp_width(ctx, b, w, Some(cb.width), bp_h);
            if (clamped - w).abs() > EPS {
                (left, w, ml, mr) = solve(&h_axis, Some(clamped), &mut |_| clamped);
            }
            let explicit_h = block::specified_height(s, ccb, bp.vertical());
            let ratio_h = explicit_h
                .is_none()
                .then(|| block::ratio_height(s, w, &bp))
                .flatten();
            let spec_h = explicit_h.or(ratio_h);
            let inner = block::layout_independent(ctx, b, c, w, spec_h.or(inset_height));
            let content = inner.height;
            let spec_h = spec_h.map(|h| block::ratio_floor(s, h, ratio_h.is_some(), content));
            let (mut top, mut h, mut mt, mut mb) = solve(&v_axis, spec_h, &mut |_| content);
            let clamped = block::clamp_height(s, h, Some(cb.height), bp.vertical(), content);
            if (clamped - h).abs() > EPS {
                (top, h, mt, mb) = solve(&v_axis, Some(clamped), &mut |_| clamped);
            }
            frag = Fragment::new(FragmentKind::OutOfFlow, s.clone());
            frag.margin = Sides::new(mt, mr, mb, ml);
            frag.border = m.border;
            frag.padding = m.padding;
            frag.rect = Rect::new(
                cb.x + left + ml,
                cb.y + top + mt,
                w + bp_h,
                h + bp.vertical(),
            );
            frag.children = inner.children;
            for ch in &mut frag.children {
                ch.translate(bp.left, bp.top);
            }
            if let Some(marker) = &c.marker {
                block::place_marker(ctx, marker, &mut frag, ccb);
            }
        }
    }
    frag.node = b.node;
    frag.pseudo = b.pseudo;
    // The children are relative to the border box, which is absolute.
    frag.absolutize(0.0, 0.0);
    frag
}

// ----- overflow and stacking -------------------------------------------------------------

/// Sets the clip, scroll container and stacking of every fragment under
/// `f` (children first: a scroll container's area needs theirs).
fn finish(
    f: &mut Fragment,
    parent: Option<&Rc<ComputedStyle>>,
    is_root: bool,
    view: &Viewport,
    placed: &[Placed],
) {
    let style = f.style.clone();
    let viewport = matches!(f.kind, FragmentKind::Viewport);
    for c in &mut f.children {
        finish(c, Some(&style), viewport, view, placed);
    }
    f.stacking = stacking(f, parent, is_root);
    let (ox, oy) = if viewport {
        view.overflow
    } else {
        used_overflow(f, view)
    };
    if ox != Overflow::Visible || oy != Overflow::Visible {
        f.clip = Some(clip(f, ox, oy));
    }
    if ox.scrolls() || oy.scrolls() {
        let scrollport = f.padding_rect();
        f.scroll = Some(Box::new(Scroll {
            overflow_x: ox,
            overflow_y: oy,
            scrollport,
            scrollable: scrollable_area(f, placed),
            offset: (0.0, 0.0),
        }));
    }
}

/// How box fragment `f` (whose parent box has style `parent`) takes part
/// in stacking.
fn stacking(f: &Fragment, parent: Option<&Rc<ComputedStyle>>, is_root: bool) -> Stacking {
    if is_root {
        return Stacking::Context(0);
    }
    if !is_box(f) {
        return Stacking::None;
    }
    let s = &f.style;
    let z = values::z_index(s);
    let positioned = values::position(s) != Position::Static;
    let item = parent.is_some_and(|p| {
        matches!(
            values::display(p),
            Display::Box {
                inner: Inner::Flex | Inner::Grid,
                ..
            }
        )
    });
    match z {
        Some(z) if positioned || item => Stacking::Context(z),
        _ if values::forces_stacking_context(s) => Stacking::Context(0),
        _ if positioned => Stacking::Positioned,
        _ => Stacking::None,
    }
}

/// The overflow clip of `f`: the padding box in the axes that scroll or
/// are `hidden`, the box `overflow-clip-margin` gives in those that are
/// `clip`.
fn clip(f: &Fragment, ox: Overflow, oy: Overflow) -> Clip {
    let pad = f.padding_rect();
    let (reference, margin) = values::overflow_clip_margin(&f.style);
    let clip_box = match reference {
        0 => f.content_rect(),
        2 => f.rect,
        _ => pad,
    };
    let axis = |o: Overflow, (p0, p1): (f64, f64), (c0, c1): (f64, f64)| match o {
        Overflow::Visible => None,
        Overflow::Clip => Some((c0 - margin, c1 + margin)),
        _ => Some((p0, p1)),
    };
    Clip {
        x: axis(ox, (pad.x, pad.right()), (clip_box.x, clip_box.right())),
        y: axis(oy, (pad.y, pad.bottom()), (clip_box.y, clip_box.bottom())),
    }
}

/// The scrollable overflow area of scroll container `f` (css-overflow-3
/// §2.2): its padding box, the border boxes and text of what it contains
/// (an out-of-flow box only if its containing block is inside it; a
/// clipping box without what it clips), and the end padding after its
/// in-flow content; nothing on the unreachable side.
fn scrollable_area(f: &Fragment, placed: &[Placed]) -> Rect {
    let pad = f.padding_rect();
    let (mut x0, mut y0, mut x1, mut y1) = (pad.x, pad.y, pad.right(), pad.bottom());
    let mut add = |r: Rect| {
        x0 = x0.min(r.x);
        y0 = y0.min(r.y);
        x1 = x1.max(r.right());
        y1 = y1.max(r.bottom());
    };
    let scroller = if matches!(f.kind, FragmentKind::Viewport) {
        None
    } else {
        f.node
    };
    for c in &f.children {
        contribute(c, scroller, placed, &mut add);
    }
    // The end padding after the in-flow content.
    for c in &f.children {
        if matches!(
            c.kind,
            FragmentKind::Block { .. }
                | FragmentKind::Line { .. }
                | FragmentKind::Float
                | FragmentKind::Replaced(_)
        ) && !values::out_of_flow(&c.style)
        {
            let mr = c.margin_rect();
            add(Rect::new(
                mr.x - f.padding.left,
                mr.y,
                mr.width + f.padding.horizontal(),
                mr.height + f.padding.bottom,
            ));
        }
    }
    // The scroll origin is the top left corner (top right in `rtl`).
    if f.style.rtl() {
        x1 = pad.right();
    } else {
        x0 = pad.x;
    }
    y0 = pad.y;
    Rect::new(x0, y0, (x1 - x0).max(0.0), (y1 - y0).max(0.0))
}

fn contribute(
    c: &Fragment,
    scroller: Option<NodeId>,
    placed: &[Placed],
    add: &mut dyn FnMut(Rect),
) {
    if values::out_of_flow(&c.style) && is_box(c) {
        let record = placed
            .iter()
            .find(|p| Some(p.node) == c.node && p.pseudo == c.pseudo);
        if let Some(p) = record {
            let inside = match scroller {
                Some(s) => p.inside.contains(&s),
                // The viewport holds everything but the fixed boxes it
                // is the containing block of.
                None => !(p.initial && values::position(&c.style) == Position::Fixed),
            };
            if !inside {
                return;
            }
        }
    }
    match c.kind {
        FragmentKind::Line { .. } => {}
        FragmentKind::Block {
            anonymous: true, ..
        } => {}
        _ => add(c.rect),
    }
    if c.clip.is_some() {
        return;
    }
    for g in &c.children {
        contribute(g, scroller, placed, add);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn axis(start: Option<f64>, end: Option<f64>, reverse: bool) -> Axis {
        Axis {
            total: 100.0,
            start,
            end,
            margin_start: Some(0.0),
            margin_end: Some(0.0),
            bp: 0.0,
            static_start: 30.0,
            static_end: 50.0,
            reverse,
            horizontal: true,
        }
    }

    #[test]
    fn constraint_equation_cases() {
        // All auto: the static position and shrink-to-fit.
        let (s, w, _, _) = solve(&axis(None, None, false), None, &mut |avail| avail.min(20.0));
        assert_eq!((s, w), (30.0, 20.0));
        let (s, w, _, _) = solve(&axis(None, None, true), None, &mut |avail| avail.min(20.0));
        assert_eq!((s, w), (30.0, 20.0));
        // Over-constrained: `right` ignored in ltr, `left` in rtl.
        let (s, ..) = solve(
            &axis(Some(10.0), Some(10.0), false),
            Some(50.0),
            &mut |_| 0.0,
        );
        assert_eq!(s, 10.0);
        let (s, ..) = solve(&axis(Some(10.0), Some(10.0), true), Some(50.0), &mut |_| {
            0.0
        });
        assert_eq!(s, 40.0);
        // Both insets: the width fills the rest.
        let (_, w, _, _) = solve(&axis(Some(10.0), Some(20.0), false), None, &mut |_| 0.0);
        assert_eq!(w, 70.0);
        // Auto margins center; negative ones only on the end side.
        let mut a = axis(Some(10.0), Some(10.0), false);
        a.margin_start = None;
        a.margin_end = None;
        let (_, _, ms, me) = solve(&a, Some(40.0), &mut |_| 0.0);
        assert_eq!((ms, me), (20.0, 20.0));
        let (_, _, ms, me) = solve(&a, Some(100.0), &mut |_| 0.0);
        assert_eq!((ms, me), (0.0, -20.0));
    }
}
