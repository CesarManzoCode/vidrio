//! The Selectors Level 4 grammar over CSS tokens: selector lists
//! (forgiving and not), complex and relative selectors, compounds, type,
//! attribute, pseudo-class and pseudo-element selectors, and `An+B`.

use super::{
    AttrOp, AttrSelector, CaseFlag, Combinator, Compound, Direction, ElementState, NsMatch, Nth,
    PseudoClass, PseudoElement, Selector, SelectorList, Simple, Specificity, compound_specificity,
    pseudo_element_specificity,
};
use crate::css::tokenizer::{Token, tokenize};
use crate::dom::Namespace;

/// A selector that does not parse.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseError;

/// The `@namespace` declarations in effect.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NamespaceMap {
    pub default: Option<Namespace>,
    pub prefixes: Vec<(String, Namespace)>,
}

impl NamespaceMap {
    fn lookup(&self, prefix: &str) -> Option<Namespace> {
        self.prefixes
            .iter()
            .find(|(p, _)| p == prefix)
            .map(|(_, ns)| *ns)
    }
}

/// Parses a selector list without namespace declarations
/// (`querySelector`, `matches`, `closest`).
pub fn parse_selector_list(input: &str) -> Result<SelectorList, ParseError> {
    parse_selector_list_with(input, &NamespaceMap::default())
}

/// Parses a selector list with the given namespaces.
pub fn parse_selector_list_with(
    input: &str,
    namespaces: &NamespaceMap,
) -> Result<SelectorList, ParseError> {
    let tokens = tokenize(input);
    let p = Parser {
        namespaces,
        in_has: false,
    };
    p.complex_list(&tokens, false)
}

/// Splits `tokens` at commas outside blocks.
fn split_commas(tokens: &[Token]) -> Vec<&[Token]> {
    let mut parts = Vec::new();
    let mut depth = 0usize;
    let mut start = 0;
    for (i, t) in tokens.iter().enumerate() {
        match t {
            Token::Function(_) | Token::LeftParen | Token::LeftBracket | Token::LeftBrace => {
                depth += 1
            }
            Token::RightParen | Token::RightBracket | Token::RightBrace => {
                depth = depth.saturating_sub(1)
            }
            Token::Comma if depth == 0 => {
                parts.push(&tokens[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&tokens[start..]);
    parts
}

/// The index just past the block that the opening token at `open` starts.
fn block_end(tokens: &[Token], open: usize) -> Result<usize, ParseError> {
    let mut depth = 0usize;
    for (i, t) in tokens.iter().enumerate().skip(open) {
        match t {
            Token::Function(_) | Token::LeftParen | Token::LeftBracket | Token::LeftBrace => {
                depth += 1
            }
            Token::RightParen | Token::RightBracket | Token::RightBrace => {
                depth -= 1;
                if depth == 0 {
                    return Ok(i + 1);
                }
            }
            _ => {}
        }
    }
    // An unclosed block ends at the end of the input (CSS Syntax §5).
    Ok(tokens.len() + 1)
}

/// The tokens inside the block opened at `open` (without the closing
/// token), and the index after it.
fn block_contents(tokens: &[Token], open: usize) -> Result<(&[Token], usize), ParseError> {
    let end = block_end(tokens, open)?;
    let inner_end = (end - 1).min(tokens.len());
    Ok((&tokens[open + 1..inner_end], end.min(tokens.len())))
}

fn trim_ws(mut tokens: &[Token]) -> &[Token] {
    while let [Token::Whitespace, rest @ ..] = tokens {
        tokens = rest;
    }
    while let [rest @ .., Token::Whitespace] = tokens {
        tokens = rest;
    }
    tokens
}

struct Parser<'a> {
    namespaces: &'a NamespaceMap,
    in_has: bool,
}

/// A cursor over a token slice.
struct Cursor<'t> {
    tokens: &'t [Token],
    pos: usize,
}

impl<'t> Cursor<'t> {
    fn peek(&self) -> Option<&'t Token> {
        self.tokens.get(self.pos)
    }

    fn peek_at(&self, n: usize) -> Option<&'t Token> {
        self.tokens.get(self.pos + n)
    }

    fn skip_ws(&mut self) -> bool {
        let start = self.pos;
        while matches!(self.peek(), Some(Token::Whitespace)) {
            self.pos += 1;
        }
        self.pos > start
    }

    fn at_end(&self) -> bool {
        self.pos >= self.tokens.len()
    }
}

