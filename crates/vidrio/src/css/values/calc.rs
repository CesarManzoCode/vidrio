//! The math functions of css-values-4 §10: `calc()`, `min()`, `max()`,
//! `clamp()`, `round()`, `mod()`, `rem()`, the trigonometric functions,
//! `pow()`, `sqrt()`, `hypot()`, `log()`, `exp()`, `abs()` and `sign()`,
//! with the numeric constants. A math function parses into a calculation
//! tree (§10.9), is type-checked with the typed arithmetic of §10.8,
//! simplified (§10.10.1) at parse time (absolute units to their
//! canonical unit) or at computed-value time (relative lengths and
//! percentages through a [`Resolver`]) and serialized (§10.13).

use super::{Category, number, to_canonical, unit};
use crate::css::parser::ComponentValue;
use crate::css::tokenizer::Token;

// ----- types ---------------------------------------------------------------------------

/// A CSS type (§10.8): the exponent of each base type, the percentage
/// one last.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Type {
    pub exp: [i8; 7],
}

const PERCENT_INDEX: usize = 6;

fn index(c: Category) -> usize {
    match c {
        Category::Length => 0,
        Category::Angle => 1,
        Category::Time => 2,
        Category::Frequency => 3,
        Category::Resolution => 4,
        Category::Flex => 5,
    }
}

const CATEGORIES: [Category; 6] = [
    Category::Length,
    Category::Angle,
    Category::Time,
    Category::Frequency,
    Category::Resolution,
    Category::Flex,
];

impl Type {
    pub const NUMBER: Type = Type { exp: [0; 7] };
    pub const PERCENT: Type = Type {
        exp: [0, 0, 0, 0, 0, 0, 1],
    };

    /// The type of one base type to the first power.
    pub fn base(c: Category) -> Type {
        let mut exp = [0; 7];
        exp[index(c)] = 1;
        Type { exp }
    }

    fn mul(self, other: Type) -> Type {
        let mut exp = [0; 7];
        for (i, e) in exp.iter_mut().enumerate() {
            *e = self.exp[i].saturating_add(other.exp[i]);
        }
        Type { exp }
    }

    fn inv(self) -> Type {
        let mut exp = self.exp;
        for e in exp.iter_mut() {
            *e = -*e;
        }
        Type { exp }
    }

    /// The single base type of the type, if it is one to the first power.
    pub fn category(self) -> Option<Category> {
        if self.exp[PERCENT_INDEX] != 0 {
            return None;
        }
        let mut found = None;
        for (i, e) in self.exp[..6].iter().enumerate() {
            match e {
                0 => {}
                1 if found.is_none() => found = Some(CATEGORIES[i]),
                _ => return None,
            }
        }
        found
    }
}

/// What a percentage in a math function resolves against.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PercentAs {
    /// Percentages are not allowed.
    Invalid,
    /// Percentages are their own type (`<percentage>`).
    Percent,
    /// Percentages resolve against a base type (`<length-percentage>`).
    Base(Category),
}

// ----- the calculation tree -------------------------------------------------------------

/// The rounding strategies of `round()`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rounding {
    Nearest,
    Up,
    Down,
    ToZero,
}

/// The math functions besides `calc()`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Func {
    Min,
    Max,
    Clamp,
    Round(Rounding),
    Mod,
    Rem,
    Sin,
    Cos,
    Tan,
    Asin,
    Acos,
    Atan,
    Atan2,
    Pow,
    Sqrt,
    Hypot,
    Log,
    Exp,
    Abs,
    Sign,
}

impl Func {
    fn name(self) -> &'static str {
        match self {
            Func::Min => "min",
            Func::Max => "max",
            Func::Clamp => "clamp",
            Func::Round(_) => "round",
            Func::Mod => "mod",
            Func::Rem => "rem",
            Func::Sin => "sin",
            Func::Cos => "cos",
            Func::Tan => "tan",
            Func::Asin => "asin",
            Func::Acos => "acos",
            Func::Atan => "atan",
            Func::Atan2 => "atan2",
            Func::Pow => "pow",
            Func::Sqrt => "sqrt",
            Func::Hypot => "hypot",
            Func::Log => "log",
            Func::Exp => "exp",
            Func::Abs => "abs",
            Func::Sign => "sign",
        }
    }
}

