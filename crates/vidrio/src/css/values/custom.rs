//! Custom properties (css-variables-1/2): `var()` substitution over token
//! streams with fallbacks, dependency cycles and guaranteed-invalid
//! values; `attr()` of css-values-5 §7.7 with `type()`, `raw-string`,
//! `number` and unit types; and registered custom properties of
//! css-properties-values-api-1 (`@property` and `CSS.registerProperty`):
//! the syntax string, the initial value, inheritance and the typed
//! computed value.

use std::collections::{HashMap, HashSet};

use super::compute::{self, Context};
use super::grammar::{G, match_grammar, parse_grammar};
use super::{Value, serialize_list, significant, to_components};
use crate::css::properties;
use crate::css::tokenizer::{Token, tokenize};

// ----- syntax strings --------------------------------------------------------------------

/// The data type names of a syntax string (§5.1).
pub const SYNTAX_TYPES: &[&str] = &[
    "length",
    "number",
    "percentage",
    "length-percentage",
    "color",
    "image",
    "url",
    "integer",
    "angle",
    "time",
    "resolution",
    "transform-function",
    "custom-ident",
    "transform-list",
    "string",
];

/// The multiplier of a syntax component.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Multiplier {
    None,
    /// `+`: a space-separated list.
    Space,
    /// `#`: a comma-separated list.
    Comma,
}

/// A syntax component: a data type or an identifier, with a multiplier.
#[derive(Clone, Debug, PartialEq)]
pub struct SyntaxComponent {
    /// A data type name (`length`) when `is_type`, else an identifier.
    pub name: String,
    pub is_type: bool,
    pub multiplier: Multiplier,
}

/// A parsed syntax string.
#[derive(Clone, Debug, PartialEq)]
pub enum Syntax {
    /// `*`.
    Universal,
    Components(Vec<SyntaxComponent>),
}

fn is_ascii_ws(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r' | '\u{C}')
}

impl Syntax {
    /// Parses a syntax string (§5.4 "consume a syntax definition").
    pub fn parse(text: &str) -> Option<Syntax> {
        let s = text.trim_matches(is_ascii_ws);
        if s.is_empty() {
            return None;
        }
        if s == "*" {
            return Some(Syntax::Universal);
        }
        let mut out = Vec::new();
        for part in s.split('|') {
            let c = part.trim_matches(is_ascii_ws);
            if c.is_empty() {
                return None;
            }
            let (body, multiplier) = if let Some(b) = c.strip_suffix('+') {
                (b, Multiplier::Space)
            } else if let Some(b) = c.strip_suffix('#') {
                (b, Multiplier::Comma)
            } else {
                (c, Multiplier::None)
            };
            if let Some(inner) = body.strip_prefix('<') {
                let name = inner.strip_suffix('>')?;
                if !SYNTAX_TYPES.contains(&name) {
                    return None;
                }
                if name == "transform-list" && multiplier != Multiplier::None {
                    return None;
                }
                out.push(SyntaxComponent {
                    name: name.to_string(),
                    is_type: true,
                    multiplier,
                });
            } else {
                match tokenize(body).as_slice() {
                    [Token::Ident(id)]
                        if !properties::is_css_wide_keyword(id)
                            && !id.eq_ignore_ascii_case("default") =>
                    {
                        out.push(SyntaxComponent {
                            name: id.clone(),
                            is_type: false,
                            multiplier,
                        })
                    }
                    _ => return None,
                }
            }
        }
        Some(Syntax::Components(out))
    }

    /// The syntax in the value definition syntax of [`super::grammar`].
    pub fn grammar(&self) -> Option<G> {
        let Syntax::Components(components) = self else {
            return None;
        };
        let parts: Vec<String> = components
            .iter()
            .map(|c| {
                let base = if !c.is_type {
                    c.name.clone()
                } else if c.name == "transform-list" {
                    "<transform-function>+".to_string()
                } else {
                    format!("<{}>", c.name)
                };
                match c.multiplier {
                    Multiplier::None => base,
                    Multiplier::Space => format!("{base}+"),
                    Multiplier::Comma => format!("{base}#"),
                }
            })
            .collect();
        parse_grammar(&parts.join(" | "))
    }

