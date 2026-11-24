//! CSS declaration blocks (CSSOM §6.7): parsing a block's contents into
//! declarations, the property names CSSOM accepts, the expansion of
//! shorthands into their longhands, the serialization of a block with
//! shorthands (CSSOM §6.7.2) and of component values, for the `style`
//! attribute, style rules and the descriptors of `@font-face`, `@page`
//! and `@property`.
//!
//! Values are kept as their component values (tokens) and serialized
//! back from them; the property grammars that validate and canonicalize
//! each value come with the CSS values sprint (LIMITATIONS.md).

use super::parser::{declarations_of, parse_block_contents, to_tokens};
use super::properties::{self, Kind, Shorthand};
use super::tokenizer::{Numeric, Token, tokenize};

/// One declaration: a property name (lowercase unless custom), its value
/// and its importance. A longhand set by a shorthand whose value has
/// `var()` is "pending substitution": `pending` names the shorthand and
/// `value` holds the shorthand's value.
#[derive(Clone, Debug, PartialEq)]
pub struct Declaration {
    pub name: String,
    pub value: Vec<Token>,
    pub important: bool,
    pub pending: Option<&'static str>,
}

/// What a declaration block holds: properties, or the descriptors of an
/// at-rule.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DeclContext {
    #[default]
    Style,
    FontFace,
    /// `@page` and its margin rules.
    Page,
    /// `@property`.
    Property,
}

/// A CSS declaration block.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DeclarationBlock {
    pub declarations: Vec<Declaration>,
    pub context: DeclContext,
}

/// A custom property name (`--*`).
pub fn is_custom_property(name: &str) -> bool {
    name.starts_with("--")
}

/// The property name `name` as stored: custom properties keep their
/// case, every other name is ASCII-lowercased.
pub fn normalize_property(name: &str) -> String {
    if is_custom_property(name) {
        name.to_string()
    } else {
        name.to_ascii_lowercase()
    }
}

/// The name a declaration of `name` is stored under in `context`, or
/// `None` if `name` is not a supported property or descriptor there.
pub fn resolve_name(context: DeclContext, name: &str) -> Option<String> {
    if is_custom_property(name) {
        return match context {
            DeclContext::Style | DeclContext::Page => Some(name.to_string()),
            _ => None,
        };
    }
    let lower = name.to_ascii_lowercase();
    match context {
        DeclContext::Style => properties::canonical(&lower).map(str::to_string),
        DeclContext::FontFace => properties::FONT_FACE_DESCRIPTORS
            .contains(&lower.as_str())
            .then_some(lower),
        DeclContext::Page => {
            if properties::PAGE_DESCRIPTORS.contains(&lower.as_str()) {
                Some(lower)
            } else {
                properties::canonical(&lower).map(str::to_string)
            }
        }
        DeclContext::Property => properties::PROPERTY_DESCRIPTORS
            .contains(&lower.as_str())
            .then_some(lower),
    }
}

/// The closing token of a block opened by `t`, if `t` opens one.
fn closer(t: &Token) -> Option<Token> {
    match t {
        Token::Function(_) | Token::LeftParen => Some(Token::RightParen),
        Token::LeftBracket => Some(Token::RightBracket),
        Token::LeftBrace => Some(Token::RightBrace),
        _ => None,
    }
}

fn trim_whitespace(mut v: &[Token]) -> &[Token] {
    while v.first() == Some(&Token::Whitespace) {
        v = &v[1..];
    }
    while v.last() == Some(&Token::Whitespace) {
        v = &v[..v.len() - 1];
    }
    v
}

/// True if `value` is acceptable as the value of property `name`: no bad
/// tokens, balanced blocks, no top-level semicolon and, unless `name` is
/// custom, not empty, without `!` and with a CSS-wide keyword only alone
/// (CSSOM's "parse a CSS value" without the property grammar).
pub fn valid_value(name: &str, value: &[Token]) -> bool {
    let custom = is_custom_property(name);
    if !custom && value.is_empty() {
        return false;
    }
    let mut stack: Vec<Token> = Vec::new();
    for t in value {
        if let Some(c) = closer(t) {
            stack.push(c);
            continue;
        }
        match t {
            Token::BadString | Token::BadUrl => return false,
            Token::RightParen | Token::RightBracket | Token::RightBrace => {
                if stack.last() != Some(t) {
                    return false;
                }
                stack.pop();
            }
            Token::Semicolon if stack.is_empty() => return false,
            Token::Delim('!') if !custom => return false,
            Token::Ident(k)
                if !custom && stack.is_empty() && properties::is_css_wide_keyword(k) =>
            {
                if value.len() != 1 {
                    return false;
                }
            }
            _ => {}
        }
    }
    stack.is_empty()
}

/// True if `value` has a `var()` (or `env()`, `attr()`) function.
fn has_substitution(value: &[Token]) -> bool {
    value.iter().any(|t| match t {
        Token::Function(f) => {
            f.eq_ignore_ascii_case("var")
                || f.eq_ignore_ascii_case("env")
                || f.eq_ignore_ascii_case("attr")
        }
        _ => false,
    })
}

// ----- shorthands --------------------------------------------------------------------

