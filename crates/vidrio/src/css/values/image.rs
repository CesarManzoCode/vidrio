//! Images of css-images-3/4 as values: `url()` and the gradient
//! functions `linear-gradient()`, `radial-gradient()`, `conic-gradient()`
//! and their `repeating-` forms, with color interpolation methods, color
//! stops with one or two positions and color hints.

use super::color::{self, Color, Interpolation};
use super::grammar::match_production;
use super::{Category, Numeric, Value, calc, is_ident, parse_numeric, parse_url, serialize_list};
use crate::css::parser::ComponentValue;
use crate::css::tokenizer::Token;

/// The kinds of gradient.
#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    /// `linear-gradient()` with its direction, if written.
    Linear(Option<Direction>),
    /// `radial-gradient()`: shape keyword, size (an extent keyword or one
    /// or two lengths) and position, as written.
    Radial {
        shape: Option<String>,
        size: Vec<Value>,
        position: Option<Vec<Value>>,
    },
    /// `conic-gradient()`: the starting angle and position.
    Conic {
        from: Option<Value>,
        position: Option<Vec<Value>>,
    },
}

/// The direction of a linear gradient.
#[derive(Clone, Debug, PartialEq)]
pub enum Direction {
    Angle(Value),
    /// `to` with one or two side keywords.
    To(Vec<String>),
}

/// An item of a color stop list.
#[derive(Clone, Debug, PartialEq)]
pub enum Stop {
    /// A color with zero, one or two positions.
    Color(Color, Vec<Value>),
    /// A color hint.
    Hint(Value),
}

/// A gradient image.
#[derive(Clone, Debug, PartialEq)]
pub struct Image {
    pub kind: Kind,
    pub repeating: bool,
    pub interpolation: Option<Interpolation>,
    pub stops: Vec<Stop>,
}

/// Parses an `<image>`: a URL or a gradient.
pub fn parse(cv: &ComponentValue) -> Option<Value> {
    if let Some(url) = parse_url(cv) {
        return Some(url);
    }
    let ComponentValue::Function(name, args) = cv else {
        return None;
    };
    let lower = name.to_ascii_lowercase();
    let (repeating, base) = match lower.strip_prefix("repeating-") {
        Some(rest) => (true, rest),
        None => (false, lower.as_str()),
    };
    let sig: Vec<&ComponentValue> = args.iter().filter(|c| !c.is_whitespace()).collect();
    let mut parts: Vec<&[&ComponentValue]> = Vec::new();
    let mut start = 0;
    for (i, c) in sig.iter().enumerate() {
        if matches!(c, ComponentValue::Token(Token::Comma)) {
            parts.push(&sig[start..i]);
            start = i + 1;
        }
    }
    parts.push(&sig[start..]);
    let image = match base {
        "linear-gradient" => gradient(&parts, repeating, Shape::Linear)?,
        "radial-gradient" => gradient(&parts, repeating, Shape::Radial)?,
        "conic-gradient" => gradient(&parts, repeating, Shape::Conic)?,
        _ => return None,
    };
    Some(Value::Image(Box::new(image)))
}

#[derive(Clone, Copy, PartialEq)]
enum Shape {
    Linear,
    Radial,
    Conic,
}

/// Splits `in <space> [<hue> hue]?` off the start or the end of a
/// prelude.
fn split_interpolation<'a, 'b>(
    v: &'b [&'a ComponentValue],
) -> Option<(Option<Interpolation>, Vec<&'a ComponentValue>)> {
    let Some(i) = v.iter().position(|c| is_ident(c, "in")) else {
        return Some((None, v.to_vec()));
    };
    for len in [4, 2] {
        if i + len > v.len() {
            continue;
        }
        let at_start = i == 0;
        let at_end = i + len == v.len();
        if !at_start && !at_end {
            continue;
        }
        if let Some(method) = color::parse_interpolation(&v[i..i + len]) {
            let mut rest = v[..i].to_vec();
            rest.extend_from_slice(&v[i + len..]);
            return Some((Some(method), rest));
        }
    }
    None
}