    /// Whether the syntax only accepts integers.
    fn integer_only(&self) -> bool {
        matches!(self, Syntax::Components(c) if c.iter().all(|c| c.is_type && c.name == "integer"))
    }
}

// ----- registrations ---------------------------------------------------------------------

/// Why a registration fails.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegistrationError {
    /// An invalid name, syntax or initial value (`SyntaxError`).
    Syntax,
    /// The name is already registered (`InvalidModificationError`).
    InvalidModification,
}

/// A registered custom property.
#[derive(Clone, Debug, PartialEq)]
pub struct Registration {
    pub name: String,
    pub syntax: Syntax,
    pub inherits: bool,
    /// The initial value: computed for typed syntaxes, as written for
    /// `*` (absent: the guaranteed-invalid value).
    pub initial: Option<Vec<Token>>,
}

fn has_substitution(tokens: &[Token]) -> bool {
    tokens.iter().any(|t| match t {
        Token::Function(f) => {
            f.eq_ignore_ascii_case("var")
                || f.eq_ignore_ascii_case("env")
                || f.eq_ignore_ascii_case("attr")
        }
        _ => false,
    })
}

fn trim(mut v: &[Token]) -> &[Token] {
    while v.first() == Some(&Token::Whitespace) {
        v = &v[1..];
    }
    while v.last() == Some(&Token::Whitespace) {
        v = &v[..v.len() - 1];
    }
    v
}

impl Registration {
    /// Validates a registration (`registerProperty` steps 1–6 and the
    /// `@property` rule): a custom property name, a valid syntax and, for
    /// typed syntaxes, an initial value that matches it and is
    /// computationally independent.
    pub fn new(
        name: &str,
        syntax: &str,
        inherits: bool,
        initial: Option<&str>,
    ) -> Result<Registration, RegistrationError> {
        if !name.starts_with("--") || name.len() < 3 {
            return Err(RegistrationError::Syntax);
        }
        let syntax = Syntax::parse(syntax).ok_or(RegistrationError::Syntax)?;
        let initial_tokens = initial.map(|t| trim(&tokenize(t)).to_vec());
        let initial = match (&syntax, initial_tokens) {
            (Syntax::Universal, t) => t,
            (_, None) => return Err(RegistrationError::Syntax),
            (_, Some(t)) => {
                if t.is_empty() || has_substitution(&t) {
                    return Err(RegistrationError::Syntax);
                }
                let values = parse_typed(&syntax, &t).ok_or(RegistrationError::Syntax)?;
                if !compute::is_computationally_independent(&values) {
                    return Err(RegistrationError::Syntax);
                }
                Some(computed_tokens(&syntax, &values, &Context::default()))
            }
        };
        Ok(Registration {
            name: name.to_string(),
            syntax,
            inherits,
            initial,
        })
    }

    /// The computed value of `tokens` (with `var()` already substituted)
    /// for this registration, or `None` if they do not match the syntax.
    pub fn compute(&self, tokens: &[Token], ctx: &Context) -> Option<Vec<Token>> {
        let tokens = trim(tokens);
        if self.syntax == Syntax::Universal {
            return Some(tokens.to_vec());
        }
        let values = parse_typed(&self.syntax, tokens)?;
        Some(computed_tokens(&self.syntax, &values, ctx))
    }
}

/// Matches tokens against a typed syntax.
pub fn parse_typed(syntax: &Syntax, tokens: &[Token]) -> Option<Vec<Value>> {
    let g = syntax.grammar()?;
    match_grammar(&g, &significant(&to_components(tokens)))
}

fn computed_tokens(syntax: &Syntax, values: &[Value], ctx: &Context) -> Vec<Token> {
    let mut computed = compute::compute_values(values, ctx);
    if syntax.integer_only() {
        for v in computed.iter_mut() {
            if let Value::Number(n) = v {
                *v = Value::Integer(super::round_half_up(*n));
            }
        }
    }
    tokenize(&serialize_list(&computed))
}

