//! Media Queries Level 4 and 5 (the features vidrio knows): parsing a
//! media query list with the range syntax, its serialization (CSSOM
//! §4.1–§4.2) and its evaluation against an [`Environment`] (the
//! viewport). An invalid media query is kept as `not all`.

use super::declaration::{serialize_identifier, serialize_tokens};
use super::parser::{
    ComponentValue, non_whitespace, parse_component_values, split_commas, to_tokens, trim,
};
use super::tokenizer::Token;

/// What media queries are evaluated against.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Environment {
    /// The viewport size in CSS pixels.
    pub width: f64,
    pub height: f64,
    /// Device pixels per CSS pixel.
    pub resolution: f64,
    pub scripting: bool,
}

impl Default for Environment {
    /// The viewport wptrunner uses (vault/CONTRACT.md §3.5).
    fn default() -> Environment {
        Environment {
            width: 800.0,
            height: 600.0,
            resolution: 1.0,
            scripting: true,
        }
    }
}

/// A media query list.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MediaQueryList {
    pub queries: Vec<MediaQuery>,
}

/// A media query; `None` stands for an invalid one (`not all`).
#[derive(Clone, Debug, PartialEq)]
pub struct MediaQuery(pub Option<Query>);

/// A valid media query.
#[derive(Clone, Debug, PartialEq)]
pub struct Query {
    pub qualifier: Option<Qualifier>,
    /// The media type, lowercase (`None`: no type, as `all`).
    pub media_type: Option<String>,
    pub condition: Option<Condition>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Qualifier {
    Not,
    Only,
}

/// A media condition.
#[derive(Clone, Debug, PartialEq)]
pub enum Condition {
    Not(Box<Condition>),
    And(Vec<Condition>),
    Or(Vec<Condition>),
    Feature(Feature),
    /// `<general-enclosed>`: always false; kept as its text.
    General(String),
}

/// A comparison of the range syntax.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cmp {
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
}

impl Cmp {
    fn as_str(self) -> &'static str {
        match self {
            Cmp::Lt => "<",
            Cmp::Le => "<=",
            Cmp::Gt => ">",
            Cmp::Ge => ">=",
            Cmp::Eq => "=",
        }
    }

    /// The comparison with its sides swapped.
    fn flip(self) -> Cmp {
        match self {
            Cmp::Lt => Cmp::Gt,
            Cmp::Le => Cmp::Ge,
            Cmp::Gt => Cmp::Lt,
            Cmp::Ge => Cmp::Le,
            Cmp::Eq => Cmp::Eq,
        }
    }
}

/// A media feature.
#[derive(Clone, Debug, PartialEq)]
pub enum Feature {
    /// `(name)`.
    Boolean(String),
    /// `(name: value)`, `min-`/`max-` prefixes included in `name`.
    Plain(String, Value),
    /// `(value op name op value)`: the left and the right comparisons.
    Range {
        name: String,
        left: Option<(Value, Cmp)>,
        right: Option<(Cmp, Value)>,
    },
}

/// A media feature value.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// A length in CSS pixels, with its text.
    Length(f64, String),
    Number(f64, String),
    /// `a / b`.
    Ratio(f64, f64, String),
    /// Dots per CSS pixel, with its text.
    Resolution(f64, String),
    Ident(String),
}

impl Value {
    fn text(&self) -> String {
        match self {
            Value::Length(_, t)
            | Value::Number(_, t)
            | Value::Ratio(_, _, t)
            | Value::Resolution(_, t) => t.clone(),
            Value::Ident(s) => serialize_identifier(s),
        }
    }
}

/// How a feature is written and compared.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FeatureType {
    Length,
    Ratio,
    Resolution,
    Integer,
    /// A discrete feature with its keywords.
    Discrete(&'static [&'static str]),
}