fn single_position(v: &[&ComponentValue]) -> Option<Vec<Value>> {
    let owned: Vec<ComponentValue> = v.iter().map(|c| (*c).clone()).collect();
    match_production("position", &owned)
}

/// The prelude of a linear gradient: a direction.
fn linear_prelude(v: &[&ComponentValue]) -> Option<Option<Direction>> {
    match v {
        [] => Some(None),
        [c] => {
            if let ComponentValue::Token(Token::Number(n)) = c {
                return (n.value == 0.0)
                    .then(|| Some(Direction::Angle(Value::Dimension(0.0, "deg".into()))));
            }
            parse_numeric(c, Numeric::Angle, None).map(|a| Some(Direction::Angle(a)))
        }
        [to, sides @ ..] if is_ident(to, "to") && (1..=2).contains(&sides.len()) => {
            let mut words = Vec::new();
            for s in sides {
                let w = s.ident()?.to_ascii_lowercase();
                if !matches!(w.as_str(), "left" | "right" | "top" | "bottom") {
                    return None;
                }
                words.push(w);
            }
            if words.len() == 2 {
                let horizontal = |w: &str| w == "left" || w == "right";
                if horizontal(&words[0]) == horizontal(&words[1]) {
                    return None;
                }
            }
            Some(Some(Direction::To(words)))
        }
        _ => None,
    }
}

type RadialPrelude = (Option<String>, Vec<Value>, Option<Vec<Value>>);

/// The prelude of a radial gradient: shape and size in any order, then
/// `at <position>`.
fn radial_prelude(v: &[&ComponentValue]) -> Option<RadialPrelude> {
    let at = v.iter().position(|c| is_ident(c, "at"));
    let (head, position) = match at {
        Some(i) => (&v[..i], Some(single_position(&v[i + 1..])?)),
        None => (v, None),
    };
    let mut shape: Option<String> = None;
    let mut size: Vec<Value> = Vec::new();
    let mut extent = false;
    for c in head {
        if let Some(id) = c.ident() {
            let w = id.to_ascii_lowercase();
            match w.as_str() {
                "circle" | "ellipse" if shape.is_none() => shape = Some(w),
                "closest-side" | "closest-corner" | "farthest-side" | "farthest-corner"
                    if size.is_empty() =>
                {
                    size.push(Value::Keyword(w));
                    extent = true;
                }
                _ => return None,
            }
            continue;
        }
        if extent || size.len() == 2 {
            return None;
        }
        size.push(parse_numeric(
            c,
            Numeric::LengthPercentage,
            Some((0.0, f64::INFINITY)),
        )?);
    }
    // A shape keyword may not split the size.
    if size.len() == 2 {
        let idx: Vec<usize> = head
            .iter()
            .enumerate()
            .filter(|(_, c)| c.ident().is_none())
            .map(|(i, _)| i)
            .collect();
        if idx.len() == 2 && idx[1] != idx[0] + 1 {
            return None;
        }
    }
    let is_percent = |v: &Value| match v {
        Value::Percentage(_) => true,
        Value::Math(m) => m.ty != calc::Type::base(Category::Length),
        _ => false,
    };
    match (shape.as_deref(), size.as_slice()) {
        (Some("circle"), [one]) if is_percent(one) => return None,
        (Some("circle"), [_, _]) => return None,
        (Some("ellipse"), [one]) if !extent && !is_percent(one) => return None,
        (None, [one]) if !extent && is_percent(one) => return None,
        _ => {}
    }
    if shape.is_none() && size.is_empty() && position.is_none() && !head.is_empty() {
        return None;
    }
    Some((shape, size, position))
}

