//! Computed values (css-cascade-5 §4.4 and the "Computed value" line of
//! each property): relative lengths resolved against the element's font,
//! the root's font and the viewport, absolute dimensions in their
//! canonical unit, math functions simplified as far as their percentages
//! allow, colors and images computed, and the property rules of
//! [`Computed`] (font sizes and weights, line heights, line widths,
//! integers and alpha values). Serialization is the one of specified
//! values applied to the computed list (CSSOM §6.7.2).

use super::calc::{Math, Node, Resolver};
use super::color::{Color, ColorContext};
use super::{Category, Value, grammar, round_half_up, serialize_list, to_canonical, unit};
use crate::css::properties::{self, Computed};
use crate::css::tokenizer::Token;

/// What computing a value needs from the element and the document.
#[derive(Clone, Copy, Debug)]
pub struct Context {
    /// The element's computed `font-size` in pixels.
    pub font_size: f64,
    /// The parent's computed `font-size` (for `font-size` itself).
    pub parent_font_size: f64,
    /// The root element's computed `font-size`.
    pub root_font_size: f64,
    /// The element's computed line height in pixels (`lh`).
    pub line_height: f64,
    /// The root element's computed line height in pixels (`rlh`).
    pub root_line_height: f64,
    /// The viewport's width and height in pixels.
    pub viewport: (f64, f64),
    /// The writing mode is vertical (`vi`, `vb`).
    pub vertical: bool,
    /// The parent's computed `font-weight` (`bolder`, `lighter`).
    pub parent_font_weight: f64,
    /// `currentcolor` and the color scheme.
    pub color: ColorContext,
}

impl Default for Context {
    /// The initial context: 16px fonts, `normal` line heights (1.2) and
    /// the 800×600 viewport of the contract (§3.5).
    fn default() -> Context {
        Context {
            font_size: 16.0,
            parent_font_size: 16.0,
            root_font_size: 16.0,
            line_height: 19.2,
            root_line_height: 19.2,
            viewport: (800.0, 600.0),
            vertical: false,
            parent_font_weight: 400.0,
            color: ColorContext::default(),
        }
    }
}

/// Font metrics vidrio uses before fonts are loaded: the fallbacks of
/// css-values-4 §6.1 for `ex` and `ch` (0.5em) and `ic` (1em), and 0.7em
/// for `cap` (LIMITATIONS.md).
const EX: f64 = 0.5;
const CH: f64 = 0.5;
const CAP: f64 = 0.7;
const IC: f64 = 1.0;

impl Context {
    /// `value` of length unit `unit_name` in pixels.
    pub fn length(&self, value: f64, unit_name: &str) -> Option<f64> {
        if let Some((px, "px")) = to_canonical(value, unit_name) {
            return Some(px);
        }
        let u = unit(unit_name)?;
        if u.category != Category::Length {
            return None;
        }
        let em = self.font_size;
        let rem = self.root_font_size;
        let (w, h) = self.viewport;
        let (inline, block) = if self.vertical { (h, w) } else { (w, h) };
        let factor = match u.name {
            "em" => em,
            "ex" => em * EX,
            "ch" => em * CH,
            "cap" => em * CAP,
            "ic" => em * IC,
            "lh" => self.line_height,
            "rem" => rem,
            "rex" => rem * EX,
            "rch" => rem * CH,
            "rcap" => rem * CAP,
            "ric" => rem * IC,
            "rlh" => self.root_line_height,
            // Container units fall back to the small viewport when there
            // is no query container (css-contain-3 §6).
            "vw" | "svw" | "lvw" | "dvw" | "cqw" => w / 100.0,
            "vh" | "svh" | "lvh" | "dvh" | "cqh" => h / 100.0,
            "vi" | "svi" | "lvi" | "dvi" | "cqi" => inline / 100.0,
            "vb" | "svb" | "lvb" | "dvb" | "cqb" => block / 100.0,
            "vmin" | "svmin" | "lvmin" | "dvmin" | "cqmin" => w.min(h) / 100.0,
            "vmax" | "svmax" | "lvmax" | "dvmax" | "cqmax" => w.max(h) / 100.0,
            _ => return None,
        };
        Some(value * factor)
    }

