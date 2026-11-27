//! Flex layout (css-flexbox-1 §9) with the box alignment of css-align-3:
//! flex items in `order`-modified document order, `flex-flow` (direction
//! and wrapping, with `rtl` and `wrap-reverse`), line breaking with
//! `gap`, the resolution of flexible lengths with the automatic minimum
//! size of items (§4.5), cross sizes with `stretch` and baseline sharing
//! groups (`baseline` and `last baseline`), `justify-content`,
//! `align-content`, `align-items` and `align-self` with `safe` and
//! `unsafe` (`space-evenly` included), `auto` margins in both axes,
//! `visibility: collapse` items that leave a strut, the static position
//! of absolutely positioned children (§4.1) and the intrinsic sizes of
//! the container (§9.9).
//!
//! Positions are worked out in flow-relative coordinates (offsets from
//! the main-start and cross-start edges of the content box) and turned
//! into physical ones at the end; the fragments come relative to the
//! container's content box, as `block::layout_independent` returns them.

use crate::css::style::ComputedStyle;
use crate::css::values::Value;

use super::Ctx;
use super::block::{self, Cb, Independent, Model};
use super::boxtree::{BoxKind, BoxNode, Container, Contents};
use super::fragment::{Fragment, FragmentKind, Sides};
use super::values::{self, Display, Inner, Margins, Size, kw};

const EPS: f64 = 1e-7;

const TOP: usize = 0;
const RIGHT: usize = 1;
const BOTTOM: usize = 2;
const LEFT: usize = 3;

/// The box is a flex container.
pub(super) fn is_flex(s: &ComputedStyle) -> bool {
    matches!(
        values::display(s),
        Display::Box {
            inner: Inner::Flex,
            ..
        }
    )
}

fn number(s: &ComputedStyle, name: &str, default: f64) -> f64 {
    match s.get(name).and_then(|v| v.values.as_deref()) {
        Some([Value::Number(n)]) | Some([Value::Integer(n)]) => *n,
        _ => default,
    }
}

fn opposite(i: usize) -> usize {
    (i + 2) % 4
}

/// The axes of a flex container.
#[derive(Clone, Copy, Debug)]
struct Axes {
    /// The main axis is horizontal.
    row: bool,
    /// `row-reverse` or `column-reverse`: flex-start is the writing
    /// mode's end.
    reverse: bool,
    /// The container is multi-line.
    wrap: bool,
    wrap_reverse: bool,
    /// Main-start is the physical right or bottom edge.
    main_flip: bool,
    /// Cross-start is the physical bottom or right edge.
    cross_flip: bool,
}

impl Axes {
    fn of(s: &ComputedStyle) -> Axes {
        let dir = kw(s, "flex-direction");
        let wrap = kw(s, "flex-wrap");
        let row = !dir.starts_with("column");
        let reverse = dir.ends_with("-reverse");
        let rtl = s.rtl();
        let wrap_reverse = wrap == "wrap-reverse";
        Axes {
            row,
            reverse,
            wrap: wrap == "wrap" || wrap_reverse,
            wrap_reverse,
            main_flip: if row { rtl != reverse } else { reverse },
            cross_flip: if row {
                wrap_reverse
            } else {
                rtl != wrap_reverse
            },
        }
    }

    fn main_start(&self) -> usize {
        match (self.row, self.main_flip) {
            (true, false) => LEFT,
            (true, true) => RIGHT,
            (false, false) => TOP,
            (false, true) => BOTTOM,
        }
    }

    fn cross_start(&self) -> usize {
        match (self.row, self.cross_flip) {
            (true, false) => TOP,
            (true, true) => BOTTOM,
            (false, false) => LEFT,
            (false, true) => RIGHT,
        }
    }
}

// ----- alignment -----------------------------------------------------------------------------

/// An alignment keyword of css-align-3.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Pos {
    Normal,
    Stretch,
    FlexStart,
    FlexEnd,
    Center,
    Start,
    End,
    SelfStart,
    SelfEnd,
    Left,
    Right,
    Baseline,
    LastBaseline,
    SpaceBetween,
    SpaceAround,
    SpaceEvenly,
}

/// A computed alignment value: its keyword and whether it is `safe`.
pub(super) fn parse_align(text: &str) -> (Pos, bool) {
    let mut safe = false;
    let mut last = false;
    let mut pos = Pos::Normal;
    for w in text.split_ascii_whitespace() {
        pos = match w {
            "safe" => {
                safe = true;
                continue;
            }
            "unsafe" | "first" => continue,
            "last" => {
                last = true;
                continue;
            }
            "baseline" if last => Pos::LastBaseline,
            "baseline" => Pos::Baseline,
            "stretch" => Pos::Stretch,
            "flex-start" => Pos::FlexStart,
            "flex-end" => Pos::FlexEnd,
            "center" | "anchor-center" => Pos::Center,
            "start" => Pos::Start,
            "end" => Pos::End,
            "self-start" => Pos::SelfStart,
            "self-end" => Pos::SelfEnd,
            "left" => Pos::Left,
            "right" => Pos::Right,
            "space-between" => Pos::SpaceBetween,
            "space-around" => Pos::SpaceAround,
            "space-evenly" => Pos::SpaceEvenly,
            _ => Pos::Normal,
        };
    }
    (pos, safe)
}

