//! Reading the computed values layout needs: lengths and percentages
//! (with `calc()` resolved against a basis), sizes, the box model sides,
//! `display` split into its outer and inner types, floats, clearance and
//! positioning schemes.

use crate::css::style::ComputedStyle;
use crate::css::values::Value;
use crate::css::values::calc::Math;
use crate::css::values::compute::{Context, compute_math};

use super::fragment::Sides;

/// A computed `<length-percentage>`.
#[derive(Clone, Debug)]
pub(crate) enum Lp {
    Px(f64),
    Pct(f64),
    Calc(Box<Math>),
}

impl Lp {
    /// The value in pixels with percentages of `basis`.
    pub fn resolve(&self, basis: f64) -> f64 {
        self.resolve_opt(Some(basis)).unwrap_or(0.0)
    }

    /// The value in pixels; `None` if it needs an unknown basis.
    pub fn resolve_opt(&self, basis: Option<f64>) -> Option<f64> {
        match self {
            Lp::Px(v) => Some(*v),
            Lp::Pct(p) => basis.map(|b| b * p / 100.0),
            Lp::Calc(m) => match compute_math(m, &Context::default(), basis) {
                Value::Dimension(v, _) => Some(v),
                Value::Number(v) if v == 0.0 => Some(0.0),
                Value::Percentage(p) => basis.map(|b| b * p / 100.0),
                _ => None,
            },
        }
    }

    pub fn has_percent(&self) -> bool {
        match self {
            Lp::Px(_) => false,
            Lp::Pct(_) => true,
            Lp::Calc(m) => m.to_css().contains('%'),
        }
    }
}

/// The `<length-percentage>` of one value.
pub(crate) fn value_lp(v: &Value) -> Option<Lp> {
    match v {
        Value::Dimension(px, unit) if unit == "px" => Some(Lp::Px(*px)),
        Value::Dimension(..) => None,
        Value::Percentage(p) => Some(Lp::Pct(*p)),
        Value::Number(n) | Value::Integer(n) if *n == 0.0 => Some(Lp::Px(0.0)),
        Value::Math(m) => Some(Lp::Calc(m.clone())),
        _ => None,
    }
}

/// The computed `<length-percentage>` of `name`, or `None` for keywords.
pub(crate) fn lp(style: &ComputedStyle, name: &str) -> Option<Lp> {
    match style.get(name)?.values.as_deref()? {
        [v] => value_lp(v),
        _ => None,
    }
}

/// A length in pixels (`0` for anything else).
pub(crate) fn px(style: &ComputedStyle, name: &str) -> f64 {
    lp(style, name).map_or(0.0, |l| l.resolve(0.0))
}

/// The serialized computed value of `name`, lowercase.
pub(crate) fn kw(style: &ComputedStyle, name: &str) -> String {
    style.text(name).to_ascii_lowercase()
}

/// A computed `width`, `height`, `min-*` or `max-*`.
#[derive(Clone, Debug)]
pub(crate) enum Size {
    Auto,
    /// `none` (maximums only).
    None,
    Lp(Lp),
    MinContent,
    MaxContent,
    FitContent(Option<Lp>),
    Stretch,
}

pub(crate) fn size(style: &ComputedStyle, name: &str) -> Size {
    let Some(v) = style.get(name) else {
        return Size::Auto;
    };
    match v.values.as_deref() {
        Some([Value::Keyword(k)]) => match k.as_str() {
            "none" => Size::None,
            "min-content" => Size::MinContent,
            "max-content" => Size::MaxContent,
            "fit-content" => Size::FitContent(None),
            "stretch" | "-webkit-fill-available" => Size::Stretch,
            _ => Size::Auto,
        },
        Some([Value::Function(f, args)]) if f == "fit-content" => {
            Size::FitContent(args.first().and_then(value_lp))
        }
        Some([v]) => value_lp(v).map_or(Size::Auto, Size::Lp),
        _ => Size::Auto,
    }
}