    /// The context with `em` relative to the parent (for `font-size`).
    fn for_font_size(&self) -> Context {
        Context {
            font_size: self.parent_font_size,
            ..*self
        }
    }
}

fn leaf_value(n: &Node) -> Option<Value> {
    Some(match n {
        Node::Number(v) => Value::Number(*v),
        Node::Percentage(v) => Value::Percentage(*v),
        Node::Dimension(v, u) => Value::Dimension(*v, u.clone()),
        _ => return None,
    })
}

/// Resolves a math function with `ctx` (and percentages against `basis`
/// in pixels, when known): a single value when it simplifies to one,
/// otherwise the function with its simplified tree.
pub fn compute_math(m: &Math, ctx: &Context, basis: Option<f64>) -> Value {
    let relative = |v: f64, u: &str| ctx.length(v, u);
    let r = Resolver {
        relative: Some(&relative),
        percent: basis.map(|b| (b, Category::Length)),
        symbol: None,
    };
    let root = m.resolve(&r);
    match leaf_value(&root) {
        Some(v) => v,
        None => Value::Math(Box::new(m.with_root(root))),
    }
}

/// The computed form of one value: lengths in pixels, other dimensions
/// in their canonical unit, math functions resolved, colors and images
/// computed.
pub fn compute_value(v: &Value, ctx: &Context) -> Value {
    match v {
        Value::Dimension(n, u) => match unit(u) {
            Some(info) if info.category == Category::Length => match ctx.length(*n, u) {
                Some(px) => Value::Dimension(px, "px".to_string()),
                None => v.clone(),
            },
            _ => match to_canonical(*n, u) {
                Some((c, cu)) => Value::Dimension(c, cu.to_string()),
                None => v.clone(),
            },
        },
        Value::Math(m) => compute_math(m, ctx, None),
        Value::Color(c) => Value::Color(Box::new(c.compute(&ctx.color))),
        Value::Image(i) => {
            let value = |x: &Value| compute_value(x, ctx);
            let color = |c: &Color| c.compute(&ctx.color);
            Value::Image(Box::new(i.map(&value, &color)))
        }
        Value::Function(name, args) => Value::Function(
            name.clone(),
            args.iter().map(|a| compute_value(a, ctx)).collect(),
        ),
        Value::Block(items) => Value::Block(items.iter().map(|a| compute_value(a, ctx)).collect()),
        _ => v.clone(),
    }
}

/// The computed values of a list.
pub fn compute_values(values: &[Value], ctx: &Context) -> Vec<Value> {
    values.iter().map(|v| compute_value(v, ctx)).collect()
}

fn number_of(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) | Value::Integer(n) => Some(*n),
        _ => None,
    }
}

/// The font size in pixels of an `<absolute-size>` keyword (medium is
/// 16px; the table of the reference browsers).
pub fn absolute_size(keyword: &str) -> Option<f64> {
    Some(match keyword {
        "xx-small" => 9.0,
        "x-small" => 10.0,
        "small" => 13.0,
        "medium" => 16.0,
        "large" => 18.0,
        "x-large" => 24.0,
        "xx-large" => 32.0,
        "xxx-large" => 48.0,
        _ => return None,
    })
}

/// The weight `bolder` or `lighter` gives relative to `parent` (CSS
/// Fonts 4 §2.2).
pub fn relative_weight(parent: f64, bolder: bool) -> f64 {
    if bolder {
        if parent < 350.0 {
            400.0
        } else if parent < 550.0 {
            700.0
        } else if parent < 900.0 {
            900.0
        } else {
            parent
        }
    } else if parent < 100.0 {
        parent
    } else if parent < 550.0 {
        100.0
    } else if parent < 750.0 {
        400.0
    } else {
        700.0
    }
}

