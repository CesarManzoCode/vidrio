//! Generated content (css-content-3 and css-lists-3): counters with their
//! scopes (`counter-reset`, `counter-increment`, `counter-set`, the
//! implicit `list-item` counter and the HTML list attributes), the
//! counter styles of `list-style-type` and `counter()`, quotes and the
//! `content` property of `::before`, `::after` and `::marker`.

use crate::css::style::ComputedStyle;
use crate::css::tokenizer::{Token, tokenize};
use crate::css::values::Value;

use super::values::kw;

#[derive(Clone, Debug)]
struct Counter {
    name: String,
    value: i64,
    reversed: bool,
}

/// The counters in scope during a tree-order walk: one frame per open
/// element, holding the counters its children (and their following
/// siblings) created.
#[derive(Debug, Default)]
pub(crate) struct Counters {
    frames: Vec<Vec<Counter>>,
}

/// One `<counter-name> <integer>?` entry of a counter property.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CounterEntry {
    pub name: String,
    pub value: Option<i64>,
    pub reversed: bool,
}

/// The entries of `counter-reset`, `counter-increment` or `counter-set`.
pub(crate) fn counter_entries(style: &ComputedStyle, property: &str) -> Vec<CounterEntry> {
    let text = style.text(property);
    let mut out: Vec<CounterEntry> = Vec::new();
    let tokens = tokenize(&text);
    let mut i = 0;
    while i < tokens.len() {
        match &tokens[i] {
            Token::Ident(name) if !name.eq_ignore_ascii_case("none") => {
                out.push(CounterEntry {
                    name: name.clone(),
                    value: None,
                    reversed: false,
                });
            }
            Token::Function(f) if f.eq_ignore_ascii_case("reversed") => {
                let mut j = i + 1;
                while j < tokens.len() && tokens[j] != Token::RightParen {
                    if let Token::Ident(name) = &tokens[j] {
                        out.push(CounterEntry {
                            name: name.clone(),
                            value: None,
                            reversed: true,
                        });
                    }
                    j += 1;
                }
                i = j;
            }
            Token::Number(n) => {
                if let Some(last) = out.last_mut() {
                    last.value = Some(n.value as i64);
                }
            }
            _ => {}
        }
        i += 1;
    }
    out
}

impl Counters {
    pub fn push(&mut self) {
        self.frames.push(Vec::new());
    }

    pub fn pop(&mut self) {
        self.frames.pop();
    }

    fn top(&mut self) -> &mut Vec<Counter> {
        if self.frames.is_empty() {
            self.frames.push(Vec::new());
        }
        self.frames.last_mut().expect("a frame")
    }

    fn find_mut(&mut self, name: &str) -> Option<&mut Counter> {
        self.frames
            .iter_mut()
            .rev()
            .flat_map(|f| f.iter_mut().rev())
            .find(|c| c.name == name)
    }

    /// Instantiates a counter (a previous sibling's counter of the same
    /// name ends its scope here).
    pub fn reset(&mut self, name: &str, value: i64, reversed: bool) {
        let frame = self.top();
        let c = Counter {
            name: name.to_string(),
            value,
            reversed,
        };
        match frame.iter_mut().find(|c| c.name == name) {
            Some(existing) => *existing = c,
            None => frame.push(c),
        }
    }

    pub fn increment(&mut self, name: &str, by: i64) {
        if self.find_mut(name).is_none() {
            self.reset(name, 0, false);
        }
        if let Some(c) = self.find_mut(name) {
            c.value = c.value.saturating_add(by);
        }
    }

    pub fn set(&mut self, name: &str, value: i64) {
        match self.find_mut(name) {
            Some(c) => c.value = value,
            None => self.reset(name, value, false),
        }
    }

    pub fn is_reversed(&mut self, name: &str) -> bool {
        self.find_mut(name).is_some_and(|c| c.reversed)
    }

