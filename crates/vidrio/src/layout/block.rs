//! Block formatting contexts (CSS 2 §9.4.1, §10.3, §10.6, §8.3.1, §9.5):
//! widths of block-level, floating, inline-block and replaced boxes
//! (with `min-*`, `max-*`, `box-sizing` and `aspect-ratio`), heights,
//! margin collapsing (between siblings, between parents and their first
//! and last children, and through empty boxes), clearance, floats, boxes
//! that establish formatting contexts beside floats, list markers
//! outside their items, and the min-content and max-content
//! contributions every layout mode uses. Sizes follow css-sizing-3 and
//! -4: `min-content`, `max-content`, `fit-content()` and `stretch` in
//! both axes, `aspect-ratio` in whichever axis is automatic,
//! `contain-intrinsic-size` under size containment, and the object
//! rectangle of replaced elements (`object-fit`, `object-position`).
//! Out-of-flow boxes leave a placeholder at their static position for
//! positioned layout.
//!
//! Inside one formatting context, positions are those of its root's
//! content box; a box converts its children to coordinates relative to
//! its own border box when it knows where it is.

use std::rc::Rc;

use crate::css::style::ComputedStyle;

use super::Ctx;
use super::boxtree::{BoxKind, BoxNode, Container, Contents, Replaced};
use super::flex;
use super::float::FloatContext;
use super::fragment::{Fragment, FragmentKind, Rect, Sides};
use super::grid;
use super::inline;
use super::table;
use super::values::{self, Float, Margins, Size};

const EPS: f64 = 1e-7;

/// A containing block: its content width and, when definite, height.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Cb {
    pub width: f64,
    pub height: Option<f64>,
    /// The containing block's `direction` is `rtl`: it decides which
    /// margin gives way when they are over-constrained (CSS 2 §10.3.3).
    pub rtl: bool,
}

/// Adjoining margins (CSS 2 §8.3.1): the largest positive and the most
/// negative.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Collapsed {
    pos: f64,
    neg: f64,
}

impl Collapsed {
    pub fn adjoin(&mut self, m: f64) {
        if m > 0.0 {
            self.pos = self.pos.max(m);
        } else {
            self.neg = self.neg.min(m);
        }
    }

    pub fn solve(&self) -> f64 {
        self.pos + self.neg
    }
}

/// The position in a block formatting context while its in-flow content
/// is laid out: the bottom of the content so far, the margins that
/// adjoin it and are not resolved yet, and the boxes whose top border
/// edge is where those margins end.
#[derive(Debug, Default)]
pub(crate) struct Flow {
    pub y: f64,
    margin: Collapsed,
    waiting: Vec<usize>,
    tops: Vec<Option<f64>>,
    /// The box just laid out collapsed through (its margins adjoin what
    /// follows).
    through: bool,
}

impl Flow {
    pub fn new(y: f64) -> Flow {
        Flow {
            y,
            ..Flow::default()
        }
    }

    /// Ends the pending margins: content follows.
    pub fn resolve(&mut self) {
        self.y += self.margin.solve();
        self.margin = Collapsed::default();
        let y = self.y;
        for slot in self.waiting.drain(..) {
            self.tops[slot] = Some(y);
        }
    }

    /// Where content would start if it came now.
    pub fn hypothetical(&self) -> f64 {
        self.y + self.margin.solve()
    }

    fn wait(&mut self) -> usize {
        self.tops.push(None);
        let slot = self.tops.len() - 1;
        self.waiting.push(slot);
        slot
    }

    fn unwait(&mut self, slot: usize) {
        self.waiting.retain(|&s| s != slot);
    }

    fn top_of(&self, slot: usize) -> Option<f64> {
        self.tops[slot]
    }

    /// Clearance: the boxes waiting for their top get it from the margins
    /// before the cleared box, which do not collapse with it.
    fn fill_waiting(&mut self, y: f64) {
        for slot in self.waiting.drain(..) {
            self.tops[slot] = Some(y);
        }
    }
}

/// The margins, borders and paddings of a box for a containing block
/// width.
pub(super) struct Model {
    pub margin: Margins,
    pub border: Sides,
    pub padding: Sides,
}

impl Model {
    pub fn of(style: &ComputedStyle, cbw: f64) -> Model {
        Model {
            margin: values::margins(style, cbw),
            border: values::border(style),
            padding: values::padding(style, cbw),
        }
    }

    pub fn bp(&self) -> Sides {
        self.border.add(&self.padding)
    }
}

// ----- sizes -------------------------------------------------------------------------------

/// The content width `width` asks for (`None` for `auto`).
pub(super) fn specified_width(
    ctx: &mut Ctx,
    b: &BoxNode,
    cbw: Option<f64>,
    bp_h: f64,
    avail: f64,
) -> Option<f64> {
    // A table wrapper box is as wide as its table (css-tables-3 §3.9.2).
    if table::is_wrapper(b) {
        return Some(table::used_width(ctx, b, cbw, avail));
    }
    let s = &b.style;
    let border_box = values::border_box_sizing(s);
    let content = |w: f64| {
        if border_box { (w - bp_h).max(0.0) } else { w }
    };
    match values::size(s, "width") {
        Size::Lp(l) => l.resolve_opt(cbw).map(content),
        Size::MinContent => Some(intrinsic(ctx, b).0),
        Size::MaxContent => Some(intrinsic(ctx, b).1),
        Size::FitContent(limit) => {
            let (min, max) = intrinsic(ctx, b);
            let avail = match limit.and_then(|l| l.resolve_opt(cbw)) {
                Some(l) => content(l),
                None => avail,
            };
            Some(max.min(avail).max(min))
        }
        Size::Stretch => Some(avail.max(0.0)),
        _ => None,
    }
}

/// `width` clamped by `max-width` and then `min-width` (CSS 2 §10.4).
pub(super) fn clamp_width(ctx: &mut Ctx, b: &BoxNode, w: f64, cbw: Option<f64>, bp_h: f64) -> f64 {
    let s = &b.style;
    let border_box = values::border_box_sizing(s);
    let content = |v: f64| {
        if border_box { (v - bp_h).max(0.0) } else { v }
    };
    let max = match values::size(s, "max-width") {
        Size::Lp(l) => l.resolve_opt(cbw).map(content),
        Size::MinContent => Some(intrinsic(ctx, b).0),
        Size::MaxContent => Some(intrinsic(ctx, b).1),
        _ => None,
    };
    let min = match values::size(s, "min-width") {
        Size::Lp(l) => l.resolve_opt(cbw).map(content).unwrap_or(0.0),
        Size::MinContent => intrinsic(ctx, b).0,
        Size::MaxContent => intrinsic(ctx, b).1,
        _ => 0.0,
    };
    let mut w = w;
    if let Some(max) = max {
        w = w.min(max);
    }
    w.max(min)
}