/// A node of a calculation tree.
#[derive(Clone, Debug, PartialEq)]
pub enum Node {
    Number(f64),
    Percentage(f64),
    /// A dimension with its lowercase unit.
    Dimension(f64, String),
    Sum(Vec<Node>),
    Product(Vec<Node>),
    Negate(Box<Node>),
    Invert(Box<Node>),
    Func(Func, Vec<Node>),
    /// A keyword that resolves to a number later: the channel keywords
    /// of a relative color (`r`, `h`, `alpha`, …).
    Symbol(String),
}

/// A parsed math function.
#[derive(Clone, Debug, PartialEq)]
pub struct Math {
    /// The simplified calculation tree.
    pub root: Node,
    /// The type the function resolves to.
    pub ty: Type,
    /// What its percentages resolve against.
    pub percent: PercentAs,
    /// The function was written as `calc()`.
    pub calc: bool,
}

const MATH_FUNCTIONS: &[&str] = &[
    "calc", "min", "max", "clamp", "round", "mod", "rem", "sin", "cos", "tan", "asin", "acos",
    "atan", "atan2", "pow", "sqrt", "hypot", "log", "exp", "abs", "sign",
];

/// True if `name` is a math function.
pub fn is_math_function(name: &str) -> bool {
    MATH_FUNCTIONS.iter().any(|f| f.eq_ignore_ascii_case(name))
}

// ----- parsing ---------------------------------------------------------------------------

struct Parser<'a> {
    symbols: &'a [&'a str],
}

fn split_commas(v: &[ComponentValue]) -> Vec<&[ComponentValue]> {
    let mut out = Vec::new();
    let mut start = 0;
    for (i, c) in v.iter().enumerate() {
        if matches!(c, ComponentValue::Token(Token::Comma)) {
            out.push(&v[start..i]);
            start = i + 1;
        }
    }
    out.push(&v[start..]);
    out
}