impl Parser<'_> {
    /// A non-forgiving list of complex (or relative) selectors.
    fn complex_list(&self, tokens: &[Token], relative: bool) -> Result<SelectorList, ParseError> {
        let mut out = Vec::new();
        for part in split_commas(tokens) {
            out.push(self.complex(part, relative)?);
        }
        Ok(SelectorList(out))
    }

    /// A forgiving selector list: invalid entries are dropped.
    fn forgiving_list(&self, tokens: &[Token]) -> SelectorList {
        if trim_ws(tokens).is_empty() {
            return SelectorList::default();
        }
        SelectorList(
            split_commas(tokens)
                .into_iter()
                .filter_map(|part| self.complex(part, false).ok())
                .collect(),
        )
    }

    /// A complex selector; with `relative`, a relative selector whose
    /// leading combinator joins the anchor.
    fn complex(&self, tokens: &[Token], relative: bool) -> Result<Selector, ParseError> {
        let tokens = trim_ws(tokens);
        if tokens.is_empty() {
            return Err(ParseError);
        }
        let mut c = Cursor { tokens, pos: 0 };
        let mut compounds = Vec::new();
        let mut combinators = Vec::new();
        if relative {
            compounds.push(Compound(vec![Simple::Anchor]));
            let leading = self
                .explicit_combinator(&mut c)
                .unwrap_or(Combinator::Descendant);
            combinators.push(leading);
            c.skip_ws();
        }
        let mut pseudo_element = None;
        loop {
            let (compound, pe) = self.compound(&mut c)?;
            if compound.0.is_empty() && pe.is_none() {
                return Err(ParseError);
            }
            compounds.push(compound);
            let had_ws = c.skip_ws();
            if c.at_end() {
                pseudo_element = pe;
                break;
            }
            if pe.is_some() {
                // A pseudo-element ends the selector.
                return Err(ParseError);
            }
            let combinator = match self.explicit_combinator(&mut c) {
                Some(comb) => {
                    c.skip_ws();
                    comb
                }
                None if had_ws => Combinator::Descendant,
                None => return Err(ParseError),
            };
            combinators.push(combinator);
        }
        let mut specificity = compounds
            .iter()
            .map(compound_specificity)
            .fold(Specificity::ZERO, |a, b| a + b);
        if let Some(pe) = &pseudo_element {
            specificity = specificity + pseudo_element_specificity(pe);
        }
        Ok(Selector {
            compounds,
            combinators,
            pseudo_element,
            specificity,
        })
    }

    fn explicit_combinator(&self, c: &mut Cursor) -> Option<Combinator> {
        let comb = match c.peek()? {
            Token::Delim('>') => Combinator::Child,
            Token::Delim('+') => Combinator::NextSibling,
            Token::Delim('~') => Combinator::SubsequentSibling,
            _ => return None,
        };
        c.pos += 1;
        Some(comb)
    }

    /// Resolves a namespace prefix: `Some("*")` any, `Some("")` none.
    fn resolve_prefix(
        &self,
        prefix: Option<&str>,
        default_applies: bool,
    ) -> Result<NsMatch, ParseError> {
        match prefix {
            Some("*") => Ok(NsMatch::Any),
            Some("") => Ok(NsMatch::None),
            Some(p) => self.namespaces.lookup(p).map(NsMatch::Is).ok_or(ParseError),
            None if default_applies => {
                Ok(self.namespaces.default.map_or(NsMatch::Any, NsMatch::Is))
            }
            None => Ok(NsMatch::None),
        }
    }

    /// An optional type or universal selector with its namespace prefix.
    fn type_selector(&self, c: &mut Cursor) -> Result<Option<Simple>, ParseError> {
        let name_like =
            |t: Option<&Token>| matches!(t, Some(Token::Ident(_)) | Some(Token::Delim('*')));
        let text = |t: &Token| match t {
            Token::Ident(s) => s.clone(),
            _ => "*".to_string(),
        };
        let mut prefix: Option<String> = None;
        if name_like(c.peek())
            && c.peek_at(1) == Some(&Token::Delim('|'))
            && name_like(c.peek_at(2))
        {
            prefix = Some(text(c.peek().expect("token")));
            c.pos += 2;
        } else if c.peek() == Some(&Token::Delim('|')) && name_like(c.peek_at(1)) {
            prefix = Some(String::new());
            c.pos += 1;
        }
        if !name_like(c.peek()) {
            return if prefix.is_some() {
                Err(ParseError)
            } else {
                Ok(None)
            };
        }
        let name = text(c.peek().expect("token"));
        c.pos += 1;
        let ns = self.resolve_prefix(prefix.as_deref(), true)?;
        if name == "*" {
            return Ok(Some(Simple::Universal(ns)));
        }
        Ok(Some(Simple::Type {
            ns,
            local_lower: name.to_ascii_lowercase(),
            local: name,
        }))
    }

    /// A compound selector and its pseudo-element, if any.
    fn compound(&self, c: &mut Cursor) -> Result<(Compound, Option<PseudoElement>), ParseError> {
        let mut simples = Vec::new();
        match self.type_selector(c)? {
            Some(t) => simples.push(t),
            None => {
                if let Some(ns) = self.namespaces.default {
                    simples.push(Simple::Universal(NsMatch::Is(ns)));
                }
            }
        }
        let mut pseudo_element = None;
        loop {
            match c.peek() {
                Some(Token::Hash { value, id }) if pseudo_element.is_none() => {
                    if !id {
                        return Err(ParseError);
                    }
                    simples.push(Simple::Id(value.clone()));
                    c.pos += 1;
                }
                Some(Token::Delim('.')) if pseudo_element.is_none() => match c.peek_at(1) {
                    Some(Token::Ident(name)) => {
                        simples.push(Simple::Class(name.clone()));
                        c.pos += 2;
                    }
                    _ => return Err(ParseError),
                },
                Some(Token::LeftBracket) if pseudo_element.is_none() => {
                    let (inner, next) = block_contents(c.tokens, c.pos)?;
                    simples.push(Simple::Attribute(Box::new(self.attribute(inner)?)));
                    c.pos = next;
                }
                Some(Token::Colon) => {
                    if c.peek_at(1) == Some(&Token::Colon) {
                        if pseudo_element.is_some() {
                            return Err(ParseError);
                        }
                        c.pos += 2;
                        pseudo_element = Some(self.pseudo_element(c)?);
                    } else {
                        c.pos += 1;
                        match self.pseudo_class(c)? {
                            Ok(pc) => {
                                if pseudo_element.is_some() {
                                    // Only user action states may follow a
                                    // pseudo-element; they do not change
                                    // what the selector matches here.
                                    if !matches!(pc, PseudoClass::State(_)) {
                                        return Err(ParseError);
                                    }
                                } else {
                                    simples.push(Simple::PseudoClass(Box::new(pc)));
                                }
                            }
                            Err(pe) => {
                                if pseudo_element.is_some() {
                                    return Err(ParseError);
                                }
                                pseudo_element = Some(pe);
                            }
                        }
                    }
                }
                _ => break,
            }
        }
        // An implicit default namespace universal alone is no compound.
        if simples.len() == 1
            && pseudo_element.is_none()
            && matches!(simples[0], Simple::Universal(_))
        {
            let explicit = matches!(c.tokens.get(c.pos.wrapping_sub(1)), Some(Token::Delim('*')));
            if !explicit && self.namespaces.default.is_some() {
                simples.clear();
            }
        }
        Ok((Compound(simples), pseudo_element))
    }

    /// The contents of `[...]`.
    fn attribute(&self, tokens: &[Token]) -> Result<AttrSelector, ParseError> {
        let tokens = trim_ws(tokens);
        let mut c = Cursor { tokens, pos: 0 };
        let mut prefix: Option<String> = None;
        let ident_at = |c: &Cursor, n: usize| matches!(c.peek_at(n), Some(Token::Ident(_)));
        match (c.peek(), c.peek_at(1)) {
            (Some(Token::Ident(p)), Some(Token::Delim('|'))) if ident_at(&c, 2) => {
                prefix = Some(p.clone());
                c.pos += 2;
            }
            (Some(Token::Delim('*')), Some(Token::Delim('|'))) if ident_at(&c, 2) => {
                prefix = Some("*".into());
                c.pos += 2;
            }
            (Some(Token::Delim('|')), Some(Token::Ident(_))) => {
                prefix = Some(String::new());
                c.pos += 1;
            }
            _ => {}
        }
        let Some(Token::Ident(local)) = c.peek() else {
            return Err(ParseError);
        };
        let local = local.clone();
        c.pos += 1;
        let ns = self.resolve_prefix(prefix.as_deref(), false)?;
        c.skip_ws();
        let mut selector = AttrSelector {
            ns,
            local_lower: local.to_ascii_lowercase(),
            local,
            op: None,
            case: CaseFlag::Default,
        };
        if c.at_end() {
            return Ok(selector);
        }
        let op = match c.peek() {
            Some(Token::Delim('=')) => {
                c.pos += 1;
                AttrOp::Equals
            }
            Some(Token::Delim(d)) if c.peek_at(1) == Some(&Token::Delim('=')) => {
                let op = match d {
                    '~' => AttrOp::Includes,
                    '|' => AttrOp::DashMatch,
                    '^' => AttrOp::Prefix,
                    '$' => AttrOp::Suffix,
                    '*' => AttrOp::Substring,
                    _ => return Err(ParseError),
                };
                c.pos += 2;
                op
            }
            _ => return Err(ParseError),
        };
        c.skip_ws();
        let value = match c.peek() {
            Some(Token::Ident(v)) | Some(Token::String(v)) => v.clone(),
            _ => return Err(ParseError),
        };
        c.pos += 1;
        c.skip_ws();
        if let Some(Token::Ident(flag)) = c.peek() {
            selector.case = match flag.to_ascii_lowercase().as_str() {
                "i" => CaseFlag::Insensitive,
                "s" => CaseFlag::Sensitive,
                _ => return Err(ParseError),
            };
            c.pos += 1;
            c.skip_ws();
        }
        if !c.at_end() {
            return Err(ParseError);
        }
        selector.op = Some((op, value));
        Ok(selector)
    }

    /// A pseudo-class after `:`; `Err(pe)` inside `Ok` for the legacy
    /// single-colon pseudo-elements.
    fn pseudo_class(
        &self,
        c: &mut Cursor,
    ) -> Result<Result<PseudoClass, PseudoElement>, ParseError> {
        match c.peek() {
            Some(Token::Ident(name)) => {
                c.pos += 1;
                let name = name.to_ascii_lowercase();
                let pc = match name.as_str() {
                    "before" => return Ok(Err(PseudoElement::Before)),
                    "after" => return Ok(Err(PseudoElement::After)),
                    "first-line" => return Ok(Err(PseudoElement::FirstLine)),
                    "first-letter" => return Ok(Err(PseudoElement::FirstLetter)),
                    "root" => PseudoClass::Root,
                    "empty" => PseudoClass::Empty,
                    "first-child" => PseudoClass::FirstChild,
                    "last-child" => PseudoClass::LastChild,
                    "only-child" => PseudoClass::OnlyChild,
                    "first-of-type" => PseudoClass::FirstOfType,
                    "last-of-type" => PseudoClass::LastOfType,
                    "only-of-type" => PseudoClass::OnlyOfType,
                    "scope" => PseudoClass::Scope,
                    "defined" => PseudoClass::Defined,
                    "host" => PseudoClass::Host(None),
                    other => PseudoClass::State(state_by_name(other).ok_or(ParseError)?),
                };
                Ok(Ok(pc))
            }
            Some(Token::Function(name)) => {
                let name = name.to_ascii_lowercase();
                let (args, next) = block_contents(c.tokens, c.pos)?;
                c.pos = next;
                let pc = match name.as_str() {
                    "nth-child" | "nth-last-child" => {
                        let (nth, of) = self.nth_with_of(args)?;
                        if name == "nth-child" {
                            PseudoClass::NthChild(nth, of)
                        } else {
                            PseudoClass::NthLastChild(nth, of)
                        }
                    }
                    "nth-of-type" | "nth-last-of-type" => {
                        let mut ac = Cursor {
                            tokens: trim_ws(args),
                            pos: 0,
                        };
                        let nth = parse_nth(&mut ac)?;
                        ac.skip_ws();
                        if !ac.at_end() {
                            return Err(ParseError);
                        }
                        if name == "nth-of-type" {
                            PseudoClass::NthOfType(nth)
                        } else {
                            PseudoClass::NthLastOfType(nth)
                        }
                    }
                    "is" => PseudoClass::Is(self.no_pseudo_elements(self.forgiving_list(args))),
                    "where" => {
                        PseudoClass::Where(self.no_pseudo_elements(self.forgiving_list(args)))
                    }
                    "not" => {
                        let list = self.complex_list(args, false)?;
                        if list.0.iter().any(|s| s.pseudo_element.is_some()) {
                            return Err(ParseError);
                        }
                        PseudoClass::Not(list)
                    }
                    "has" => {
                        if self.in_has {
                            return Err(ParseError);
                        }
                        let inner = Parser {
                            namespaces: self.namespaces,
                            in_has: true,
                        };
                        let list = inner.complex_list(args, true)?;
                        if list.0.iter().any(|s| s.pseudo_element.is_some()) {
                            return Err(ParseError);
                        }
                        PseudoClass::Has(list)
                    }
                    "host" => PseudoClass::Host(Some(self.single_compound(args)?)),
                    "host-context" => PseudoClass::HostContext(self.single_compound(args)?),
                    "lang" => {
                        let mut ranges = Vec::new();
                        for part in split_commas(args) {
                            match trim_ws(part) {
                                [Token::Ident(s)] | [Token::String(s)] => ranges.push(s.clone()),
                                _ => return Err(ParseError),
                            }
                        }
                        PseudoClass::Lang(ranges)
                    }
                    "dir" => match trim_ws(args) {
                        [Token::Ident(d)] if d.eq_ignore_ascii_case("ltr") => {
                            PseudoClass::Dir(Direction::Ltr)
                        }
                        [Token::Ident(d)] if d.eq_ignore_ascii_case("rtl") => {
                            PseudoClass::Dir(Direction::Rtl)
                        }
                        _ => return Err(ParseError),
                    },
                    _ => return Err(ParseError),
                };
                Ok(Ok(pc))
            }
            _ => Err(ParseError),
        }
    }

    /// Drops the entries with pseudo-elements from a forgiving list.
    fn no_pseudo_elements(&self, list: SelectorList) -> SelectorList {
        SelectorList(
            list.0
                .into_iter()
                .filter(|s| s.pseudo_element.is_none())
                .collect(),
        )
    }

    /// A single compound selector (the argument of `:host()`).
    fn single_compound(&self, tokens: &[Token]) -> Result<Compound, ParseError> {
        let mut c = Cursor {
            tokens: trim_ws(tokens),
            pos: 0,
        };
        let (compound, pe) = self.compound(&mut c)?;
        if pe.is_some() || !c.at_end() || compound.0.is_empty() {
            return Err(ParseError);
        }
        Ok(compound)
    }

    /// `An+B [of S]?`.
    fn nth_with_of(&self, tokens: &[Token]) -> Result<(Nth, Option<SelectorList>), ParseError> {
        let mut c = Cursor {
            tokens: trim_ws(tokens),
            pos: 0,
        };
        let nth = parse_nth(&mut c)?;
        let had_ws = c.skip_ws();
        if c.at_end() {
            return Ok((nth, None));
        }
        match c.peek() {
            Some(Token::Ident(of)) if had_ws && of.eq_ignore_ascii_case("of") => {
                c.pos += 1;
                let list = self.complex_list(&c.tokens[c.pos..], false)?;
                if list.0.iter().any(|s| s.pseudo_element.is_some()) {
                    return Err(ParseError);
                }
                Ok((nth, Some(list)))
            }
            _ => Err(ParseError),
        }
    }

    /// A pseudo-element after `::`.
    fn pseudo_element(&self, c: &mut Cursor) -> Result<PseudoElement, ParseError> {
        match c.peek() {
            Some(Token::Ident(name)) => {
                c.pos += 1;
                Ok(match name.to_ascii_lowercase().as_str() {
                    "before" => PseudoElement::Before,
                    "after" => PseudoElement::After,
                    "first-line" => PseudoElement::FirstLine,
                    "first-letter" => PseudoElement::FirstLetter,
                    "marker" => PseudoElement::Marker,
                    "placeholder" => PseudoElement::Placeholder,
                    "selection" => PseudoElement::Selection,
                    "backdrop" => PseudoElement::Backdrop,
                    "file-selector-button" => PseudoElement::FileSelectorButton,
                    _ => return Err(ParseError),
                })
            }
            Some(Token::Function(name)) => {
                let name = name.to_ascii_lowercase();
                let (args, next) = block_contents(c.tokens, c.pos)?;
                c.pos = next;
                match name.as_str() {
                    "slotted" => Ok(PseudoElement::Slotted(self.single_compound(args)?)),
                    "part" => {
                        let mut names = Vec::new();
                        for t in trim_ws(args) {
                            match t {
                                Token::Ident(n) => names.push(n.clone()),
                                Token::Whitespace => {}
                                _ => return Err(ParseError),
                            }
                        }
                        if names.is_empty() {
                            return Err(ParseError);
                        }
                        Ok(PseudoElement::Part(names))
                    }
                    _ => Err(ParseError),
                }
            }
            _ => Err(ParseError),
        }
    }
}