/// The content height `height` asks for in containing block `cb`
/// (`None` for `auto` and for percentages of an indefinite height). The
/// content-based keywords behave as `auto` in the block axis of a block
/// container (css-sizing-3 §5.2); `stretch` fills the containing block.
pub(super) fn specified_height(s: &ComputedStyle, cb: Cb, bp_v: f64) -> Option<f64> {
    let h = match values::size(s, "height") {
        Size::Stretch => {
            let m = values::margins(s, cb.width).or_zero();
            return cb.height.map(|h| (h - m.vertical() - bp_v).max(0.0));
        }
        sz => sz.definite(cb.height)?,
    };
    Some(if values::border_box_sizing(s) {
        (h - bp_v).max(0.0)
    } else {
        h
    })
}

/// `height` clamped by `max-height` and then `min-height`; `content` is
/// the content height the content-based keywords stand for.
pub(super) fn clamp_height(
    s: &ComputedStyle,
    h: f64,
    cbh: Option<f64>,
    bp_v: f64,
    content: f64,
) -> f64 {
    let border_box = values::border_box_sizing(s);
    let conv = |v: f64| {
        if border_box { (v - bp_v).max(0.0) } else { v }
    };
    let limit = |name: &str| match values::size(s, name) {
        Size::Lp(l) => l.resolve_opt(cbh).map(conv),
        Size::MinContent | Size::MaxContent | Size::FitContent(_) => Some(content),
        _ => None,
    };
    let mut h = h;
    if let Some(max) = limit("max-height") {
        h = h.min(max);
    }
    h.max(limit("min-height").unwrap_or(0.0))
}

/// The height `aspect-ratio` gives a box whose width is known.
pub(super) fn ratio_height(s: &ComputedStyle, w: f64, bp: &Sides) -> Option<f64> {
    let (_, ratio) = values::aspect_ratio(s);
    let r = ratio?;
    if values::border_box_sizing(s) {
        Some(((w + bp.horizontal()) / r - bp.vertical()).max(0.0))
    } else {
        Some(w / r)
    }
}

/// The width `aspect-ratio` gives a box whose height is known.
pub(super) fn ratio_width(s: &ComputedStyle, h: f64, bp: &Sides) -> Option<f64> {
    let (_, ratio) = values::aspect_ratio(s);
    let r = ratio?;
    if values::border_box_sizing(s) {
        Some(((h + bp.vertical()) * r - bp.horizontal()).max(0.0))
    } else {
        Some(h * r)
    }
}

/// The height of a box whose height came from `aspect-ratio`: with
/// `min-height: auto` and visible overflow it is at least the content
/// height (css-sizing-4 §5.1, the automatic minimum size).
pub(super) fn ratio_floor(s: &ComputedStyle, h: f64, from_ratio: bool, content: f64) -> f64 {
    let (_, oy) = values::overflow(s);
    if from_ratio && values::size(s, "min-height").is_auto() && !oy.scrolls() {
        h.max(content)
    } else {
        h
    }
}

/// The content height of a box under size containment in the block
/// axis (css-contain-2 §3.1), or `content`.
pub(super) fn contained_height(s: &ComputedStyle, content: f64) -> f64 {
    if values::size_contained(s).1 {
        values::contain_intrinsic(s, "contain-intrinsic-height")
    } else {
        content
    }
}

/// The used size of a replaced element's content box (CSS 2 §10.3.2,
/// §10.6.2 and the table of §10.4).
pub(super) fn replaced_size(
    ctx: &mut Ctx,
    b: &BoxNode,
    r: &Replaced,
    cbw: Option<f64>,
    cbh: Option<f64>,
) -> (f64, f64) {
    let s = &b.style;
    let m = Model::of(s, cbw.unwrap_or(0.0));
    let bp = m.bp();
    let sw = match values::size(s, "width") {
        Size::Lp(l) => l.resolve_opt(cbw).map(|w| {
            if values::border_box_sizing(s) {
                (w - bp.horizontal()).max(0.0)
            } else {
                w
            }
        }),
        _ => None,
    };
    let cb = Cb {
        width: cbw.unwrap_or(0.0),
        height: cbh,
        rtl: s.rtl(),
    };
    let sh = specified_height(s, cb, bp.vertical());
    let (auto, given) = values::aspect_ratio(s);
    // Under size containment the natural size is the
    // `contain-intrinsic-size` and there is no natural ratio.
    let (ci, cb_) = values::size_contained(s);
    let natural_ratio = if ci || cb_ { None } else { r.natural_ratio() };
    let ratio = if auto { natural_ratio.or(given) } else { given };
    let nw = if ci {
        Some(values::contain_intrinsic(s, "contain-intrinsic-width"))
    } else {
        r.natural_width
    };
    let nh = if cb_ {
        Some(values::contain_intrinsic(s, "contain-intrinsic-height"))
    } else {
        r.natural_height
    };
    let (w, h) = match (sw, sh) {
        (Some(w), Some(h)) => (w, h),
        (Some(w), None) => (w, ratio.map_or(nh.unwrap_or(150.0), |q| w / q)),
        (None, Some(h)) => (ratio.map_or(nw.unwrap_or(300.0), |q| h * q), h),
        (None, None) => match (nw, nh, ratio) {
            (Some(w), Some(h), _) => (w, h),
            (Some(w), None, Some(q)) => (w, w / q),
            (None, Some(h), Some(q)) => (h * q, h),
            (None, None, Some(q)) => {
                let w = cbw.unwrap_or(300.0);
                (w, w / q)
            }
            (Some(w), None, None) => (w, 150.0),
            (None, Some(h), None) => (300.0, h),
            (None, None, None) => (300.0, 150.0),
        },
    };
    let bp_h = bp.horizontal();
    let bp_v = bp.vertical();
    let max_w = {
        let big = clamp_width(ctx, b, f64::INFINITY, cbw, bp_h);
        if big.is_finite() { big } else { f64::INFINITY }
    };
    let min_w = clamp_width(ctx, b, 0.0, cbw, bp_h);
    let max_h = clamp_height(s, f64::INFINITY, cbh, bp_v, h);
    let min_h = clamp_height(s, 0.0, cbh, bp_v, h);
    let max_w = max_w.max(min_w);
    let max_h = max_h.max(min_h);
    if sw.is_some() || sh.is_some() || ratio.is_none() || w <= 0.0 || h <= 0.0 {
        let w = w.min(max_w).max(min_w);
        let h = h.min(max_h).max(min_h);
        // A constrained width or height carries the other through the
        // ratio when the other is automatic.
        return match (sw, sh, ratio) {
            (Some(_), None, Some(q)) if q > 0.0 => (w, (w / q).min(max_h).max(min_h)),
            (None, Some(_), Some(q)) => ((h * q).min(max_w).max(min_w), h),
            _ => (w, h),
        };
    }
    // Both automatic with a ratio: the constraint table.
    let (w_gt, w_lt) = (w > max_w + EPS, w < min_w - EPS);
    let (h_gt, h_lt) = (h > max_h + EPS, h < min_h - EPS);
    match (w_gt, w_lt, h_gt, h_lt) {
        (false, false, false, false) => (w, h),
        (true, _, true, _) => {
            if max_w / w <= max_h / h {
                (max_w, (max_w * h / w).max(min_h))
            } else {
                ((max_h * w / h).max(min_w), max_h)
            }
        }
        (_, true, _, true) => {
            if min_w / w <= min_h / h {
                ((min_h * w / h).min(max_w), min_h)
            } else {
                (min_w, (min_w * h / w).min(max_h))
            }
        }
        (_, true, true, _) => (min_w, max_h),
        (true, _, _, true) => (max_w, min_h),
        (true, _, _, _) => (max_w, (max_w * h / w).max(min_h)),
        (_, true, _, _) => (min_w, (min_w * h / w).min(max_h)),
        (_, _, true, _) => ((max_h * w / h).max(min_w), max_h),
        (_, _, _, true) => ((min_h * w / h).min(max_w), min_h),
    }
}