/// The top-level components of a value: each token, function or block,
/// split at whitespace.
fn components(tokens: &[Token]) -> Vec<&[Token]> {
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut start: Option<usize> = None;
    for (i, t) in tokens.iter().enumerate() {
        if depth == 0 && *t == Token::Whitespace {
            if let Some(s) = start.take() {
                out.push(&tokens[s..i]);
            }
            continue;
        }
        if start.is_none() {
            start = Some(i);
        }
        if closer(t).is_some() {
            depth += 1;
        } else if matches!(
            t,
            Token::RightParen | Token::RightBracket | Token::RightBrace
        ) {
            depth = depth.saturating_sub(1);
        }
        if depth == 0 {
            // A single token, or the end of a block, is one component;
            // only whitespace separates components otherwise.
            let next_is_part = tokens
                .get(i + 1)
                .is_some_and(|n| *n != Token::Whitespace && is_glued(t, n));
            if !next_is_part {
                if let Some(s) = start.take() {
                    out.push(&tokens[s..=i]);
                }
            }
        }
    }
    if let Some(s) = start {
        out.push(&tokens[s..]);
    }
    out
}

/// Whether `next` continues the component `t` ends (only a `/` stands
/// alone without whitespace).
fn is_glued(t: &Token, next: &Token) -> bool {
    !matches!(t, Token::Delim('/')) && !matches!(next, Token::Delim('/'))
}

fn ident_of(c: &[Token]) -> Option<&str> {
    match c {
        [Token::Ident(s)] => Some(s),
        _ => None,
    }
}

fn is_ident_in(c: &[Token], set: &[&str]) -> bool {
    ident_of(c).is_some_and(|s| set.iter().any(|k| s.eq_ignore_ascii_case(k)))
}

fn is_math_function(c: &[Token]) -> bool {
    match c.first() {
        Some(Token::Function(f)) => ["calc", "min", "max", "clamp", "var", "env"]
            .iter()
            .any(|m| f.eq_ignore_ascii_case(m)),
        _ => false,
    }
}

const LENGTH_UNITS: &[&str] = &[
    "px", "em", "rem", "ex", "rex", "ch", "rch", "ic", "ric", "cap", "rcap", "lh", "rlh", "vw",
    "vh", "vi", "vb", "vmin", "vmax", "svw", "svh", "lvw", "lvh", "dvw", "dvh", "svi", "svb",
    "lvi", "lvb", "dvi", "dvb", "svmin", "svmax", "lvmin", "lvmax", "dvmin", "dvmax", "cqw", "cqh",
    "cqi", "cqb", "cqmin", "cqmax", "cm", "mm", "q", "in", "pt", "pc",
];

/// A `<length>` (or a zero, or a math function).
fn is_length(c: &[Token]) -> bool {
    match c {
        [Token::Dimension(_, u)] => LENGTH_UNITS.iter().any(|l| u.eq_ignore_ascii_case(l)),
        [Token::Number(n)] => n.value == 0.0,
        _ => is_math_function(c),
    }
}

fn is_length_percentage(c: &[Token]) -> bool {
    is_length(c) || matches!(c, [Token::Percentage(_)])
}

fn is_number(c: &[Token]) -> bool {
    matches!(c, [Token::Number(_)])
}

const LINE_STYLES: &[&str] = &[
    "none", "hidden", "dotted", "dashed", "solid", "double", "groove", "ridge", "inset", "outset",
];

fn is_line_width(c: &[Token]) -> bool {
    is_length(c) || is_ident_in(c, &["thin", "medium", "thick"])
}

fn is_color(c: &[Token]) -> bool {
    match c {
        [Token::Hash { .. }] => true,
        [Token::Ident(s)] => !properties::is_css_wide_keyword(s),
        [Token::Function(f), ..] => [
            "rgb",
            "rgba",
            "hsl",
            "hsla",
            "hwb",
            "lab",
            "lch",
            "oklab",
            "oklch",
            "color",
            "color-mix",
            "light-dark",
            "contrast-color",
            "var",
            "env",
        ]
        .iter()
        .any(|n| f.eq_ignore_ascii_case(n)),
        _ => false,
    }
}

fn is_image(c: &[Token]) -> bool {
    match c {
        [Token::Url(_)] => true,
        [Token::Function(f), ..] => {
            let f = f.to_ascii_lowercase();
            f == "url"
                || f == "image"
                || f == "image-set"
                || f == "cross-fade"
                || f.ends_with("gradient")
        }
        _ => false,
    }
}

/// The values of a one-to-four value box shorthand: top, right, bottom,
/// left.
fn box_values<'a>(v: &[&'a [Token]]) -> Option<[&'a [Token]; 4]> {
    Some(match v {
        [a] => [a, a, a, a],
        [a, b] => [a, b, a, b],
        [a, b, c] => [a, b, c, b],
        [a, b, c, d] => [a, b, c, d],
        _ => return None,
    })
}

/// Joins components with single spaces.
fn join(parts: &[&[Token]]) -> Vec<Token> {
    let mut out = Vec::new();
    for (i, p) in parts.iter().enumerate() {
        if i > 0 {
            out.push(Token::Whitespace);
        }
        out.extend_from_slice(p);
    }
    out
}

/// Splits components at `/` delimiters.
fn split_slash<'a>(v: &[&'a [Token]]) -> Vec<Vec<&'a [Token]>> {
    let mut out = vec![Vec::new()];
    for c in v {
        if matches!(c, [Token::Delim('/')]) {
            out.push(Vec::new());
        } else if let Some(last) = out.last_mut() {
            last.push(*c);
        }
    }
    out
}

/// Width, style and color in any order, each at most once.
fn side_values(v: &[&[Token]]) -> Option<[Option<Vec<Token>>; 3]> {
    let mut out: [Option<Vec<Token>>; 3] = [None, None, None];
    if v.is_empty() {
        return None;
    }
    for c in v {
        let slot = if is_ident_in(c, LINE_STYLES) || is_ident_in(c, &["auto"]) {
            1
        } else if is_line_width(c) {
            0
        } else if is_color(c) {
            2
        } else {
            return None;
        };
        if out[slot].is_some() {
            return None;
        }
        out[slot] = Some(c.to_vec());
    }
    Some(out)
}

