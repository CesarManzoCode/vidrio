//! The value definition syntax of css-values-4 §2 and its matcher: a
//! grammar string (`<length [0,∞]> | auto`, `[ a || b ]#`, `minmax( … )`)
//! parses into a tree that is matched with backtracking against the
//! significant component values of a declaration. Juxtaposition, `&&`,
//! `||`, `|`, brackets, the multipliers `?`, `*`, `+`, `#`, `{A,B}`, `#{A,B}`
//! and `!`, literal `,` and `/`, functions and ranges on numeric types are
//! supported. Components of `&&` and `||` come out in the order of the
//! grammar (the canonical order of CSSOM's specified values).
//!
//! The grammars of the properties live in the property table
//! ([`crate::css::properties::property`]); the shared productions
//! (`<position>`, `<line-style>`, …) live in [`DEFINITIONS`].

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use super::{
    Numeric, Range, Value, color, image, parse_custom_ident, parse_dashed_ident, parse_numeric,
    parse_string, parse_url, serialize_list, significant, to_components, tokens_of,
};
use crate::css::parser::ComponentValue;
use crate::css::properties;
use crate::css::tokenizer::Token;

// ----- the grammar tree ------------------------------------------------------------------

/// A node of a parsed grammar.
#[derive(Clone, Debug, PartialEq)]
pub enum G {
    Keyword(String),
    /// A data type or a named production, with its range.
    Type(String, Range),
    /// Juxtaposition.
    Seq(Vec<G>),
    /// `&&`.
    All(Vec<G>),
    /// `||`.
    Any(Vec<G>),
    /// `|`.
    One(Vec<G>),
    /// A multiplier: the node, minimum and maximum counts, and whether
    /// repetitions are separated by commas.
    Repeat(Box<G>, usize, usize, bool),
    /// `!`: the group must produce at least one value.
    NonEmpty(Box<G>),
    /// A function with its argument grammar.
    Func(String, Box<G>),
    Comma,
    Slash,
}

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Word(String),
    Func(String),
    Type(String, Range),
    Open,
    Close,
    CloseParen,
    Bar,
    DoubleBar,
    DoubleAmp,
    Question,
    Star,
    Plus,
    Hash,
    Bang,
    Braces(usize, usize),
    Comma,
    Slash,
}

fn parse_bound(s: &str) -> Option<f64> {
    match s.trim() {
        "∞" | "+∞" => Some(f64::INFINITY),
        "-∞" => Some(f64::NEG_INFINITY),
        other => other.parse().ok(),
    }
}

fn lex(src: &str) -> Option<Vec<Tok>> {
    let chars: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        i += 1;
        let tok = match c {
            ' ' | '\n' | '\t' => continue,
            '[' => Tok::Open,
            ']' => Tok::Close,
            ')' => Tok::CloseParen,
            ',' => Tok::Comma,
            '/' => Tok::Slash,
            '?' => Tok::Question,
            '*' => Tok::Star,
            '+' => Tok::Plus,
            '#' => Tok::Hash,
            '!' => Tok::Bang,
            '|' => {
                if chars.get(i) == Some(&'|') {
                    i += 1;
                    Tok::DoubleBar
                } else {
                    Tok::Bar
                }
            }
            '&' => {
                if chars.get(i) != Some(&'&') {
                    return None;
                }
                i += 1;
                Tok::DoubleAmp
            }
            '{' => {
                let start = i;
                while i < chars.len() && chars[i] != '}' {
                    i += 1;
                }
                let inner: String = chars[start..i].iter().collect();
                i += 1;
                let (a, b) = match inner.split_once(',') {
                    Some((a, "")) => (a.trim().parse().ok()?, usize::MAX),
                    Some((a, b)) => (a.trim().parse().ok()?, b.trim().parse().ok()?),
                    None => {
                        let n = inner.trim().parse().ok()?;
                        (n, n)
                    }
                };
                Tok::Braces(a, b)
            }
            '<' => {
                let start = i;
                while i < chars.len() && chars[i] != '>' {
                    i += 1;
                }
                let inner: String = chars[start..i].iter().collect();
                i += 1;
                let (name, range) = match inner.split_once(' ') {
                    Some((name, range)) => {
                        let r = range.trim().strip_prefix('[')?.strip_suffix(']')?;
                        let (lo, hi) = r.split_once(',')?;
                        (name.to_string(), Some((parse_bound(lo)?, parse_bound(hi)?)))
                    }
                    None => (inner.clone(), None),
                };
                Tok::Type(name, range)
            }
            c if c.is_ascii_alphanumeric() || c == '-' => {
                let start = i - 1;
                while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '-') {
                    i += 1;
                }
                let word: String = chars[start..i].iter().collect();
                if chars.get(i) == Some(&'(') {
                    i += 1;
                    Tok::Func(word)
                } else {
                    Tok::Word(word)
                }
            }
            _ => return None,
        };
        out.push(tok);
    }
    Some(out)
}

