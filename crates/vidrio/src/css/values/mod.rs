//! CSS values (css-values-4/5, CSSOM §6.7.2): the units and their
//! conversions, the typed values a property grammar produces, the
//! parsing of the numeric, textual and URL data types and the
//! serialization of specified values. Math functions live in [`calc`],
//! colors in [`color`], images in [`image`], the value definition syntax
//! and its matcher in [`grammar`], custom properties, `var()`, `attr()`
//! and `@property` in [`custom`], and computed values in [`compute`].

pub mod calc;
pub mod color;
pub mod compute;
pub mod custom;
pub mod grammar;
pub mod image;

use super::declaration::{serialize_identifier, serialize_string, serialize_tokens};
use super::parser::ComponentValue;
use super::properties;
use super::tokenizer::Token;

// ----- numbers -----------------------------------------------------------------------

/// Serializes a number (CSSOM "serialize a `<number>`"): at most six
/// significant digits after the integer part, no trailing zeros, no
/// exponent, `-0` as `0`; infinities and NaN as the calc keywords.
pub fn number(v: f64) -> String {
    if v.is_nan() {
        return "NaN".to_string();
    }
    if v.is_infinite() {
        return if v > 0.0 { "infinity" } else { "-infinity" }.to_string();
    }
    if v == 0.0 {
        return "0".to_string();
    }
    let magnitude = v.abs().log10().floor() as i32;
    let decimals = (5 - magnitude).clamp(0, 20) as usize;
    let mut s = format!("{:.*}", decimals, v);
    if s.contains('.') {
        while s.ends_with('0') {
            s.pop();
        }
        if s.ends_with('.') {
            s.pop();
        }
    }
    if s == "-0" {
        s = "0".to_string();
    }
    s
}

/// Rounds half toward positive infinity (CSS's rounding of integers).
pub fn round_half_up(v: f64) -> f64 {
    (v + 0.5).floor()
}

// ----- units -------------------------------------------------------------------------

/// The kinds of dimension.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Category {
    Length,
    Angle,
    Time,
    Frequency,
    Resolution,
    Flex,
}

impl Category {
    /// The canonical unit of the category.
    pub fn canonical(self) -> &'static str {
        match self {
            Category::Length => "px",
            Category::Angle => "deg",
            Category::Time => "s",
            Category::Frequency => "hz",
            Category::Resolution => "dppx",
            Category::Flex => "fr",
        }
    }
}

/// How a relative length resolves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Relative {
    /// Absolute: a fixed factor to the canonical unit.
    None,
    /// Relative to the element's font (`em`, `ex`, `ch`, `cap`, `ic`,
    /// `lh`).
    Font,
    /// Relative to the root element's font (`rem`, `rlh`, …).
    RootFont,
    /// Relative to the viewport (`vw`, `svh`, …).
    Viewport,
    /// Relative to a query container (`cqw`, …).
    Container,
}

/// A unit: its lowercase name, category, factor to the canonical unit
/// (when absolute) and what it is relative to.
pub struct Unit {
    pub name: &'static str,
    pub category: Category,
    pub factor: f64,
    pub relative: Relative,
}

const fn u(name: &'static str, category: Category, factor: f64, relative: Relative) -> Unit {
    Unit {
        name,
        category,
        factor,
        relative,
    }
}

use Category as C;
use Relative as R;