impl Parser<'_> {
    /// `<calc-sum>`: products separated by `+` and `-` with whitespace on
    /// both sides.
    fn sum(&self, input: &[ComponentValue]) -> Option<Node> {
        let mut terms: Vec<(char, &[ComponentValue])> = Vec::new();
        let mut start = 0;
        let mut sign = '+';
        for (i, c) in input.iter().enumerate() {
            if let ComponentValue::Token(Token::Delim(d @ ('+' | '-'))) = c {
                let before = i > 0 && input[i - 1].is_whitespace();
                let after = input.get(i + 1).is_some_and(ComponentValue::is_whitespace);
                if !before || !after {
                    return None;
                }
                terms.push((sign, &input[start..i]));
                sign = *d;
                start = i + 1;
            }
        }
        terms.push((sign, &input[start..]));
        let mut nodes = Vec::with_capacity(terms.len());
        for (sign, term) in terms {
            let p = self.product(term)?;
            nodes.push(if sign == '-' {
                Node::Negate(Box::new(p))
            } else {
                p
            });
        }
        if nodes.len() == 1 {
            nodes.pop()
        } else {
            Some(Node::Sum(nodes))
        }
    }

    /// `<calc-product>`: values separated by `*` and `/`.
    fn product(&self, input: &[ComponentValue]) -> Option<Node> {
        let sig: Vec<&ComponentValue> = input.iter().filter(|c| !c.is_whitespace()).collect();
        let first = sig.first()?;
        let mut nodes = vec![self.value(first)?];
        let mut i = 1;
        while i < sig.len() {
            let operand = sig.get(i + 1)?;
            match sig[i] {
                ComponentValue::Token(Token::Delim('*')) => nodes.push(self.value(operand)?),
                ComponentValue::Token(Token::Delim('/')) => {
                    nodes.push(Node::Invert(Box::new(self.value(operand)?)))
                }
                _ => return None,
            }
            i += 2;
        }
        if nodes.len() == 1 {
            nodes.pop()
        } else {
            Some(Node::Product(nodes))
        }
    }

    /// `<calc-value>`.
    fn value(&self, cv: &ComponentValue) -> Option<Node> {
        match cv {
            ComponentValue::Token(Token::Number(n)) => Some(Node::Number(n.value)),
            ComponentValue::Token(Token::Percentage(n)) => Some(Node::Percentage(n.value)),
            ComponentValue::Token(Token::Dimension(n, u)) => {
                Some(Node::Dimension(n.value, unit(u)?.name.to_string()))
            }
            ComponentValue::Token(Token::Ident(s)) => {
                let lower = s.to_ascii_lowercase();
                match lower.as_str() {
                    "e" => Some(Node::Number(std::f64::consts::E)),
                    "pi" => Some(Node::Number(std::f64::consts::PI)),
                    "infinity" => Some(Node::Number(f64::INFINITY)),
                    "-infinity" => Some(Node::Number(f64::NEG_INFINITY)),
                    "nan" => Some(Node::Number(f64::NAN)),
                    _ if self.symbols.contains(&lower.as_str()) => Some(Node::Symbol(lower)),
                    _ => None,
                }
            }
            ComponentValue::Block('(', inner) => self.sum(inner),
            ComponentValue::Function(name, args) if is_math_function(name) => {
                self.function(name, args)
            }
            _ => None,
        }
    }

    fn function(&self, name: &str, args: &[ComponentValue]) -> Option<Node> {
        let lower = name.to_ascii_lowercase();
        let parts = split_commas(args);
        let sums = |parts: &[&[ComponentValue]]| -> Option<Vec<Node>> {
            parts.iter().map(|p| self.sum(p)).collect()
        };
        let n = parts.len();
        let f = match lower.as_str() {
            "calc" => {
                return if n == 1 { self.sum(parts[0]) } else { None };
            }
            "min" => Func::Min,
            "max" => Func::Max,
            "hypot" => Func::Hypot,
            "clamp" if n == 3 => Func::Clamp,
            "round" if (1..=3).contains(&n) => {
                let first: Vec<&ComponentValue> =
                    parts[0].iter().filter(|c| !c.is_whitespace()).collect();
                let strategy = match first.as_slice() {
                    [c] => match c.ident().map(str::to_ascii_lowercase).as_deref() {
                        Some("nearest") => Some(Rounding::Nearest),
                        Some("up") => Some(Rounding::Up),
                        Some("down") => Some(Rounding::Down),
                        Some("to-zero") => Some(Rounding::ToZero),
                        _ => None,
                    },
                    _ => None,
                };
                let (strategy, rest) = match strategy {
                    Some(s) => (s, &parts[1..]),
                    None => (Rounding::Nearest, &parts[..]),
                };
                let mut nodes = match rest.len() {
                    1 | 2 => sums(rest)?,
                    _ => return None,
                };
                if nodes.len() == 1 {
                    nodes.push(Node::Number(1.0));
                }
                return Some(Node::Func(Func::Round(strategy), nodes));
            }
            "mod" if n == 2 => Func::Mod,
            "rem" if n == 2 => Func::Rem,
            "atan2" if n == 2 => Func::Atan2,
            "pow" if n == 2 => Func::Pow,
            "log" if n == 1 || n == 2 => Func::Log,
            "sin" if n == 1 => Func::Sin,
            "cos" if n == 1 => Func::Cos,
            "tan" if n == 1 => Func::Tan,
            "asin" if n == 1 => Func::Asin,
            "acos" if n == 1 => Func::Acos,
            "atan" if n == 1 => Func::Atan,
            "sqrt" if n == 1 => Func::Sqrt,
            "exp" if n == 1 => Func::Exp,
            "abs" if n == 1 => Func::Abs,
            "sign" if n == 1 => Func::Sign,
            _ => return None,
        };
        Some(Node::Func(f, sums(&parts)?))
    }
}

/// The type of a calculation tree, or `None` if it is invalid.
pub fn type_of(n: &Node, percent: PercentAs) -> Option<Type> {
    let same = |args: &[Node]| -> Option<Type> {
        let first = type_of(args.first()?, percent)?;
        for a in &args[1..] {
            if type_of(a, percent)? != first {
                return None;
            }
        }
        Some(first)
    };
    match n {
        Node::Number(_) | Node::Symbol(_) => Some(Type::NUMBER),
        Node::Percentage(_) => match percent {
            PercentAs::Invalid => None,
            PercentAs::Percent => Some(Type::PERCENT),
            PercentAs::Base(c) => Some(Type::base(c)),
        },
        Node::Dimension(_, u) => Some(Type::base(unit(u)?.category)),
        Node::Sum(children) => same(children),
        Node::Product(children) => {
            let mut t = Type::NUMBER;
            for c in children {
                t = t.mul(type_of(c, percent)?);
            }
            Some(t)
        }
        Node::Negate(c) => type_of(c, percent),
        Node::Invert(c) => type_of(c, percent).map(Type::inv),
        Node::Func(f, args) => match f {
            Func::Min
            | Func::Max
            | Func::Clamp
            | Func::Hypot
            | Func::Round(_)
            | Func::Mod
            | Func::Rem
            | Func::Abs => same(args),
            Func::Sign => {
                type_of(args.first()?, percent)?;
                Some(Type::NUMBER)
            }
            Func::Sin | Func::Cos | Func::Tan => {
                let t = type_of(args.first()?, percent)?;
                (t == Type::NUMBER || t == Type::base(Category::Angle)).then_some(Type::NUMBER)
            }
            Func::Asin | Func::Acos | Func::Atan => (type_of(args.first()?, percent)?
                == Type::NUMBER)
                .then(|| Type::base(Category::Angle)),
            Func::Atan2 => same(args).map(|_| Type::base(Category::Angle)),
            Func::Pow | Func::Sqrt | Func::Log | Func::Exp => {
                for a in args {
                    if type_of(a, percent)? != Type::NUMBER {
                        return None;
                    }
                }
                Some(Type::NUMBER)
            }
        },
    }
}