/// The longhand values a shorthand's value expands to, or `None` if the
/// value does not match the shorthand.
fn expand(sh: &Shorthand, tokens: &[Token]) -> Option<Vec<Vec<Token>>> {
    let comps = components(tokens);
    let initial = |i: usize| tokenize(sh.longhands[i].1);
    let n = sh.longhands.len();
    if comps
        .iter()
        .any(|c| matches!(c, [Token::Ident(k)] if properties::is_css_wide_keyword(k)))
    {
        // A CSS-wide keyword alone sets every longhand to it.
        return (comps.len() == 1).then(|| vec![comps[0].to_vec(); n]);
    }
    match sh.kind {
        Kind::Box => {
            let b = box_values(&comps)?;
            Some(b.iter().map(|c| c.to_vec()).collect())
        }
        Kind::Pair => match comps[..] {
            [a] => Some(vec![a.to_vec(), a.to_vec()]),
            [a, b] => Some(vec![a.to_vec(), b.to_vec()]),
            _ => None,
        },
        Kind::Side => {
            let s = side_values(&comps)?;
            if s[1].as_deref().is_some_and(|c| is_ident_in(c, &["auto"])) && sh.name != "outline" {
                return None;
            }
            Some(
                (0..3)
                    .map(|i| s[i].clone().unwrap_or_else(|| initial(i)))
                    .collect(),
            )
        }
        Kind::SidePair => {
            let s = side_values(&comps)?;
            if s[1].as_deref().is_some_and(|c| is_ident_in(c, &["auto"])) {
                return None;
            }
            let side: Vec<Vec<Token>> = (0..3)
                .map(|i| s[i].clone().unwrap_or_else(|| initial(i)))
                .collect();
            Some(side.iter().chain(side.iter()).cloned().collect())
        }
        Kind::Border => {
            let s = side_values(&comps)?;
            if s[1].as_deref().is_some_and(|c| is_ident_in(c, &["auto"])) {
                return None;
            }
            let mut out = Vec::with_capacity(n);
            for part in 0..3 {
                let v = s[part].clone().unwrap_or_else(|| initial(part * 4));
                for _ in 0..4 {
                    out.push(v.clone());
                }
            }
            for i in 12..n {
                out.push(initial(i));
            }
            Some(out)
        }
        Kind::Radius => {
            let parts = split_slash(&comps);
            if parts.len() > 2 || parts.iter().any(|p| p.is_empty()) {
                return None;
            }
            if parts.iter().flatten().any(|c| !is_length_percentage(c)) {
                return None;
            }
            let h = box_values(&parts[0])?;
            let v = match parts.get(1) {
                Some(p) => box_values(p)?,
                None => h,
            };
            Some(
                (0..4)
                    .map(|i| {
                        if h[i] == v[i] {
                            h[i].to_vec()
                        } else {
                            join(&[h[i], v[i]])
                        }
                    })
                    .collect(),
            )
        }
        Kind::Flex => {
            if let [c] = comps[..] {
                if is_ident_in(c, &["none"]) {
                    return Some(vec![tokenize("0"), tokenize("0"), tokenize("auto")]);
                }
                if is_ident_in(c, &["auto"]) {
                    return Some(vec![tokenize("1"), tokenize("1"), tokenize("auto")]);
                }
            }
            // `<'flex-grow'> <'flex-shrink'>? || <'flex-basis'>`
            let mut grow: Option<Vec<Token>> = None;
            let mut shrink: Option<Vec<Token>> = None;
            let mut basis: Option<Vec<Token>> = None;
            let mut i = 0;
            while i < comps.len() {
                let c = comps[i];
                if grow.is_none() && is_number(c) {
                    grow = Some(c.to_vec());
                    if comps.get(i + 1).is_some_and(|n| is_number(n)) {
                        shrink = Some(comps[i + 1].to_vec());
                        i += 1;
                    }
                } else if basis.is_none()
                    && (is_length_percentage(c) || is_ident_in(c, &["auto", "content"]))
                {
                    basis = Some(c.to_vec());
                } else {
                    return None;
                }
                i += 1;
            }
            if comps.is_empty() {
                return None;
            }
            let basis = basis.unwrap_or_else(|| {
                if grow.is_none() {
                    tokenize("auto")
                } else {
                    tokenize("0%")
                }
            });
            let grow = grow.unwrap_or_else(|| tokenize("1"));
            let shrink = shrink.unwrap_or_else(|| tokenize("1"));
            Some(vec![grow, shrink, basis])
        }
        Kind::FlexFlow => {
            let mut out: [Option<Vec<Token>>; 2] = [None, None];
            for c in &comps {
                let slot = if is_ident_in(c, &["row", "row-reverse", "column", "column-reverse"]) {
                    0
                } else if is_ident_in(c, &["nowrap", "wrap", "wrap-reverse"]) {
                    1
                } else {
                    return None;
                };
                if out[slot].is_some() {
                    return None;
                }
                out[slot] = Some(c.to_vec());
            }
            if comps.is_empty() {
                return None;
            }
            Some(
                (0..2)
                    .map(|i| out[i].clone().unwrap_or_else(|| initial(i)))
                    .collect(),
            )
        }
        Kind::ListStyle => {
            let mut out: [Option<Vec<Token>>; 3] = [None, None, None];
            let mut nones = 0;
            for c in &comps {
                if is_ident_in(c, &["none"]) {
                    nones += 1;
                    continue;
                }
                let slot = if is_ident_in(c, &["inside", "outside"]) {
                    0
                } else if is_image(c) {
                    1
                } else if ident_of(c).is_some()
                    || matches!(c, [Token::String(_)])
                    || matches!(c.first(), Some(Token::Function(f)) if f.eq_ignore_ascii_case("symbols"))
                {
                    2
                } else {
                    return None;
                };
                if out[slot].is_some() {
                    return None;
                }
                out[slot] = Some(c.to_vec());
            }
            if comps.is_empty() {
                return None;
            }
            // `none` sets the image, or the type if an image is given; a
            // lone `none` sets both.
            let free = [out[1].is_none(), out[2].is_none()];
            match (nones, free) {
                (0, _) => {}
                (1, [true, true]) => {
                    out[1] = Some(tokenize("none"));
                    out[2] = Some(tokenize("none"));
                }
                (1, [true, false]) => out[1] = Some(tokenize("none")),
                (1, [false, true]) => out[2] = Some(tokenize("none")),
                (2, [true, true]) => {
                    out[1] = Some(tokenize("none"));
                    out[2] = Some(tokenize("none"));
                }
                _ => return None,
            }
            Some(
                (0..3)
                    .map(|i| out[i].clone().unwrap_or_else(|| initial(i)))
                    .collect(),
            )
        }
        Kind::TextDecoration => {
            let lines = [
                "none",
                "underline",
                "overline",
                "line-through",
                "blink",
                "spelling-error",
                "grammar-error",
            ];
            let mut line: Vec<&[Token]> = Vec::new();
            let mut out: [Option<Vec<Token>>; 4] = [None, None, None, None];
            for c in &comps {
                if is_ident_in(c, &lines) {
                    line.push(c);
                } else if out[2].is_none()
                    && is_ident_in(c, &["solid", "double", "dotted", "dashed", "wavy"])
                {
                    out[2] = Some(c.to_vec());
                } else if out[1].is_none()
                    && (is_length_percentage(c) || is_ident_in(c, &["auto", "from-font"]))
                {
                    out[1] = Some(c.to_vec());
                } else if out[3].is_none() && is_color(c) {
                    out[3] = Some(c.to_vec());
                } else {
                    return None;
                }
            }
            if comps.is_empty() {
                return None;
            }
            if !line.is_empty() {
                if line.len() > 1 && line.iter().any(|c| is_ident_in(c, &["none"])) {
                    return None;
                }
                out[0] = Some(join(&line));
            }
            Some(
                (0..4)
                    .map(|i| out[i].clone().unwrap_or_else(|| initial(i)))
                    .collect(),
            )
        }
        Kind::Columns => {
            let mut out: [Option<Vec<Token>>; 2] = [None, None];
            for c in &comps {
                if is_ident_in(c, &["auto"]) {
                    // `auto` leaves its longhand at its initial value.
                } else if out[0].is_none() && is_length(c) && !is_number(c) {
                    out[0] = Some(c.to_vec());
                } else if out[1].is_none() && is_number(c) {
                    out[1] = Some(c.to_vec());
                } else {
                    return None;
                }
            }
            if comps.is_empty() || comps.len() > 2 {
                return None;
            }
            Some(
                (0..2)
                    .map(|i| out[i].clone().unwrap_or_else(|| initial(i)))
                    .collect(),
            )
        }
        Kind::GridLine => {
            let parts = split_slash(&comps);
            if parts.len() > 2 || parts.iter().any(|p| p.is_empty()) {
                return None;
            }
            let start = join(&parts[0]);
            let end = match parts.get(1) {
                Some(p) => join(p),
                None => line_default(&start),
            };
            Some(vec![start, end])
        }
        Kind::GridArea => {
            let parts = split_slash(&comps);
            if parts.len() > 4 || parts.iter().any(|p| p.is_empty()) {
                return None;
            }
            let rs = join(&parts[0]);
            let cs = parts.get(1).map_or_else(|| line_default(&rs), |p| join(p));
            let re = parts.get(2).map_or_else(|| line_default(&rs), |p| join(p));
            let ce = parts.get(3).map_or_else(|| line_default(&cs), |p| join(p));
            Some(vec![rs, cs, re, ce])
        }
    }
}