/// Every unit vidrio knows.
pub static UNITS: &[Unit] = &[
    u("px", C::Length, 1.0, R::None),
    u("cm", C::Length, 96.0 / 2.54, R::None),
    u("mm", C::Length, 96.0 / 25.4, R::None),
    u("q", C::Length, 96.0 / 101.6, R::None),
    u("in", C::Length, 96.0, R::None),
    u("pt", C::Length, 96.0 / 72.0, R::None),
    u("pc", C::Length, 16.0, R::None),
    u("em", C::Length, 0.0, R::Font),
    u("ex", C::Length, 0.0, R::Font),
    u("ch", C::Length, 0.0, R::Font),
    u("cap", C::Length, 0.0, R::Font),
    u("ic", C::Length, 0.0, R::Font),
    u("lh", C::Length, 0.0, R::Font),
    u("rem", C::Length, 0.0, R::RootFont),
    u("rex", C::Length, 0.0, R::RootFont),
    u("rch", C::Length, 0.0, R::RootFont),
    u("rcap", C::Length, 0.0, R::RootFont),
    u("ric", C::Length, 0.0, R::RootFont),
    u("rlh", C::Length, 0.0, R::RootFont),
    u("vw", C::Length, 0.0, R::Viewport),
    u("vh", C::Length, 0.0, R::Viewport),
    u("vi", C::Length, 0.0, R::Viewport),
    u("vb", C::Length, 0.0, R::Viewport),
    u("vmin", C::Length, 0.0, R::Viewport),
    u("vmax", C::Length, 0.0, R::Viewport),
    u("svw", C::Length, 0.0, R::Viewport),
    u("svh", C::Length, 0.0, R::Viewport),
    u("svi", C::Length, 0.0, R::Viewport),
    u("svb", C::Length, 0.0, R::Viewport),
    u("svmin", C::Length, 0.0, R::Viewport),
    u("svmax", C::Length, 0.0, R::Viewport),
    u("lvw", C::Length, 0.0, R::Viewport),
    u("lvh", C::Length, 0.0, R::Viewport),
    u("lvi", C::Length, 0.0, R::Viewport),
    u("lvb", C::Length, 0.0, R::Viewport),
    u("lvmin", C::Length, 0.0, R::Viewport),
    u("lvmax", C::Length, 0.0, R::Viewport),
    u("dvw", C::Length, 0.0, R::Viewport),
    u("dvh", C::Length, 0.0, R::Viewport),
    u("dvi", C::Length, 0.0, R::Viewport),
    u("dvb", C::Length, 0.0, R::Viewport),
    u("dvmin", C::Length, 0.0, R::Viewport),
    u("dvmax", C::Length, 0.0, R::Viewport),
    u("cqw", C::Length, 0.0, R::Container),
    u("cqh", C::Length, 0.0, R::Container),
    u("cqi", C::Length, 0.0, R::Container),
    u("cqb", C::Length, 0.0, R::Container),
    u("cqmin", C::Length, 0.0, R::Container),
    u("cqmax", C::Length, 0.0, R::Container),
    u("deg", C::Angle, 1.0, R::None),
    u("grad", C::Angle, 0.9, R::None),
    u("rad", C::Angle, 180.0 / std::f64::consts::PI, R::None),
    u("turn", C::Angle, 360.0, R::None),
    u("s", C::Time, 1.0, R::None),
    u("ms", C::Time, 0.001, R::None),
    u("hz", C::Frequency, 1.0, R::None),
    u("khz", C::Frequency, 1000.0, R::None),
    u("dppx", C::Resolution, 1.0, R::None),
    u("x", C::Resolution, 1.0, R::None),
    u("dpi", C::Resolution, 1.0 / 96.0, R::None),
    u("dpcm", C::Resolution, 2.54 / 96.0, R::None),
    u("fr", C::Flex, 1.0, R::None),
];

/// The unit named `name` (ASCII case-insensitive).
pub fn unit(name: &str) -> Option<&'static Unit> {
    UNITS.iter().find(|u| u.name.eq_ignore_ascii_case(name))
}

/// `value` of `unit` in the canonical unit, if the unit is absolute.
pub fn to_canonical(value: f64, unit_name: &str) -> Option<(f64, &'static str)> {
    let u = unit(unit_name)?;
    if u.relative != Relative::None {
        return None;
    }
    if u.category == Category::Flex {
        return Some((value, "fr"));
    }
    Some((value * u.factor, u.category.canonical()))
}

// ----- typed values ------------------------------------------------------------------