/// Parses a math function with percentages resolving as `percent`.
pub fn parse(cv: &ComponentValue, percent: PercentAs) -> Option<Math> {
    parse_with(cv, percent, &[])
}

/// [`parse`] where the identifiers in `symbols` stand for numbers (the
/// channel keywords of a relative color).
pub fn parse_with(cv: &ComponentValue, percent: PercentAs, symbols: &[&str]) -> Option<Math> {
    let ComponentValue::Function(name, args) = cv else {
        return None;
    };
    if !is_math_function(name) {
        return None;
    }
    let root = Parser { symbols }.function(name, args)?;
    let ty = type_of(&root, percent)?;
    let root = simplify(root, &Resolver::default());
    Some(Math {
        root,
        ty,
        percent,
        calc: name.eq_ignore_ascii_case("calc"),
    })
}

// ----- simplification --------------------------------------------------------------------

/// What simplification may resolve beyond absolute units.
#[derive(Default)]
pub struct Resolver<'a> {
    /// Resolves a relative length (value, unit) to pixels.
    pub relative: Option<&'a dyn Fn(f64, &str) -> Option<f64>>,
    /// The basis percentages resolve against, in the canonical unit of
    /// the category: (basis, category).
    pub percent: Option<(f64, Category)>,
    /// Resolves a symbol (a relative color channel).
    pub symbol: Option<&'a dyn Fn(&str) -> Option<f64>>,
}

/// A numeric leaf: its value and unit (`""` for numbers, `"%"`).
fn leaf(n: &Node) -> Option<(f64, &str)> {
    match n {
        Node::Number(v) => Some((*v, "")),
        Node::Percentage(v) => Some((*v, "%")),
        Node::Dimension(v, u) => Some((*v, u.as_str())),
        _ => None,
    }
}

fn make_leaf(v: f64, unit: &str) -> Node {
    match unit {
        "" => Node::Number(v),
        "%" => Node::Percentage(v),
        u => Node::Dimension(v, u.to_string()),
    }
}

/// Simplifies a calculation tree (css-values-4 §10.10.1).
pub fn simplify(n: Node, r: &Resolver) -> Node {
    match n {
        Node::Number(_) => n,
        Node::Percentage(p) => match r.percent {
            Some((basis, c)) => Node::Dimension(p * basis / 100.0, c.canonical().to_string()),
            None => Node::Percentage(p),
        },
        Node::Dimension(v, u) => {
            if let Some((cv, cu)) = to_canonical(v, &u) {
                return Node::Dimension(cv, cu.to_string());
            }
            if let Some(px) = r.relative.and_then(|f| f(v, &u)) {
                return Node::Dimension(px, "px".to_string());
            }
            Node::Dimension(v, u)
        }
        Node::Symbol(s) => match r.symbol.and_then(|f| f(&s)) {
            Some(v) => Node::Number(v),
            None => Node::Symbol(s),
        },
        Node::Negate(c) => match simplify(*c, r) {
            Node::Number(v) => Node::Number(-v),
            Node::Percentage(v) => Node::Percentage(-v),
            Node::Dimension(v, u) => Node::Dimension(-v, u),
            Node::Negate(g) => *g,
            other => Node::Negate(Box::new(other)),
        },
        Node::Invert(c) => match simplify(*c, r) {
            Node::Number(v) => Node::Number(1.0 / v),
            Node::Invert(g) => *g,
            other => Node::Invert(Box::new(other)),
        },
        Node::Sum(children) => simplify_sum(children.into_iter().map(|c| simplify(c, r)).collect()),
        Node::Product(children) => {
            simplify_product(children.into_iter().map(|c| simplify(c, r)).collect())
        }
        Node::Func(f, args) => simplify_func(f, args.into_iter().map(|c| simplify(c, r)).collect()),
    }
}