/// The omitted line of a grid shorthand: the other line if it is a
/// custom identifier, `auto` otherwise.
fn line_default(other: &[Token]) -> Vec<Token> {
    match other {
        [Token::Ident(s)] if !s.eq_ignore_ascii_case("auto") && !s.eq_ignore_ascii_case("span") => {
            other.to_vec()
        }
        _ => tokenize("auto"),
    }
}

/// Serializes a shorthand from the serialized values of its longhands
/// (in the order of [`Shorthand::longhands`]); empty if the values cannot
/// be written as the shorthand.
fn serialize_shorthand(sh: &Shorthand, values: &[String]) -> String {
    let initial = |i: usize| sh.longhands[i].1;
    let is_initial = |i: usize| values[i].eq_ignore_ascii_case(initial(i));
    let compress = |v: &[String]| -> String {
        let (t, r, b, l) = (&v[0], &v[1], &v[2], &v[3]);
        if r == l {
            if t == b {
                if t == r {
                    return t.clone();
                }
                return format!("{t} {r}");
            }
            return format!("{t} {r} {b}");
        }
        format!("{t} {r} {b} {l}")
    };
    let omit_initial = |indices: &[usize], fallback: usize| -> String {
        let parts: Vec<&str> = indices
            .iter()
            .filter(|i| !is_initial(**i))
            .map(|i| values[*i].as_str())
            .collect();
        if parts.is_empty() {
            values[fallback].clone()
        } else {
            parts.join(" ")
        }
    };
    match sh.kind {
        Kind::Box => compress(values),
        Kind::Pair => {
            if values[0] == values[1] {
                values[0].clone()
            } else {
                format!("{} {}", values[0], values[1])
            }
        }
        Kind::Side => omit_initial(&[0, 1, 2], 1),
        Kind::SidePair => {
            if values[0..3] != values[3..6] {
                return String::new();
            }
            omit_initial(&[0, 1, 2], 1)
        }
        Kind::Border => {
            for group in 0..3 {
                let g = &values[group * 4..group * 4 + 4];
                if g.iter().any(|v| *v != g[0]) {
                    return String::new();
                }
            }
            if (12..values.len()).any(|i| !is_initial(i)) {
                return String::new();
            }
            omit_initial(&[0, 4, 8], 4)
        }
        Kind::Radius => {
            let mut h = Vec::new();
            let mut v = Vec::new();
            for value in values {
                let mut it = value.splitn(2, ' ');
                let a = it.next().unwrap_or("").to_string();
                let b = it.next().map(str::to_string).unwrap_or_else(|| a.clone());
                h.push(a);
                v.push(b);
            }
            if h == v {
                compress(&h)
            } else {
                format!("{} / {}", compress(&h), compress(&v))
            }
        }
        Kind::Flex => values.join(" "),
        Kind::FlexFlow => omit_initial(&[0, 1], 0),
        Kind::ListStyle => omit_initial(&[0, 1, 2], 2),
        Kind::TextDecoration => omit_initial(&[0, 1, 2, 3], 0),
        Kind::Columns => omit_initial(&[0, 1], 0),
        Kind::GridLine => {
            let start = tokenize(&values[0]);
            if values[1] == serialize_tokens(&line_default(&start)) {
                values[0].clone()
            } else {
                format!("{} / {}", values[0], values[1])
            }
        }
        Kind::GridArea => {
            let default = |other: usize| serialize_tokens(&line_default(&tokenize(&values[other])));
            let mut n = 4;
            if values[3] == default(1) {
                n = 3;
                if values[2] == default(0) {
                    n = 2;
                    if values[1] == default(0) {
                        n = 1;
                    }
                }
            }
            values[..n].join(" / ")
        }
    }
}