/// The percentage of a `font-stretch` keyword.
pub fn stretch_percentage(keyword: &str) -> Option<f64> {
    Some(match keyword {
        "ultra-condensed" => 50.0,
        "extra-condensed" => 62.5,
        "condensed" => 75.0,
        "semi-condensed" => 87.5,
        "normal" => 100.0,
        "semi-expanded" => 112.5,
        "expanded" => 125.0,
        "extra-expanded" => 150.0,
        "ultra-expanded" => 200.0,
        _ => return None,
    })
}

/// The computed value of property `name` from its specified values.
pub fn compute(name: &str, values: &[Value], ctx: &Context) -> Vec<Value> {
    let rule = properties::property(name).map_or(Computed::Generic, |p| p.computed);
    match rule {
        Computed::FontSize => {
            let parent = ctx.parent_font_size;
            let fctx = ctx.for_font_size();
            let px = match values {
                [Value::Keyword(k)] => match k.as_str() {
                    "larger" => Some(parent * 1.2),
                    "smaller" => Some(parent / 1.2),
                    "math" => Some(parent),
                    other => absolute_size(other),
                },
                [Value::Percentage(p)] => Some(parent * p / 100.0),
                [Value::Dimension(n, u)] => fctx.length(*n, u),
                [Value::Math(m)] => match compute_math(m, &fctx, Some(parent)) {
                    Value::Dimension(v, _) => Some(v.max(0.0)),
                    _ => None,
                },
                _ => None,
            };
            match px {
                Some(px) => vec![Value::Dimension(px, "px".to_string())],
                None => compute_values(values, &fctx),
            }
        }
        Computed::FontWeight => match values {
            [Value::Keyword(k)] => {
                let w = match k.as_str() {
                    "normal" => 400.0,
                    "bold" => 700.0,
                    "bolder" => relative_weight(ctx.parent_font_weight, true),
                    _ => relative_weight(ctx.parent_font_weight, false),
                };
                vec![Value::Number(w)]
            }
            _ => compute_values(values, ctx)
                .into_iter()
                .map(|v| match v {
                    Value::Number(n) => Value::Number(n.clamp(1.0, 1000.0)),
                    other => other,
                })
                .collect(),
        },
        Computed::FontStretch => match values {
            [Value::Keyword(k)] => match stretch_percentage(k) {
                Some(p) => vec![Value::Percentage(p)],
                None => values.to_vec(),
            },
            _ => compute_values(values, ctx),
        },
        Computed::LineHeight => match values {
            [Value::Percentage(p)] => {
                vec![Value::Dimension(
                    ctx.font_size * p / 100.0,
                    "px".to_string(),
                )]
            }
            [Value::Math(m)] => match compute_math(m, ctx, Some(ctx.font_size)) {
                Value::Dimension(v, u) => vec![Value::Dimension(v.max(0.0), u)],
                Value::Number(v) => vec![Value::Number(v.max(0.0))],
                other => vec![other],
            },
            _ => compute_values(values, ctx),
        },
        Computed::LineWidth => values
            .iter()
            .map(|v| match v.keyword() {
                Some("thin") => Value::Dimension(1.0, "px".to_string()),
                Some("medium") => Value::Dimension(3.0, "px".to_string()),
                Some("thick") => Value::Dimension(5.0, "px".to_string()),
                _ => match compute_value(v, ctx) {
                    Value::Dimension(n, u) => Value::Dimension(n.max(0.0), u),
                    other => other,
                },
            })
            .collect(),
        Computed::Integer => compute_values(values, ctx)
            .into_iter()
            .map(|v| match v {
                Value::Number(n) => Value::Integer(round_half_up(n)),
                other => other,
            })
            .collect(),
        Computed::AlphaValue => compute_values(values, ctx)
            .into_iter()
            .map(|v| match v {
                Value::Number(n) => Value::Number(n.clamp(0.0, 1.0)),
                Value::Percentage(p) => Value::Number((p / 100.0).clamp(0.0, 1.0)),
                other => other,
            })
            .collect(),
        Computed::Number => compute_values(values, ctx)
            .into_iter()
            .map(|v| match number_of(&v) {
                Some(n) => Value::Number(n),
                None => v,
            })
            .collect(),
        Computed::Generic
        | Computed::Length
        | Computed::LengthPercentage
        | Computed::Color
        | Computed::Image => compute_values(values, ctx),
    }
}