/// The content-box min-content and max-content widths of a box.
pub(crate) fn intrinsic(ctx: &mut Ctx, b: &BoxNode) -> (f64, f64) {
    match &b.kind {
        BoxKind::Replaced(r) => {
            let (w, _) = replaced_size(ctx, b, r, None, None);
            (w, w)
        }
        BoxKind::Container(c) => {
            if values::size_contained(&b.style).0 {
                let w = values::contain_intrinsic(&b.style, "contain-intrinsic-width");
                return (w, w);
            }
            if table::is_wrapper(b) {
                return table::intrinsic(ctx, b);
            }
            if flex::is_flex(&b.style) {
                return flex::intrinsic(ctx, c, &b.style);
            }
            if grid::is_grid(&b.style) {
                return grid::intrinsic(ctx, b, c);
            }
            if let Some(cs) = c.control {
                if let Some(chars) = cs.chars {
                    let m = ctx.fonts.metrics(&b.style);
                    let w = chars * m.zero;
                    return (w, w);
                }
            }
            match &c.contents {
                Contents::Inline(ic) => inline::intrinsic(ctx, ic, &b.style, c.single_line),
                Contents::Blocks(children) => {
                    let (mut min, mut max) = (0.0f64, 0.0f64);
                    let mut floats_max = 0.0f64;
                    for child in children {
                        if values::out_of_flow(&child.style) {
                            continue;
                        }
                        let (cmin, cmax) = contribution(ctx, child, None);
                        min = min.max(cmin);
                        if values::float(&child.style) != Float::None {
                            let (l, r) = values::clear(&child.style);
                            if l || r {
                                max = max.max(floats_max);
                                floats_max = 0.0;
                            }
                            floats_max += cmax;
                        } else {
                            let (l, r) = values::clear(&child.style);
                            if l || r {
                                max = max.max(floats_max);
                                floats_max = 0.0;
                                max = max.max(cmax);
                            } else {
                                max = max.max(cmax + floats_max_if_bfc(child, floats_max));
                            }
                        }
                    }
                    (min, max.max(floats_max))
                }
            }
        }
    }
}

/// A box that establishes a formatting context sits beside the floats
/// before it; other boxes flow around them.
fn floats_max_if_bfc(b: &BoxNode, floats: f64) -> f64 {
    match &b.kind {
        BoxKind::Container(c) if c.bfc_root => floats,
        BoxKind::Replaced(_) => floats,
        _ => 0.0,
    }
}

/// The min-content and max-content contributions of a box: its outer
/// size (CSS Sizing 3 §5.1).
pub(crate) fn contribution(ctx: &mut Ctx, b: &BoxNode, cbw: Option<f64>) -> (f64, f64) {
    let s = &b.style;
    let m = Model::of(s, cbw.unwrap_or(0.0));
    let fixed = |v: Option<f64>, name: &str| {
        if values::lp(s, name).is_some_and(|l| l.has_percent()) && cbw.is_none() {
            0.0
        } else {
            v.unwrap_or(0.0)
        }
    };
    let mh = fixed(m.margin.left, "margin-left") + fixed(m.margin.right, "margin-right");
    let bp_h = m.bp().horizontal();
    let spec = match &b.kind {
        BoxKind::Replaced(r) => Some(replaced_size(ctx, b, r, cbw, None).0),
        _ => match values::size(s, "width") {
            Size::Lp(l) if !l.has_percent() || cbw.is_some() => {
                specified_width(ctx, b, cbw, bp_h, 0.0)
            }
            Size::MinContent | Size::MaxContent => specified_width(ctx, b, cbw, bp_h, 0.0),
            _ => None,
        },
    };
    let (cmin, cmax) = match spec {
        Some(w) => (w, w),
        None => intrinsic(ctx, b),
    };
    let cmin = clamp_width(ctx, b, cmin, cbw, bp_h);
    let cmax = clamp_width(ctx, b, cmax, cbw, bp_h);
    (cmin + bp_h + mh, cmax + bp_h + mh)
}