fn simplify_sum(children: Vec<Node>) -> Node {
    let mut flat = Vec::new();
    for c in children {
        match c {
            Node::Sum(inner) => flat.extend(inner),
            other => flat.push(other),
        }
    }
    let mut out: Vec<Node> = Vec::new();
    for c in flat {
        if let Some((v, u)) = leaf(&c) {
            let existing = out
                .iter()
                .position(|o| leaf(o).is_some_and(|(_, ou)| ou == u));
            if let Some(i) = existing {
                let (ov, _) = leaf(&out[i]).unwrap_or((0.0, ""));
                let unit_name = u.to_string();
                out[i] = make_leaf(ov + v, &unit_name);
                continue;
            }
        }
        out.push(c);
    }
    if out.len() == 1 {
        out.pop().unwrap_or(Node::Number(0.0))
    } else {
        Node::Sum(out)
    }
}

fn simplify_product(children: Vec<Node>) -> Node {
    let mut flat = Vec::new();
    for c in children {
        match c {
            Node::Product(inner) => flat.extend(inner),
            other => flat.push(other),
        }
    }
    let mut factor = 1.0;
    let mut has_number = false;
    let mut others = Vec::new();
    for c in flat {
        match c {
            Node::Number(v) => {
                factor *= v;
                has_number = true;
            }
            other => others.push(other),
        }
    }
    if others.is_empty() {
        return Node::Number(factor);
    }
    if others.len() == 1 {
        let only = others.pop().unwrap_or(Node::Number(1.0));
        match only {
            Node::Percentage(v) => return Node::Percentage(v * factor),
            Node::Dimension(v, u) => return Node::Dimension(v * factor, u),
            Node::Sum(items) if has_number && items.iter().all(|i| leaf(i).is_some()) => {
                return Node::Sum(
                    items
                        .into_iter()
                        .map(|i| {
                            let (v, u) = leaf(&i).unwrap_or((0.0, ""));
                            make_leaf(v * factor, u)
                        })
                        .collect(),
                );
            }
            other => others.push(other),
        }
    }
    // Dimensions in canonical units that multiply to a number or to a
    // single base type.
    let mut ty = Type::NUMBER;
    let mut value = factor;
    let mut combinable = true;
    for c in &others {
        let (v, u, inverted) = match c {
            Node::Dimension(v, u) => (*v, u.as_str(), false),
            Node::Invert(inner) => match inner.as_ref() {
                Node::Dimension(v, u) => (*v, u.as_str(), true),
                _ => {
                    combinable = false;
                    break;
                }
            },
            _ => {
                combinable = false;
                break;
            }
        };
        let Some(info) = unit(u) else {
            combinable = false;
            break;
        };
        if info.category.canonical() != info.name {
            combinable = false;
            break;
        }
        let t = Type::base(info.category);
        if inverted {
            ty = ty.mul(t.inv());
            value /= v;
        } else {
            ty = ty.mul(t);
            value *= v;
        }
    }
    if combinable {
        if ty == Type::NUMBER {
            return Node::Number(value);
        }
        if let Some(c) = ty.category() {
            return Node::Dimension(value, c.canonical().to_string());
        }
    }
    let mut out = Vec::with_capacity(others.len() + 1);
    if has_number && factor != 1.0 {
        out.push(Node::Number(factor));
    }
    out.extend(others);
    if out.len() == 1 {
        out.pop().unwrap_or(Node::Number(1.0))
    } else {
        Node::Product(out)
    }
}

fn nan_min(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else {
        a.min(b)
    }
}

fn nan_max(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else {
        a.max(b)
    }
}

/// `round(strategy, a, b)` (§10.6).
pub fn round_to(strategy: Rounding, a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() || b == 0.0 || (a.is_infinite() && b.is_infinite()) {
        return f64::NAN;
    }
    if a.is_infinite() {
        return a;
    }
    if b.is_infinite() {
        return match strategy {
            Rounding::Up => {
                if a > 0.0 {
                    f64::INFINITY
                } else if a < 0.0 {
                    -0.0
                } else {
                    a
                }
            }
            Rounding::Down => {
                if a < 0.0 {
                    f64::NEG_INFINITY
                } else if a > 0.0 {
                    0.0
                } else {
                    a
                }
            }
            Rounding::Nearest | Rounding::ToZero => {
                if a.is_sign_negative() {
                    -0.0
                } else {
                    0.0
                }
            }
        };
    }
    let b = b.abs();
    let q = a / b;
    let lo = q.floor() * b;
    let hi = q.ceil() * b;
    match strategy {
        Rounding::Down => lo,
        Rounding::Up => hi,
        Rounding::ToZero => {
            if a >= 0.0 {
                lo
            } else {
                hi
            }
        }
        Rounding::Nearest => {
            if a - lo < hi - a {
                lo
            } else {
                hi
            }
        }
    }
}