/// The `align-self` of an item (`auto` is the container's
/// `align-items`).
fn align_self(item: &ComputedStyle, container: &ComputedStyle) -> (Pos, bool) {
    let own = kw(item, "align-self");
    if own.is_empty() || own == "auto" {
        parse_align(&kw(container, "align-items"))
    } else {
        parse_align(&own)
    }
}

/// The offset from main-start of a positional `justify-content` with
/// `free` space (css-align-3 §4, §5.4).
fn main_offset(p: Pos, free: f64, safe: bool, ax: &Axes) -> f64 {
    let start = if ax.reverse { free } else { 0.0 };
    let left = if ax.main_flip { free } else { 0.0 };
    let o = match p {
        Pos::FlexEnd => free,
        Pos::Center => free / 2.0,
        Pos::Start => start,
        Pos::End => free - start,
        Pos::Left if ax.row => left,
        Pos::Right if ax.row => free - left,
        Pos::Left => start,
        Pos::Right => free - start,
        _ => 0.0,
    };
    if safe && free < 0.0 { start } else { o }
}

/// The offset from main-start of the first item and the extra space
/// between items for `justify-content` (css-flexbox-1 §8.2: the
/// distributed values fall back to `flex-start` and `center`).
fn justify(p: Pos, safe: bool, free: f64, n: usize, ax: &Axes) -> (f64, f64) {
    let n = n.max(1) as f64;
    match p {
        Pos::SpaceBetween => {
            if n > 1.0 && free > 0.0 {
                (0.0, free / (n - 1.0))
            } else {
                (0.0, 0.0)
            }
        }
        Pos::SpaceAround => {
            if free > 0.0 {
                (free / n / 2.0, free / n)
            } else {
                (free / 2.0, 0.0)
            }
        }
        Pos::SpaceEvenly => {
            if free > 0.0 {
                (free / (n + 1.0), free / (n + 1.0))
            } else {
                (free / 2.0, 0.0)
            }
        }
        Pos::Normal | Pos::Stretch | Pos::Baseline | Pos::LastBaseline => (0.0, 0.0),
        p => (main_offset(p, free, safe, ax), 0.0),
    }
}

/// The offset from cross-start of a positional alignment with `free`
/// space in the cross axis; `item_rtl` is the direction of the item
/// for `self-start` and `self-end`.
fn cross_offset(p: Pos, free: f64, safe: bool, ax: &Axes, item_rtl: bool) -> f64 {
    let start = if ax.wrap_reverse { free } else { 0.0 };
    let self_start = if ax.row {
        if ax.cross_flip { free } else { 0.0 }
    } else if item_rtl == ax.cross_flip {
        0.0
    } else {
        free
    };
    let o = match p {
        Pos::FlexEnd | Pos::LastBaseline => free,
        Pos::Center => free / 2.0,
        Pos::Start => start,
        Pos::End => free - start,
        Pos::SelfStart => self_start,
        Pos::SelfEnd => free - self_start,
        _ => 0.0,
    };
    if safe && free < 0.0 { start } else { o }
}

// ----- items ---------------------------------------------------------------------------------

/// A flex item while the algorithm runs. Sizes are content-box sizes.
struct Item<'a> {
    b: &'a BoxNode,
    /// The margins by physical side (`None` for `auto`).
    margin: [Option<f64>; 4],
    border: Sides,
    padding: Sides,
    bp_main: f64,
    bp_cross: f64,
    grow: f64,
    shrink: f64,
    align: Pos,
    safe: bool,
    collapse: bool,
    /// The cross size of the line a collapsed item was in (its strut).
    strut: Option<f64>,
    /// The width a column item is measured at.
    measure_w: f64,
    base: f64,
    hypo: f64,
    min_main: f64,
    max_main: f64,
    target: f64,
    frozen: bool,
    main: f64,
    hypo_cross: f64,
    cross: f64,
    stretched: bool,
    /// The distance from the top margin edge to the first baseline, and
    /// from the last baseline to the bottom margin edge (rows only).
    ascent: f64,
    last_descent: f64,
    frag: Option<Fragment>,
    used_margin: [f64; 4],
    main_pos: f64,
    cross_pos: f64,
}

impl Item<'_> {
    fn style(&self) -> &ComputedStyle {
        &self.b.style
    }

    fn m(&self, i: usize) -> f64 {
        self.margin[i].unwrap_or(0.0)
    }

    /// The main-axis margins (`auto` as 0).
    fn mm(&self, ax: &Axes) -> f64 {
        let s = ax.main_start();
        self.m(s) + self.m(opposite(s))
    }

    /// The cross-axis margins (`auto` as 0).
    fn mc(&self, ax: &Axes) -> f64 {
        let s = ax.cross_start();
        self.m(s) + self.m(opposite(s))
    }

    fn cross_auto(&self, ax: &Axes) -> bool {
        let s = ax.cross_start();
        self.margin[s].is_none() || self.margin[opposite(s)].is_none()
    }

    /// The outer main size for content main size `size` (0 for a
    /// collapsed item).
    fn outer_main(&self, ax: &Axes, size: f64) -> f64 {
        if self.strut.is_some() {
            0.0
        } else {
            size + self.bp_main + self.mm(ax)
        }
    }

    fn outer_cross(&self, ax: &Axes, size: f64) -> f64 {
        size + self.bp_cross + self.mc(ax)
    }

    fn bp(&self) -> Sides {
        self.border.add(&self.padding)
    }

    /// The item takes part in a baseline sharing group (only when the
    /// cross axis is the block axis).
    fn baseline_group(&self, ax: &Axes) -> Option<bool> {
        if !ax.row || self.cross_auto(ax) || self.strut.is_some() {
            return None;
        }
        match self.align {
            Pos::Baseline => Some(false),
            Pos::LastBaseline => Some(true),
            _ => None,
        }
    }

    /// The item is stretched in the cross axis (css-flexbox-1 §9.4
    /// step 11).
    fn stretches(&self, ax: &Axes) -> bool {
        let prop = if ax.row { "height" } else { "width" };
        matches!(self.align, Pos::Stretch | Pos::Normal)
            && values::size(self.style(), prop).is_auto()
            && !self.cross_auto(ax)
            && self.strut.is_none()
    }
}