/// The state pseudo-class named `name` (lowercase).
fn state_by_name(name: &str) -> Option<ElementState> {
    use ElementState::*;
    Some(match name {
        "link" => Link,
        "any-link" => AnyLink,
        "visited" => Visited,
        "local-link" => LocalLink,
        "hover" => Hover,
        "active" => Active,
        "focus" => Focus,
        "focus-visible" => FocusVisible,
        "focus-within" => FocusWithin,
        "target" => Target,
        "target-within" => TargetWithin,
        "checked" => Checked,
        "indeterminate" => Indeterminate,
        "default" => Default,
        "disabled" => Disabled,
        "enabled" => Enabled,
        "required" => Required,
        "optional" => Optional,
        "read-only" => ReadOnly,
        "read-write" => ReadWrite,
        "placeholder-shown" => PlaceholderShown,
        "valid" => Valid,
        "invalid" => Invalid,
        "in-range" => InRange,
        "out-of-range" => OutOfRange,
        "user-valid" => UserValid,
        "user-invalid" => UserInvalid,
        "autofill" => Autofill,
        "modal" => Modal,
        "popover-open" => PopoverOpen,
        "fullscreen" => Fullscreen,
        "open" => Open,
        "closed" => Closed,
        "playing" => Playing,
        "paused" => Paused,
        "blank" => Blank,
        _ => return None,
    })
}