/// The serialization of the computed value of `name` from its tokens,
/// or `None` if the property has no grammar here or the value does not
/// match it.
pub fn computed_text(name: &str, tokens: &[Token], ctx: &Context) -> Option<String> {
    let values = grammar::parse_property(name, tokens)??;
    Some(serialize_list(&compute(name, &values, ctx)))
}

/// True if a math function's tree uses no relative unit.
pub fn math_is_absolute(n: &Node) -> bool {
    match n {
        Node::Dimension(_, u) => unit(u).is_some_and(|i| i.relative == super::Relative::None),
        Node::Sum(c) | Node::Product(c) | Node::Func(_, c) => c.iter().all(math_is_absolute),
        Node::Negate(c) | Node::Invert(c) => math_is_absolute(c),
        _ => true,
    }
}

/// True if the values use no relative unit (computationally independent
/// in the sense of css-properties-values-api §2.4, `var()` aside).
pub fn is_computationally_independent(values: &[Value]) -> bool {
    values.iter().all(|v| match v {
        Value::Dimension(_, u) => unit(u).is_some_and(|i| i.relative == super::Relative::None),
        Value::Math(m) => math_is_absolute(&m.root),
        Value::Function(_, a) | Value::Block(a) => is_computationally_independent(a),
        _ => true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::css::tokenizer::tokenize;

    fn computed(name: &str, value: &str, ctx: &Context) -> Option<String> {
        computed_text(name, &tokenize(value), ctx)
    }

    #[test]
    fn lengths_resolve_against_fonts_and_the_viewport() {
        let ctx = Context {
            font_size: 20.0,
            ..Context::default()
        };
        assert_eq!(computed("width", "2em", &ctx).as_deref(), Some("40px"));
        assert_eq!(computed("width", "10vw", &ctx).as_deref(), Some("80px"));
        assert_eq!(computed("width", "10dvh", &ctx).as_deref(), Some("60px"));
        assert_eq!(computed("width", "1rlh", &ctx).as_deref(), Some("19.2px"));
        assert_eq!(
            computed("width", "calc(10% + 1em)", &ctx).as_deref(),
            Some("calc(10% + 20px)")
        );
        assert_eq!(
            computed("transition-duration", "500ms", &ctx).as_deref(),
            Some("0.5s")
        );
    }

    #[test]
    fn property_rules() {
        let ctx = Context {
            parent_font_size: 10.0,
            parent_font_weight: 700.0,
            ..Context::default()
        };
        assert_eq!(computed("font-size", "150%", &ctx).as_deref(), Some("15px"));
        assert_eq!(computed("font-size", "2em", &ctx).as_deref(), Some("20px"));
        assert_eq!(
            computed("font-size", "large", &ctx).as_deref(),
            Some("18px")
        );
        assert_eq!(
            computed("font-weight", "bolder", &ctx).as_deref(),
            Some("900")
        );
        assert_eq!(
            computed("line-height", "150%", &ctx).as_deref(),
            Some("24px")
        );
        assert_eq!(
            computed("border-top-width", "thick", &ctx).as_deref(),
            Some("5px")
        );
        assert_eq!(computed("z-index", "calc(2.5)", &ctx).as_deref(), Some("3"));
        assert_eq!(computed("opacity", "50%", &ctx).as_deref(), Some("0.5"));
        assert_eq!(
            computed("background-color", "red", &ctx).as_deref(),
            Some("rgb(255, 0, 0)")
        );
    }
}