/// The content height `height` asks for.
fn height_spec(it: &Item, cb: Cb) -> Option<f64> {
    block::specified_height(it.style(), cb, it.bp().vertical())
}

/// The combined aspect ratio of a replaced item.
fn replaced_ratio(b: &BoxNode, natural: Option<f64>) -> Option<f64> {
    let (auto, given) = values::aspect_ratio(&b.style);
    if auto { natural.or(given) } else { given }
}

/// The content height of an item laid out at content width `w` with an
/// automatic height (before `min-height` and `max-height`).
fn auto_height(ctx: &mut Ctx, it: &Item, w: f64, cb: Cb) -> f64 {
    let b = it.b;
    let s = &b.style;
    match &b.kind {
        BoxKind::Replaced(r) => {
            let (rw, rh) = block::replaced_size(ctx, b, r, Some(cb.width), cb.height);
            match replaced_ratio(b, r.natural_ratio()) {
                Some(q) if q > 0.0 && (rw - w).abs() > EPS && height_spec(it, cb).is_none() => {
                    w / q
                }
                _ => rh,
            }
        }
        BoxKind::Container(c) => {
            let content = block::layout_independent(ctx, b, c, w, None).height;
            match block::ratio_height(s, w, &it.bp()) {
                Some(h) => block::ratio_floor(s, h, true, content),
                None => content,
            }
        }
    }
}

/// The min-content and max-content widths of an item (with the width
/// an aspect ratio transfers from a definite height).
fn widths(ctx: &mut Ctx, it: &Item, cb: Cb) -> (f64, f64) {
    let b = it.b;
    match &b.kind {
        BoxKind::Replaced(r) => {
            let w = block::replaced_size(ctx, b, r, Some(cb.width), cb.height).0;
            (w, w)
        }
        BoxKind::Container(_) => {
            if let Some(h) = height_spec(it, cb) {
                if let Some(w) = block::ratio_width(&b.style, h, &it.bp()) {
                    return (w, w);
                }
            }
            block::intrinsic(ctx, b)
        }
    }
}

/// The definite size the main size property (`width` or `height`) of
/// an item gives, as a content size.
fn main_spec(it: &Item, ax: &Axes, cb: Cb) -> Option<f64> {
    let (prop, basis) = if ax.row {
        ("width", Some(cb.width))
    } else {
        ("height", cb.height)
    };
    let Size::Lp(l) = values::size(it.style(), prop) else {
        return None;
    };
    let v = l.resolve_opt(basis)?;
    Some(if values::border_box_sizing(it.style()) {
        (v - it.bp_main).max(0.0)
    } else {
        v
    })
}