/// Shrink-to-fit (CSS 2 §10.3.5): min(max(min-content, available),
/// max-content).
pub(super) fn shrink_to_fit(ctx: &mut Ctx, b: &BoxNode, avail: f64) -> f64 {
    let (min, max) = intrinsic(ctx, b);
    max.min(avail.max(min))
}

/// The used width and horizontal margins of a block-level box in normal
/// flow (CSS 2 §10.3.3), with `min-width` and `max-width` (§10.4).
fn block_width(
    ctx: &mut Ctx,
    b: &BoxNode,
    cbw: f64,
    cbh: Option<f64>,
    rtl: bool,
    m: &Model,
) -> (f64, f64, f64) {
    let bp_h = m.bp().horizontal();
    let solve = |w: Option<f64>| -> (f64, f64, f64) {
        let (ml, mr) = (m.margin.left, m.margin.right);
        match w {
            None => {
                let ml = ml.unwrap_or(0.0);
                let mr = mr.unwrap_or(0.0);
                ((cbw - ml - mr - bp_h).max(0.0), ml, mr)
            }
            Some(w) => {
                let rest = cbw - w - bp_h;
                match (ml, mr) {
                    (None, None) => {
                        let half = rest / 2.0;
                        if half < 0.0 {
                            if rtl { (w, rest, 0.0) } else { (w, 0.0, rest) }
                        } else {
                            (w, half, half)
                        }
                    }
                    (None, Some(mr)) => (w, rest - mr, mr),
                    (Some(ml), None) => (w, ml, rest - ml),
                    (Some(ml), Some(mr)) => {
                        if rtl {
                            (w, rest - mr, mr)
                        } else {
                            (w, ml, rest - ml)
                        }
                    }
                }
            }
        }
    };
    let avail = cbw - m.margin.left.unwrap_or(0.0) - m.margin.right.unwrap_or(0.0) - bp_h;
    let mut spec = specified_width(ctx, b, Some(cbw), bp_h, avail);
    if spec.is_none() {
        spec = ratio_dependent_width(b, cbw, cbh, &m.bp());
    }
    if spec.is_none() {
        if let BoxKind::Container(c) = &b.kind {
            if let Some(cs) = c.control {
                if let Some(chars) = cs.chars {
                    spec = Some(chars * ctx.fonts.metrics(&b.style).zero);
                }
            }
        }
    }
    let (w, ml, mr) = solve(spec);
    let clamped = clamp_width(ctx, b, w, Some(cbw), bp_h);
    if (clamped - w).abs() > EPS {
        solve(Some(clamped))
    } else {
        (w, ml, mr)
    }
}

/// The width of a box with `width: auto` whose `aspect-ratio` makes
/// the width depend on a definite height (css-sizing-4 §5.1).
pub(super) fn ratio_dependent_width(
    b: &BoxNode,
    cbw: f64,
    cbh: Option<f64>,
    bp: &Sides,
) -> Option<f64> {
    let s = &b.style;
    let cb = Cb {
        width: cbw,
        height: cbh,
        rtl: s.rtl(),
    };
    let h = specified_height(s, cb, bp.vertical())?;
    let h = clamp_height(s, h, cbh, bp.vertical(), h);
    ratio_width(s, h, bp)
}

/// Sets where replaced element `r` (fragment `frag`, sized) draws its
/// content: its natural size (or the content box when it has none)
/// fitted by `object-fit` and placed by `object-position`, relative to
/// the border box.
pub(super) fn set_object(frag: &mut Fragment, b: &BoxNode, r: &Replaced) {
    let s = &b.style;
    let content = frag.content_rect();
    let (w, h) = (content.width, content.height);
    let natural = match (r.natural_width, r.natural_height) {
        (Some(nw), Some(nh)) if nw > 0.0 && nh > 0.0 => Some((nw, nh)),
        _ => None,
    };
    let (ow, oh) = match natural {
        None => (w, h),
        Some((nw, nh)) => {
            let contain = (w / nw).min(h / nh);
            let cover = (w / nw).max(h / nh);
            match values::object_fit(s) {
                values::ObjectFit::Fill => (w, h),
                values::ObjectFit::Contain => (nw * contain, nh * contain),
                values::ObjectFit::Cover => (nw * cover, nh * cover),
                values::ObjectFit::None => (nw, nh),
                values::ObjectFit::ScaleDown => {
                    let k = contain.min(1.0);
                    (nw * k, nh * k)
                }
            }
        }
    };
    let (dx, dy) = values::object_position(s, w, h, ow, oh);
    let bp = frag.border.add(&frag.padding);
    frag.object = Some(Rect::new(bp.left + dx, bp.top + dy, ow, oh));
}

// ----- baselines -----------------------------------------------------------------------

/// The baseline of the first (or last) line box in `frags` and their
/// in-flow block descendants, relative to the frame `frags` are in.
pub(crate) fn find_baseline(frags: &[Fragment], last: bool) -> Option<f64> {
    let order: Vec<&Fragment> = if last {
        frags.iter().rev().collect()
    } else {
        frags.iter().collect()
    };
    for f in order {
        match &f.kind {
            FragmentKind::Line { baseline } if !f.children.is_empty() || f.rect.height > 0.0 => {
                return Some(f.rect.y + baseline);
            }
            FragmentKind::Block { .. } => {
                let overflow_visible = values::kw(&f.style, "overflow-x") == "visible";
                if overflow_visible
                    || matches!(
                        f.kind,
                        FragmentKind::Block {
                            bfc_root: false,
                            ..
                        }
                    )
                {
                    if let Some(b) = find_baseline(&f.children, last) {
                        return Some(f.rect.y + b);
                    }
                }
            }
            _ => {}
        }
    }
    None
}

// ----- formatting context roots --------------------------------------------------------------

/// The laid out content of a box that establishes a formatting context.
pub(super) struct Independent {
    /// Relative to the content box.
    pub children: Vec<Fragment>,
    /// The auto height of the content box.
    pub height: f64,
}