fn modulo(a: f64, b: f64) -> f64 {
    if b == 0.0 || a.is_infinite() || a.is_nan() || b.is_nan() {
        return f64::NAN;
    }
    if b.is_infinite() {
        return if a == 0.0 || a.is_sign_negative() == b.is_sign_negative() {
            a
        } else {
            f64::NAN
        };
    }
    let r = a - b * (a / b).floor();
    if r == 0.0 && b < 0.0 { -0.0 } else { r }
}

fn remainder(a: f64, b: f64) -> f64 {
    if b == 0.0 || a.is_infinite() || a.is_nan() || b.is_nan() {
        return f64::NAN;
    }
    if b.is_infinite() {
        return a;
    }
    a % b
}

/// Degrees of a number (radians) or an angle leaf in degrees.
fn radians(v: f64, unit: &str) -> Option<f64> {
    match unit {
        "" => Some(v),
        "deg" => Some(v.to_radians()),
        _ => None,
    }
}

fn simplify_func(f: Func, mut args: Vec<Node>) -> Node {
    let leaves: Option<Vec<(f64, String)>> = args
        .iter()
        .map(|a| leaf(a).map(|(v, u)| (v, u.to_string())))
        .collect();
    let consistent = leaves
        .as_ref()
        .is_some_and(|l| l.iter().all(|(_, u)| *u == l[0].1));
    match f {
        Func::Min | Func::Max => {
            let combine = if f == Func::Min { nan_min } else { nan_max };
            // Combine the children of each unit.
            let mut out: Vec<Node> = Vec::new();
            for a in args {
                if let Some((v, u)) = leaf(&a) {
                    let unit_name = u.to_string();
                    if let Some(i) = out
                        .iter()
                        .position(|o| leaf(o).is_some_and(|(_, ou)| ou == unit_name))
                    {
                        let (ov, _) = leaf(&out[i]).unwrap_or((0.0, ""));
                        out[i] = make_leaf(combine(ov, v), &unit_name);
                        continue;
                    }
                }
                out.push(a);
            }
            if out.len() == 1 {
                return out.pop().unwrap_or(Node::Number(0.0));
            }
            Node::Func(f, out)
        }
        _ if !consistent => {
            if f == Func::Sign {
                if let Some(Node::Number(v) | Node::Dimension(v, _)) = args.first() {
                    return Node::Number(sign(*v));
                }
            }
            if matches!(f, Func::Sin | Func::Cos | Func::Tan) {
                if let Some((v, u)) = args.first().and_then(leaf) {
                    if let Some(rad) = radians(v, u) {
                        return Node::Number(trig(f, rad, v, u));
                    }
                }
            }
            Node::Func(f, args)
        }
        _ => {
            let l = leaves.unwrap_or_default();
            let unit_name = l.first().map(|(_, u)| u.clone()).unwrap_or_default();
            let v: Vec<f64> = l.iter().map(|(v, _)| *v).collect();
            let result = match f {
                Func::Clamp => make_leaf(nan_max(v[0], nan_min(v[1], v[2])), &unit_name),
                Func::Round(s) => make_leaf(round_to(s, v[0], v[1]), &unit_name),
                Func::Mod => make_leaf(modulo(v[0], v[1]), &unit_name),
                Func::Rem => make_leaf(remainder(v[0], v[1]), &unit_name),
                Func::Abs => {
                    if unit_name == "%" {
                        return Node::Func(f, args);
                    }
                    make_leaf(v[0].abs(), &unit_name)
                }
                Func::Sign => {
                    if unit_name == "%" {
                        return Node::Func(f, args);
                    }
                    Node::Number(sign(v[0]))
                }
                Func::Hypot => {
                    if unit_name == "%" {
                        return Node::Func(f, args);
                    }
                    make_leaf(v.iter().map(|x| x * x).sum::<f64>().sqrt(), &unit_name)
                }
                Func::Sin | Func::Cos | Func::Tan => match radians(v[0], &unit_name) {
                    Some(rad) => Node::Number(trig(f, rad, v[0], &unit_name)),
                    None => return Node::Func(f, args),
                },
                Func::Asin | Func::Acos | Func::Atan => {
                    if !unit_name.is_empty() {
                        return Node::Func(f, args);
                    }
                    let r = match f {
                        Func::Asin => v[0].asin(),
                        Func::Acos => v[0].acos(),
                        _ => v[0].atan(),
                    };
                    Node::Dimension(r.to_degrees(), "deg".to_string())
                }
                Func::Atan2 => {
                    if unit_name == "%" {
                        return Node::Func(f, args);
                    }
                    Node::Dimension(v[0].atan2(v[1]).to_degrees(), "deg".to_string())
                }
                Func::Pow => Node::Number(v[0].powf(v[1])),
                Func::Sqrt => Node::Number(v[0].sqrt()),
                Func::Exp => Node::Number(v[0].exp()),
                Func::Log => {
                    if v.len() == 2 {
                        Node::Number(v[0].ln() / v[1].ln())
                    } else {
                        Node::Number(v[0].ln())
                    }
                }
                Func::Min | Func::Max => unreachable_min_max(&mut args),
            };
            // Percentages cannot be compared without their basis, except
            // by functions that keep their unit.
            if unit_name == "%" && matches!(f, Func::Round(_) | Func::Mod | Func::Rem) {
                return Node::Func(f, args);
            }
            result
        }
    }
}