    /// The innermost value of `name` (0 without one).
    pub fn value(&self, name: &str) -> i64 {
        self.frames
            .iter()
            .rev()
            .flat_map(|f| f.iter().rev())
            .find(|c| c.name == name)
            .map_or(0, |c| c.value)
    }

    /// Every value of `name`, outermost first.
    pub fn values(&self, name: &str) -> Vec<i64> {
        let mut v: Vec<i64> = self
            .frames
            .iter()
            .flat_map(|f| f.iter())
            .filter(|c| c.name == name)
            .map(|c| c.value)
            .collect();
        if v.is_empty() {
            v.push(0);
        }
        v
    }
}

/// What the HTML list attributes say for an element (`ol start` and
/// `reversed`, `li value`), and how many list items it has.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ListAttributes {
    pub start: Option<i64>,
    pub reversed: bool,
    pub value: Option<i64>,
    pub items: i64,
}

/// Applies the counter properties of an element (or pseudo-element) in
/// the order of css-lists-3 §4.5: resets, increments (with the implicit
/// `list-item` increment of list items), then sets.
pub(crate) fn apply_counters(
    counters: &mut Counters,
    style: &ComputedStyle,
    list_item: bool,
    html: ListAttributes,
) {
    for e in counter_entries(style, "counter-reset") {
        let mut reversed = e.reversed;
        let value = match e.value {
            Some(v) => v,
            None if e.name == "list-item" && (html.start.is_some() || html.reversed) => {
                reversed |= html.reversed;
                match (html.start, html.reversed) {
                    (Some(s), false) => s - 1,
                    (Some(s), true) => s + 1,
                    (None, _) => html.items + 1,
                }
            }
            None if reversed => html.items + 1,
            None => 0,
        };
        counters.reset(&e.name, value, reversed);
    }
    let increments = counter_entries(style, "counter-increment");
    for e in &increments {
        let by = match e.value {
            Some(v) => v,
            None if counters.is_reversed(&e.name) => -1,
            None => 1,
        };
        counters.increment(&e.name, by);
    }
    if list_item && !increments.iter().any(|e| e.name == "list-item") {
        let by = if counters.is_reversed("list-item") {
            -1
        } else {
            1
        };
        counters.increment("list-item", by);
    }
    for e in counter_entries(style, "counter-set") {
        counters.set(&e.name, e.value.unwrap_or(0));
    }
    if let Some(v) = html.value {
        counters.set("list-item", v);
    }
}

fn alphabetic(mut n: i64, digits: &[char]) -> String {
    if n < 1 {
        return n.to_string();
    }
    let base = digits.len() as i64;
    let mut out = Vec::new();
    while n > 0 {
        n -= 1;
        out.push(digits[(n % base) as usize]);
        n /= base;
    }
    out.iter().rev().collect()
}

fn roman(n: i64, upper: bool) -> String {
    if !(1..=3999).contains(&n) {
        return n.to_string();
    }
    const TABLE: &[(i64, &str)] = &[
        (1000, "m"),
        (900, "cm"),
        (500, "d"),
        (400, "cd"),
        (100, "c"),
        (90, "xc"),
        (50, "l"),
        (40, "xl"),
        (10, "x"),
        (9, "ix"),
        (5, "v"),
        (4, "iv"),
        (1, "i"),
    ];
    let mut n = n;
    let mut s = String::new();
    for &(v, r) in TABLE {
        while n >= v {
            s.push_str(r);
            n -= v;
        }
    }
    if upper { s.to_uppercase() } else { s }
}

/// The marker symbol of a symbolic counter style.
fn symbol(style: &str) -> Option<&'static str> {
    Some(match style {
        "disc" => "\u{2022}",
        "circle" => "\u{25E6}",
        "square" => "\u{25AA}",
        "disclosure-open" => "\u{25BE}",
        "disclosure-closed" => "\u{25B8}",
        _ => return None,
    })
}