// ----- the block ----------------------------------------------------------------------

/// One declaration to set: a longhand (or a property without expansion)
/// with its value.
struct Item {
    name: String,
    value: Vec<Token>,
    pending: Option<&'static str>,
}

/// "Parse a CSS value" of `name` (already resolved) in `context`: the
/// declarations it sets, or `None` if the value is invalid.
fn parse_value(context: DeclContext, name: &str, value: &[Token]) -> Option<Vec<Item>> {
    let value = trim_whitespace(value);
    if !valid_value(name, value) {
        return None;
    }
    if context == DeclContext::Style || context == DeclContext::Page {
        if let Some(sh) = properties::shorthand(name) {
            if has_substitution(value) {
                return Some(
                    sh.longhands
                        .iter()
                        .map(|(l, _)| Item {
                            name: l.to_string(),
                            value: value.to_vec(),
                            pending: Some(sh.name),
                        })
                        .collect(),
                );
            }
            let values = expand(sh, value)?;
            return Some(
                sh.longhands
                    .iter()
                    .zip(values)
                    .map(|((l, _), v)| Item {
                        name: l.to_string(),
                        value: v,
                        pending: None,
                    })
                    .collect(),
            );
        }
    }
    Some(vec![Item {
        name: name.to_string(),
        value: value.to_vec(),
        pending: None,
    }])
}

impl DeclarationBlock {
    /// An empty block of `context`.
    pub fn new(context: DeclContext) -> DeclarationBlock {
        DeclarationBlock {
            declarations: Vec::new(),
            context,
        }
    }

    /// Parses `text` as a block's contents and keeps its declarations
    /// (the `style` attribute or `cssText`).
    pub fn parse(text: &str) -> DeclarationBlock {
        DeclarationBlock::parse_in(DeclContext::Style, text)
    }

    /// [`DeclarationBlock::parse`] for the declarations of `context`.
    pub fn parse_in(context: DeclContext, text: &str) -> DeclarationBlock {
        let items = parse_block_contents(text);
        let mut block = DeclarationBlock::new(context);
        for d in declarations_of(&items) {
            block.push_raw(&d.name, &to_tokens(&d.value), d.important);
        }
        block
    }

    /// Adds a parsed declaration: a later declaration of a property
    /// replaces an earlier one unless only the earlier one is important.
    pub fn push_raw(&mut self, name: &str, value: &[Token], important: bool) {
        let Some(name) = resolve_name(self.context, name) else {
            return;
        };
        let Some(items) = parse_value(self.context, &name, value) else {
            return;
        };
        for item in items {
            if let Some(i) = self.index_of(&item.name) {
                if self.declarations[i].important && !important {
                    continue;
                }
                self.declarations.remove(i);
            }
            self.declarations.push(Declaration {
                name: item.name,
                value: item.value,
                important,
                pending: item.pending,
            });
        }
    }

    fn index_of(&self, name: &str) -> Option<usize> {
        self.declarations.iter().position(|d| d.name == name)
    }

    fn get(&self, name: &str) -> Option<&Declaration> {
        self.declarations.iter().find(|d| d.name == name)
    }

    /// `length`.
    pub fn len(&self) -> usize {
        self.declarations.len()
    }

    pub fn is_empty(&self) -> bool {
        self.declarations.is_empty()
    }

    /// `item(index)`: the property name at `index`.
    pub fn item(&self, index: usize) -> Option<&str> {
        self.declarations.get(index).map(|d| d.name.as_str())
    }

    /// The name `property` resolves to for the getters (`None` if it is
    /// not supported in this block).
    fn lookup_name(&self, property: &str) -> Option<String> {
        resolve_name(self.context, property)
    }

    /// The declarations of the longhands of `sh`, if all are present.
    fn longhands_of(&self, sh: &Shorthand) -> Option<Vec<&Declaration>> {
        sh.longhands.iter().map(|(l, _)| self.get(l)).collect()
    }