/// One piece of a parsed value: what the grammar of a property matches,
/// flattened into a list where commas and slashes are their own items.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// A keyword of the grammar (lowercase).
    Keyword(String),
    /// A `<custom-ident>` or `<dashed-ident>` (case kept).
    Ident(String),
    Number(f64),
    /// An `<integer>`.
    Integer(f64),
    Percentage(f64),
    /// A dimension with its lowercase unit.
    Dimension(f64, String),
    Math(Box<calc::Math>),
    String(String),
    Url(String),
    Color(Box<color::Color>),
    Image(Box<image::Image>),
    /// A function of a grammar (`minmax(…)`, `repeat(…)`, `counter(…)`)
    /// with its lowercase name and arguments.
    Function(String, Vec<Value>),
    /// A `[…]` block of grid line names.
    Block(Vec<Value>),
    Comma,
    Slash,
    /// Component values kept as written (functions vidrio does not
    /// analyze, such as `<basic-shape>` and transform functions).
    Raw(Vec<Token>),
}

impl Value {
    /// Serializes the value as a specified value.
    pub fn to_css(&self) -> String {
        match self {
            Value::Keyword(k) => k.clone(),
            Value::Ident(s) => serialize_identifier(s),
            Value::Number(n) | Value::Integer(n) => number(*n),
            Value::Percentage(n) => format!("{}%", number(*n)),
            Value::Dimension(n, unit) => format!("{}{}", number(*n), unit),
            Value::Math(m) => m.to_css(),
            Value::String(s) => serialize_string(s),
            Value::Url(s) => format!("url({})", serialize_string(s)),
            Value::Color(c) => c.to_css(),
            Value::Image(i) => i.to_css(),
            Value::Function(name, args) => format!("{}({})", name, serialize_list(args)),
            Value::Block(items) => format!("[{}]", serialize_list(items)),
            Value::Comma => ",".to_string(),
            Value::Slash => "/".to_string(),
            Value::Raw(tokens) => serialize_tokens(tokens),
        }
    }

    /// The keyword, if the value is one.
    pub fn keyword(&self) -> Option<&str> {
        match self {
            Value::Keyword(k) => Some(k),
            _ => None,
        }
    }
}

/// Serializes a list of values: single spaces between items, `, ` after
/// commas and ` / ` around slashes.
pub fn serialize_list(items: &[Value]) -> String {
    let mut s = String::new();
    for v in items {
        match v {
            Value::Comma => s.push(','),
            Value::Slash => {
                if !s.is_empty() {
                    s.push(' ');
                }
                s.push('/');
            }
            _ => {
                if !s.is_empty() {
                    s.push(' ');
                }
                s.push_str(&v.to_css());
            }
        }
    }
    s
}

// ----- component values --------------------------------------------------------------

/// Folds a token list into component values (functions and blocks with
/// their contents).
pub fn to_components(tokens: &[Token]) -> Vec<ComponentValue> {
    let mut i = 0;
    fold(tokens, &mut i, None)
}

fn fold(t: &[Token], i: &mut usize, end: Option<&Token>) -> Vec<ComponentValue> {
    let mut out = Vec::new();
    while *i < t.len() {
        let tok = &t[*i];
        *i += 1;
        if end == Some(tok) {
            return out;
        }
        match tok {
            Token::Function(name) => {
                let args = fold(t, i, Some(&Token::RightParen));
                out.push(ComponentValue::Function(name.clone(), args));
            }
            Token::LeftParen => {
                let c = fold(t, i, Some(&Token::RightParen));
                out.push(ComponentValue::Block('(', c));
            }
            Token::LeftBracket => {
                let c = fold(t, i, Some(&Token::RightBracket));
                out.push(ComponentValue::Block('[', c));
            }
            Token::LeftBrace => {
                let c = fold(t, i, Some(&Token::RightBrace));
                out.push(ComponentValue::Block('{', c));
            }
            _ => out.push(ComponentValue::Token(tok.clone())),
        }
    }
    out
}

/// The component values without whitespace.
pub fn significant(v: &[ComponentValue]) -> Vec<ComponentValue> {
    v.iter().filter(|c| !c.is_whitespace()).cloned().collect()
}