/// The representation of `n` in the predefined counter style `style`
/// (css-counter-styles-3 §6); unknown styles fall back to `decimal`.
pub(crate) fn format_counter(n: i64, style: &str) -> String {
    if style == "none" {
        return String::new();
    }
    if let Some(s) = symbol(style) {
        return s.to_string();
    }
    let latin: Vec<char> = ('a'..='z').collect();
    match style {
        "decimal-leading-zero" => {
            if (0..10).contains(&n) {
                format!("0{n}")
            } else if (-9..0).contains(&n) {
                format!("-0{}", -n)
            } else {
                n.to_string()
            }
        }
        "lower-roman" => roman(n, false),
        "upper-roman" => roman(n, true),
        "lower-alpha" | "lower-latin" => alphabetic(n, &latin),
        "upper-alpha" | "upper-latin" => alphabetic(n, &latin).to_uppercase(),
        "lower-greek" => {
            let greek: Vec<char> = "αβγδεζηθικλμνξοπρστυφχψω".chars().collect();
            alphabetic(n, &greek)
        }
        "cjk-decimal" => {
            let digits: Vec<char> = "〇一二三四五六七八九".chars().collect();
            if n < 0 {
                return n.to_string();
            }
            n.to_string()
                .chars()
                .map(|c| digits[c.to_digit(10).unwrap_or(0) as usize])
                .collect()
        }
        _ => n.to_string(),
    }
}

/// The text of a list item's marker from `list-style-type` (css-lists-3
/// §3.2): the symbol and a space for symbolic styles, the counter and
/// ". " for numeric ones, a string as is; `None` for `none`.
pub(crate) fn marker_text(style: &ComputedStyle, counters: &Counters) -> Option<String> {
    let v = style.get("list-style-type")?;
    if let Some([Value::String(s)]) = v.values.as_deref() {
        return Some(s.clone());
    }
    let t = kw(style, "list-style-type");
    if t == "none" {
        return None;
    }
    if let Some(s) = symbol(&t) {
        return Some(format!("{s} "));
    }
    Some(format!(
        "{}. ",
        format_counter(counters.value("list-item"), &t)
    ))
}

/// The quotes of `style`: open and close pairs, outermost first.
fn quote_pairs(style: &ComputedStyle) -> Vec<(String, String)> {
    let v = style.text("quotes");
    if v == "none" {
        return Vec::new();
    }
    let strings: Vec<String> = tokenize(&v)
        .into_iter()
        .filter_map(|t| match t {
            Token::String(s) => Some(s),
            _ => None,
        })
        .collect();
    if strings.len() < 2 {
        // `auto`: the quotes of English.
        return vec![
            ("\u{201C}".to_string(), "\u{201D}".to_string()),
            ("\u{2018}".to_string(), "\u{2019}".to_string()),
        ];
    }
    strings
        .chunks(2)
        .filter(|c| c.len() == 2)
        .map(|c| (c[0].clone(), c[1].clone()))
        .collect()
}

/// The quote depth while walking the tree.
#[derive(Debug, Default)]
pub(crate) struct Quotes {
    depth: i64,
}

impl Quotes {
    fn open(&mut self, style: &ComputedStyle, out: &mut String) {
        let pairs = quote_pairs(style);
        if !pairs.is_empty() {
            let i = (self.depth.max(0) as usize).min(pairs.len() - 1);
            out.push_str(&pairs[i].0);
        }
        self.depth += 1;
    }

    fn close(&mut self, style: &ComputedStyle, out: &mut String) {
        if self.depth > 0 {
            self.depth -= 1;
            let pairs = quote_pairs(style);
            if !pairs.is_empty() {
                let i = (self.depth as usize).min(pairs.len() - 1);
                out.push_str(&pairs[i].1);
            }
        }
    }
}