impl Size {
    /// The size in pixels when it is a length or a percentage of a known
    /// basis.
    pub fn definite(&self, basis: Option<f64>) -> Option<f64> {
        match self {
            Size::Lp(l) => l.resolve_opt(basis),
            _ => None,
        }
    }

    pub fn is_auto(&self) -> bool {
        matches!(self, Size::Auto)
    }
}

/// The used border widths (the computed value is already 0 for `none`
/// and `hidden`).
pub(crate) fn border(style: &ComputedStyle) -> Sides {
    Sides::new(
        px(style, "border-top-width"),
        px(style, "border-right-width"),
        px(style, "border-bottom-width"),
        px(style, "border-left-width"),
    )
}

/// The used paddings; percentages refer to the containing block's width.
pub(crate) fn padding(style: &ComputedStyle, cbw: f64) -> Sides {
    let p = |n: &str| lp(style, n).map_or(0.0, |l| l.resolve(cbw).max(0.0));
    Sides::new(
        p("padding-top"),
        p("padding-right"),
        p("padding-bottom"),
        p("padding-left"),
    )
}

/// The margins, `None` for `auto`; percentages refer to the containing
/// block's width.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Margins {
    pub top: Option<f64>,
    pub right: Option<f64>,
    pub bottom: Option<f64>,
    pub left: Option<f64>,
}

pub(crate) fn margins(style: &ComputedStyle, cbw: f64) -> Margins {
    let m = |n: &str| lp(style, n).map(|l| l.resolve(cbw));
    Margins {
        top: m("margin-top"),
        right: m("margin-right"),
        bottom: m("margin-bottom"),
        left: m("margin-left"),
    }
}

impl Margins {
    /// The margins with `auto` as 0.
    pub fn or_zero(&self) -> Sides {
        Sides::new(
            self.top.unwrap_or(0.0),
            self.right.unwrap_or(0.0),
            self.bottom.unwrap_or(0.0),
            self.left.unwrap_or(0.0),
        )
    }
}

/// The outer display type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Outer {
    Block,
    Inline,
}

/// The inner display type. Tables, flex and grid containers are laid out
/// as flow roots until their own layout modes exist.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Inner {
    Flow,
    FlowRoot,
    Table,
    Flex,
    Grid,
}

/// The used `display`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Display {
    None,
    Contents,
    Box {
        outer: Outer,
        inner: Inner,
        list_item: bool,
    },
}

pub(crate) fn display(style: &ComputedStyle) -> Display {
    let text = kw(style, "display");
    let words: Vec<&str> = text.split_ascii_whitespace().collect();
    let b = |outer, inner| Display::Box {
        outer,
        inner,
        list_item: false,
    };
    match words.as_slice() {
        ["none"] => return Display::None,
        ["contents"] => return Display::Contents,
        ["inline-block"] => return b(Outer::Inline, Inner::FlowRoot),
        ["inline-table"] => return b(Outer::Inline, Inner::Table),
        ["inline-flex"] => return b(Outer::Inline, Inner::Flex),
        ["inline-grid"] => return b(Outer::Inline, Inner::Grid),
        // Table-internal boxes (until table layout) and ruby boxes.
        [w] if w.starts_with("table-") => return b(Outer::Block, Inner::Flow),
        ["ruby-base"] | ["ruby-text"] | ["ruby-base-container"] | ["ruby-text-container"] => {
            return b(Outer::Inline, Inner::Flow);
        }
        _ => {}
    }
    let mut outer = None;
    let mut inner = None;
    let mut list_item = false;
    for w in &words {
        match *w {
            "block" => outer = Some(Outer::Block),
            "inline" => outer = Some(Outer::Inline),
            "run-in" => outer = Some(Outer::Block),
            "flow" => inner = Some(Inner::Flow),
            "flow-root" => inner = Some(Inner::FlowRoot),
            "table" => inner = Some(Inner::Table),
            "flex" => inner = Some(Inner::Flex),
            "grid" => inner = Some(Inner::Grid),
            "ruby" | "math" => inner = Some(Inner::Flow),
            "list-item" => list_item = true,
            _ => {}
        }
    }
    let inner_given = inner.is_some();
    let outer = outer.unwrap_or(if inner_given || list_item {
        Outer::Block
    } else {
        Outer::Inline
    });
    let inner = inner.unwrap_or(Inner::Flow);
    // `display: ruby` and `math` alone are inline-level.
    let outer = if !list_item && words.len() == 1 && (words[0] == "ruby" || words[0] == "math") {
        Outer::Inline
    } else {
        outer
    };
    Display::Box {
        outer,
        inner,
        list_item,
    }
}