struct GrammarParser {
    t: Vec<Tok>,
    i: usize,
}

impl GrammarParser {
    fn peek(&self) -> Option<&Tok> {
        self.t.get(self.i)
    }

    fn list(
        &mut self,
        sep: Tok,
        next: fn(&mut Self) -> Option<G>,
        wrap: fn(Vec<G>) -> G,
    ) -> Option<G> {
        let mut items = vec![next(self)?];
        while self.peek() == Some(&sep) {
            self.i += 1;
            items.push(next(self)?);
        }
        if items.len() == 1 {
            items.pop()
        } else {
            Some(wrap(items))
        }
    }

    fn one(&mut self) -> Option<G> {
        self.list(Tok::Bar, Self::any, G::One)
    }

    fn any(&mut self) -> Option<G> {
        self.list(Tok::DoubleBar, Self::all, G::Any)
    }

    fn all(&mut self) -> Option<G> {
        self.list(Tok::DoubleAmp, Self::seq, G::All)
    }

    fn seq(&mut self) -> Option<G> {
        let mut items = Vec::new();
        while let Some(t) = self.peek() {
            if matches!(
                t,
                Tok::Bar | Tok::DoubleBar | Tok::DoubleAmp | Tok::Close | Tok::CloseParen
            ) {
                break;
            }
            items.push(self.term()?);
        }
        match items.len() {
            0 => None,
            1 => items.pop(),
            _ => Some(G::Seq(items)),
        }
    }

    fn term(&mut self) -> Option<G> {
        let mut g = self.primary()?;
        loop {
            let (min, max, comma) = match self.peek() {
                Some(Tok::Question) => (0, 1, false),
                Some(Tok::Star) => (0, usize::MAX, false),
                Some(Tok::Plus) => (1, usize::MAX, false),
                Some(Tok::Braces(a, b)) => (*a, *b, false),
                Some(Tok::Hash) => {
                    self.i += 1;
                    if let Some(Tok::Braces(a, b)) = self.peek() {
                        let (a, b) = (*a, *b);
                        self.i += 1;
                        g = G::Repeat(Box::new(g), a, b, true);
                    } else {
                        g = G::Repeat(Box::new(g), 1, usize::MAX, true);
                    }
                    continue;
                }
                Some(Tok::Bang) => {
                    self.i += 1;
                    g = G::NonEmpty(Box::new(g));
                    continue;
                }
                _ => return Some(g),
            };
            self.i += 1;
            g = G::Repeat(Box::new(g), min, max, comma);
        }
    }

    fn primary(&mut self) -> Option<G> {
        let t = self.t.get(self.i)?.clone();
        self.i += 1;
        Some(match t {
            Tok::Word(w) => G::Keyword(w),
            Tok::Type(n, r) => G::Type(n, r),
            Tok::Comma => G::Comma,
            Tok::Slash => G::Slash,
            Tok::Open => {
                let g = self.one()?;
                if self.peek() != Some(&Tok::Close) {
                    return None;
                }
                self.i += 1;
                g
            }
            Tok::Func(name) => {
                let g = self.one()?;
                if self.peek() != Some(&Tok::CloseParen) {
                    return None;
                }
                self.i += 1;
                G::Func(name, Box::new(g))
            }
            _ => return None,
        })
    }
}

/// Parses a grammar string.
pub fn parse_grammar(src: &str) -> Option<G> {
    let mut p = GrammarParser { t: lex(src)?, i: 0 };
    let g = p.one()?;
    (p.i == p.t.len()).then_some(g)
}