    /// Serializes shorthand `sh` from the declarations of its longhands
    /// (CSSOM "serialize a CSS value" of a list of declarations).
    fn shorthand_value(sh: &Shorthand, decls: &[&Declaration]) -> String {
        if decls.iter().any(|d| d.important != decls[0].important) {
            return String::new();
        }
        if decls.iter().any(|d| d.pending.is_some()) {
            let same = decls
                .iter()
                .all(|d| d.pending == Some(sh.name) && d.value == decls[0].value);
            return if same {
                serialize_tokens(&decls[0].value)
            } else {
                String::new()
            };
        }
        let values: Vec<String> = decls.iter().map(|d| serialize_tokens(&d.value)).collect();
        let wide = values
            .iter()
            .filter(|v| properties::is_css_wide_keyword(v))
            .count();
        if wide > 0 {
            return if wide == values.len() && values.iter().all(|v| *v == values[0]) {
                values[0].clone()
            } else {
                String::new()
            };
        }
        serialize_shorthand(sh, &values)
    }

    /// `getPropertyValue(property)`.
    pub fn property_value(&self, property: &str) -> String {
        let Some(name) = self.lookup_name(property) else {
            return String::new();
        };
        if let Some(sh) = properties::shorthand(&name) {
            return match self.longhands_of(sh) {
                Some(decls) => Self::shorthand_value(sh, &decls),
                None => String::new(),
            };
        }
        match self.get(&name) {
            Some(d) if d.pending.is_none() => serialize_tokens(&d.value),
            _ => String::new(),
        }
    }

    /// `getPropertyPriority(property)`.
    pub fn property_priority(&self, property: &str) -> &'static str {
        let Some(name) = self.lookup_name(property) else {
            return "";
        };
        let important = match properties::shorthand(&name) {
            Some(sh) => self
                .longhands_of(sh)
                .is_some_and(|d| d.iter().all(|d| d.important)),
            None => self.get(&name).is_some_and(|d| d.important),
        };
        if important { "important" } else { "" }
    }

    /// `setProperty(property, value, priority)`; true if the block
    /// changed (the owner is then updated).
    pub fn set_property(&mut self, property: &str, value: &str, priority: &str) -> bool {
        let Some(name) = resolve_name(self.context, property) else {
            return false;
        };
        if value.is_empty() {
            return self.remove_property(&name).is_some();
        }
        let important = if priority.is_empty() {
            false
        } else if priority.eq_ignore_ascii_case("important") {
            true
        } else {
            return false;
        };
        let tokens = tokenize(value);
        let Some(items) = parse_value(self.context, &name, &tokens) else {
            return false;
        };
        let mut changed = false;
        for item in items {
            let d = Declaration {
                name: item.name,
                value: item.value,
                important,
                pending: item.pending,
            };
            match self.index_of(&d.name) {
                Some(i) => {
                    if self.declarations[i] != d {
                        self.declarations[i] = d;
                        changed = true;
                    }
                }
                None => {
                    self.declarations.push(d);
                    changed = true;
                }
            }
        }
        changed
    }

    /// `removeProperty(property)`: the removed value, or `None` if the
    /// block had no such property (a shorthand removes its longhands).
    pub fn remove_property(&mut self, property: &str) -> Option<String> {
        let name = self.lookup_name(property)?;
        let value = self.property_value(&name);
        let names: Vec<&str> = match properties::shorthand(&name) {
            Some(sh) => sh.longhands.iter().map(|(l, _)| *l).collect(),
            None => vec![name.as_str()],
        };
        let before = self.declarations.len();
        self.declarations
            .retain(|d| !names.contains(&d.name.as_str()));
        (self.declarations.len() != before).then_some(value)
    }

    /// Serializes the block (CSSOM §6.7.2 "serialize a CSS declaration
    /// block"), writing shorthands where all their longhands allow it.
    pub fn serialize(&self) -> String {
        let mut out: Vec<String> = Vec::new();
        let mut done: Vec<&str> = Vec::new();
        'declarations: for d in &self.declarations {
            if done.contains(&d.name.as_str()) {
                continue;
            }
            let mut candidates: Vec<&Shorthand> = properties::shorthands_of(&d.name).collect();
            candidates.sort_by_key(|s| std::cmp::Reverse(s.longhands.len()));
            for sh in candidates {
                if sh.longhands.iter().any(|(l, _)| done.contains(l)) {
                    continue;
                }
                let Some(decls) = self.longhands_of(sh) else {
                    continue;
                };
                let value = Self::shorthand_value(sh, &decls);
                if value.is_empty() {
                    continue;
                }
                out.push(declaration_text(sh.name, &value, decls[0].important));
                done.extend(sh.longhands.iter().map(|(l, _)| *l));
                continue 'declarations;
            }
            let value = if d.pending.is_some() {
                String::new()
            } else {
                serialize_tokens(&d.value)
            };
            out.push(declaration_text(&d.name, &value, d.important));
            done.push(d.name.as_str());
        }
        out.join(" ")
    }
}

/// "Serialize a CSS declaration".
fn declaration_text(name: &str, value: &str, important: bool) -> String {
    let mut s = format!("{name}: {value}");
    if important {
        s.push_str(" !important");
    }
    s.push(';');
    s
}

// ----- serialization -------------------------------------------------------------

/// Escapes a code point as `\HEX ` (CSSOM §2.1).
fn escape_code_point(out: &mut String, c: char) {
    out.push_str(&format!("\\{:x} ", c as u32));
}