/// The text `content` generates for a pseudo-element with `style`, or
/// `None` when it generates no box (`none`, and `normal` outside
/// `::marker`). `attr` reads attributes of the originating element.
pub(crate) fn content_text(
    style: &ComputedStyle,
    counters: &Counters,
    quotes: &mut Quotes,
    attr: &dyn Fn(&str) -> Option<String>,
) -> Option<String> {
    let text = style.text("content");
    let tokens = tokenize(&text);
    let significant: Vec<&Token> = tokens.iter().filter(|t| **t != Token::Whitespace).collect();
    if let [Token::Ident(k)] = significant.as_slice() {
        if k.eq_ignore_ascii_case("none") || k.eq_ignore_ascii_case("normal") {
            return None;
        }
    }
    let mut out = String::new();
    let mut i = 0;
    while i < tokens.len() {
        match &tokens[i] {
            Token::String(s) => out.push_str(s),
            Token::Delim('/') => break,
            Token::Ident(k) => match k.to_ascii_lowercase().as_str() {
                "open-quote" => quotes.open(style, &mut out),
                "close-quote" => quotes.close(style, &mut out),
                "no-open-quote" => quotes.depth += 1,
                "no-close-quote" => {
                    if quotes.depth > 0 {
                        quotes.depth -= 1;
                    }
                }
                _ => {}
            },
            Token::Function(f) => {
                let mut j = i + 1;
                let mut depth = 1;
                let mut args: Vec<Token> = Vec::new();
                while j < tokens.len() {
                    match &tokens[j] {
                        Token::Function(_) | Token::LeftParen => depth += 1,
                        Token::RightParen => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                    if tokens[j] != Token::Whitespace {
                        args.push(tokens[j].clone());
                    }
                    j += 1;
                }
                i = j;
                let parts: Vec<&[Token]> = args.split(|t| *t == Token::Comma).collect();
                let ident = |p: Option<&&[Token]>| match p.and_then(|p| p.first()) {
                    Some(Token::Ident(s)) => Some(s.clone()),
                    _ => None,
                };
                match f.to_ascii_lowercase().as_str() {
                    "counter" => {
                        if let Some(name) = ident(parts.first()) {
                            let st = ident(parts.get(1)).unwrap_or_else(|| "decimal".into());
                            out.push_str(&format_counter(counters.value(&name), &st));
                        }
                    }
                    "counters" => {
                        if let Some(name) = ident(parts.first()) {
                            let sep = match parts.get(1).and_then(|p| p.first()) {
                                Some(Token::String(s)) => s.clone(),
                                _ => String::new(),
                            };
                            let st = ident(parts.get(2)).unwrap_or_else(|| "decimal".into());
                            let all: Vec<String> = counters
                                .values(&name)
                                .into_iter()
                                .map(|v| format_counter(v, &st))
                                .collect();
                            out.push_str(&all.join(&sep));
                        }
                    }
                    "attr" => {
                        if let Some(name) = ident(parts.first()) {
                            let fallback = match parts.get(1).and_then(|p| p.first()) {
                                Some(Token::String(s)) => s.clone(),
                                _ => String::new(),
                            };
                            out.push_str(&attr(&name).unwrap_or(fallback));
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        i += 1;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counter_styles() {
        assert_eq!(format_counter(4, "decimal"), "4");
        assert_eq!(format_counter(4, "decimal-leading-zero"), "04");
        assert_eq!(format_counter(1994, "upper-roman"), "MCMXCIV");
        assert_eq!(format_counter(28, "lower-alpha"), "ab");
        assert_eq!(format_counter(3, "lower-greek"), "γ");
        assert_eq!(format_counter(0, "lower-alpha"), "0");
        assert_eq!(format_counter(5, "disc"), "\u{2022}");
    }

    #[test]
    fn counter_scopes() {
        let mut c = Counters::default();
        c.push();
        c.reset("x", 0, false);
        c.push();
        c.increment("x", 1);
        c.reset("x", 5, false);
        c.increment("x", 1);
        assert_eq!(c.values("x"), vec![1, 6]);
        c.pop();
        assert_eq!(c.value("x"), 1);
    }
}