/// Lays out the content of a formatting context root with content width
/// `w`.
pub(super) fn layout_independent(
    ctx: &mut Ctx,
    b: &BoxNode,
    c: &Container,
    w: f64,
    cbh: Option<f64>,
) -> Independent {
    if table::is_wrapper(b) {
        return table::layout(ctx, b, w, cbh);
    }
    if flex::is_flex(&b.style) {
        let mut inner = flex::layout(ctx, b, c, w, cbh);
        inner.height = contained_height(&b.style, inner.height);
        return inner;
    }
    if grid::is_grid(&b.style) {
        let mut inner = grid::layout(ctx, b, c, w, cbh);
        inner.height = contained_height(&b.style, inner.height);
        return inner;
    }
    let mut floats = FloatContext::default();
    let mut flow = Flow::new(0.0);
    let children = layout_contents(
        ctx,
        b,
        c,
        0.0,
        w,
        cbh,
        &mut flow,
        &mut floats,
        None,
        &mut Vec::new(),
    );
    flow.resolve();
    let mut height = flow.y;
    if let Some(fb) = floats.bottom() {
        height = height.max(fb);
    }
    if let Some(cs) = c.control {
        if let Some(lines) = cs.lines {
            let m = ctx.fonts.metrics(&b.style);
            let lh = inline::line_height(&b.style, &m);
            height = lines * lh;
        }
    }
    let height = contained_height(&b.style, height);
    Independent { children, height }
}

/// Lays out a box that does not take part in its parent's flow (float,
/// atomic inline, out of flow, list marker) with containing block `cb`;
/// returns its fragment (border box placed at its margins, so the margin
/// box starts at 0, 0) and the margin box size.
fn layout_detached(ctx: &mut Ctx, b: &BoxNode, cb: Cb, kind: FragmentKind) -> (Fragment, f64, f64) {
    let s = &b.style;
    let m = Model::of(s, cb.width);
    let bp = m.bp();
    let margin = m.margin.or_zero();
    let mut frag = Fragment::new(kind, s.clone());
    frag.node = b.node;
    frag.pseudo = b.pseudo;
    frag.border = m.border;
    frag.padding = m.padding;
    frag.margin = margin;
    let (w, h) = match &b.kind {
        BoxKind::Replaced(r) => {
            let (w, h) = replaced_size(ctx, b, r, Some(cb.width), cb.height);
            frag.kind = FragmentKind::Replaced(r.kind);
            (w, h)
        }
        BoxKind::Container(c) => {
            let avail = cb.width - margin.horizontal() - bp.horizontal();
            let mut w = specified_width(ctx, b, Some(cb.width), bp.horizontal(), avail);
            if w.is_none() {
                w = ratio_dependent_width(b, cb.width, cb.height, &bp);
            }
            if w.is_none() {
                if let Some(cs) = c.control {
                    if let Some(chars) = cs.chars {
                        w = Some(chars * ctx.fonts.metrics(s).zero);
                    }
                }
            }
            let w = match w {
                Some(w) => w,
                None => shrink_to_fit(ctx, b, avail),
            };
            let w = clamp_width(ctx, b, w, Some(cb.width), bp.horizontal());
            let explicit_h = specified_height(s, cb, bp.vertical());
            let ratio_h = explicit_h
                .is_none()
                .then(|| ratio_height(s, w, &bp))
                .flatten();
            let spec_h = explicit_h.or(ratio_h);
            let inner = layout_independent(ctx, b, c, w, spec_h);
            let used = ratio_floor(
                s,
                spec_h.unwrap_or(inner.height),
                ratio_h.is_some(),
                inner.height,
            );
            let h = clamp_height(s, used, cb.height, bp.vertical(), inner.height);
            let mut children = inner.children;
            for ch in &mut children {
                ch.translate(bp.left, bp.top);
            }
            frag.children = children;
            (w, h)
        }
    };
    frag.rect.x = margin.left;
    frag.rect.y = margin.top;
    frag.rect.width = w + bp.horizontal();
    frag.rect.height = h + bp.vertical();
    if let BoxKind::Replaced(r) = &b.kind {
        set_object(&mut frag, b, r);
    }
    let mw = frag.rect.width + margin.horizontal();
    let mh = frag.rect.height + margin.vertical();
    (frag, mw, mh)
}

/// A float laid out for containing block `cb`: its fragment (margin box
/// at 0, 0) and margin box size.
pub(crate) fn layout_float(ctx: &mut Ctx, b: &BoxNode, cb: Cb) -> (Fragment, f64, f64) {
    layout_detached(ctx, b, cb, FragmentKind::Float)
}

/// An atomic inline: its fragment (margin box at 0, 0), margin box size
/// and the distance from the margin box top to its baseline (CSS 2
/// §10.8.1: the last line box, or the bottom margin edge).
pub(crate) fn layout_atomic(ctx: &mut Ctx, b: &BoxNode, cb: Cb) -> (Fragment, f64, f64, f64) {
    let (frag, mw, mh) = layout_detached(ctx, b, cb, FragmentKind::InlineBlock);
    let mut baseline = mh;
    if table::is_wrapper(b) {
        // An inline table sits on the baseline of its first row (CSS 2
        // §10.8.1).
        if let Some(bl) = table::baseline(&frag) {
            baseline = frag.rect.y + bl;
        }
    } else if matches!(frag.kind, FragmentKind::InlineBlock)
        && values::kw(&b.style, "overflow-x") == "visible"
    {
        if let Some(bl) = find_baseline(&frag.children, true) {
            baseline = frag.rect.y + bl;
        }
    }
    (frag, mw, mh, baseline)
}

/// The placeholder of out-of-flow box `b` at its static position
/// (`x`, `y`): positioned layout lays the box out once its containing
/// block is known. `width` is the width of the containing block the
/// static position is in, so that the right static position is
/// `x + width` (0 inside a line, where both are `x`).
pub(crate) fn static_position(b: &BoxNode, x: f64, y: f64, width: f64) -> Fragment {
    let mut frag = Fragment::new(FragmentKind::Placeholder, b.style.clone());
    frag.node = b.node;
    frag.pseudo = b.pseudo;
    frag.rect = Rect::new(x, y, width, 0.0);
    frag
}

// ----- normal flow ---------------------------------------------------------------------------