/// "Serialize an identifier" (CSSOM §2.1).
pub fn serialize_identifier(s: &str) -> String {
    let mut out = String::new();
    let chars: Vec<char> = s.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        match c {
            '\0' => out.push('\u{FFFD}'),
            '\u{1}'..='\u{1F}' | '\u{7F}' => escape_code_point(&mut out, c),
            '0'..='9' if i == 0 || (i == 1 && chars[0] == '-') => escape_code_point(&mut out, c),
            '-' if i == 0 && chars.len() == 1 => out.push_str("\\-"),
            c if c >= '\u{80}' || c == '-' || c == '_' || c.is_ascii_alphanumeric() => out.push(c),
            c => {
                out.push('\\');
                out.push(c);
            }
        }
    }
    out
}

/// The name part of a hash or a unit: identifier characters without the
/// rules for the start of an identifier.
fn serialize_name(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        match c {
            '\0' => out.push('\u{FFFD}'),
            '\u{1}'..='\u{1F}' | '\u{7F}' => escape_code_point(&mut out, c),
            c if c >= '\u{80}' || c == '-' || c == '_' || c.is_ascii_alphanumeric() => out.push(c),
            c => {
                out.push('\\');
                out.push(c);
            }
        }
    }
    out
}

/// "Serialize a string" (CSSOM §2.1).
pub fn serialize_string(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '\0' => out.push('\u{FFFD}'),
            '\u{1}'..='\u{1F}' | '\u{7F}' => escape_code_point(&mut out, c),
            '"' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn serialize_number(n: &Numeric) -> String {
    let mut out = String::new();
    if n.signed && n.value >= 0.0 && !n.value.is_sign_negative() {
        out.push('+');
    }
    if n.integer && n.value.fract() == 0.0 && n.value.abs() < 1e15 {
        out.push_str(&format!("{}", n.value as i64));
    } else {
        out.push_str(&format!("{}", n.value));
    }
    out
}

/// The kinds of token the comment table of CSS Syntax §9 tells apart.
#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Ident,
    Function,
    Url,
    Number,
    Percentage,
    Dimension,
    AtKeyword,
    Hash,
    Cdc,
    LeftParen,
    Delim(char),
    Other,
}

fn kind(t: &Token) -> Kind {
    match t {
        Token::Ident(_) => Kind::Ident,
        Token::Function(_) => Kind::Function,
        Token::Url(_) | Token::BadUrl => Kind::Url,
        Token::Number(_) => Kind::Number,
        Token::Percentage(_) => Kind::Percentage,
        Token::Dimension(..) => Kind::Dimension,
        Token::AtKeyword(_) => Kind::AtKeyword,
        Token::Hash { .. } => Kind::Hash,
        Token::Cdc => Kind::Cdc,
        Token::LeftParen => Kind::LeftParen,
        Token::Delim(c) => Kind::Delim(*c),
        _ => Kind::Other,
    }
}

/// True if a comment must separate `a` and `b` so that the text
/// tokenizes back to the same tokens (CSS Syntax §9).
fn needs_comment(a: Kind, b: Kind) -> bool {
    use Kind::*;
    let identish = matches!(b, Ident | Function | Url);
    let numeric = matches!(b, Number | Percentage | Dimension);
    match a {
        Ident => identish || numeric || matches!(b, Delim('-') | Cdc | LeftParen),
        AtKeyword | Hash | Dimension => identish || numeric || matches!(b, Delim('-') | Cdc),
        Delim('#') => identish || numeric || b == Delim('-'),
        Delim('-') => identish || numeric,
        Number => identish || numeric || b == Delim('%'),
        Delim('@') => identish || b == Delim('-'),
        Delim('.') | Delim('+') => numeric,
        Delim('/') => b == Delim('*'),
        _ => false,
    }
}