/// The tokens of one component value.
pub fn tokens_of(cv: &ComponentValue) -> Vec<Token> {
    let mut out = Vec::new();
    cv.push_tokens(&mut out);
    out
}

/// True for an identifier equal to `word`, ASCII case-insensitively.
pub fn is_ident(cv: &ComponentValue, word: &str) -> bool {
    cv.ident().is_some_and(|s| s.eq_ignore_ascii_case(word))
}

// ----- the numeric data types ---------------------------------------------------------

/// The numeric data types of the value definition syntax.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Numeric {
    Number,
    Integer,
    Percentage,
    Length,
    LengthPercentage,
    Angle,
    AnglePercentage,
    Time,
    TimePercentage,
    Frequency,
    Resolution,
    Flex,
    NumberPercentage,
}

impl Numeric {
    /// The types a math function may resolve to for this data type, with
    /// what its percentages resolve against.
    fn calc_types(self) -> Vec<(calc::Type, calc::PercentAs)> {
        use calc::{PercentAs, Type};
        let base = |c: Category| (Type::base(c), PercentAs::Invalid);
        let mixed = |c: Category| (Type::base(c), PercentAs::Base(c));
        match self {
            Numeric::Number | Numeric::Integer => vec![(Type::NUMBER, PercentAs::Invalid)],
            Numeric::Percentage => vec![(Type::PERCENT, PercentAs::Percent)],
            Numeric::NumberPercentage => vec![
                (Type::NUMBER, PercentAs::Invalid),
                (Type::PERCENT, PercentAs::Percent),
            ],
            Numeric::Length => vec![base(Category::Length)],
            Numeric::LengthPercentage => vec![mixed(Category::Length)],
            Numeric::Angle => vec![base(Category::Angle)],
            Numeric::AnglePercentage => vec![mixed(Category::Angle)],
            Numeric::Time => vec![base(Category::Time)],
            Numeric::TimePercentage => vec![mixed(Category::Time)],
            Numeric::Frequency => vec![base(Category::Frequency)],
            Numeric::Resolution => vec![base(Category::Resolution)],
            Numeric::Flex => vec![base(Category::Flex)],
        }
    }

    fn category(self) -> Option<Category> {
        match self {
            Numeric::Length | Numeric::LengthPercentage => Some(Category::Length),
            Numeric::Angle | Numeric::AnglePercentage => Some(Category::Angle),
            Numeric::Time | Numeric::TimePercentage => Some(Category::Time),
            Numeric::Frequency => Some(Category::Frequency),
            Numeric::Resolution => Some(Category::Resolution),
            Numeric::Flex => Some(Category::Flex),
            _ => None,
        }
    }

    fn allows_percentage(self) -> bool {
        matches!(
            self,
            Numeric::Percentage
                | Numeric::LengthPercentage
                | Numeric::AnglePercentage
                | Numeric::TimePercentage
                | Numeric::NumberPercentage
        )
    }
}

/// A closed range of allowed literal values (`[0,∞]`).
pub type Range = Option<(f64, f64)>;

fn in_range(v: f64, range: Range) -> bool {
    range.is_none_or(|(lo, hi)| v >= lo && v <= hi)
}