/// Parses an integer made only of ASCII digits.
fn digits(s: &str) -> Option<i32> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// The `An+B` microsyntax (CSS Syntax §6.2).
fn parse_nth(c: &mut Cursor) -> Result<Nth, ParseError> {
    c.skip_ws();
    let int = |v: f64| -> Result<i32, ParseError> {
        if v.fract() != 0.0 || v.abs() > i32::MAX as f64 {
            return Err(ParseError);
        }
        Ok(v as i32)
    };
    let tok = c.peek().ok_or(ParseError)?.clone();
    c.pos += 1;
    // `a` and how `b` continues after the `n`.
    enum After {
        /// `b` may follow as a signed integer or `± integer`.
        Optional,
        /// `n-` written: a signless integer must follow, negated.
        DashThen,
        /// `b` is known.
        Done(i32),
    }
    let (a, after) = match &tok {
        Token::Ident(s) if s.eq_ignore_ascii_case("odd") => return Ok(Nth { a: 2, b: 1 }),
        Token::Ident(s) if s.eq_ignore_ascii_case("even") => return Ok(Nth { a: 2, b: 0 }),
        Token::Number(n) if n.integer => {
            return Ok(Nth {
                a: 0,
                b: int(n.value)?,
            });
        }
        Token::Dimension(n, unit) if n.integer => (int(n.value)?, n_unit(unit)?),
        Token::Ident(s) => {
            let lower = s.to_ascii_lowercase();
            match lower.strip_prefix('-') {
                Some(rest) => (-1, n_unit(rest)?),
                None => (1, n_unit(&lower)?),
            }
        }
        Token::Delim('+') => match c.peek() {
            Some(Token::Ident(s)) if !s.starts_with('-') => {
                let lower = s.to_ascii_lowercase();
                c.pos += 1;
                (1, n_unit(&lower)?)
            }
            _ => return Err(ParseError),
        },
        _ => return Err(ParseError),
    };
    let b = match after {
        After::Done(b) => b,
        After::DashThen => {
            c.skip_ws();
            match c.peek() {
                Some(Token::Number(n)) if n.integer && !n.signed => {
                    c.pos += 1;
                    -int(n.value)?
                }
                _ => return Err(ParseError),
            }
        }
        After::Optional => {
            let save = c.pos;
            c.skip_ws();
            match c.peek() {
                Some(Token::Number(n)) if n.integer && n.signed => {
                    c.pos += 1;
                    int(n.value)?
                }
                Some(Token::Delim(sign @ ('+' | '-'))) => {
                    let sign = if *sign == '-' { -1 } else { 1 };
                    c.pos += 1;
                    c.skip_ws();
                    match c.peek() {
                        Some(Token::Number(n)) if n.integer && !n.signed => {
                            c.pos += 1;
                            sign * int(n.value)?
                        }
                        _ => return Err(ParseError),
                    }
                }
                _ => {
                    c.pos = save;
                    0
                }
            }
        }
    };
    return Ok(Nth { a, b });

    /// What follows `a` in an `n`-unit: `n`, `n-`, or `n-<digits>`.
    fn n_unit(unit: &str) -> Result<After, ParseError> {
        let lower = unit.to_ascii_lowercase();
        if lower == "n" {
            return Ok(After::Optional);
        }
        if lower == "n-" {
            return Ok(After::DashThen);
        }
        match lower.strip_prefix("n-").and_then(digits) {
            Some(d) => Ok(After::Done(-d)),
            None => Err(ParseError),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nth(s: &str) -> Result<Nth, ParseError> {
        let tokens = tokenize(s);
        let mut c = Cursor {
            tokens: &tokens,
            pos: 0,
        };
        let n = parse_nth(&mut c)?;
        c.skip_ws();
        if c.at_end() { Ok(n) } else { Err(ParseError) }
    }

    #[test]
    fn an_plus_b() {
        assert_eq!(nth("odd"), Ok(Nth { a: 2, b: 1 }));
        assert_eq!(nth("EVEN"), Ok(Nth { a: 2, b: 0 }));
        assert_eq!(nth("3"), Ok(Nth { a: 0, b: 3 }));
        assert_eq!(nth("2n+1"), Ok(Nth { a: 2, b: 1 }));
        assert_eq!(nth("2n - 1"), Ok(Nth { a: 2, b: -1 }));
        assert_eq!(nth("2n-1"), Ok(Nth { a: 2, b: -1 }));
        assert_eq!(nth("-n+3"), Ok(Nth { a: -1, b: 3 }));
        assert_eq!(nth("+n"), Ok(Nth { a: 1, b: 0 }));
        assert_eq!(nth("n- 2"), Ok(Nth { a: 1, b: -2 }));
        assert_eq!(nth("-n-2"), Ok(Nth { a: -1, b: -2 }));
        assert_eq!(nth("+ n"), Err(ParseError));
        assert_eq!(nth("2n + -1"), Err(ParseError));
        assert_eq!(nth("1.5n"), Err(ParseError));
    }

    #[test]
    fn selector_validity() {
        for ok in [
            "a b > c + d ~ e",
            "*|a, |b, *",
            "[a|=b i]",
            "[*|a]",
            "a:not(.b, #c)",
            ":is(a, ::before, !!)",
            ":where()",
            ":has(> a, + b, ~ c d)",
            ":nth-child(2n+1 of a.b)",
            "::slotted(span.x)",
            ":host(.a)",
            "a::before:hover",
            "a:before",
            ":lang(en, \"fr\")",
            ":dir(rtl)",
        ] {
            assert!(parse_selector_list(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "a,",
            "a >",
            "#1a",
            "a:unknown",
            ":has(:has(a))",
            ":has(::before)",
            ":not(::before)",
            "ns|a",
            "a::before b",
            "[a=]",
            "[a='b' x]",
            ":nth-child(2n of)",
            "a||b",
        ] {
            assert!(parse_selector_list(bad).is_err(), "{bad}");
        }
        let ns = NamespaceMap {
            default: None,
            prefixes: vec![("svg".into(), Namespace::Svg)],
        };
        assert!(parse_selector_list_with("svg|rect", &ns).is_ok());
    }
}