thread_local! {
    static CACHE: RefCell<HashMap<&'static str, Option<Rc<G>>>> = RefCell::new(HashMap::new());
}

/// The parsed form of a static grammar string, cached.
pub fn compiled(src: &'static str) -> Option<Rc<G>> {
    CACHE.with(|c| {
        c.borrow_mut()
            .entry(src)
            .or_insert_with(|| parse_grammar(src).map(Rc::new))
            .clone()
    })
}

// ----- shared productions ----------------------------------------------------------------

/// Named productions shared by property grammars.
pub static DEFINITIONS: &[(&str, &str)] = &[
    (
        "line-style",
        "none | hidden | dotted | dashed | solid | double | groove | ridge | inset | outset",
    ),
    ("line-width", "<length [0,∞]> | thin | medium | thick"),
    (
        "position",
        "[ left | center | right | top | bottom | <length-percentage> ] \
         | [ left | center | right | <length-percentage> ] [ top | center | bottom | <length-percentage> ] \
         | [ center | [ left | right ] <length-percentage>? ] && [ center | [ top | bottom ] <length-percentage>? ]",
    ),
    (
        "bg-size",
        "[ <length-percentage [0,∞]> | auto ]{1,2} | cover | contain",
    ),
    (
        "repeat-style",
        "repeat-x | repeat-y | [ repeat | space | round | no-repeat ]{1,2}",
    ),
    ("box", "border-box | padding-box | content-box"),
    (
        "blend-mode",
        "normal | multiply | screen | overlay | darken | lighten | color-dodge | color-burn \
         | hard-light | soft-light | difference | exclusion | hue | saturation | color | luminosity",
    ),
    (
        "easing-function",
        "linear | ease | ease-in | ease-out | ease-in-out | step-start | step-end \
         | cubic-bezier( <number [0,1]> , <number> , <number [0,1]> , <number> ) \
         | steps( <integer [1,∞]> [ , <step-position> ]? )",
    ),
    (
        "step-position",
        "jump-start | jump-end | jump-none | jump-both | start | end",
    ),
    (
        "shadow",
        "<color>? && [ <length>{2} <length [0,∞]>? <length>? ] && inset?",
    ),
    (
        "text-shadow-item",
        "<color>? && [ <length>{2} <length [0,∞]>? ]",
    ),
    (
        "content-position",
        "center | start | end | flex-start | flex-end",
    ),
    (
        "content-distribution",
        "space-between | space-around | space-evenly | stretch",
    ),
    ("overflow-position", "unsafe | safe"),
    (
        "self-position",
        "center | start | end | self-start | self-end | flex-start | flex-end",
    ),
    ("baseline-position", "[ first | last ]? && baseline"),
    (
        "size",
        "auto | <length-percentage [0,∞]> | min-content | max-content | fit-content \
         | fit-content( <length-percentage [0,∞]> ) | stretch",
    ),
    (
        "max-size",
        "none | <length-percentage [0,∞]> | min-content | max-content | fit-content \
         | fit-content( <length-percentage [0,∞]> ) | stretch",
    ),
    (
        "track-breadth",
        "<length-percentage [0,∞]> | <flex [0,∞]> | min-content | max-content | auto",
    ),
    (
        "inflexible-breadth",
        "<length-percentage [0,∞]> | min-content | max-content | auto",
    ),
    ("fixed-breadth", "<length-percentage [0,∞]>"),
    (
        "track-size",
        "<track-breadth> | minmax( <inflexible-breadth> , <track-breadth> ) \
         | fit-content( <length-percentage [0,∞]> )",
    ),
    (
        "fixed-size",
        "<fixed-breadth> | minmax( <fixed-breadth> , <track-breadth> ) \
         | minmax( <inflexible-breadth> , <fixed-breadth> )",
    ),
    (
        "track-repeat",
        "repeat( <integer [1,∞]> , [ <line-names>? <track-size> ]+ <line-names>? )",
    ),
    (
        "fixed-repeat",
        "repeat( <integer [1,∞]> , [ <line-names>? <fixed-size> ]+ <line-names>? )",
    ),
    (
        "auto-repeat",
        "repeat( [ auto-fill | auto-fit ] , [ <line-names>? <fixed-size> ]+ <line-names>? )",
    ),
    (
        "track-list",
        "[ <line-names>? [ <track-size> | <track-repeat> ] ]+ <line-names>?",
    ),
    (
        "auto-track-list",
        "[ <line-names>? [ <fixed-size> | <fixed-repeat> ] ]* <line-names>? <auto-repeat> \
         [ <line-names>? [ <fixed-size> | <fixed-repeat> ] ]* <line-names>?",
    ),
    (
        "line-name-list",
        "[ <line-names> | repeat( [ <integer [1,∞]> | auto-fill ] , <line-names>+ ) ]+",
    ),
    (
        "grid-line",
        "auto | <custom-ident> | [ <integer> && <custom-ident>? ] \
         | [ span && [ <integer [1,∞]> || <custom-ident> ] ]",
    ),
    (
        "paint",
        "none | <color> | <url> [ none | <color> ]? | context-fill | context-stroke",
    ),
    ("ratio", "<number [0,∞]> [ / <number [0,∞]> ]?"),
    (
        "absolute-size",
        "xx-small | x-small | small | medium | large | x-large | xx-large | xxx-large",
    ),
    (
        "generic-family",
        "serif | sans-serif | cursive | fantasy | monospace | system-ui | emoji | math \
         | fangsong | ui-serif | ui-sans-serif | ui-monospace | ui-rounded",
    ),
];