/// Prepares an item: its margins, factors, measuring width, flex base
/// size, hypothetical main size and main size limits (css-flexbox-1
/// §9.2 step 3, §4.5).
fn prepare<'a>(
    ctx: &mut Ctx,
    b: &'a BoxNode,
    container: &ComputedStyle,
    ax: &Axes,
    cb: Cb,
) -> Item<'a> {
    let s = &b.style;
    let m = Model::of(s, cb.width);
    let Margins {
        top,
        right,
        bottom,
        left,
    } = m.margin;
    let bp = m.bp();
    let (align, safe) = align_self(s, container);
    let mut it = Item {
        b,
        margin: [top, right, bottom, left],
        border: m.border,
        padding: m.padding,
        bp_main: if ax.row {
            bp.horizontal()
        } else {
            bp.vertical()
        },
        bp_cross: if ax.row {
            bp.vertical()
        } else {
            bp.horizontal()
        },
        grow: number(s, "flex-grow", 0.0).max(0.0),
        shrink: number(s, "flex-shrink", 1.0).max(0.0),
        align,
        safe,
        collapse: kw(s, "visibility") == "collapse",
        strut: None,
        measure_w: 0.0,
        base: 0.0,
        hypo: 0.0,
        min_main: 0.0,
        max_main: f64::INFINITY,
        target: 0.0,
        frozen: false,
        main: 0.0,
        hypo_cross: 0.0,
        cross: 0.0,
        stretched: false,
        ascent: 0.0,
        last_descent: 0.0,
        frag: None,
        used_margin: [0.0; 4],
        main_pos: 0.0,
        cross_pos: 0.0,
    };
    let bp_h = bp.horizontal();
    let bp_v = bp.vertical();
    let avail_w = (cb.width - it.m(LEFT) - it.m(RIGHT) - bp_h).max(0.0);
    // A column item is measured at its cross size: its `width`, the
    // stretched width of a single line, or fit-content.
    let mut col_content = 0.0;
    if !ax.row {
        let w = match &b.kind {
            BoxKind::Replaced(r) => block::replaced_size(ctx, b, r, Some(cb.width), cb.height).0,
            BoxKind::Container(_) => {
                match block::specified_width(ctx, b, Some(cb.width), bp_h, avail_w) {
                    Some(w) => w,
                    None if it.stretches(ax) && !ax.wrap => avail_w,
                    None => block::shrink_to_fit(ctx, b, avail_w),
                }
            }
        };
        it.measure_w = block::clamp_width(ctx, b, w, Some(cb.width), bp_h);
        col_content = auto_height(ctx, &it, it.measure_w, cb);
    }
    let mut row_widths = None;
    let mut widths_of = |ctx: &mut Ctx, it: &Item| -> (f64, f64) {
        *row_widths.get_or_insert_with(|| widths(ctx, it, cb))
    };

    // The main size limits.
    let min_prop = if ax.row { "min-width" } else { "min-height" };
    let (mut min_main, max_main) = if ax.row {
        (
            block::clamp_width(ctx, b, 0.0, Some(cb.width), bp_h),
            block::clamp_width(ctx, b, f64::INFINITY, Some(cb.width), bp_h),
        )
    } else {
        (
            block::clamp_height(s, 0.0, cb.height, bp_v, col_content),
            block::clamp_height(s, f64::INFINITY, cb.height, bp_v, col_content),
        )
    };
    if values::size(s, min_prop).is_auto() {
        // The automatic minimum size: 0 for scroll containers, otherwise
        // the smaller of the specified and content size suggestions.
        let (ox, oy) = values::overflow(s);
        min_main = if ox.scrolls() || oy.scrolls() {
            0.0
        } else {
            let content = if ax.row {
                widths_of(ctx, &it).0
            } else {
                col_content
            };
            let content = content.min(max_main);
            match main_spec(&it, ax, cb) {
                Some(sp) => sp.min(max_main).min(content),
                None => content,
            }
        };
    }
    let max_main = max_main.max(min_main);

    // The flex base size.
    let basis = match kw(s, "flex-basis").as_str() {
        "auto" => values::size(s, if ax.row { "width" } else { "height" }),
        _ => values::size(s, "flex-basis"),
    };
    let main_basis = if ax.row { Some(cb.width) } else { cb.height };
    let border_box = values::border_box_sizing(s);
    let base = match basis {
        Size::Lp(l) => l.resolve_opt(main_basis).map(|v| {
            if border_box {
                (v - it.bp_main).max(0.0)
            } else {
                v
            }
        }),
        Size::MinContent if ax.row => Some(widths_of(ctx, &it).0),
        Size::MaxContent if ax.row => Some(widths_of(ctx, &it).1),
        Size::FitContent(_) if ax.row => {
            let (lo, hi) = widths_of(ctx, &it);
            Some(hi.min(avail_w).max(lo))
        }
        _ => None,
    };
    let base = match base {
        Some(v) => v,
        None if ax.row => widths_of(ctx, &it).1,
        None => col_content,
    };
    it.base = base;
    it.min_main = min_main;
    it.max_main = max_main;
    it.hypo = base.min(max_main).max(min_main);
    it
}

/// Lays out an item with content width `w` and content height `h`
/// (`None`: automatic); `children_cbh` is the height percentages inside
/// it resolve against. Returns the fragment, border box at 0, 0, and its
/// content height.
fn lay_box(
    ctx: &mut Ctx,
    it: &Item,
    w: f64,
    h: Option<f64>,
    children_cbh: Option<f64>,
    cb: Cb,
) -> (Fragment, f64) {
    let b = it.b;
    let s = &b.style;
    let bp = it.bp();
    let (mut frag, h) = match &b.kind {
        BoxKind::Replaced(r) => {
            let h = h.unwrap_or_else(|| {
                let a = auto_height(ctx, it, w, cb);
                block::clamp_height(s, a, cb.height, bp.vertical(), a)
            });
            (Fragment::new(FragmentKind::Replaced(r.kind), s.clone()), h)
        }
        BoxKind::Container(c) => {
            let inner = block::layout_independent(ctx, b, c, w, children_cbh);
            let h = h.unwrap_or_else(|| {
                let a = match block::ratio_height(s, w, &bp) {
                    Some(rh) => block::ratio_floor(s, rh, true, inner.height),
                    None => inner.height,
                };
                block::clamp_height(s, a, cb.height, bp.vertical(), inner.height)
            });
            let mut f = Fragment::new(
                FragmentKind::Block {
                    anonymous: c.anonymous,
                    bfc_root: true,
                },
                s.clone(),
            );
            f.children = inner.children;
            for ch in &mut f.children {
                ch.translate(bp.left, bp.top);
            }
            (f, h)
        }
    };
    frag.node = b.node;
    frag.pseudo = b.pseudo;
    frag.border = it.border;
    frag.padding = it.padding;
    frag.rect.width = w + bp.horizontal();
    frag.rect.height = h + bp.vertical();
    match &b.kind {
        BoxKind::Replaced(r) => block::set_object(&mut frag, b, r),
        BoxKind::Container(c) => {
            if let Some(marker) = &c.marker {
                block::place_marker(ctx, marker, &mut frag, cb);
            }
        }
    }
    (frag, h)
}

// ----- lines ---------------------------------------------------------------------------------