const FEATURES: &[(&str, FeatureType)] = &[
    ("width", FeatureType::Length),
    ("height", FeatureType::Length),
    ("device-width", FeatureType::Length),
    ("device-height", FeatureType::Length),
    ("aspect-ratio", FeatureType::Ratio),
    ("device-aspect-ratio", FeatureType::Ratio),
    ("resolution", FeatureType::Resolution),
    ("color", FeatureType::Integer),
    ("color-index", FeatureType::Integer),
    ("monochrome", FeatureType::Integer),
    ("grid", FeatureType::Discrete(&[])),
    (
        "orientation",
        FeatureType::Discrete(&["portrait", "landscape"]),
    ),
    ("scan", FeatureType::Discrete(&["interlace", "progressive"])),
    ("update", FeatureType::Discrete(&["none", "slow", "fast"])),
    ("hover", FeatureType::Discrete(&["none", "hover"])),
    ("any-hover", FeatureType::Discrete(&["none", "hover"])),
    (
        "pointer",
        FeatureType::Discrete(&["none", "coarse", "fine"]),
    ),
    (
        "any-pointer",
        FeatureType::Discrete(&["none", "coarse", "fine"]),
    ),
    (
        "prefers-color-scheme",
        FeatureType::Discrete(&["light", "dark"]),
    ),
    (
        "prefers-reduced-motion",
        FeatureType::Discrete(&["no-preference", "reduce"]),
    ),
    (
        "prefers-reduced-transparency",
        FeatureType::Discrete(&["no-preference", "reduce"]),
    ),
    (
        "prefers-reduced-data",
        FeatureType::Discrete(&["no-preference", "reduce"]),
    ),
    (
        "prefers-contrast",
        FeatureType::Discrete(&["no-preference", "less", "more", "custom"]),
    ),
    ("forced-colors", FeatureType::Discrete(&["none", "active"])),
    (
        "inverted-colors",
        FeatureType::Discrete(&["none", "inverted"]),
    ),
    (
        "dynamic-range",
        FeatureType::Discrete(&["standard", "high"]),
    ),
    (
        "video-dynamic-range",
        FeatureType::Discrete(&["standard", "high"]),
    ),
    (
        "color-gamut",
        FeatureType::Discrete(&["srgb", "p3", "rec2020"]),
    ),
    (
        "display-mode",
        FeatureType::Discrete(&[
            "fullscreen",
            "standalone",
            "minimal-ui",
            "browser",
            "picture-in-picture",
        ]),
    ),
    (
        "overflow-block",
        FeatureType::Discrete(&["none", "scroll", "paged"]),
    ),
    (
        "overflow-inline",
        FeatureType::Discrete(&["none", "scroll"]),
    ),
    (
        "scripting",
        FeatureType::Discrete(&["none", "initial-only", "enabled"]),
    ),
];

fn feature_type(name: &str) -> Option<FeatureType> {
    FEATURES.iter().find(|(n, _)| *n == name).map(|(_, t)| *t)
}

/// The media types; the deprecated ones parse and never match.
const MEDIA_TYPES: &[&str] = &[
    "all",
    "print",
    "screen",
    "tty",
    "tv",
    "projection",
    "handheld",
    "braille",
    "embossed",
    "aural",
    "speech",
];

// ----- parsing ---------------------------------------------------------------------

/// Parses a media query list from text.
pub fn parse_media_query_list(text: &str) -> MediaQueryList {
    parse_media_query_list_values(&parse_component_values(text))
}

/// Parses a media query list from component values (an `@media` or
/// `@import` prelude).
pub fn parse_media_query_list_values(values: &[ComponentValue]) -> MediaQueryList {
    if trim(values).is_empty() {
        return MediaQueryList::default();
    }
    MediaQueryList {
        queries: split_commas(values)
            .into_iter()
            .map(|q| MediaQuery(parse_query(q)))
            .collect(),
    }
}

/// Parses one media query (`appendMedium`, `deleteMedium`); `None` if it
/// is not one.
pub fn parse_media_query(text: &str) -> Option<MediaQuery> {
    let values = parse_component_values(text);
    if split_commas(&values).len() != 1 {
        return None;
    }
    Some(MediaQuery(parse_query(&values)))
}