fn definition(name: &str) -> Option<&'static str> {
    DEFINITIONS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, src)| *src)
}

/// The transform functions (css-transforms-1/2).
const TRANSFORM_FUNCTIONS: &[&str] = &[
    "matrix",
    "matrix3d",
    "translate",
    "translatex",
    "translatey",
    "translatez",
    "translate3d",
    "scale",
    "scalex",
    "scaley",
    "scalez",
    "scale3d",
    "rotate",
    "rotatex",
    "rotatey",
    "rotatez",
    "rotate3d",
    "skew",
    "skewx",
    "skewy",
    "perspective",
];

// ----- matching --------------------------------------------------------------------------

/// The possible matches from a position: (end, values), first preferred,
/// one per end.
type Res = Vec<(usize, Vec<Value>)>;

fn push_unique(out: &mut Res, end: usize, values: Vec<Value>) {
    if !out.iter().any(|(e, _)| *e == end) {
        out.push((end, values));
    }
}

fn collect_keywords(g: &G, out: &mut Vec<String>) {
    match g {
        G::Keyword(k) => out.push(k.to_ascii_lowercase()),
        G::Seq(v) | G::All(v) | G::Any(v) | G::One(v) => {
            for c in v {
                collect_keywords(c, out);
            }
        }
        G::Repeat(c, ..) | G::NonEmpty(c) | G::Func(_, c) => collect_keywords(c, out),
        G::Type(name, _) => {
            if let Some(g) = definition(name).and_then(compiled) {
                collect_keywords(&g, out);
            }
        }
        _ => {}
    }
}

struct Matcher {
    /// Keywords a `<custom-ident>` may not be.
    excluded: Vec<String>,
}