fn unreachable_min_max(args: &mut Vec<Node>) -> Node {
    args.pop().unwrap_or(Node::Number(0.0))
}

fn sign(v: f64) -> f64 {
    if v.is_nan() || v == 0.0 {
        v
    } else if v > 0.0 {
        1.0
    } else {
        -1.0
    }
}

fn trig(f: Func, rad: f64, v: f64, unit: &str) -> f64 {
    if f == Func::Tan && unit == "deg" && v.is_finite() {
        let d = v.rem_euclid(360.0);
        if d == 90.0 {
            return f64::INFINITY;
        }
        if d == 270.0 {
            return f64::NEG_INFINITY;
        }
    }
    match f {
        Func::Sin => rad.sin(),
        Func::Cos => rad.cos(),
        _ => rad.tan(),
    }
}

// ----- serialization ---------------------------------------------------------------------

fn sort_key(n: &Node) -> (u8, String) {
    match n {
        Node::Number(_) => (0, String::new()),
        Node::Percentage(_) => (1, String::new()),
        Node::Dimension(_, u) => (2, u.to_ascii_lowercase()),
        _ => (3, String::new()),
    }
}

fn sorted(children: &[Node]) -> Vec<&Node> {
    let mut v: Vec<&Node> = children.iter().collect();
    v.sort_by(|a, b| sort_key(a).cmp(&sort_key(b)));
    v
}

fn leaf_text(v: f64, unit: &str, nested: bool) -> String {
    if v.is_finite() || unit.is_empty() {
        return format!("{}{}", number(v), unit);
    }
    let s = format!("{} * 1{}", number(v), unit);
    if nested { format!("({s})") } else { s }
}

fn is_negative_leaf(n: &Node) -> bool {
    leaf(n).is_some_and(|(v, _)| v < 0.0 || (v == f64::NEG_INFINITY))
}

/// Serializes a calculation tree node; `nested` wraps operator nodes in
/// parentheses.
pub fn serialize_node(n: &Node, nested: bool) -> String {
    match n {
        Node::Number(v) => number(*v),
        Node::Percentage(v) => leaf_text(*v, "%", nested),
        Node::Dimension(v, u) => leaf_text(*v, u, nested),
        Node::Symbol(s) => s.clone(),
        Node::Sum(children) => {
            let items = sorted(children);
            let mut s = String::new();
            for (i, c) in items.iter().enumerate() {
                if i == 0 {
                    s.push_str(&serialize_node(c, true));
                    continue;
                }
                match c {
                    Node::Negate(inner) => {
                        s.push_str(" - ");
                        s.push_str(&serialize_node(inner, true));
                    }
                    _ if is_negative_leaf(c) => {
                        let (v, u) = leaf(c).unwrap_or((0.0, ""));
                        s.push_str(" - ");
                        s.push_str(&serialize_node(&make_leaf(-v, u), true));
                    }
                    _ => {
                        s.push_str(" + ");
                        s.push_str(&serialize_node(c, true));
                    }
                }
            }
            if nested { format!("({s})") } else { s }
        }
        Node::Product(children) => {
            let items = sorted(children);
            let mut s = String::new();
            for (i, c) in items.iter().enumerate() {
                match c {
                    Node::Invert(inner) if i > 0 => {
                        s.push_str(" / ");
                        s.push_str(&serialize_node(inner, true));
                    }
                    _ => {
                        if i > 0 {
                            s.push_str(" * ");
                        }
                        s.push_str(&serialize_node(c, true));
                    }
                }
            }
            if nested { format!("({s})") } else { s }
        }
        Node::Negate(c) => format!("(-1 * {})", serialize_node(c, true)),
        Node::Invert(c) => format!("(1 / {})", serialize_node(c, true)),
        Node::Func(f, args) => {
            let mut parts: Vec<String> = Vec::new();
            let mut args: &[Node] = args;
            if let Func::Round(s) = f {
                match s {
                    Rounding::Nearest => {}
                    Rounding::Up => parts.push("up".to_string()),
                    Rounding::Down => parts.push("down".to_string()),
                    Rounding::ToZero => parts.push("to-zero".to_string()),
                }
                if args.len() == 2 && args[1] == Node::Number(1.0) {
                    args = &args[..1];
                }
            }
            parts.extend(args.iter().map(|a| serialize_node(a, false)));
            format!("{}({})", f.name(), parts.join(", "))
        }
    }
}