fn ident_eq(v: &ComponentValue, word: &str) -> bool {
    v.ident().is_some_and(|s| s.eq_ignore_ascii_case(word))
}

fn parse_query(values: &[ComponentValue]) -> Option<Query> {
    let v = non_whitespace(values);
    if v.is_empty() {
        return None;
    }
    // `<media-condition>` alone.
    if !matches!(v[0], ComponentValue::Token(Token::Ident(_))) || ident_eq(v[0], "not") {
        if let Some(c) = parse_condition(&v, true) {
            return Some(Query {
                qualifier: None,
                media_type: None,
                condition: Some(c),
            });
        }
        if !ident_eq(v[0], "not") {
            return None;
        }
    }
    let mut i = 0;
    let mut qualifier = None;
    if ident_eq(v[0], "not") {
        qualifier = Some(Qualifier::Not);
        i = 1;
    } else if ident_eq(v[0], "only") {
        qualifier = Some(Qualifier::Only);
        i = 1;
    }
    let media_type = v.get(i)?.ident()?.to_ascii_lowercase();
    if ["only", "not", "and", "or", "layer"].contains(&media_type.as_str()) {
        return None;
    }
    i += 1;
    if i == v.len() {
        return Some(Query {
            qualifier,
            media_type: Some(media_type),
            condition: None,
        });
    }
    if !ident_eq(v[i], "and") {
        return None;
    }
    let condition = parse_condition(&v[i + 1..], false)?;
    Some(Query {
        qualifier,
        media_type: Some(media_type),
        condition: Some(condition),
    })
}

/// `<media-condition>` (or `<media-condition-without-or>` when
/// `allow_or` is false).
fn parse_condition(v: &[&ComponentValue], allow_or: bool) -> Option<Condition> {
    let first = v.first()?;
    if ident_eq(first, "not") {
        if v.len() != 2 {
            return None;
        }
        return Some(Condition::Not(Box::new(parse_in_parens(v[1])?)));
    }
    let mut parts = vec![parse_in_parens(first)?];
    let mut op: Option<&str> = None;
    let mut i = 1;
    while i < v.len() {
        let word = if ident_eq(v[i], "and") {
            "and"
        } else if ident_eq(v[i], "or") && allow_or {
            "or"
        } else {
            return None;
        };
        if op.is_some_and(|o| o != word) {
            return None;
        }
        op = Some(word);
        parts.push(parse_in_parens(v.get(i + 1)?)?);
        i += 2;
    }
    Some(match op {
        None => parts.pop()?,
        Some("and") => Condition::And(parts),
        Some(_) => Condition::Or(parts),
    })
}

/// `<media-in-parens>`.
fn parse_in_parens(v: &ComponentValue) -> Option<Condition> {
    match v {
        ComponentValue::Block('(', inner) => {
            let items = non_whitespace(inner);
            if let Some(c) = parse_condition(&items, true) {
                if !matches!(c, Condition::General(_)) || !items.is_empty() {
                    return Some(c);
                }
            }
            match parse_feature(inner) {
                Some(f) => Some(Condition::Feature(f?)),
                None => Some(Condition::General(serialize_tokens(&to_tokens(
                    std::slice::from_ref(v),
                )))),
            }
        }
        ComponentValue::Function(..) => Some(Condition::General(serialize_tokens(&to_tokens(
            std::slice::from_ref(v),
        )))),
        _ => None,
    }
}