/// Lays out the content of block container `c` (box `b`) whose content
/// box starts at `x` (formatting context coordinates) with width `w`, in
/// `flow` and `floats`. Fragments come in formatting context coordinates.
/// `followers` gets the indices of the children that collapsed through
/// while `b`'s top margin was still pending: their margins collapse with
/// it, so their top border edge is `b`'s (CSS 2 §8.3.1).
#[allow(clippy::too_many_arguments)]
fn layout_contents(
    ctx: &mut Ctx,
    b: &BoxNode,
    c: &Container,
    x: f64,
    w: f64,
    cbh: Option<f64>,
    flow: &mut Flow,
    floats: &mut FloatContext,
    first_line: Option<&Rc<ComputedStyle>>,
    followers: &mut Vec<usize>,
) -> Vec<Fragment> {
    let first_line = c.first_line.as_ref().or(first_line);
    match &c.contents {
        Contents::Inline(ic) => {
            let significant = ic.is_significant();
            if significant {
                flow.resolve();
            }
            let y = if significant {
                flow.y
            } else {
                flow.hypothetical()
            };
            let r = inline::layout(
                ctx,
                ic,
                &b.style,
                first_line,
                inline::Place {
                    x,
                    y,
                    width: w,
                    cb_height: cbh,
                    single_line: c.single_line,
                },
                floats,
            );
            if significant {
                flow.y += r.height;
            }
            let mut out = r.lines;
            out.extend(r.others);
            out
        }
        Contents::Blocks(children) => {
            let mut out = Vec::with_capacity(children.len());
            let mut pending_first_line = first_line;
            let cb = Cb {
                width: w,
                height: cbh,
                rtl: b.style.rtl(),
            };
            for child in children {
                if values::out_of_flow(&child.style) {
                    let y = flow.hypothetical();
                    out.push(static_position(child, x, y, w));
                    continue;
                }
                if values::float(&child.style) != Float::None {
                    out.push(flow_float(ctx, child, cb, x, flow, floats));
                    continue;
                }
                let fl = pending_first_line.take();
                let parent_pending = !flow.waiting.is_empty();
                flow.through = false;
                let frag = layout_in_flow(ctx, child, cb, x, flow, floats, fl);
                if parent_pending && flow.through {
                    followers.push(out.len());
                }
                flow.through = false;
                out.push(frag);
            }
            out
        }
    }
}

/// Places a float that is a child of a block container with block
/// children, at the current position of the flow.
fn flow_float(
    ctx: &mut Ctx,
    b: &BoxNode,
    cb: Cb,
    x: f64,
    flow: &Flow,
    floats: &mut FloatContext,
) -> Fragment {
    let mut y = flow.hypothetical();
    let (l, r) = values::clear(&b.style);
    if l || r {
        if let Some(cy) = floats.clearance_y(l, r) {
            y = y.max(cy);
        }
    }
    place_float(ctx, b, cb, x, y, floats)
}

/// Lays out float `b` and places it no higher than `y` in the containing
/// block whose content box starts at `x`; the fragment is in formatting
/// context coordinates.
pub(crate) fn place_float(
    ctx: &mut Ctx,
    b: &BoxNode,
    cb: Cb,
    x: f64,
    y: f64,
    floats: &mut FloatContext,
) -> Fragment {
    let (mut frag, mw, mh) = layout_float(ctx, b, cb);
    let side = match values::float(&b.style) {
        Float::Right => Float::Right,
        _ => Float::Left,
    };
    let (left, top) = floats.place(side, mw, mh, y, x, x + cb.width);
    frag.translate(left, top);
    frag
}

/// Lays out an in-flow block-level box.
fn layout_in_flow(
    ctx: &mut Ctx,
    b: &BoxNode,
    cb: Cb,
    x: f64,
    flow: &mut Flow,
    floats: &mut FloatContext,
    first_line: Option<&Rc<ComputedStyle>>,
) -> Fragment {
    match &b.kind {
        BoxKind::Container(c) if !c.bfc_root => {
            flow_block(ctx, b, c, cb, x, flow, floats, first_line)
        }
        _ => flow_independent(ctx, b, cb, x, flow, floats, first_line),
    }
}

/// Applies `clear` to a box whose top margin was just added to `flow`
/// (`before` = the margins before it): true if it got clearance.
fn apply_clearance(b: &BoxNode, flow: &mut Flow, before: Collapsed, floats: &FloatContext) -> bool {
    let (l, r) = values::clear(&b.style);
    if !(l || r) {
        return false;
    }
    let Some(cy) = floats.clearance_y(l, r) else {
        return false;
    };
    let hyp = flow.hypothetical();
    if hyp >= cy - EPS {
        return false;
    }
    // The margins above do not collapse with the cleared box's: the
    // boxes waiting for a top end there, and the border edge goes to
    // the float bottom (CSS 2 §9.5.2).
    let prev = flow.y + before.solve();
    flow.fill_waiting(prev);
    flow.y = cy.max(hyp);
    flow.margin = Collapsed::default();
    true
}