/// A flex line: its items (indices into the ordered items), its cross
/// size and position, and its baseline sharing groups.
struct Line {
    items: Vec<usize>,
    cross: f64,
    pos: f64,
    ascent: f64,
    last_descent: f64,
}

/// What one run of the line algorithm gives.
struct Run {
    lines: Vec<Line>,
    main_size: f64,
    cross_size: f64,
}

/// The container's facts every step needs.
struct Flex<'s> {
    style: &'s ComputedStyle,
    ax: Axes,
    cb: Cb,
    main_def: Option<f64>,
    cross_def: Option<f64>,
    main_gap: f64,
    cross_gap: f64,
    /// The container's vertical borders and paddings (for its own
    /// `min-height` and `max-height` under `box-sizing: border-box`).
    bp_v: f64,
}

impl Flex<'_> {
    /// The container's height clamped by its `min-height` and
    /// `max-height` (the size is indefinite, so percentages do not
    /// apply).
    fn clamp_height(&self, h: f64) -> f64 {
        block::clamp_height(self.style, h, None, self.bp_v, h)
    }

    /// Collects items into lines (§9.3).
    fn break_lines(&self, items: &[Item]) -> Vec<Vec<usize>> {
        let ax = &self.ax;
        let limit = match self.main_def {
            Some(m) => m,
            None => self.clamp_height(f64::INFINITY),
        };
        let mut lines: Vec<Vec<usize>> = Vec::new();
        let mut cur: Vec<usize> = Vec::new();
        let mut used = 0.0;
        for (i, it) in items.iter().enumerate() {
            let outer = it.outer_main(ax, it.hypo);
            let add = if cur.is_empty() {
                outer
            } else {
                outer + self.main_gap
            };
            if ax.wrap && !cur.is_empty() && used + add > limit + EPS {
                lines.push(std::mem::take(&mut cur));
                used = outer;
            } else {
                used += add;
            }
            cur.push(i);
        }
        if !cur.is_empty() || lines.is_empty() {
            lines.push(cur);
        }
        lines
    }

    /// Resolves the flexible lengths of one line (§9.7).
    fn flex_line(&self, items: &mut [Item], idx: &[usize], avail: f64) {
        let ax = &self.ax;
        let gaps = self.main_gap * idx.len().saturating_sub(1) as f64;
        let sum_hypo: f64 = idx
            .iter()
            .map(|&i| items[i].outer_main(ax, items[i].hypo))
            .sum::<f64>()
            + gaps;
        let grow = sum_hypo < avail;
        for &i in idx {
            let it = &mut items[i];
            it.target = it.hypo;
            it.frozen = false;
            if it.strut.is_some() {
                it.target = 0.0;
                it.frozen = true;
                continue;
            }
            let factor = if grow { it.grow } else { it.shrink };
            if factor == 0.0 || (grow && it.base > it.hypo) || (!grow && it.base < it.hypo) {
                it.frozen = true;
            }
        }
        let free_of = |items: &[Item]| {
            avail
                - gaps
                - idx
                    .iter()
                    .map(|&i| {
                        let it = &items[i];
                        it.outer_main(ax, if it.frozen { it.target } else { it.base })
                    })
                    .sum::<f64>()
        };
        let initial = free_of(items);
        loop {
            let open: Vec<usize> = idx.iter().copied().filter(|&i| !items[i].frozen).collect();
            if open.is_empty() {
                break;
            }
            let mut free = free_of(items);
            let factors: f64 = open
                .iter()
                .map(|&i| if grow { items[i].grow } else { items[i].shrink })
                .sum();
            if factors < 1.0 {
                let scaled = initial * factors;
                if scaled.abs() < free.abs() {
                    free = scaled;
                }
            }
            if grow {
                for &i in &open {
                    let it = &mut items[i];
                    it.target = it.base + free * it.grow / factors;
                }
            } else {
                let scaled: f64 = open.iter().map(|&i| items[i].shrink * items[i].base).sum();
                for &i in &open {
                    let it = &mut items[i];
                    it.target = if scaled > 0.0 {
                        it.base - free.abs() * it.shrink * it.base / scaled
                    } else {
                        it.base
                    };
                }
            }
            let mut total = 0.0;
            let mut violation = vec![0.0; open.len()];
            for (k, &i) in open.iter().enumerate() {
                let it = &mut items[i];
                let clamped = it.target.min(it.max_main).max(it.min_main).max(0.0);
                violation[k] = clamped - it.target;
                total += violation[k];
                it.target = clamped;
            }
            for (k, &i) in open.iter().enumerate() {
                let v = violation[k];
                let freeze = if total.abs() < EPS {
                    true
                } else if total > 0.0 {
                    v > 0.0
                } else {
                    v < 0.0
                };
                if freeze {
                    items[i].frozen = true;
                }
            }
        }
        for &i in idx {
            items[i].main = items[i].target;
        }
    }

    /// Lines, flexible lengths, hypothetical cross sizes and line cross
    /// sizes (§9.3, §9.4 steps 7 and 8).
    fn run(&self, ctx: &mut Ctx, items: &mut [Item]) -> Run {
        let ax = &self.ax;
        let cb = self.cb;
        let groups = self.break_lines(items);
        let main_size = match self.main_def {
            Some(m) => m,
            None => {
                let widest = groups
                    .iter()
                    .map(|g| {
                        g.iter()
                            .map(|&i| items[i].outer_main(ax, items[i].hypo))
                            .sum::<f64>()
                            + self.main_gap * g.len().saturating_sub(1) as f64
                    })
                    .fold(0.0, f64::max);
                self.clamp_height(widest)
            }
        };
        for g in &groups {
            self.flex_line(items, g, main_size);
        }
        // The hypothetical cross sizes.
        for it in items.iter_mut() {
            if let Some(strut) = it.strut {
                it.hypo_cross = strut;
                continue;
            }
            if ax.row {
                let spec = height_spec(it, cb)
                    .map(|h| block::clamp_height(it.style(), h, cb.height, it.bp_cross, h));
                let (frag, h) = lay_box(ctx, it, it.main, spec, spec, cb);
                it.hypo_cross = h;
                let mt = it.m(TOP);
                let bh = frag.rect.height;
                let first = block::find_baseline(&frag.children, false).unwrap_or(bh);
                let last = block::find_baseline(&frag.children, true).unwrap_or(bh);
                it.ascent = mt + first;
                it.last_descent = bh - last + it.m(BOTTOM);
                it.frag = Some(frag);
            } else {
                it.hypo_cross = it.measure_w;
            }
        }
        // The line cross sizes.
        let mut lines: Vec<Line> = groups
            .into_iter()
            .map(|g| {
                let (mut outer, mut asc, mut desc, mut lasc, mut ldesc) =
                    (0.0f64, 0.0f64, 0.0f64, 0.0f64, 0.0f64);
                for &i in &g {
                    let it = &items[i];
                    if let Some(strut) = it.strut {
                        outer = outer.max(strut);
                        continue;
                    }
                    let o = it.outer_cross(ax, it.hypo_cross);
                    match it.baseline_group(ax) {
                        Some(false) => {
                            asc = asc.max(it.ascent);
                            desc = desc.max(o - it.ascent);
                        }
                        Some(true) => {
                            ldesc = ldesc.max(it.last_descent);
                            lasc = lasc.max(o - it.last_descent);
                        }
                        None => outer = outer.max(o),
                    }
                }
                Line {
                    items: g,
                    cross: outer.max(asc + desc).max(lasc + ldesc),
                    pos: 0.0,
                    ascent: asc,
                    last_descent: ldesc,
                }
            })
            .collect();
        if !ax.wrap {
            lines[0].cross = match self.cross_def {
                Some(c) => c,
                None if ax.row => self.clamp_height(lines[0].cross),
                None => lines[0].cross,
            };
        }
        let total = lines.iter().map(|l| l.cross).sum::<f64>()
            + self.cross_gap * lines.len().saturating_sub(1) as f64;
        let cross_size = match self.cross_def {
            Some(c) => c,
            None => self.clamp_height(total),
        };
        Run {
            lines,
            main_size,
            cross_size,
        }
    }
}