/// A media feature from the contents of its parentheses: `None` if they
/// are not a feature at all (general-enclosed), `Some(None)` if they are
/// one that is invalid (an unknown feature or a bad value, which makes the
/// whole query invalid).
fn parse_feature(inner: &[ComponentValue]) -> Option<Option<Feature>> {
    let v = non_whitespace(inner);
    let first = v.first()?;
    // `(name)` and `(name: value)`.
    if let Some(name) = first.ident() {
        let name = name.to_ascii_lowercase();
        if v.len() == 1 {
            let ok = feature_type(&name).is_some();
            return Some(ok.then_some(Feature::Boolean(name)));
        }
        if matches!(v[1], ComponentValue::Token(Token::Colon)) {
            let (base, prefixed) = match name
                .strip_prefix("min-")
                .or_else(|| name.strip_prefix("max-"))
            {
                Some(b) => (b.to_string(), true),
                None => (name.clone(), false),
            };
            let Some(ty) = feature_type(&base) else {
                return Some(None);
            };
            if prefixed && matches!(ty, FeatureType::Discrete(_)) {
                return Some(None);
            }
            let value = parse_value(ty, &v[2..]);
            return Some(value.map(|value| Feature::Plain(name, value)));
        }
    }
    // The range syntax.
    let cmps: Vec<(usize, Cmp, usize)> = comparisons(&v);
    if cmps.is_empty() || cmps.len() > 2 {
        return None;
    }
    let range = || -> Option<Feature> {
        let (s0, c0, e0) = cmps[0];
        if cmps.len() == 1 {
            let left = &v[..s0];
            let right = &v[e0..];
            if let [n] = left {
                if let Some(name) = n.ident() {
                    let name = name.to_ascii_lowercase();
                    let ty = range_type(&name)?;
                    let value = parse_value(ty, right)?;
                    return Some(Feature::Range {
                        name,
                        left: None,
                        right: Some((c0, value)),
                    });
                }
            }
            let [n] = right else {
                return None;
            };
            let name = n.ident()?.to_ascii_lowercase();
            let ty = range_type(&name)?;
            let value = parse_value(ty, left)?;
            return Some(Feature::Range {
                name,
                left: Some((value, c0)),
                right: None,
            });
        }
        let (s1, c1, e1) = cmps[1];
        let lt = matches!(c0, Cmp::Lt | Cmp::Le) && matches!(c1, Cmp::Lt | Cmp::Le);
        let gt = matches!(c0, Cmp::Gt | Cmp::Ge) && matches!(c1, Cmp::Gt | Cmp::Ge);
        if !lt && !gt {
            return None;
        }
        let [n] = &v[e0..s1] else {
            return None;
        };
        let name = n.ident()?.to_ascii_lowercase();
        let ty = range_type(&name)?;
        let a = parse_value(ty, &v[..s0])?;
        let b = parse_value(ty, &v[e1..])?;
        Some(Feature::Range {
            name,
            left: Some((a, c0)),
            right: Some((c1, b)),
        })
    };
    Some(range())
}

/// The type of a feature usable in the range syntax.
fn range_type(name: &str) -> Option<FeatureType> {
    feature_type(name).filter(|t| !matches!(t, FeatureType::Discrete(_)))
}

/// The comparison operators of a range: start index, operator, index
/// after it.
fn comparisons(v: &[&ComponentValue]) -> Vec<(usize, Cmp, usize)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < v.len() {
        let d = match v[i] {
            ComponentValue::Token(Token::Delim(c @ ('<' | '>' | '='))) => *c,
            _ => {
                i += 1;
                continue;
            }
        };
        let eq_next = matches!(v.get(i + 1), Some(ComponentValue::Token(Token::Delim('='))));
        let (cmp, len) = match (d, eq_next) {
            ('<', true) => (Cmp::Le, 2),
            ('>', true) => (Cmp::Ge, 2),
            ('<', false) => (Cmp::Lt, 1),
            ('>', false) => (Cmp::Gt, 1),
            _ => (Cmp::Eq, 1),
        };
        out.push((i, cmp, i + len));
        i += len;
    }
    out
}