/// A block container in normal flow that does not establish a
/// formatting context: its margins collapse with its children's.
#[allow(clippy::too_many_arguments)]
fn flow_block(
    ctx: &mut Ctx,
    b: &BoxNode,
    c: &Container,
    cb: Cb,
    cb_x: f64,
    flow: &mut Flow,
    floats: &mut FloatContext,
    first_line: Option<&Rc<ComputedStyle>>,
) -> Fragment {
    let s = &b.style;
    let m = Model::of(s, cb.width);
    let bp = m.bp();
    let (w, ml, mr) = block_width(ctx, b, cb.width, cb.height, cb.rtl, &m);
    let bx = cb_x + ml;
    let mt = m.margin.top.unwrap_or(0.0);
    let mb = m.margin.bottom.unwrap_or(0.0);
    let before = flow.margin;
    flow.margin.adjoin(mt);
    let cleared = apply_clearance(b, flow, before, floats);
    let slot = if bp.top > 0.0 || cleared {
        flow.resolve();
        None
    } else {
        Some(flow.wait())
    };
    let known_top = if slot.is_none() { Some(flow.y) } else { None };
    flow.y += bp.top;
    let explicit_h = specified_height(s, cb, bp.vertical());
    let ratio_h = explicit_h
        .is_none()
        .then(|| ratio_height(s, w, &bp))
        .flatten();
    let spec_h = explicit_h.or(ratio_h);
    let mut followers = Vec::new();
    let mut children = layout_contents(
        ctx,
        b,
        c,
        bx + bp.left,
        w,
        spec_h,
        flow,
        floats,
        first_line,
        &mut followers,
    );
    let min_h = clamp_height(s, 0.0, cb.height, bp.vertical(), 0.0);
    let can_collapse_bottom = bp.bottom == 0.0 && spec_h.is_none() && min_h <= EPS;
    let (top, collapsed_through) = match (known_top, slot) {
        (Some(t), _) => (t, false),
        (None, Some(sl)) => match flow.top_of(sl) {
            Some(t) => (t, false),
            None => {
                flow.unwait(sl);
                if can_collapse_bottom {
                    (flow.hypothetical(), true)
                } else {
                    flow.resolve();
                    (flow.y, false)
                }
            }
        },
        (None, None) => (flow.y, false),
    };
    if !can_collapse_bottom && !collapsed_through {
        flow.resolve();
    }
    let content_top = top + bp.top;
    let auto_h = if collapsed_through {
        0.0
    } else {
        contained_height(s, (flow.y - content_top).max(0.0))
    };
    let used = ratio_floor(s, spec_h.unwrap_or(auto_h), ratio_h.is_some(), auto_h);
    let h = clamp_height(s, used, cb.height, bp.vertical(), auto_h);
    for &i in &followers {
        children[i].rect.y = top;
    }
    flow.through = collapsed_through && h <= EPS;
    if flow.through {
        flow.margin.adjoin(mb);
    } else {
        flow.y = content_top + h + bp.bottom;
        flow.margin.adjoin(mb);
    }
    let mut frag = Fragment::new(
        FragmentKind::Block {
            anonymous: c.anonymous,
            bfc_root: false,
        },
        s.clone(),
    );
    frag.node = b.node;
    frag.pseudo = b.pseudo;
    frag.border = m.border;
    frag.padding = m.padding;
    frag.margin = Sides::new(mt, mr, mb, ml);
    frag.rect.x = bx;
    frag.rect.y = top;
    frag.rect.width = w + bp.horizontal();
    frag.rect.height = h + bp.vertical();
    frag.children = children;
    for ch in &mut frag.children {
        ch.translate(-bx, -top);
    }
    if let Some(marker) = &c.marker {
        place_marker(ctx, marker, &mut frag, cb);
    }
    frag
}

/// A block-level box that establishes a formatting context (or a
/// block-level replaced element) in normal flow: its border box does not
/// overlap the floats of the outer context (CSS 2 §9.5).
fn flow_independent(
    ctx: &mut Ctx,
    b: &BoxNode,
    cb: Cb,
    cb_x: f64,
    flow: &mut Flow,
    floats: &mut FloatContext,
    first_line: Option<&Rc<ComputedStyle>>,
) -> Fragment {
    let _ = first_line;
    let s = &b.style;
    let m = Model::of(s, cb.width);
    let bp = m.bp();
    let mt = m.margin.top.unwrap_or(0.0);
    let mb = m.margin.bottom.unwrap_or(0.0);
    let before = flow.margin;
    flow.margin.adjoin(mt);
    apply_clearance(b, flow, before, floats);
    flow.resolve();
    let cb_left = cb_x;
    let cb_right = cb_x + cb.width;
    let rtl = s.rtl();
    let mut y = flow.y;
    let mut band_h = 0.0;
    let mut result: Option<(Fragment, f64, f64, f64)> = None;
    for _attempt in 0..4 {
        let (l, r) = floats.available(y, band_h, cb_left, cb_right);
        let narrowed = l > cb_left + EPS || r < cb_right - EPS;
        // The width and margins for the space beside the floats.
        let (w, ml, mr, x) = if !narrowed {
            let (w, ml, mr, h_override) =
                independent_width(ctx, b, cb.width, cb.height, cb.rtl, &m);
            let _ = h_override;
            (w, ml, mr, cb_left + ml)
        } else {
            let ml = m.margin.left.unwrap_or(0.0);
            let mr = m.margin.right.unwrap_or(0.0);
            let left_edge = (cb_left + ml).max(l);
            let right_edge = (cb_right - mr).min(r);
            let space = (right_edge - left_edge - bp.horizontal()).max(0.0);
            let (w, _, _, _) = independent_width(
                ctx,
                b,
                space + bp.horizontal(),
                cb.height,
                cb.rtl,
                &Model {
                    margin: Margins {
                        left: Some(0.0),
                        right: Some(0.0),
                        ..m.margin
                    },
                    border: m.border,
                    padding: m.padding,
                },
            );
            let x = if rtl {
                right_edge - w - bp.horizontal()
            } else {
                left_edge
            };
            if w + bp.horizontal() > right_edge - left_edge + EPS && !floats.is_empty() {
                // Too wide beside the floats: try below the next one.
                let (ny, _, _) =
                    floats.find_space(y, w + bp.horizontal(), band_h.max(1.0), cb_left, cb_right);
                if ny > y + EPS {
                    y = ny;
                    continue;
                }
            }
            (w, x - cb_left, cb_right - (x + w + bp.horizontal()), x)
        };
        let (frag, h) = independent_box(ctx, b, w, cb);
        let border_h = h + bp.vertical();
        let fits = {
            let (l2, r2) = floats.available(y, border_h, cb_left, cb_right);
            (l2 - l).abs() < EPS && (r2 - r).abs() < EPS
        };
        result = Some((frag, x, ml, mr));
        if fits || floats.is_empty() {
            break;
        }
        band_h = border_h;
    }
    let (mut frag, x, ml, mr) = result.unwrap_or_else(|| {
        let (w, ml, mr, _) = independent_width(ctx, b, cb.width, cb.height, cb.rtl, &m);
        let (frag, _) = independent_box(ctx, b, w, cb);
        (frag, cb_left + ml, ml, mr)
    });
    frag.rect.x = x;
    frag.rect.y = y;
    frag.margin = Sides::new(mt, mr, mb, ml);
    flow.y = y + frag.rect.height;
    flow.margin.adjoin(mb);
    frag
}