// ----- the algorithm -------------------------------------------------------------------------

/// Lays out the content of flex container `b` with content width `w`
/// and, when definite, content height `cbh`.
pub(super) fn layout(
    ctx: &mut Ctx,
    b: &BoxNode,
    c: &Container,
    w: f64,
    cbh: Option<f64>,
) -> Independent {
    let s = &b.style;
    let ax = Axes::of(s);
    let children: &[BoxNode] = match &c.contents {
        Contents::Blocks(v) => v,
        Contents::Inline(_) => &[],
    };
    let cb = Cb {
        width: w,
        height: cbh,
        rtl: s.rtl(),
    };
    let gap = |name: &str, basis: Option<f64>| {
        values::lp(s, name)
            .and_then(|l| l.resolve_opt(basis))
            .unwrap_or(0.0)
            .max(0.0)
    };
    let (main_gap, cross_gap) = if ax.row {
        (gap("column-gap", Some(w)), gap("row-gap", cbh))
    } else {
        (gap("row-gap", cbh), gap("column-gap", Some(w)))
    };
    let fl = Flex {
        style: s,
        ax,
        cb,
        main_def: if ax.row { Some(w) } else { cbh },
        cross_def: if ax.row { cbh } else { Some(w) },
        main_gap,
        cross_gap,
        bp_v: Model::of(s, w).bp().vertical(),
    };

    let mut ordered: Vec<&BoxNode> = children
        .iter()
        .filter(|ch| !values::out_of_flow(&ch.style))
        .collect();
    ordered.sort_by_key(|ch| number(&ch.style, "order", 0.0) as i64);
    let mut items: Vec<Item> = ordered
        .into_iter()
        .map(|ch| prepare(ctx, ch, s, &ax, cb))
        .collect();

    let mut run = fl.run(ctx, &mut items);
    // `visibility: collapse` (§4.4): the collapsed items keep the cross
    // size of their line as a strut and the algorithm runs again.
    if items.iter().any(|it| it.collapse) {
        for line in &run.lines {
            for &i in &line.items {
                if items[i].collapse {
                    items[i].strut = Some(line.cross);
                }
            }
        }
        run = fl.run(ctx, &mut items);
    }
    let Run {
        mut lines,
        main_size,
        cross_size,
    } = run;

    // `align-content` (§9.4 step 15, §9.6 step 16).
    let n = lines.len();
    let total = lines.iter().map(|l| l.cross).sum::<f64>() + cross_gap * n.saturating_sub(1) as f64;
    let free = cross_size - total;
    let (mut offset, mut between) = (0.0, 0.0);
    if ax.wrap {
        let (p, safe) = parse_align(&kw(s, "align-content"));
        match p {
            Pos::Normal | Pos::Stretch => {
                if free > 0.0 {
                    for l in &mut lines {
                        l.cross += free / n as f64;
                    }
                }
            }
            Pos::SpaceBetween | Pos::SpaceAround | Pos::SpaceEvenly => {
                let k = n as f64;
                (offset, between) = match p {
                    Pos::SpaceBetween if n > 1 && free > 0.0 => (0.0, free / (k - 1.0)),
                    Pos::SpaceBetween => (0.0, 0.0),
                    _ if free <= 0.0 => (free / 2.0, 0.0),
                    Pos::SpaceAround => (free / k / 2.0, free / k),
                    _ => (free / (k + 1.0), free / (k + 1.0)),
                };
            }
            Pos::Baseline => {}
            p => offset = cross_offset(p, free, safe, &ax, false),
        }
    }
    let mut pos = offset;
    for l in &mut lines {
        l.pos = pos;
        pos += l.cross + cross_gap + between;
    }

    let ms = ax.main_start();
    let me = opposite(ms);
    let cs = ax.cross_start();
    let ce = opposite(cs);
    for line in &lines {
        // Stretched cross sizes (§9.4 step 11).
        for &i in &line.items {
            let it = &mut items[i];
            it.stretched = it.stretches(&ax);
            it.cross = if let Some(strut) = it.strut {
                strut
            } else if it.stretched {
                let v = (line.cross - it.mc(&ax) - it.bp_cross).max(0.0);
                if ax.row {
                    block::clamp_height(it.style(), v, cbh, it.bp_cross, v)
                } else {
                    block::clamp_width(ctx, it.b, v, Some(w), it.bp_cross)
                }
            } else {
                it.hypo_cross
            };
        }
        // Main-axis alignment (§9.5).
        let count = line.items.len();
        let used: f64 = line
            .items
            .iter()
            .map(|&i| items[i].outer_main(&ax, items[i].main))
            .sum::<f64>()
            + main_gap * count.saturating_sub(1) as f64;
        let free = main_size - used;
        let autos = line
            .items
            .iter()
            .filter(|&&i| items[i].strut.is_none())
            .map(|&i| {
                items[i].margin[ms].is_none() as usize + items[i].margin[me].is_none() as usize
            })
            .sum::<usize>();
        let (start, between, per_auto) = if free > 0.0 && autos > 0 {
            (0.0, 0.0, free / autos as f64)
        } else {
            let (p, safe) = parse_align(&kw(s, "justify-content"));
            let (o, bt) = justify(p, safe, free, count, &ax);
            (o, bt, 0.0)
        };
        let mut pos = start;
        for &i in &line.items {
            let it = &mut items[i];
            let collapsed = it.strut.is_some();
            let side_margin = |k: usize| match (collapsed, it.margin[k]) {
                (true, _) => 0.0,
                (false, Some(v)) => v,
                (false, None) => per_auto,
            };
            let (m_s, m_e) = (side_margin(ms), side_margin(me));
            it.used_margin[ms] = m_s;
            it.used_margin[me] = m_e;
            pos += m_s;
            it.main_pos = pos;
            let size = if collapsed { 0.0 } else { it.main + it.bp_main };
            pos += size + m_e + main_gap + between;
        }
        // Cross-axis alignment (§9.6).
        for &i in &line.items {
            let it = &mut items[i];
            let outer = it.outer_cross(&ax, it.cross);
            let free = line.cross - outer;
            let (a_s, a_e) = (it.margin[cs].is_none(), it.margin[ce].is_none());
            let (mut m_s, mut m_e) = (it.m(cs), it.m(ce));
            let off = if it.strut.is_some() {
                0.0
            } else if a_s || a_e {
                if free > 0.0 {
                    match (a_s, a_e) {
                        (true, true) => {
                            m_s = free / 2.0;
                            m_e = free / 2.0;
                        }
                        (true, false) => m_s = free,
                        _ => m_e = free,
                    }
                    0.0
                } else if ax.wrap_reverse {
                    // Aligned with the writing mode's cross-start edge.
                    free
                } else {
                    0.0
                }
            } else {
                match it.baseline_group(&ax) {
                    Some(last) => {
                        let from_top = if last {
                            free - (line.last_descent - it.last_descent)
                        } else {
                            line.ascent - it.ascent
                        };
                        if ax.cross_flip {
                            free - from_top
                        } else {
                            from_top
                        }
                    }
                    None => {
                        let p = match it.align {
                            Pos::Baseline | Pos::Normal | Pos::Stretch => Pos::FlexStart,
                            p => p,
                        };
                        cross_offset(p, free, it.safe, &ax, it.style().rtl())
                    }
                }
            };
            it.used_margin[cs] = m_s;
            it.used_margin[ce] = m_e;
            it.cross_pos = line.pos + off + m_s;
        }
    }

    // The fragments, in `order`-modified document order.
    let mut out: Vec<Fragment> = Vec::with_capacity(items.len());
    for it in &mut items {
        let collapsed = it.strut.is_some();
        let main = if collapsed { 0.0 } else { it.main };
        let (cw, ch) = if ax.row {
            (main, it.cross)
        } else {
            (it.cross, main)
        };
        // A row item that is not stretched keeps its hypothetical layout.
        let saved = it.frag.take();
        let mut frag = if let (true, Some(f)) = (ax.row && !it.stretched && !collapsed, saved) {
            f
        } else {
            let definite = if ax.row {
                (it.stretched && cbh.is_some()) || height_spec(it, cb).is_some()
            } else {
                cbh.is_some() || height_spec(it, cb).is_some()
            };
            let children_cbh = definite.then_some(ch);
            lay_box(ctx, it, cw, Some(ch), children_cbh, cb).0
        };
        let main_border = main + it.bp_main;
        let cross_border = it.cross + it.bp_cross;
        let mp = if ax.main_flip {
            main_size - it.main_pos - main_border
        } else {
            it.main_pos
        };
        let cp = if ax.cross_flip {
            cross_size - it.cross_pos - cross_border
        } else {
            it.cross_pos
        };
        let (x, y) = if ax.row { (mp, cp) } else { (cp, mp) };
        frag.rect.x = x;
        frag.rect.y = y;
        let um = it.used_margin;
        frag.margin = Sides::new(um[TOP], um[RIGHT], um[BOTTOM], um[LEFT]);
        out.push(frag);
    }

    // The static positions of absolutely positioned children (§4.1): as
    // if each were the only item of the container.
    for ch in children.iter().filter(|ch| values::out_of_flow(&ch.style)) {
        let (_, mw, mh) = block::layout_float(ctx, ch, cb);
        let (om, oc) = if ax.row { (mw, mh) } else { (mh, mw) };
        let free_m = main_size - om;
        let (p, safe) = parse_align(&kw(s, "justify-content"));
        let off_m = match p {
            Pos::SpaceAround | Pos::SpaceEvenly => free_m / 2.0,
            p => justify(p, safe, free_m, 1, &ax).0,
        };
        let free_c = cross_size - oc;
        let (p, safe) = align_self(&ch.style, s);
        let off_c = match p {
            Pos::Normal | Pos::Stretch | Pos::Baseline => 0.0,
            p => cross_offset(p, free_c, safe, &ax, ch.style.rtl()),
        };
        let mp = if ax.main_flip {
            main_size - off_m - om
        } else {
            off_m
        };
        let cp = if ax.cross_flip {
            cross_size - off_c - oc
        } else {
            off_c
        };
        let (x, y) = if ax.row { (mp, cp) } else { (cp, mp) };
        out.push(block::static_position(ch, x, y, mw));
    }

    Independent {
        children: out,
        height: if ax.row { cross_size } else { main_size },
    }
}