/// Serializes component values back to text.
pub fn serialize_tokens(tokens: &[Token]) -> String {
    let mut out = String::new();
    let mut prev: Option<Kind> = None;
    for t in tokens {
        let k = kind(t);
        if t == &Token::Whitespace {
            if prev.is_some() && !out.ends_with(' ') {
                out.push(' ');
            }
            prev = None;
            continue;
        }
        if prev.is_some_and(|p| needs_comment(p, k)) {
            out.push_str("/**/");
        }
        match t {
            Token::Ident(s) => out.push_str(&serialize_identifier(s)),
            Token::Function(s) => {
                out.push_str(&serialize_identifier(s));
                out.push('(');
            }
            Token::AtKeyword(s) => {
                out.push('@');
                out.push_str(&serialize_identifier(s));
            }
            Token::Hash { value, .. } => {
                out.push('#');
                out.push_str(&serialize_name(value));
            }
            Token::String(s) => out.push_str(&serialize_string(s)),
            Token::BadString => out.push_str("\"\n"),
            Token::Url(s) => {
                out.push_str("url(");
                out.push_str(&serialize_string(s));
                out.push(')');
            }
            Token::BadUrl => out.push_str("url(\\)"),
            Token::Delim('\\') => out.push_str("\\\n"),
            Token::Delim(c) => out.push(*c),
            Token::Number(n) => out.push_str(&serialize_number(n)),
            Token::Percentage(n) => {
                out.push_str(&serialize_number(n));
                out.push('%');
            }
            Token::Dimension(n, unit) => {
                out.push_str(&serialize_number(n));
                let unit = serialize_name(unit);
                // A unit that reads as an exponent (`e3`, `e-3`) needs an
                // escape.
                let b = unit.as_bytes();
                let exponent = b.len() >= 2
                    && matches!(b[0], b'e' | b'E')
                    && (b[1].is_ascii_digit()
                        || (b[1] == b'-' && b.get(2).is_some_and(u8::is_ascii_digit)));
                if exponent {
                    escape_code_point(&mut out, unit.chars().next().unwrap_or('e'));
                    out.push_str(&unit[1..]);
                } else {
                    out.push_str(&unit);
                }
            }
            Token::Whitespace => {}
            Token::Cdo => out.push_str("<!--"),
            Token::Cdc => out.push_str("-->"),
            Token::Colon => out.push(':'),
            Token::Semicolon => out.push(';'),
            Token::Comma => out.push(','),
            Token::LeftBracket => out.push('['),
            Token::RightBracket => out.push(']'),
            Token::LeftParen => out.push('('),
            Token::RightParen => out.push(')'),
            Token::LeftBrace => out.push('{'),
            Token::RightBrace => out.push('}'),
        }
        prev = Some(k);
    }
    if out.ends_with(' ') {
        out.pop();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_serializes_the_style_attribute() {
        let b = DeclarationBlock::parse("DISPLAY : none ; color:red!important; bogus; --X: a  b");
        assert_eq!(b.len(), 3);
        assert_eq!(b.item(0), Some("display"));
        assert_eq!(b.property_value("Display"), "none");
        assert_eq!(b.property_priority("color"), "important");
        assert_eq!(b.property_value("--X"), "a b");
        assert_eq!(b.property_value("--x"), "");
        assert_eq!(
            b.serialize(),
            "display: none; color: red !important; --X: a b;"
        );
    }

    #[test]
    fn later_declarations_win_unless_only_the_earlier_is_important() {
        let b =
            DeclarationBlock::parse("color: red !important; color: blue; width: 1px; width: 2px");
        assert_eq!(b.property_value("color"), "red");
        assert_eq!(b.property_value("width"), "2px");
        assert_eq!(b.serialize(), "color: red !important; width: 2px;");
    }

    #[test]
    fn unknown_properties_and_misplaced_keywords_are_dropped() {
        let b = DeclarationBlock::parse("foo: 1; color: inherit red; word-wrap: break-word");
        assert_eq!(b.serialize(), "overflow-wrap: break-word;");
    }

    #[test]
    fn set_and_remove_property() {
        let mut b = DeclarationBlock::default();
        assert!(b.set_property("display", "none", ""));
        assert!(!b.set_property("display", "none", ""));
        assert!(!b.set_property("color", "red", "bogus"));
        assert!(!b.set_property("color", "red;", ""));
        assert!(!b.set_property("bogus", "red", ""));
        assert!(b.set_property("color", " red ", "IMPORTANT"));
        assert_eq!(b.serialize(), "display: none; color: red !important;");
        assert!(b.set_property("display", "block", ""));
        assert_eq!(b.item(0), Some("display"));
        assert_eq!(b.remove_property("display").as_deref(), Some("block"));
        assert_eq!(b.remove_property("display"), None);
        assert!(b.set_property("color", "", ""));
        assert!(b.is_empty());
    }

    #[test]
    fn shorthands_expand_and_serialize() {
        let mut b = DeclarationBlock::parse("margin: 1px 2px");
        assert_eq!(b.len(), 4);
        assert_eq!(b.property_value("margin-left"), "2px");
        assert_eq!(b.serialize(), "margin: 1px 2px;");
        b.set_property("margin-bottom", "3px", "");
        assert_eq!(b.property_value("margin"), "1px 2px 3px");
        b.set_property("margin-top", "0", "important");
        assert_eq!(b.property_value("margin"), "");
        assert_eq!(
            b.serialize(),
            "margin-top: 0 !important; margin-right: 2px; margin-bottom: 3px; margin-left: 2px;"
        );
        let b = DeclarationBlock::parse("border: 1px solid red");
        assert_eq!(b.serialize(), "border: 1px solid red;");
        assert_eq!(b.property_value("border-top"), "1px solid red");
        assert_eq!(b.property_value("border-left-color"), "red");
        let b = DeclarationBlock::parse("border-top: dashed");
        assert_eq!(b.serialize(), "border-top: dashed;");
        assert_eq!(b.property_value("border-top-width"), "medium");
        let b = DeclarationBlock::parse("flex: 1");
        assert_eq!(b.property_value("flex"), "1 1 0%");
        let b = DeclarationBlock::parse("overflow: hidden; border-radius: 1px 2px / 3px");
        assert_eq!(
            b.serialize(),
            "overflow: hidden; border-radius: 1px 2px / 3px;"
        );
        let b = DeclarationBlock::parse("margin: inherit");
        assert_eq!(b.serialize(), "margin: inherit;");
        assert!(DeclarationBlock::parse("margin: 1px 2px 3px 4px 5px").is_empty());
    }

    #[test]
    fn shorthands_with_var_are_pending_substitution() {
        let b = DeclarationBlock::parse("margin: var(--m) 1px");
        assert_eq!(b.property_value("margin-top"), "");
        assert_eq!(b.property_value("margin"), "var(--m) 1px");
        assert_eq!(b.serialize(), "margin: var(--m) 1px;");
    }

    #[test]
    fn descriptor_blocks_accept_their_descriptors_only() {
        let b = DeclarationBlock::parse_in(DeclContext::FontFace, "font-family: x; color: red");
        assert_eq!(b.serialize(), "font-family: x;");
        let b = DeclarationBlock::parse_in(DeclContext::Page, "size: a4; margin: 1in");
        assert_eq!(b.serialize(), "size: a4; margin: 1in;");
    }

    #[test]
    fn serializes_tokens_so_they_round_trip() {
        let t = tokenize("1em url(ab) 'x\"' a/**/b #f00 1e3 50% rgb(0, 0, 0)");
        assert_eq!(
            serialize_tokens(&t),
            "1em url(\"ab\") \"x\\\"\" a/**/b #f00 1000 50% rgb(0, 0, 0)"
        );
    }
}