/// The CSS pixels of a length.
fn length_px(value: f64, unit: &str) -> Option<f64> {
    Some(match unit.to_ascii_lowercase().as_str() {
        "px" => value,
        // Relative units use the initial font size (16px).
        "em" | "rem" | "lh" | "rlh" => value * 16.0,
        "ex" | "rex" | "cap" | "rcap" => value * 8.0,
        "ch" | "rch" | "ic" | "ric" => value * 8.0,
        "vw" | "vi" | "svw" | "lvw" | "dvw" | "svi" | "lvi" | "dvi" => value * 8.0,
        "vh" | "vb" | "svh" | "lvh" | "dvh" | "svb" | "lvb" | "dvb" => value * 6.0,
        "vmin" | "svmin" | "lvmin" | "dvmin" => value * 6.0,
        "vmax" | "svmax" | "lvmax" | "dvmax" => value * 8.0,
        "cm" => value * 96.0 / 2.54,
        "mm" => value * 96.0 / 25.4,
        "q" => value * 96.0 / 101.6,
        "in" => value * 96.0,
        "pt" => value * 96.0 / 72.0,
        "pc" => value * 16.0,
        _ => return None,
    })
}

fn number_text(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

/// Parses a feature value of type `ty` from non-whitespace component
/// values.
fn parse_value(ty: FeatureType, v: &[&ComponentValue]) -> Option<Value> {
    match ty {
        FeatureType::Length => match v {
            [ComponentValue::Token(Token::Dimension(n, unit))] => {
                let px = length_px(n.value, unit)?;
                Some(Value::Length(
                    px,
                    format!("{}{}", number_text(n.value), unit.to_ascii_lowercase()),
                ))
            }
            [ComponentValue::Token(Token::Number(n))] if n.value == 0.0 => {
                Some(Value::Length(0.0, "0px".to_string()))
            }
            _ => None,
        },
        FeatureType::Integer => match v {
            [ComponentValue::Token(Token::Number(n))] if n.integer && n.value >= 0.0 => {
                Some(Value::Number(n.value, number_text(n.value)))
            }
            _ => None,
        },
        FeatureType::Ratio => {
            let num = |c: &ComponentValue| match c {
                ComponentValue::Token(Token::Number(n)) if n.value >= 0.0 => Some(n.value),
                _ => None,
            };
            match v {
                [a] => {
                    let a = num(a)?;
                    Some(Value::Ratio(a, 1.0, format!("{} / 1", number_text(a))))
                }
                [a, ComponentValue::Token(Token::Delim('/')), b] => {
                    let (a, b) = (num(a)?, num(b)?);
                    Some(Value::Ratio(
                        a,
                        b,
                        format!("{} / {}", number_text(a), number_text(b)),
                    ))
                }
                _ => None,
            }
        }
        FeatureType::Resolution => match v {
            [ComponentValue::Token(Token::Dimension(n, unit))] => {
                let dppx = match unit.to_ascii_lowercase().as_str() {
                    "dppx" | "x" => n.value,
                    "dpi" => n.value / 96.0,
                    "dpcm" => n.value * 2.54 / 96.0,
                    _ => return None,
                };
                if n.value < 0.0 {
                    return None;
                }
                Some(Value::Resolution(
                    dppx,
                    format!("{}{}", number_text(n.value), unit.to_ascii_lowercase()),
                ))
            }
            [ComponentValue::Token(Token::Ident(s))] if s.eq_ignore_ascii_case("infinite") => {
                Some(Value::Resolution(f64::INFINITY, "infinite".to_string()))
            }
            _ => None,
        },
        FeatureType::Discrete(keywords) => match v {
            [ComponentValue::Token(Token::Ident(s))] => {
                let s = s.to_ascii_lowercase();
                keywords.contains(&s.as_str()).then_some(Value::Ident(s))
            }
            [ComponentValue::Token(Token::Number(n))] if keywords.is_empty() => {
                // `grid` takes 0 or 1.
                (n.integer && (n.value == 0.0 || n.value == 1.0))
                    .then(|| Value::Number(n.value, number_text(n.value)))
            }
            _ => None,
        },
    }
}

/// The condition makes the query invalid if it has an invalid feature.
fn condition_valid(c: &Condition) -> bool {
    match c {
        Condition::Not(c) => condition_valid(c),
        Condition::And(v) | Condition::Or(v) => v.iter().all(condition_valid),
        Condition::Feature(_) | Condition::General(_) => true,
    }
}

// ----- serialization ---------------------------------------------------------------

fn serialize_condition(c: &Condition, nested: bool) -> String {
    match c {
        Condition::Not(inner) => {
            let s = format!("not {}", serialize_condition(inner, true));
            if nested { format!("({s})") } else { s }
        }
        Condition::And(parts) | Condition::Or(parts) => {
            let sep = if matches!(c, Condition::And(_)) {
                " and "
            } else {
                " or "
            };
            let s = parts
                .iter()
                .map(|p| serialize_condition(p, true))
                .collect::<Vec<_>>()
                .join(sep);
            if nested { format!("({s})") } else { s }
        }
        Condition::Feature(f) => match f {
            Feature::Boolean(n) => format!("({n})"),
            Feature::Plain(n, v) => format!("({n}: {})", v.text()),
            Feature::Range { name, left, right } => {
                let mut s = String::from("(");
                if let Some((v, op)) = left {
                    s.push_str(&format!("{} {} ", v.text(), op.as_str()));
                }
                s.push_str(name);
                if let Some((op, v)) = right {
                    s.push_str(&format!(" {} {}", op.as_str(), v.text()));
                }
                s.push(')');
                s
            }
        },
        Condition::General(text) => text.clone(),
    }
}

impl MediaQuery {
    /// "Serialize a media query".
    pub fn serialize(&self) -> String {
        let Some(q) = &self.0 else {
            return "not all".to_string();
        };
        let mut s = String::new();
        match q.qualifier {
            Some(Qualifier::Not) => s.push_str("not "),
            Some(Qualifier::Only) => s.push_str("only "),
            None => {}
        }
        let ty = q.media_type.as_deref();
        let omit_type = q.qualifier.is_none() && ty == Some("all") && q.condition.is_some();
        if let Some(t) = ty.filter(|_| !omit_type) {
            s.push_str(&serialize_identifier(t));
            if q.condition.is_some() {
                s.push_str(" and ");
            }
        }
        if let Some(c) = &q.condition {
            s.push_str(&serialize_condition(c, ty.is_some() && !omit_type));
        }
        s
    }

    /// Evaluates the media query in `env`.
    pub fn matches(&self, env: &Environment) -> bool {
        let Some(q) = &self.0 else {
            return false;
        };
        let type_ok = match q.media_type.as_deref() {
            None | Some("all") | Some("screen") => true,
            Some(_) => false,
        };
        let cond_ok = q
            .condition
            .as_ref()
            .is_none_or(|c| evaluate(c, env) == Some(true));
        let r = type_ok && cond_ok;
        if q.qualifier == Some(Qualifier::Not) {
            !r
        } else {
            r
        }
    }
}

impl MediaQueryList {
    /// "Serialize a media query list".
    pub fn serialize(&self) -> String {
        self.queries
            .iter()
            .map(MediaQuery::serialize)
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// An empty list matches; otherwise any query must match.
    pub fn matches(&self, env: &Environment) -> bool {
        self.queries.is_empty() || self.queries.iter().any(|q| q.matches(env))
    }

    /// `appendMedium(medium)`.
    pub fn append_medium(&mut self, medium: &str) {
        let Some(m) = parse_media_query(medium) else {
            return;
        };
        let text = m.serialize();
        if self.queries.iter().any(|q| q.serialize() == text) {
            return;
        }
        self.queries.push(m);
    }

    /// `deleteMedium(medium)`: false if nothing was removed.
    pub fn delete_medium(&mut self, medium: &str) -> bool {
        let Some(m) = parse_media_query(medium) else {
            return false;
        };
        let text = m.serialize();
        let before = self.queries.len();
        self.queries.retain(|q| q.serialize() != text);
        self.queries.len() != before
    }
}

/// Builds a [`MediaQueryList`] keeping the invalid feature rule: a query
/// with an invalid feature is `not all`.
fn validated(q: Option<Query>) -> Option<Query> {
    let q = q?;
    match &q.condition {
        Some(c) if !condition_valid(c) => None,
        _ => Some(q),
    }
}

// ----- evaluation ------------------------------------------------------------------

/// Three-valued evaluation: `None` is "unknown".
fn evaluate(c: &Condition, env: &Environment) -> Option<bool> {
    match c {
        Condition::Not(inner) => evaluate(inner, env).map(|b| !b),
        Condition::And(parts) => {
            let mut unknown = false;
            for p in parts {
                match evaluate(p, env) {
                    Some(false) => return Some(false),
                    None => unknown = true,
                    Some(true) => {}
                }
            }
            if unknown { None } else { Some(true) }
        }
        Condition::Or(parts) => {
            let mut unknown = false;
            for p in parts {
                match evaluate(p, env) {
                    Some(true) => return Some(true),
                    None => unknown = true,
                    Some(false) => {}
                }
            }
            if unknown { None } else { Some(false) }
        }
        Condition::Feature(f) => Some(evaluate_feature(f, env)),
        Condition::General(_) => Some(false),
    }
}

/// The environment's value of a feature.
fn actual(name: &str, env: &Environment) -> Value {
    let px = |v: f64| Value::Length(v, String::new());
    match name {
        "width" | "device-width" => px(env.width),
        "height" | "device-height" => px(env.height),
        "aspect-ratio" | "device-aspect-ratio" => {
            Value::Ratio(env.width, env.height, String::new())
        }
        "resolution" => Value::Resolution(env.resolution, String::new()),
        "color" => Value::Number(8.0, String::new()),
        "color-index" | "monochrome" | "grid" => Value::Number(0.0, String::new()),
        "orientation" => Value::Ident(
            if env.height >= env.width {
                "portrait"
            } else {
                "landscape"
            }
            .to_string(),
        ),
        "scan" => Value::Ident(String::new()),
        "update" => Value::Ident("fast".to_string()),
        "hover" | "any-hover" => Value::Ident("hover".to_string()),
        "pointer" | "any-pointer" => Value::Ident("fine".to_string()),
        "prefers-color-scheme" => Value::Ident("light".to_string()),
        "prefers-reduced-motion"
        | "prefers-reduced-transparency"
        | "prefers-reduced-data"
        | "prefers-contrast" => Value::Ident("no-preference".to_string()),
        "forced-colors" | "inverted-colors" => Value::Ident("none".to_string()),
        "dynamic-range" | "video-dynamic-range" => Value::Ident("standard".to_string()),
        "color-gamut" => Value::Ident("srgb".to_string()),
        "display-mode" => Value::Ident("browser".to_string()),
        "overflow-block" | "overflow-inline" => Value::Ident("scroll".to_string()),
        "scripting" => Value::Ident(if env.scripting { "enabled" } else { "none" }.to_string()),
        _ => Value::Ident(String::new()),
    }
}

fn numeric(v: &Value) -> Option<f64> {
    match v {
        Value::Length(n, _) | Value::Number(n, _) | Value::Resolution(n, _) => Some(*n),
        Value::Ratio(a, b, _) => Some(if *b == 0.0 { f64::INFINITY } else { a / b }),
        Value::Ident(_) => None,
    }
}

fn compare(actual: f64, op: Cmp, value: f64) -> bool {
    let eps = 1e-9;
    match op {
        Cmp::Lt => actual < value - eps,
        Cmp::Le => actual <= value + eps,
        Cmp::Gt => actual > value + eps,
        Cmp::Ge => actual >= value - eps,
        Cmp::Eq => (actual - value).abs() <= eps,
    }
}

fn evaluate_feature(f: &Feature, env: &Environment) -> bool {
    match f {
        Feature::Boolean(name) => match actual(name, env) {
            Value::Ident(s) => !s.is_empty() && s != "none" && s != "no-preference",
            v => numeric(&v).is_some_and(|n| n != 0.0),
        },
        Feature::Plain(name, value) => {
            let (base, op) = if let Some(b) = name.strip_prefix("min-") {
                (b, Cmp::Ge)
            } else if let Some(b) = name.strip_prefix("max-") {
                (b, Cmp::Le)
            } else {
                (name.as_str(), Cmp::Eq)
            };
            let a = actual(base, env);
            match (&a, value) {
                (Value::Ident(x), Value::Ident(y)) => x == y,
                _ => match (numeric(&a), numeric(value)) {
                    (Some(x), Some(y)) => compare(x, op, y),
                    _ => false,
                },
            }
        }
        Feature::Range { name, left, right } => {
            let Some(a) = numeric(&actual(name, env)) else {
                return false;
            };
            let left_ok = left
                .as_ref()
                .is_none_or(|(v, op)| numeric(v).is_some_and(|x| compare(a, op.flip(), x)));
            let right_ok = right
                .as_ref()
                .is_none_or(|(op, v)| numeric(v).is_some_and(|x| compare(a, *op, x)));
            left_ok && right_ok
        }
    }
}

/// Parses a media query list, keeping only valid queries as valid.
pub fn parse(text: &str) -> MediaQueryList {
    let mut list = parse_media_query_list(text);
    for q in &mut list.queries {
        q.0 = validated(q.0.take());
    }
    list
}

/// [`parse`] from component values.
pub fn parse_values(values: &[ComponentValue]) -> MediaQueryList {
    let mut list = parse_media_query_list_values(values);
    for q in &mut list.queries {
        q.0 = validated(q.0.take());
    }
    list
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(s: &str) -> String {
        parse(s).serialize()
    }

    #[test]
    fn serializes_media_queries() {
        assert_eq!(text("SCREEN"), "screen");
        assert_eq!(text("all and (COLOR)"), "(color)");
        assert_eq!(
            text("not screen and (min-width:100PX)"),
            "not screen and (min-width: 100px)"
        );
        assert_eq!(text("(width>=100px) , print"), "(width >= 100px), print");
        assert_eq!(text("(100px < width <= 200px)"), "(100px < width <= 200px)");
        assert_eq!(text("(unknown-feature)"), "not all");
        assert_eq!(text("(min-orientation: portrait)"), "not all");
        assert_eq!(text("screen and"), "not all");
        assert_eq!(text("(color) and (hover) or (grid)"), "not all");
        assert_eq!(text("not (color)"), "not (color)");
        assert_eq!(text("(aspect-ratio: 16/9)"), "(aspect-ratio: 16 / 9)");
    }

    #[test]
    fn evaluates_against_the_viewport() {
        let env = Environment::default();
        let m = |s: &str| parse(s).matches(&env);
        assert!(m(""));
        assert!(m("screen"));
        assert!(!m("print"));
        assert!(m("not print"));
        assert!(m("(min-width: 800px) and (max-height: 600px)"));
        assert!(!m("(width > 800px)"));
        assert!(m("(400px < width <= 800px)"));
        assert!(m("(orientation: landscape)"));
        assert!(m("(color)"));
        assert!(!m("(monochrome)"));
        assert!(m("(min-resolution: 96dpi)"));
        assert!(!m("(unknown)"));
        assert!(m("print, (hover: hover)"));
    }

    #[test]
    fn media_list_append_and_delete() {
        let mut l = parse("screen");
        l.append_medium("SCREEN");
        l.append_medium("print");
        assert_eq!(l.serialize(), "screen, print");
        assert!(l.delete_medium("screen"));
        assert!(!l.delete_medium("tv"));
        assert_eq!(l.serialize(), "print");
    }
}