/// The used width and margins of a block-level formatting context root
/// or replaced element for a containing block width.
fn independent_width(
    ctx: &mut Ctx,
    b: &BoxNode,
    cbw: f64,
    cbh: Option<f64>,
    rtl: bool,
    m: &Model,
) -> (f64, f64, f64, Option<f64>) {
    match &b.kind {
        BoxKind::Replaced(r) => {
            let (w, h) = replaced_size(ctx, b, r, Some(cbw), cbh);
            let bp_h = m.bp().horizontal();
            let rest = cbw - w - bp_h;
            let (ml, mr) = match (m.margin.left, m.margin.right) {
                (None, None) => {
                    if rest < 0.0 {
                        if rtl { (rest, 0.0) } else { (0.0, rest) }
                    } else {
                        (rest / 2.0, rest / 2.0)
                    }
                }
                (None, Some(mr)) => (rest - mr, mr),
                (Some(ml), None) => (ml, rest - ml),
                (Some(ml), Some(mr)) => {
                    if rtl {
                        (rest - mr, mr)
                    } else {
                        (ml, rest - ml)
                    }
                }
            };
            (w, ml, mr, Some(h))
        }
        BoxKind::Container(_) => {
            let (w, ml, mr) = block_width(ctx, b, cbw, cbh, rtl, m);
            (w, ml, mr, None)
        }
    }
}

/// Lays out a formatting context root or replaced element with content
/// width `w`: its fragment (at 0, 0) and content height.
fn independent_box(ctx: &mut Ctx, b: &BoxNode, w: f64, cb: Cb) -> (Fragment, f64) {
    let s = &b.style;
    let m = Model::of(s, cb.width);
    let bp = m.bp();
    let (kind, h, children) = match &b.kind {
        BoxKind::Replaced(r) => {
            let (used_w, h) = replaced_size(ctx, b, r, Some(cb.width), cb.height);
            // Keep the used width; the height follows the ratio when it
            // is automatic and the width changed.
            let h = match (values::size(s, "height").is_auto(), r.natural_ratio()) {
                (true, Some(q)) if (used_w - w).abs() > EPS => w / q,
                _ => h,
            };
            (FragmentKind::Replaced(r.kind), h, Vec::new())
        }
        BoxKind::Container(c) => {
            let explicit_h = specified_height(s, cb, bp.vertical());
            let ratio_h = explicit_h
                .is_none()
                .then(|| ratio_height(s, w, &bp))
                .flatten();
            let spec_h = explicit_h.or(ratio_h);
            let inner = layout_independent(ctx, b, c, w, spec_h);
            let used = ratio_floor(
                s,
                spec_h.unwrap_or(inner.height),
                ratio_h.is_some(),
                inner.height,
            );
            let h = clamp_height(s, used, cb.height, bp.vertical(), inner.height);
            let mut children = inner.children;
            for ch in &mut children {
                ch.translate(bp.left, bp.top);
            }
            (
                FragmentKind::Block {
                    anonymous: c.anonymous,
                    bfc_root: true,
                },
                h,
                children,
            )
        }
    };
    let mut frag = Fragment::new(kind, s.clone());
    frag.node = b.node;
    frag.pseudo = b.pseudo;
    frag.border = m.border;
    frag.padding = m.padding;
    frag.rect.width = w + bp.horizontal();
    frag.rect.height = h + bp.vertical();
    frag.children = children;
    match &b.kind {
        BoxKind::Container(c) => {
            if let Some(marker) = &c.marker {
                place_marker(ctx, marker, &mut frag, cb);
            }
        }
        BoxKind::Replaced(r) => set_object(&mut frag, b, r),
    }
    (frag, h)
}

/// Places a list item's outside `::marker` (css-lists-3 §3.4): beside
/// the start edge of the item's content box, on the baseline of its
/// first line (or of where the first line would be).
pub(super) fn place_marker(ctx: &mut Ctx, marker: &BoxNode, item: &mut Fragment, cb: Cb) {
    let (mut mf, mw, _, baseline) = layout_atomic(ctx, marker, cb);
    mf.kind = FragmentKind::Marker;
    let content = Fragment::content_rect(item);
    let content_x = content.x - item.rect.x;
    let content_y = content.y - item.rect.y;
    let line_baseline = find_baseline(&item.children, false).unwrap_or_else(|| {
        let m = ctx.fonts.metrics(&item.style);
        let lh = inline::line_height(&item.style, &m);
        content_y + (lh - (m.ascent + m.descent)) / 2.0 + m.ascent
    });
    let rtl = item.style.rtl();
    let x = if rtl {
        content_x + content.width
    } else {
        content_x - mw
    };
    mf.translate(x, line_baseline - baseline);
    item.children.push(mf);
}

/// Lays out block-level box `b` alone at the top of a new block
/// formatting context of width `cbw` (a table caption): its fragment,
/// placed at its margins, and the height of its margin box.
pub(super) fn layout_alone(ctx: &mut Ctx, b: &BoxNode, cbw: f64) -> (Fragment, f64) {
    let cb = Cb {
        width: cbw,
        height: None,
        rtl: b.style.rtl(),
    };
    let mut flow = Flow::new(0.0);
    let mut floats = FloatContext::default();
    let frag = layout_in_flow(ctx, b, cb, 0.0, &mut flow, &mut floats, None);
    let mut height = flow.hypothetical();
    if let Some(fb) = floats.bottom() {
        height = height.max(fb);
    }
    (frag, height)
}

/// Lays out the root element's box in the initial containing block.
pub(crate) fn layout_root(ctx: &mut Ctx, root: &BoxNode, viewport: (f64, f64)) -> Fragment {
    // The initial containing block has the root's direction (CSS 2 §10.1).
    let cb = Cb {
        width: viewport.0,
        height: Some(viewport.1),
        rtl: root.style.rtl(),
    };
    let mut flow = Flow::new(0.0);
    let mut floats = FloatContext::default();
    let mut frag = match &root.kind {
        BoxKind::Container(c) if !c.bfc_root => {
            flow_block(ctx, root, c, cb, 0.0, &mut flow, &mut floats, None)
        }
        _ => flow_independent(ctx, root, cb, 0.0, &mut flow, &mut floats, None),
    };
    if let FragmentKind::Block { bfc_root, .. } = &mut frag.kind {
        *bfc_root = true;
    }
    frag
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collapsed_margins() {
        let mut c = Collapsed::default();
        c.adjoin(10.0);
        c.adjoin(20.0);
        c.adjoin(-5.0);
        c.adjoin(-15.0);
        assert_eq!(c.solve(), 5.0);
        let mut f = Flow::new(0.0);
        f.margin.adjoin(8.0);
        let slot = f.wait();
        f.margin.adjoin(16.0);
        f.resolve();
        assert_eq!(f.top_of(slot), Some(16.0));
        assert_eq!(f.y, 16.0);
    }
}