/// The prelude of a conic gradient: `from <angle>` and `at <position>`.
fn conic_prelude(v: &[&ComponentValue]) -> Option<(Option<Value>, Option<Vec<Value>>)> {
    let mut rest = v;
    let mut from = None;
    if rest.first().is_some_and(|c| is_ident(c, "from")) {
        let c = rest.get(1)?;
        from = Some(match c {
            ComponentValue::Token(Token::Number(n)) if n.value == 0.0 => {
                Value::Dimension(0.0, "deg".into())
            }
            _ => parse_numeric(c, Numeric::Angle, None)?,
        });
        rest = &rest[2..];
    }
    let position = match rest {
        [] => None,
        [at, pos @ ..] if is_ident(at, "at") => Some(single_position(pos)?),
        _ => return None,
    };
    Some((from, position))
}

fn stop_position(c: &ComponentValue, shape: Shape) -> Option<Value> {
    if shape == Shape::Conic {
        if let ComponentValue::Token(Token::Number(n)) = c {
            return (n.value == 0.0).then(|| Value::Dimension(0.0, "deg".into()));
        }
        parse_numeric(c, Numeric::AnglePercentage, None)
    } else {
        parse_numeric(c, Numeric::LengthPercentage, None)
    }
}

fn gradient(parts: &[&[&ComponentValue]], repeating: bool, shape: Shape) -> Option<Image> {
    let first = parts.first()?;
    let prelude = split_interpolation(first).and_then(|(interp, rest)| {
        let kind = match shape {
            Shape::Linear => Kind::Linear(linear_prelude(&rest)?),
            Shape::Radial => {
                let (s, size, position) = radial_prelude(&rest)?;
                Kind::Radial {
                    shape: s,
                    size,
                    position,
                }
            }
            Shape::Conic => {
                let (from, position) = conic_prelude(&rest)?;
                Kind::Conic { from, position }
            }
        };
        // A prelude must say something.
        (interp.is_some() || !rest.is_empty()).then_some((interp, kind))
    });
    let (interpolation, kind, stop_parts) = match prelude {
        Some((interp, kind)) => (interp, kind, &parts[1..]),
        None => {
            let kind = match shape {
                Shape::Linear => Kind::Linear(None),
                Shape::Radial => Kind::Radial {
                    shape: None,
                    size: Vec::new(),
                    position: None,
                },
                Shape::Conic => Kind::Conic {
                    from: None,
                    position: None,
                },
            };
            (None, kind, parts)
        }
    };
    let mut stops = Vec::new();
    for part in stop_parts {
        let stop = match part {
            [c] => match color::parse(c) {
                Some(color) => Stop::Color(color, Vec::new()),
                None => Stop::Hint(stop_position(c, shape)?),
            },
            [a, rest @ ..] if rest.len() <= 2 => match color::parse(a) {
                Some(color) => {
                    let positions: Option<Vec<Value>> =
                        rest.iter().map(|c| stop_position(c, shape)).collect();
                    Stop::Color(color, positions?)
                }
                None if rest.len() == 1 => {
                    Stop::Color(color::parse(rest[0])?, vec![stop_position(a, shape)?])
                }
                None => return None,
            },
            _ => return None,
        };
        stops.push(stop);
    }
    let colors = stops
        .iter()
        .filter(|s| matches!(s, Stop::Color(..)))
        .count();
    if colors < 2 {
        return None;
    }
    // Hints sit between two color stops.
    for (i, s) in stops.iter().enumerate() {
        if matches!(s, Stop::Hint(_))
            && (i == 0
                || i == stops.len() - 1
                || matches!(stops[i - 1], Stop::Hint(_))
                || matches!(stops[i + 1], Stop::Hint(_)))
        {
            return None;
        }
    }
    Some(Image {
        kind,
        repeating,
        interpolation,
        stops,
    })
}