/// The min-content and max-content widths of the content box of flex
/// container `c` (css-flexbox-1 §9.9.1): a row sums its items'
/// contributions (a multi-line row's min-content is the largest one); a
/// column takes the largest.
pub(super) fn intrinsic(ctx: &mut Ctx, c: &Container, style: &ComputedStyle) -> (f64, f64) {
    let ax = Axes::of(style);
    let Contents::Blocks(children) = &c.contents else {
        return (0.0, 0.0);
    };
    let contributions: Vec<(f64, f64)> = children
        .iter()
        .filter(|ch| !values::out_of_flow(&ch.style) && kw(&ch.style, "visibility") != "collapse")
        .map(|ch| block::contribution(ctx, ch, None))
        .collect();
    let n = contributions.len();
    if !ax.row {
        return contributions
            .iter()
            .fold((0.0f64, 0.0f64), |(a, b), &(lo, hi)| (a.max(lo), b.max(hi)));
    }
    let gap = values::lp(style, "column-gap")
        .and_then(|l| l.resolve_opt(None))
        .unwrap_or(0.0)
        .max(0.0);
    let gaps = gap * n.saturating_sub(1) as f64;
    let max = contributions.iter().map(|c| c.1).sum::<f64>() + gaps;
    let min = if ax.wrap {
        contributions.iter().map(|c| c.0).fold(0.0, f64::max)
    } else {
        contributions.iter().map(|c| c.0).sum::<f64>() + gaps
    };
    (min, max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alignment_keywords() {
        assert_eq!(parse_align("safe center"), (Pos::Center, true));
        assert_eq!(parse_align("unsafe flex-end"), (Pos::FlexEnd, false));
        assert_eq!(parse_align("last baseline"), (Pos::LastBaseline, false));
        assert_eq!(parse_align("first baseline"), (Pos::Baseline, false));
        assert_eq!(parse_align("space-evenly"), (Pos::SpaceEvenly, false));
    }

    #[test]
    fn justify_distribution() {
        let ax = Axes {
            row: true,
            reverse: false,
            wrap: false,
            wrap_reverse: false,
            main_flip: false,
            cross_flip: false,
        };
        assert_eq!(justify(Pos::SpaceEvenly, false, 60.0, 2, &ax), (20.0, 20.0));
        assert_eq!(justify(Pos::SpaceBetween, false, 60.0, 3, &ax), (0.0, 30.0));
        assert_eq!(justify(Pos::SpaceAround, false, 60.0, 3, &ax), (10.0, 20.0));
        assert_eq!(justify(Pos::Center, false, -20.0, 1, &ax), (-10.0, 0.0));
        assert_eq!(justify(Pos::Center, true, -20.0, 1, &ax), (0.0, 0.0));
        let rev = Axes {
            reverse: true,
            main_flip: true,
            ..ax
        };
        // `end` in a reversed row is flex-start.
        assert_eq!(justify(Pos::End, false, 40.0, 1, &rev), (0.0, 0.0));
        assert_eq!(justify(Pos::Left, false, 40.0, 1, &rev), (40.0, 0.0));
    }
}