/// Parses one component value as the numeric data type `ty`: a literal
/// in `range` or a math function of the right type (ranges apply to
/// literals only; math functions are clamped when computed). A unitless
/// zero is a length.
pub fn parse_numeric(cv: &ComponentValue, ty: Numeric, range: Range) -> Option<Value> {
    match cv {
        ComponentValue::Token(Token::Number(n)) => match ty {
            Numeric::Number | Numeric::NumberPercentage => {
                in_range(n.value, range).then_some(Value::Number(n.value))
            }
            Numeric::Integer => {
                (n.integer && in_range(n.value, range)).then_some(Value::Integer(n.value))
            }
            Numeric::Length | Numeric::LengthPercentage if n.value == 0.0 => {
                Some(Value::Dimension(0.0, "px".to_string()))
            }
            _ => None,
        },
        ComponentValue::Token(Token::Percentage(n)) => (ty.allows_percentage()
            && in_range(n.value, range))
        .then_some(Value::Percentage(n.value)),
        ComponentValue::Token(Token::Dimension(n, unit_name)) => {
            let category = ty.category()?;
            let u = unit(unit_name)?;
            (u.category == category && in_range(n.value, range))
                .then(|| Value::Dimension(n.value, u.name.to_string()))
        }
        ComponentValue::Function(name, _) if calc::is_math_function(name) => {
            for (want, percent) in ty.calc_types() {
                if let Some(m) = calc::parse(cv, percent) {
                    if m.ty == want {
                        return Some(Value::Math(Box::new(m)));
                    }
                }
            }
            None
        }
        _ => None,
    }
}

/// An `<ident>` that is not a CSS-wide keyword.
pub fn parse_custom_ident(cv: &ComponentValue, excluded: &[&str]) -> Option<Value> {
    let s = cv.ident()?;
    if properties::is_css_wide_keyword(s)
        || s.eq_ignore_ascii_case("default")
        || excluded.iter().any(|e| s.eq_ignore_ascii_case(e))
    {
        return None;
    }
    Some(Value::Ident(s.to_string()))
}

/// A `<dashed-ident>`.
pub fn parse_dashed_ident(cv: &ComponentValue) -> Option<Value> {
    let s = cv.ident()?;
    (s.starts_with("--") && s.len() > 2).then(|| Value::Ident(s.to_string()))
}

/// A `<string>`.
pub fn parse_string(cv: &ComponentValue) -> Option<Value> {
    match cv {
        ComponentValue::Token(Token::String(s)) => Some(Value::String(s.clone())),
        _ => None,
    }
}

/// A `<url>`: a URL token or `url()`/`src()` with a string.
pub fn parse_url(cv: &ComponentValue) -> Option<Value> {
    match cv {
        ComponentValue::Token(Token::Url(s)) => Some(Value::Url(s.clone())),
        ComponentValue::Function(name, args)
            if name.eq_ignore_ascii_case("url") || name.eq_ignore_ascii_case("src") =>
        {
            let args = significant(args);
            match args.as_slice() {
                [ComponentValue::Token(Token::String(s))] => Some(Value::Url(s.clone())),
                _ => None,
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::css::parser::parse_component_values;

    #[test]
    fn numbers_serialize_with_six_significant_digits() {
        assert_eq!(number(1.0), "1");
        assert_eq!(number(0.5), "0.5");
        assert_eq!(number(1.0 / 3.0), "0.333333");
        assert_eq!(number(1234.5678), "1234.57");
        assert_eq!(number(-0.0), "0");
        assert_eq!(number(f64::INFINITY), "infinity");
        assert_eq!(number(f64::NAN), "NaN");
    }

    #[test]
    fn units_convert_to_canonical() {
        assert_eq!(to_canonical(1.0, "in"), Some((96.0, "px")));
        assert_eq!(to_canonical(1.0, "TURN"), Some((360.0, "deg")));
        assert_eq!(to_canonical(500.0, "ms"), Some((0.5, "s")));
        assert_eq!(to_canonical(1.0, "em"), None);
    }

    #[test]
    fn numeric_types_parse() {
        let cv = parse_component_values("0");
        assert_eq!(
            parse_numeric(&cv[0], Numeric::Length, None),
            Some(Value::Dimension(0.0, "px".into()))
        );
        let cv = parse_component_values("-1px");
        assert_eq!(
            parse_numeric(&cv[0], Numeric::Length, Some((0.0, f64::INFINITY))),
            None
        );
        let cv = parse_component_values("2.5");
        assert_eq!(parse_numeric(&cv[0], Numeric::Integer, None), None);
        let cv = parse_component_values("10Q");
        assert_eq!(
            parse_numeric(&cv[0], Numeric::Length, None).map(|v| v.to_css()),
            Some("10q".into())
        );
    }
}