impl Matcher {
    fn m(&self, g: &G, input: &[ComponentValue], pos: usize) -> Res {
        match g {
            G::Keyword(k) => match input.get(pos).and_then(ComponentValue::ident) {
                Some(s) if s.eq_ignore_ascii_case(k) => {
                    vec![(pos + 1, vec![Value::Keyword(k.clone())])]
                }
                _ => Vec::new(),
            },
            G::Comma => match input.get(pos) {
                Some(ComponentValue::Token(Token::Comma)) => vec![(pos + 1, vec![Value::Comma])],
                _ => Vec::new(),
            },
            G::Slash => match input.get(pos) {
                Some(ComponentValue::Token(Token::Delim('/'))) => {
                    vec![(pos + 1, vec![Value::Slash])]
                }
                _ => Vec::new(),
            },
            G::Type(name, range) => self.leaf(name, *range, input, pos),
            G::Seq(children) => {
                let mut states: Res = vec![(pos, Vec::new())];
                for child in children {
                    let mut next = Res::new();
                    for (p, vals) in &states {
                        for (e, v) in self.m(child, input, *p) {
                            if next.iter().any(|(q, _)| *q == e) {
                                continue;
                            }
                            let mut all = vals.clone();
                            all.extend(v);
                            next.push((e, all));
                        }
                    }
                    states = next;
                    if states.is_empty() {
                        break;
                    }
                }
                states
            }
            G::One(alts) => {
                let mut out = Res::new();
                for a in alts {
                    for (e, v) in self.m(a, input, pos) {
                        push_unique(&mut out, e, v);
                    }
                }
                out
            }
            G::All(alts) => {
                let mut out = Res::new();
                self.combine(alts, true, input, pos, 0, Vec::new(), &mut out);
                out
            }
            G::Any(alts) => {
                let mut out = Res::new();
                self.combine(alts, false, input, pos, 0, Vec::new(), &mut out);
                out
            }
            G::Repeat(child, min, max, comma) => self.repeat(child, *min, *max, *comma, input, pos),
            G::NonEmpty(child) => self
                .m(child, input, pos)
                .into_iter()
                .filter(|(_, v)| !v.is_empty())
                .collect(),
            G::Func(name, inner) => match input.get(pos) {
                Some(ComponentValue::Function(n, args)) if n.eq_ignore_ascii_case(name) => {
                    let args = significant(args);
                    match self
                        .m(inner, &args, 0)
                        .into_iter()
                        .find(|(e, _)| *e == args.len())
                    {
                        Some((_, v)) => {
                            vec![(pos + 1, vec![Value::Function(name.to_ascii_lowercase(), v)])]
                        }
                        None => Vec::new(),
                    }
                }
                _ => Vec::new(),
            },
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn combine(
        &self,
        alts: &[G],
        all: bool,
        input: &[ComponentValue],
        pos: usize,
        used: u64,
        acc: Vec<(usize, Vec<Value>)>,
        out: &mut Res,
    ) {
        let full = (1u64 << alts.len()) - 1;
        if (all && used == full) || (!all && used != 0) {
            let mut sorted = acc.clone();
            sorted.sort_by_key(|(i, _)| *i);
            let vals: Vec<Value> = sorted.into_iter().flat_map(|(_, v)| v).collect();
            push_unique(out, pos, vals);
        }
        if used == full {
            return;
        }
        for (i, alt) in alts.iter().enumerate() {
            if used & (1 << i) != 0 {
                continue;
            }
            for (e, v) in self.m(alt, input, pos) {
                // An empty match only counts for `&&`, where optional
                // components may be absent.
                if e == pos && !all {
                    continue;
                }
                let mut next = acc.clone();
                next.push((i, v));
                self.combine(alts, all, input, e, used | (1 << i), next, out);
            }
        }
    }

    fn repeat(
        &self,
        child: &G,
        min: usize,
        max: usize,
        comma: bool,
        input: &[ComponentValue],
        pos: usize,
    ) -> Res {
        let mut results = Res::new();
        if min == 0 {
            results.push((pos, Vec::new()));
        }
        let mut frontier: Res = vec![(pos, Vec::new())];
        let mut count = 0;
        while count < max && !frontier.is_empty() && count <= input.len() {
            let mut next = Res::new();
            for (p, vals) in &frontier {
                let mut start = *p;
                if comma && count > 0 {
                    if !matches!(input.get(start), Some(ComponentValue::Token(Token::Comma))) {
                        continue;
                    }
                    start += 1;
                }
                for (e, v) in self.m(child, input, start) {
                    if e == start {
                        continue;
                    }
                    if next.iter().any(|(q, _)| *q == e) {
                        continue;
                    }
                    let mut all = vals.clone();
                    if comma && count > 0 {
                        all.push(Value::Comma);
                    }
                    all.extend(v);
                    next.push((e, all));
                }
            }
            count += 1;
            if count >= min {
                for (e, v) in &next {
                    push_unique(&mut results, *e, v.clone());
                }
            }
            frontier = next;
        }
        results
    }

    fn leaf(&self, name: &str, range: Range, input: &[ComponentValue], pos: usize) -> Res {
        if let Some(src) = definition(name) {
            return match compiled(src) {
                Some(g) => self.m(&g, input, pos),
                None => Vec::new(),
            };
        }
        let Some(cv) = input.get(pos) else {
            return Vec::new();
        };
        let one = |v: Option<Value>| match v {
            Some(v) => vec![(pos + 1, vec![v])],
            None => Vec::new(),
        };
        if let Some(ty) = numeric_type(name) {
            return one(parse_numeric(cv, ty, range));
        }
        match name {
            "color" => one(color::parse(cv).map(|c| Value::Color(Box::new(c)))),
            "image" => one(image::parse(cv)),
            "url" => one(parse_url(cv)),
            "string" => one(parse_string(cv)),
            "custom-ident" => {
                let excluded: Vec<&str> = self.excluded.iter().map(String::as_str).collect();
                one(parse_custom_ident(cv, &excluded))
            }
            "dashed-ident" => one(parse_dashed_ident(cv)),
            "ident" => one(cv
                .ident()
                .filter(|s| !properties::is_css_wide_keyword(s))
                .map(|s| Value::Ident(s.to_string()))),
            "line-names" => one(line_names(cv)),
            "transform-function" => one(match cv {
                ComponentValue::Function(n, _)
                    if TRANSFORM_FUNCTIONS.contains(&n.to_ascii_lowercase().as_str()) =>
                {
                    Some(Value::Raw(tokens_of(cv)))
                }
                _ => None,
            }),
            _ => Vec::new(),
        }
    }
}

fn numeric_type(name: &str) -> Option<Numeric> {
    Some(match name {
        "length" => Numeric::Length,
        "length-percentage" => Numeric::LengthPercentage,
        "percentage" => Numeric::Percentage,
        "number" => Numeric::Number,
        "integer" => Numeric::Integer,
        "angle" => Numeric::Angle,
        "angle-percentage" => Numeric::AnglePercentage,
        "time" => Numeric::Time,
        "time-percentage" => Numeric::TimePercentage,
        "frequency" => Numeric::Frequency,
        "resolution" => Numeric::Resolution,
        "flex" => Numeric::Flex,
        _ => return None,
    })
}

/// `[ <custom-ident>* ]` of grid line names.
fn line_names(cv: &ComponentValue) -> Option<Value> {
    let ComponentValue::Block('[', inner) = cv else {
        return None;
    };
    let mut names = Vec::new();
    for c in significant(inner) {
        names.push(parse_custom_ident(&c, &["span", "auto"])?);
    }
    Some(Value::Block(names))
}

/// Matches `input` (significant component values) against `g`
/// completely.
pub fn match_grammar(g: &G, input: &[ComponentValue]) -> Option<Vec<Value>> {
    let mut excluded = Vec::new();
    collect_keywords(g, &mut excluded);
    let m = Matcher { excluded };
    m.m(g, input, 0)
        .into_iter()
        .find(|(e, _)| *e == input.len())
        .map(|(_, v)| v)
}

/// Matches component values (whitespace allowed) against a grammar
/// string.
pub fn match_str(src: &str, input: &[ComponentValue]) -> Option<Vec<Value>> {
    let g = parse_grammar(src)?;
    match_grammar(&g, &significant(input))
}

/// Matches against a named production (`position`, …).
pub fn match_production(name: &str, input: &[ComponentValue]) -> Option<Vec<Value>> {
    let g = compiled(definition(name)?)?;
    match_grammar(&g, input)
}

// ----- properties ------------------------------------------------------------------------

/// The specified value of property `name` from its tokens: `None` when
/// the property has no grammar here (its value is kept as written),
/// `Some(None)` when the value does not match the grammar. The caller
/// handles CSS-wide keywords and `var()`.
pub fn parse_property(name: &str, tokens: &[Token]) -> Option<Option<Vec<Value>>> {
    let src = properties::property(name)?.grammar?;
    let g = compiled(src)?;
    let input = significant(&to_components(tokens));
    Some(match_grammar(&g, &input).map(|v| canonical(name, v)))
}

/// The serialization of the specified value of `name` (see
/// [`parse_property`]).
pub fn specified(name: &str, tokens: &[Token]) -> Option<Option<String>> {
    parse_property(name, tokens).map(|v| v.map(|v| serialize_list(&v)))
}

/// The canonical forms CSSOM asks for beyond the grammar order.
fn canonical(name: &str, values: Vec<Value>) -> Vec<Value> {
    match name {
        "display" => display(values),
        "border-top-left-radius"
        | "border-top-right-radius"
        | "border-bottom-right-radius"
        | "border-bottom-left-radius"
        | "border-start-start-radius"
        | "border-start-end-radius"
        | "border-end-start-radius"
        | "border-end-end-radius"
        | "border-spacing" => match values.as_slice() {
            [a, b] if a == b => vec![a.clone()],
            _ => values,
        },
        _ => values,
    }
}

/// The shortest serialization of `display` (css-display-3 §2.8).
fn display(values: Vec<Value>) -> Vec<Value> {
    let words: Vec<&str> = values.iter().filter_map(Value::keyword).collect();
    if words.len() != values.len() {
        return values;
    }
    let outside_of = |w: &str| matches!(w, "block" | "inline" | "run-in");
    let inside_of = |w: &str| {
        matches!(
            w,
            "flow" | "flow-root" | "table" | "flex" | "grid" | "ruby" | "math"
        )
    };
    let list_item = words.contains(&"list-item");
    if !words
        .iter()
        .all(|w| outside_of(*w) || inside_of(*w) || *w == "list-item")
    {
        return values;
    }
    let outside = words.iter().copied().find(|w| outside_of(*w));
    let inside = words.iter().copied().find(|w| inside_of(*w));
    let kw = |s: &str| Value::Keyword(s.to_string());
    if list_item {
        let mut out = Vec::new();
        if let Some(o) = outside.filter(|o| *o != "block") {
            out.push(kw(o));
        }
        if inside == Some("flow-root") {
            out.push(kw("flow-root"));
        }
        out.push(kw("list-item"));
        return out;
    }
    let inside = inside.unwrap_or("flow");
    let outside = outside.unwrap_or(if matches!(inside, "ruby" | "math") {
        "inline"
    } else {
        "block"
    });
    let short = match (outside, inside) {
        ("block", "flow") => "block",
        ("inline", "flow") => "inline",
        ("run-in", "flow") => "run-in",
        ("block", "flow-root") => "flow-root",
        ("inline", "flow-root") => "inline-block",
        ("block", "table") => "table",
        ("inline", "table") => "inline-table",
        ("block", "flex") => "flex",
        ("inline", "flex") => "inline-flex",
        ("block", "grid") => "grid",
        ("inline", "grid") => "inline-grid",
        ("inline", "ruby") => "ruby",
        ("inline", "math") => "math",
        (o, i) => return vec![kw(o), kw(i)],
    };
    vec![kw(short)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::css::parser::parse_component_values;

    fn m(src: &str, text: &str) -> Option<String> {
        match_str(src, &parse_component_values(text)).map(|v| serialize_list(&v))
    }

    #[test]
    fn combinators_and_multipliers() {
        assert_eq!(m("a | b", "B").as_deref(), Some("b"));
        assert_eq!(m("a || b", "b a").as_deref(), Some("a b"));
        assert_eq!(m("a && b?", "a").as_deref(), Some("a"));
        assert_eq!(m("<length>{1,2}", "1px 0").as_deref(), Some("1px 0px"));
        assert_eq!(m("<length>#", "1px,2px").as_deref(), Some("1px, 2px"));
        assert_eq!(m("<length>#", "1px,"), None);
        assert_eq!(
            m("minmax( <length> , auto )", "minmax(1px, auto)").as_deref(),
            Some("minmax(1px, auto)")
        );
        assert_eq!(m("<number [0,∞]>", "-1"), None);
        assert_eq!(m("<custom-ident> | none", "none").as_deref(), Some("none"));
    }

    #[test]
    fn positions_take_the_canonical_order() {
        assert_eq!(m("<position>", "top left").as_deref(), Some("left top"));
        assert_eq!(
            m("<position>", "right 10% bottom").as_deref(),
            Some("right 10% bottom")
        );
        assert_eq!(m("<position>", "left right"), None);
    }

    #[test]
    fn display_serializes_its_shortest_form() {
        let d = |t: &str| {
            let tokens = crate::css::tokenizer::tokenize(t);
            specified("display", &tokens).flatten()
        };
        assert_eq!(d("block flow").as_deref(), Some("block"));
        assert_eq!(d("flow-root inline").as_deref(), Some("inline-block"));
        assert_eq!(d("list-item block flow").as_deref(), Some("list-item"));
        assert_eq!(d("inline list-item").as_deref(), Some("inline list-item"));
        assert_eq!(d("block block"), Some(None));
    }
}