impl Image {
    /// Serializes the gradient.
    pub fn to_css(&self) -> String {
        let name = match self.kind {
            Kind::Linear(_) => "linear-gradient",
            Kind::Radial { .. } => "radial-gradient",
            Kind::Conic { .. } => "conic-gradient",
        };
        let mut prelude: Vec<String> = Vec::new();
        match &self.kind {
            Kind::Linear(Some(Direction::Angle(a))) => prelude.push(a.to_css()),
            Kind::Linear(Some(Direction::To(words))) => {
                prelude.push(format!("to {}", words.join(" ")))
            }
            Kind::Linear(None) => {}
            Kind::Radial {
                shape,
                size,
                position,
            } => {
                if let Some(s) = shape {
                    prelude.push(s.clone());
                }
                if !size.is_empty() {
                    prelude.push(serialize_list(size));
                }
                if let Some(p) = position {
                    prelude.push(format!("at {}", serialize_list(p)));
                }
            }
            Kind::Conic { from, position } => {
                if let Some(f) = from {
                    prelude.push(format!("from {}", f.to_css()));
                }
                if let Some(p) = position {
                    prelude.push(format!("at {}", serialize_list(p)));
                }
            }
        }
        if let Some(i) = &self.interpolation {
            prelude.push(i.to_css());
        }
        let mut items: Vec<String> = Vec::new();
        if !prelude.is_empty() {
            items.push(prelude.join(" "));
        }
        for s in &self.stops {
            items.push(match s {
                Stop::Color(c, positions) => {
                    let mut t = c.to_css();
                    for p in positions {
                        t.push(' ');
                        t.push_str(&p.to_css());
                    }
                    t
                }
                Stop::Hint(h) => h.to_css(),
            });
        }
        let prefix = if self.repeating { "repeating-" } else { "" };
        format!("{}{}({})", prefix, name, items.join(", "))
    }

    /// The gradient with its colors and numeric values mapped (computed
    /// values).
    pub fn map(&self, value: &dyn Fn(&Value) -> Value, color: &dyn Fn(&Color) -> Color) -> Image {
        let list = |v: &Vec<Value>| v.iter().map(value).collect::<Vec<_>>();
        let kind = match &self.kind {
            Kind::Linear(Some(Direction::Angle(a))) => {
                Kind::Linear(Some(Direction::Angle(value(a))))
            }
            Kind::Linear(d) => Kind::Linear(d.clone()),
            Kind::Radial {
                shape,
                size,
                position,
            } => Kind::Radial {
                shape: shape.clone(),
                size: list(size),
                position: position.as_ref().map(list),
            },
            Kind::Conic { from, position } => Kind::Conic {
                from: from.as_ref().map(value),
                position: position.as_ref().map(list),
            },
        };
        let stops = self
            .stops
            .iter()
            .map(|s| match s {
                Stop::Color(c, p) => Stop::Color(color(c), list(p)),
                Stop::Hint(h) => Stop::Hint(value(h)),
            })
            .collect();
        Image {
            kind,
            repeating: self.repeating,
            interpolation: self.interpolation,
            stops,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::css::parser::parse_component_values;

    fn css(text: &str) -> Option<String> {
        let cv = parse_component_values(text);
        parse(&cv[0]).map(|v| v.to_css())
    }

    #[test]
    fn gradients_parse_and_serialize() {
        assert_eq!(
            css("linear-gradient(to RIGHT, red, blue 50%)").as_deref(),
            Some("linear-gradient(to right, red, blue 50%)")
        );
        assert_eq!(
            css("repeating-linear-gradient(45deg in oklch longer hue, red 0 10px, blue)")
                .as_deref(),
            Some("repeating-linear-gradient(45deg in oklch longer hue, red 0px 10px, blue)")
        );
        assert_eq!(
            css("radial-gradient(10px circle at top left, red, 30%, blue)").as_deref(),
            Some("radial-gradient(circle 10px at left top, red, 30%, blue)")
        );
        assert_eq!(
            css("conic-gradient(from 90deg at 10% 20%, red, blue)").as_deref(),
            Some("conic-gradient(from 90deg at 10% 20%, red, blue)")
        );
        assert_eq!(css("linear-gradient(red)"), None);
        assert_eq!(css("linear-gradient(red, 10%, 20%, blue)"), None);
        assert_eq!(css("radial-gradient(circle 10% 20%, red, blue)"), None);
        assert_eq!(css("linear-gradient(to left right, red, blue)"), None);
    }
}