/// The used `float`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Float {
    None,
    Left,
    Right,
}

pub(crate) fn float(style: &ComputedStyle) -> Float {
    let rtl = style.rtl();
    match kw(style, "float").as_str() {
        "left" => Float::Left,
        "right" => Float::Right,
        "inline-start" => {
            if rtl {
                Float::Right
            } else {
                Float::Left
            }
        }
        "inline-end" => {
            if rtl {
                Float::Left
            } else {
                Float::Right
            }
        }
        _ => Float::None,
    }
}

/// The used `clear`: (left, right).
pub(crate) fn clear(style: &ComputedStyle) -> (bool, bool) {
    let rtl = style.rtl();
    match kw(style, "clear").as_str() {
        "left" => (true, false),
        "right" => (false, true),
        "both" | "both-inline" => (true, true),
        "inline-start" => (!rtl, rtl),
        "inline-end" => (rtl, !rtl),
        _ => (false, false),
    }
}

/// True for absolutely and fixed positioned boxes.
pub(crate) fn out_of_flow(style: &ComputedStyle) -> bool {
    matches!(kw(style, "position").as_str(), "absolute" | "fixed")
}

/// True if `overflow` makes the box a scroll container (or clips it in
/// both axes as `hidden` does), so it establishes a formatting context.
pub(crate) fn overflow_establishes_bfc(style: &ComputedStyle) -> bool {
    let x = kw(style, "overflow-x");
    let y = kw(style, "overflow-y");
    let scrolls = |v: &str| matches!(v, "hidden" | "scroll" | "auto");
    scrolls(&x) || scrolls(&y)
}

/// True if the content of the box is clipped in the inline direction
/// (`text-overflow` applies).
pub(crate) fn clips_inline(style: &ComputedStyle) -> bool {
    kw(style, "overflow-x") != "visible"
}

/// The `box-sizing` is `border-box`.
pub(crate) fn border_box_sizing(style: &ComputedStyle) -> bool {
    kw(style, "box-sizing") == "border-box"
}

/// The used `aspect-ratio`: (`auto` given, ratio).
pub(crate) fn aspect_ratio(style: &ComputedStyle) -> (bool, Option<f64>) {
    let Some(v) = style.get("aspect-ratio") else {
        return (true, None);
    };
    let Some(values) = v.values.as_deref() else {
        return (true, None);
    };
    let mut auto = false;
    let mut nums: Vec<f64> = Vec::new();
    for x in values {
        match x {
            Value::Keyword(k) if k == "auto" => auto = true,
            Value::Number(n) | Value::Integer(n) => nums.push(*n),
            _ => {}
        }
    }
    let ratio = match nums.as_slice() {
        [w] if *w > 0.0 => Some(*w),
        [w, h] if *w > 0.0 && *h > 0.0 => Some(w / h),
        _ => None,
    };
    (auto || ratio.is_none(), ratio)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lengths_and_percentages_resolve() {
        assert_eq!(Lp::Px(3.0).resolve(100.0), 3.0);
        assert_eq!(Lp::Pct(50.0).resolve(80.0), 40.0);
        assert_eq!(Lp::Pct(50.0).resolve_opt(None), None);
        assert!(Lp::Pct(1.0).has_percent());
    }
}