/// The registered custom properties of a document.
#[derive(Clone, Debug, Default)]
pub struct Registry {
    map: HashMap<String, Registration>,
}

impl Registry {
    /// Adds a registration; a name may be registered once
    /// (`registerProperty` step 7).
    pub fn register(&mut self, r: Registration) -> Result<(), RegistrationError> {
        if self.map.contains_key(&r.name) {
            return Err(RegistrationError::InvalidModification);
        }
        self.map.insert(r.name.clone(), r);
        Ok(())
    }

    pub fn get(&self, name: &str) -> Option<&Registration> {
        self.map.get(name)
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

// ----- substitution ----------------------------------------------------------------------

/// A value that is invalid at computed-value time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Invalid;

/// Where substitution gets its values.
pub struct Substitution<'a> {
    /// The computed value of a custom property (`None`: guaranteed
    /// invalid).
    pub var: &'a dyn Fn(&str) -> Option<Vec<Token>>,
    /// The value of an attribute of the element (`None`: absent).
    pub attr: &'a dyn Fn(&str) -> Option<String>,
}

/// The index of the token that closes the block or function opened at
/// `open`.
fn closing(tokens: &[Token], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (i, t) in tokens.iter().enumerate().skip(open) {
        match t {
            Token::Function(_) | Token::LeftParen | Token::LeftBracket | Token::LeftBrace => {
                depth += 1
            }
            Token::RightParen | Token::RightBracket | Token::RightBrace => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// Splits the arguments of `var()`/`attr()` at the first top-level comma:
/// (head, fallback).
fn split_fallback(args: &[Token]) -> (&[Token], Option<&[Token]>) {
    let mut depth = 0usize;
    for (i, t) in args.iter().enumerate() {
        match t {
            Token::Function(_) | Token::LeftParen | Token::LeftBracket | Token::LeftBrace => {
                depth += 1
            }
            Token::RightParen | Token::RightBracket | Token::RightBrace => {
                depth = depth.saturating_sub(1)
            }
            Token::Comma if depth == 0 => return (&args[..i], Some(&args[i + 1..])),
            _ => {}
        }
    }
    (args, None)
}

/// Replaces every `var()` and `attr()` in `value` (css-variables-1 §3,
/// css-values-5 §7.7); `Err(Invalid)` when a reference has neither a
/// value nor a fallback.
pub fn substitute(value: &[Token], s: &Substitution) -> Result<Vec<Token>, Invalid> {
    let mut out = Vec::with_capacity(value.len());
    let mut i = 0;
    while i < value.len() {
        let t = &value[i];
        let kind = match t {
            Token::Function(f) if f.eq_ignore_ascii_case("var") => Some(true),
            Token::Function(f) if f.eq_ignore_ascii_case("attr") => Some(false),
            _ => None,
        };
        let Some(is_var) = kind else {
            out.push(t.clone());
            i += 1;
            continue;
        };
        let close = closing(value, i).ok_or(Invalid)?;
        let args = &value[i + 1..close];
        let replacement = if is_var {
            substitute_var(args, s)?
        } else {
            substitute_attr(args, s)?
        };
        out.extend(replacement);
        i = close + 1;
    }
    Ok(out)
}

fn substitute_var(args: &[Token], s: &Substitution) -> Result<Vec<Token>, Invalid> {
    let (head, fallback) = split_fallback(args);
    let name = match trim(head) {
        [Token::Ident(n)] if n.starts_with("--") => n,
        _ => return Err(Invalid),
    };
    if let Some(v) = (s.var)(name) {
        return Ok(v);
    }
    match fallback {
        Some(f) => substitute(trim(f), s),
        None => Err(Invalid),
    }
}

/// Builds the text of a `type()` argument from its tokens.
fn syntax_text(tokens: &[Token]) -> String {
    let mut out = String::new();
    for t in tokens {
        match t {
            Token::Delim(c) => out.push(*c),
            Token::Ident(s) => out.push_str(s),
            Token::Whitespace => out.push(' '),
            other => out.push_str(&crate::css::declaration::serialize_tokens(&[other.clone()])),
        }
    }
    out
}

fn substitute_attr(args: &[Token], s: &Substitution) -> Result<Vec<Token>, Invalid> {
    let (head, fallback) = split_fallback(args);
    let head: Vec<&Token> = head.iter().filter(|t| **t != Token::Whitespace).collect();
    let fallback_or_invalid = || match fallback {
        Some(f) => substitute(trim(f), s),
        None => Err(Invalid),
    };
    let Some(Token::Ident(name)) = head.first() else {
        return Err(Invalid);
    };
    let Some(text) = (s.attr)(name) else {
        return fallback_or_invalid();
    };
    match &head[1..] {
        // No type: the value as a string (`raw-string`).
        [] => Ok(vec![Token::String(text)]),
        [Token::Ident(t)] if t.eq_ignore_ascii_case("raw-string") => Ok(vec![Token::String(text)]),
        [Token::Ident(t)] if t.eq_ignore_ascii_case("number") => match trim(&tokenize(&text)) {
            [Token::Number(n)] => Ok(vec![Token::Number(*n)]),
            _ => fallback_or_invalid(),
        },
        [Token::Delim('%')] => match trim(&tokenize(&text)) {
            [Token::Number(n)] => Ok(vec![Token::Percentage(*n)]),
            _ => fallback_or_invalid(),
        },
        [Token::Ident(u)] if super::unit(u).is_some() => match trim(&tokenize(&text)) {
            [Token::Number(n)] => Ok(vec![Token::Dimension(*n, u.to_ascii_lowercase())]),
            _ => fallback_or_invalid(),
        },
        [Token::Function(f), rest @ ..] if f.eq_ignore_ascii_case("type") => {
            let Some((Token::RightParen, inner)) = rest.split_last() else {
                return Err(Invalid);
            };
            let inner: Vec<Token> = inner.iter().map(|t| (*t).clone()).collect();
            let syntax = Syntax::parse(&syntax_text(&inner)).ok_or(Invalid)?;
            let tokens = tokenize(&text);
            if syntax == Syntax::Universal {
                return Ok(trim(&tokens).to_vec());
            }
            match parse_typed(&syntax, &tokens) {
                Some(values) => Ok(tokenize(&serialize_list(&values))),
                None => fallback_or_invalid(),
            }
        }
        _ => Err(Invalid),
    }
}

/// The custom property names a value references with `var()`, fallbacks
/// included.
pub fn references(value: &[Token]) -> Vec<String> {
    let mut out = Vec::new();
    for (i, t) in value.iter().enumerate() {
        if matches!(t, Token::Function(f) if f.eq_ignore_ascii_case("var")) {
            if let Some(Token::Ident(n)) = value[i + 1..].iter().find(|t| **t != Token::Whitespace)
            {
                if n.starts_with("--") && !out.contains(n) {
                    out.push(n.clone());
                }
            }
        }
    }
    out
}

// ----- computing the custom properties of an element -------------------------------------

/// The names of the custom properties that lie on a dependency cycle
/// (Tarjan's strongly connected components; a component with more than
/// one node or a self-reference is a cycle).
pub fn cyclic(specified: &HashMap<&str, &[Token]>) -> HashSet<String> {
    struct State<'a> {
        graph: HashMap<&'a str, Vec<String>>,
        index: HashMap<String, usize>,
        low: HashMap<String, usize>,
        stack: Vec<String>,
        on_stack: HashSet<String>,
        next: usize,
        out: HashSet<String>,
    }
    fn visit(st: &mut State, v: &str) {
        st.index.insert(v.to_string(), st.next);
        st.low.insert(v.to_string(), st.next);
        st.next += 1;
        st.stack.push(v.to_string());
        st.on_stack.insert(v.to_string());
        let edges = st.graph.get(v).cloned().unwrap_or_default();
        for w in &edges {
            if !st.graph.contains_key(w.as_str()) {
                continue;
            }
            if !st.index.contains_key(w) {
                visit(st, w);
                let lw = st.low[w];
                let lv = st.low[v];
                st.low.insert(v.to_string(), lv.min(lw));
            } else if st.on_stack.contains(w) {
                let iw = st.index[w];
                let lv = st.low[v];
                st.low.insert(v.to_string(), lv.min(iw));
            }
        }
        if st.low[v] == st.index[v] {
            let mut component = Vec::new();
            while let Some(w) = st.stack.pop() {
                st.on_stack.remove(&w);
                let done = w == v;
                component.push(w);
                if done {
                    break;
                }
            }
            let self_edge = edges.iter().any(|e| e == v);
            if component.len() > 1 || self_edge {
                st.out.extend(component);
            }
        }
    }
    let mut st = State {
        graph: specified.iter().map(|(k, v)| (*k, references(v))).collect(),
        index: HashMap::new(),
        low: HashMap::new(),
        stack: Vec::new(),
        on_stack: HashSet::new(),
        next: 0,
        out: HashSet::new(),
    };
    let mut names: Vec<&str> = specified.keys().copied().collect();
    names.sort_unstable();
    for n in names {
        if !st.index.contains_key(n) {
            visit(&mut st, n);
        }
    }
    st.out
}

struct Computer<'a> {
    specified: HashMap<&'a str, &'a [Token]>,
    parent: &'a HashMap<String, Vec<Token>>,
    registry: &'a Registry,
    ctx: &'a Context,
    attr: &'a dyn Fn(&str) -> Option<String>,
    cyclic: HashSet<String>,
    done: HashSet<String>,
    result: HashMap<String, Vec<Token>>,
}

impl Computer<'_> {
    /// The value a property takes when its own value is invalid at
    /// computed-value time: inherited or initial for a registered
    /// property, the guaranteed-invalid value otherwise.
    fn invalid(&mut self, name: &str) {
        match self.registry.get(name) {
            Some(r) => {
                let v = if r.inherits {
                    self.parent.get(name).cloned().or_else(|| r.initial.clone())
                } else {
                    r.initial.clone()
                };
                match v {
                    Some(v) => {
                        self.result.insert(name.to_string(), v);
                    }
                    None => {
                        self.result.remove(name);
                    }
                }
            }
            None => {
                self.result.remove(name);
            }
        }
    }

    fn set(&mut self, name: &str, tokens: Vec<Token>) {
        match self.registry.get(name) {
            Some(r) => match r.compute(&tokens, self.ctx) {
                Some(v) => {
                    self.result.insert(name.to_string(), v);
                }
                None => self.invalid(name),
            },
            None => {
                self.result.insert(name.to_string(), trim(&tokens).to_vec());
            }
        }
    }

    fn resolve(&mut self, name: &str) {
        if !self.done.insert(name.to_string()) {
            return;
        }
        let Some(tokens) = self.specified.get(name).copied() else {
            return;
        };
        let trimmed = trim(tokens);
        if let [Token::Ident(k)] = trimmed {
            if properties::is_css_wide_keyword(k) {
                let inherited = match self.registry.get(name) {
                    Some(r) => r.inherits,
                    None => true,
                };
                let use_parent = match k.to_ascii_lowercase().as_str() {
                    "inherit" => true,
                    "initial" => false,
                    _ => inherited,
                };
                let v = if use_parent {
                    self.parent.get(name).cloned()
                } else {
                    None
                };
                match v.or_else(|| self.registry.get(name).and_then(|r| r.initial.clone())) {
                    Some(v) => {
                        self.result.insert(name.to_string(), v);
                    }
                    None => {
                        self.result.remove(name);
                    }
                }
                return;
            }
        }
        if self.cyclic.contains(name) {
            self.invalid(name);
            return;
        }
        for dep in references(tokens) {
            if self.specified.contains_key(dep.as_str()) {
                self.resolve(&dep);
            }
        }
        let result = &self.result;
        let var = |n: &str| result.get(n).cloned();
        let sub = substitute(
            tokens,
            &Substitution {
                var: &var,
                attr: self.attr,
            },
        );
        match sub {
            Ok(v) => self.set(name, v),
            Err(Invalid) => self.invalid(name),
        }
    }
}

/// The computed custom properties of an element (css-variables-1 §2.2):
/// `specified` are its cascaded custom property values, `parent` the
/// parent's computed custom properties. Inherited and initial values come
/// first; `var()` and `attr()` are substituted in dependency order,
/// properties on a cycle become invalid, and registered properties are
/// parsed and computed with their syntax.
pub fn compute_custom_properties(
    specified: &[(String, Vec<Token>)],
    parent: &HashMap<String, Vec<Token>>,
    registry: &Registry,
    ctx: &Context,
    attr: &dyn Fn(&str) -> Option<String>,
) -> HashMap<String, Vec<Token>> {
    let mut result: HashMap<String, Vec<Token>> = HashMap::new();
    for (name, value) in parent {
        let inherits = registry.get(name).is_none_or(|r| r.inherits);
        if inherits {
            result.insert(name.clone(), value.clone());
        }
    }
    for r in registry.map.values() {
        if !result.contains_key(&r.name) {
            if let Some(initial) = &r.initial {
                result.insert(r.name.clone(), initial.clone());
            }
        }
    }
    let map: HashMap<&str, &[Token]> = specified
        .iter()
        .map(|(n, v)| (n.as_str(), v.as_slice()))
        .collect();
    let cyclic = cyclic(&map);
    let mut c = Computer {
        specified: map,
        parent,
        registry,
        ctx,
        attr,
        cyclic,
        done: HashSet::new(),
        result,
    };
    let mut names: Vec<&str> = specified.iter().map(|(n, _)| n.as_str()).collect();
    names.sort_unstable();
    for n in names {
        c.resolve(n);
    }
    c.result
}

/// Substitutes `var()` and `attr()` in the value of a standard property
/// with the element's computed custom properties.
pub fn substitute_with(
    value: &[Token],
    custom: &HashMap<String, Vec<Token>>,
    attr: &dyn Fn(&str) -> Option<String>,
) -> Result<Vec<Token>, Invalid> {
    let var = |n: &str| custom.get(n).cloned();
    substitute(value, &Substitution { var: &var, attr })
}

/// Serializes a custom property value (its tokens as written).
pub fn serialize_value(tokens: &[Token]) -> String {
    crate::css::declaration::serialize_tokens(trim(tokens))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_attr(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn syntax_strings() {
        assert!(Syntax::parse("<length> | auto").is_some());
        assert!(Syntax::parse("<length>+#").is_none());
        assert!(Syntax::parse("<transform-list>+").is_none());
        assert!(Syntax::parse("inherit").is_none());
        assert_eq!(Syntax::parse(" * "), Some(Syntax::Universal));
    }

    #[test]
    fn cycles_are_found() {
        let a = tokenize("var(--b)");
        let b = tokenize("var(--a)");
        let c = tokenize("var(--c)");
        let d = tokenize("var(--a, 1px)");
        let map: HashMap<&str, &[Token]> = [
            ("--a", a.as_slice()),
            ("--b", b.as_slice()),
            ("--c", c.as_slice()),
            ("--d", d.as_slice()),
        ]
        .into_iter()
        .collect();
        let cyc = cyclic(&map);
        assert!(cyc.contains("--a") && cyc.contains("--b") && cyc.contains("--c"));
        assert!(!cyc.contains("--d"));
        let specified: Vec<(String, Vec<Token>)> = map
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_vec()))
            .collect();
        let out = compute_custom_properties(
            &specified,
            &HashMap::new(),
            &Registry::default(),
            &Context::default(),
            &no_attr,
        );
        assert!(!out.contains_key("--a"));
        assert_eq!(
            out.get("--d").map(|v| serialize_value(v)).as_deref(),
            Some("1px")
        );
    }
}