impl Math {
    /// Serializes the math function as a specified value (§10.13).
    pub fn to_css(&self) -> String {
        match &self.root {
            Node::Func(..) if !self.calc => serialize_node(&self.root, false),
            root => format!("calc({})", serialize_node(root, false)),
        }
    }

    /// The tree simplified with `r` (computed-value time).
    pub fn resolve(&self, r: &Resolver) -> Node {
        simplify(self.root.clone(), r)
    }

    /// The value of the function when it simplifies to a single numeric
    /// value with `r`: (value, unit) with `""` for numbers and `"%"`.
    pub fn value(&self, r: &Resolver) -> Option<(f64, String)> {
        let n = self.resolve(r);
        leaf(&n).map(|(v, u)| (v, u.to_string()))
    }

    /// A math function with `root` as its tree, keeping this function's
    /// type.
    pub fn with_root(&self, root: Node) -> Math {
        Math {
            root,
            ty: self.ty,
            percent: self.percent,
            calc: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::css::parser::parse_component_values;

    fn css(text: &str, percent: PercentAs) -> Option<String> {
        let cv = parse_component_values(text);
        parse(&cv[0], percent).map(|m| m.to_css())
    }

    #[test]
    fn simplifies_and_sorts_sums() {
        let lp = PercentAs::Base(Category::Length);
        assert_eq!(css("calc(1px + 2px)", lp).as_deref(), Some("calc(3px)"));
        assert_eq!(css("calc(1in + 1px)", lp).as_deref(), Some("calc(97px)"));
        assert_eq!(
            css("calc(2em + 10% - 1px)", lp).as_deref(),
            Some("calc(10% + 2em - 1px)")
        );
        assert_eq!(
            css("calc(2 * (1px + 1em))", lp).as_deref(),
            Some("calc(2em + 2px)")
        );
        assert_eq!(css("min(1px, 2px)", lp).as_deref(), Some("calc(1px)"));
        assert_eq!(css("min(1px, 1em)", lp).as_deref(), Some("min(1px, 1em)"));
        assert_eq!(css("calc(1px + 1)", lp), None);
        assert_eq!(css("calc(1px+1px)", lp), None);
    }

    #[test]
    fn infinities_and_nan() {
        let lp = PercentAs::Base(Category::Length);
        assert_eq!(
            css("calc(1px * infinity)", lp).as_deref(),
            Some("calc(infinity * 1px)")
        );
        assert_eq!(
            css("calc(1px * NaN)", lp).as_deref(),
            Some("calc(NaN * 1px)")
        );
        assert_eq!(
            css("calc(1px * 1/infinity)", lp).as_deref(),
            Some("calc(0px)")
        );
    }

    #[test]
    fn stepped_and_trigonometric_functions() {
        let n = PercentAs::Invalid;
        assert_eq!(css("round(2.5)", n).as_deref(), Some("calc(3)"));
        assert_eq!(css("round(down, 7, 2)", n).as_deref(), Some("calc(6)"));
        assert_eq!(css("mod(-7, 3)", n).as_deref(), Some("calc(2)"));
        assert_eq!(css("rem(-7, 3)", n).as_deref(), Some("calc(-1)"));
        assert_eq!(css("sin(30deg)", n).as_deref(), Some("calc(0.5)"));
        let cv = parse_component_values("asin(1)");
        let m = parse(&cv[0], n).unwrap();
        assert_eq!(m.ty, Type::base(Category::Angle));
        assert_eq!(m.to_css(), "calc(90deg)");
    }
}
