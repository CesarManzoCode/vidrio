//! The parser of CSS Syntax Level 3 §5 (the current Editor's Draft, with
//! the nested rules of CSS Nesting): stylesheets, rules, at-rules, block
//! contents that mix declarations and rules, declarations and component
//! values. The grammars of each rule (selectors, media queries, `@import`,
//! …) are checked afterwards by [`super::om`].

use super::tokenizer::{Token, tokenize};

/// A component value: a preserved token, a function or a simple block.
#[derive(Clone, Debug, PartialEq)]
pub enum ComponentValue {
    Token(Token),
    /// A function: its name and its arguments.
    Function(String, Vec<ComponentValue>),
    /// A simple block: its opening token (`(`, `[` or `{`) and contents.
    Block(char, Vec<ComponentValue>),
}

/// A declaration as the parser returns it.
#[derive(Clone, Debug, PartialEq)]
pub struct RawDeclaration {
    pub name: String,
    pub value: Vec<ComponentValue>,
    pub important: bool,
}

/// An at-rule: its name (without `@`), prelude and block, if any.
#[derive(Clone, Debug, PartialEq)]
pub struct RawAtRule {
    pub name: String,
    pub prelude: Vec<ComponentValue>,
    pub block: Option<Vec<BlockItem>>,
}

/// A qualified rule: its prelude and the contents of its block.
#[derive(Clone, Debug, PartialEq)]
pub struct RawQualifiedRule {
    pub prelude: Vec<ComponentValue>,
    pub block: Vec<BlockItem>,
}

/// A rule.
#[derive(Clone, Debug, PartialEq)]
pub enum RawRule {
    At(RawAtRule),
    Qualified(RawQualifiedRule),
}

/// One item of a block's contents: a run of declarations or a rule.
#[derive(Clone, Debug, PartialEq)]
pub enum BlockItem {
    Declarations(Vec<RawDeclaration>),
    Rule(RawRule),
}

/// A syntax error of "parse a rule".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SyntaxError;

// ----- component values ------------------------------------------------------------

impl ComponentValue {
    /// Appends the tokens of the component value to `out` (blocks and
    /// functions with their closing token).
    pub fn push_tokens(&self, out: &mut Vec<Token>) {
        match self {
            ComponentValue::Token(t) => out.push(t.clone()),
            ComponentValue::Function(name, args) => {
                out.push(Token::Function(name.clone()));
                for a in args {
                    a.push_tokens(out);
                }
                out.push(Token::RightParen);
            }
            ComponentValue::Block(open, contents) => {
                let (o, c) = match open {
                    '[' => (Token::LeftBracket, Token::RightBracket),
                    '{' => (Token::LeftBrace, Token::RightBrace),
                    _ => (Token::LeftParen, Token::RightParen),
                };
                out.push(o);
                for v in contents {
                    v.push_tokens(out);
                }
                out.push(c);
            }
        }
    }

    pub fn is_whitespace(&self) -> bool {
        matches!(self, ComponentValue::Token(Token::Whitespace))
    }

    /// The identifier, if the component value is an `<ident-token>`.
    pub fn ident(&self) -> Option<&str> {
        match self {
            ComponentValue::Token(Token::Ident(s)) => Some(s),
            _ => None,
        }
    }
}

/// The tokens of a list of component values.
pub fn to_tokens(values: &[ComponentValue]) -> Vec<Token> {
    let mut out = Vec::new();
    for v in values {
        v.push_tokens(&mut out);
    }
    out
}

/// The component values without leading and trailing whitespace.
pub fn trim(mut v: &[ComponentValue]) -> &[ComponentValue] {
    while v.first().is_some_and(ComponentValue::is_whitespace) {
        v = &v[1..];
    }
    while v.last().is_some_and(ComponentValue::is_whitespace) {
        v = &v[..v.len() - 1];
    }
    v
}

/// The component values without whitespace.
pub fn non_whitespace(v: &[ComponentValue]) -> Vec<&ComponentValue> {
    v.iter().filter(|c| !c.is_whitespace()).collect()
}

/// Splits component values at top-level commas.
pub fn split_commas(v: &[ComponentValue]) -> Vec<&[ComponentValue]> {
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

/// Serializes component values back to CSS text.
pub fn serialize(values: &[ComponentValue]) -> String {
    super::declaration::serialize_tokens(&to_tokens(values))
}

// ----- the token stream ------------------------------------------------------------

/// A token stream with marks (CSS Syntax §5.3).
struct Stream {
    tokens: Vec<Token>,
    index: usize,
    marks: Vec<usize>,
}

impl Stream {
    fn new(tokens: Vec<Token>) -> Stream {
        Stream {
            tokens,
            index: 0,
            marks: Vec::new(),
        }
    }

    fn next(&self) -> Option<&Token> {
        self.tokens.get(self.index)
    }

    fn consume(&mut self) -> Option<Token> {
        let t = self.tokens.get(self.index).cloned();
        if t.is_some() {
            self.index += 1;
        }
        t
    }

    fn discard(&mut self) {
        if self.index < self.tokens.len() {
            self.index += 1;
        }
    }

    fn mark(&mut self) {
        self.marks.push(self.index);
    }

    fn restore_mark(&mut self) {
        if let Some(i) = self.marks.pop() {
            self.index = i;
        }
    }

    fn discard_mark(&mut self) {
        self.marks.pop();
    }

    fn discard_whitespace(&mut self) {
        while self.next() == Some(&Token::Whitespace) {
            self.index += 1;
        }
    }

    fn at_end(&self) -> bool {
        self.index >= self.tokens.len()
    }
}

// ----- the algorithms ------------------------------------------------------------------

/// "Consume a stylesheet's contents".
fn stylesheet_contents(s: &mut Stream) -> Vec<RawRule> {
    let mut rules = Vec::new();
    loop {
        match s.next() {
            None => return rules,
            Some(Token::Whitespace) | Some(Token::Cdo) | Some(Token::Cdc) => s.discard(),
            Some(Token::AtKeyword(_)) => {
                if let Some(r) = at_rule(s, false) {
                    rules.push(RawRule::At(r));
                }
            }
            Some(_) => {
                if let Some(r) = qualified_rule(s, false, None) {
                    rules.push(RawRule::Qualified(r));
                }
            }
        }
    }
}

/// "Consume an at-rule".
fn at_rule(s: &mut Stream, nested: bool) -> Option<RawAtRule> {
    let Some(Token::AtKeyword(name)) = s.consume() else {
        return None;
    };
    let mut rule = RawAtRule {
        name,
        prelude: Vec::new(),
        block: None,
    };
    loop {
        match s.next() {
            None => return Some(rule),
            Some(Token::Semicolon) => {
                s.discard();
                return Some(rule);
            }
            Some(Token::RightBrace) => {
                if nested {
                    return Some(rule);
                }
                s.discard();
                rule.prelude.push(ComponentValue::Token(Token::RightBrace));
            }
            Some(Token::LeftBrace) => {
                rule.block = Some(block(s));
                return Some(rule);
            }
            Some(_) => rule.prelude.push(component_value(s)),
        }
    }
}

/// True if the first two non-whitespace values of `prelude` are a custom
/// property name and a colon.
fn looks_like_custom_property(prelude: &[ComponentValue]) -> bool {
    let v = non_whitespace(prelude);
    matches!(
        (v.first(), v.get(1)),
        (Some(ComponentValue::Token(Token::Ident(n))), Some(ComponentValue::Token(Token::Colon)))
            if n.starts_with("--")
    )
}

/// "Consume a qualified rule".
fn qualified_rule(s: &mut Stream, nested: bool, stop: Option<&Token>) -> Option<RawQualifiedRule> {
    let mut prelude = Vec::new();
    loop {
        let next = s.next().cloned();
        match next {
            None => return None,
            Some(ref t) if Some(t) == stop => return None,
            Some(Token::RightBrace) => {
                if nested {
                    return None;
                }
                s.discard();
                prelude.push(ComponentValue::Token(Token::RightBrace));
            }
            Some(Token::LeftBrace) => {
                if looks_like_custom_property(&prelude) {
                    if nested {
                        bad_declaration(s, true);
                    } else {
                        block(s);
                    }
                    return None;
                }
                let block = block(s);
                return Some(RawQualifiedRule { prelude, block });
            }
            Some(_) => prelude.push(component_value(s)),
        }
    }
}

/// "Consume a block": the `{` is next.
fn block(s: &mut Stream) -> Vec<BlockItem> {
    s.discard();
    let items = block_contents(s);
    s.discard();
    items
}

/// "Consume a block's contents".
fn block_contents(s: &mut Stream) -> Vec<BlockItem> {
    let mut items = Vec::new();
    let mut decls = Vec::new();
    loop {
        match s.next() {
            Some(Token::Whitespace) | Some(Token::Semicolon) => s.discard(),
            None | Some(Token::RightBrace) => {
                if !decls.is_empty() {
                    items.push(BlockItem::Declarations(decls));
                }
                return items;
            }
            Some(Token::AtKeyword(_)) => {
                if !decls.is_empty() {
                    items.push(BlockItem::Declarations(std::mem::take(&mut decls)));
                }
                if let Some(r) = at_rule(s, true) {
                    items.push(BlockItem::Rule(RawRule::At(r)));
                }
            }
            Some(_) => {
                s.mark();
                if let Some(d) = declaration(s, true) {
                    decls.push(d);
                    s.discard_mark();
                    continue;
                }
                s.restore_mark();
                if let Some(r) = qualified_rule(s, true, Some(&Token::Semicolon)) {
                    if !decls.is_empty() {
                        items.push(BlockItem::Declarations(std::mem::take(&mut decls)));
                    }
                    items.push(BlockItem::Rule(RawRule::Qualified(r)));
                }
            }
        }
    }
}

/// "Consume a declaration".
fn declaration(s: &mut Stream, nested: bool) -> Option<RawDeclaration> {
    let name = match s.next() {
        Some(Token::Ident(n)) => n.clone(),
        _ => {
            bad_declaration(s, nested);
            return None;
        }
    };
    s.discard();
    s.discard_whitespace();
    if s.next() != Some(&Token::Colon) {
        bad_declaration(s, nested);
        return None;
    }
    s.discard();
    s.discard_whitespace();
    let mut value = component_values(s, Some(&Token::Semicolon), nested);
    let mut important = false;
    let positions: Vec<usize> = value
        .iter()
        .enumerate()
        .filter(|(_, v)| !v.is_whitespace())
        .map(|(i, _)| i)
        .collect();
    if let [.., bang, word] = positions[..] {
        let is_bang = matches!(value[bang], ComponentValue::Token(Token::Delim('!')));
        let is_important = value[word]
            .ident()
            .is_some_and(|w| w.eq_ignore_ascii_case("important"));
        if is_bang && is_important {
            value.truncate(bang);
            important = true;
        }
    }
    while value.last().is_some_and(ComponentValue::is_whitespace) {
        value.pop();
    }
    if !name.starts_with("--") {
        let has_block = value
            .iter()
            .any(|v| matches!(v, ComponentValue::Block('{', _)));
        if has_block && non_whitespace(&value).len() > 1 {
            return None;
        }
    }
    Some(RawDeclaration {
        name,
        value,
        important,
    })
}

/// "Consume the remnants of a bad declaration".
fn bad_declaration(s: &mut Stream, nested: bool) {
    loop {
        match s.next() {
            None => return,
            Some(Token::Semicolon) => {
                s.discard();
                return;
            }
            Some(Token::RightBrace) => {
                if nested {
                    return;
                }
                s.discard();
            }
            Some(_) => {
                component_value(s);
            }
        }
    }
}

/// "Consume a list of component values".
fn component_values(s: &mut Stream, stop: Option<&Token>, nested: bool) -> Vec<ComponentValue> {
    let mut out = Vec::new();
    loop {
        let next = s.next().cloned();
        match next {
            None => return out,
            Some(ref t) if Some(t) == stop => return out,
            Some(Token::RightBrace) => {
                if nested {
                    return out;
                }
                s.discard();
                out.push(ComponentValue::Token(Token::RightBrace));
            }
            Some(_) => out.push(component_value(s)),
        }
    }
}

/// "Consume a component value".
fn component_value(s: &mut Stream) -> ComponentValue {
    match s.consume() {
        Some(Token::LeftBrace) => ComponentValue::Block('{', simple_block(s, &Token::RightBrace)),
        Some(Token::LeftBracket) => {
            ComponentValue::Block('[', simple_block(s, &Token::RightBracket))
        }
        Some(Token::LeftParen) => ComponentValue::Block('(', simple_block(s, &Token::RightParen)),
        Some(Token::Function(name)) => {
            ComponentValue::Function(name, simple_block(s, &Token::RightParen))
        }
        Some(t) => ComponentValue::Token(t),
        None => ComponentValue::Token(Token::Whitespace),
    }
}

/// The contents of a simple block or function up to `end` (consumed).
fn simple_block(s: &mut Stream, end: &Token) -> Vec<ComponentValue> {
    let mut out = Vec::new();
    loop {
        if s.at_end() {
            return out;
        }
        if s.next() == Some(end) {
            s.discard();
            return out;
        }
        out.push(component_value(s));
    }
}

// ----- entry points ----------------------------------------------------------------

/// "Parse a stylesheet" from text.
pub fn parse_stylesheet(text: &str) -> Vec<RawRule> {
    let mut s = Stream::new(tokenize(text));
    stylesheet_contents(&mut s)
}

/// "Parse a rule" (for `insertRule`): exactly one rule. `nested` parses
/// it as a rule inside a style rule.
pub fn parse_rule(text: &str, nested: bool) -> Result<RawRule, SyntaxError> {
    let mut s = Stream::new(tokenize(text));
    s.discard_whitespace();
    let rule = match s.next() {
        None => return Err(SyntaxError),
        Some(Token::AtKeyword(_)) => at_rule(&mut s, nested).map(RawRule::At),
        Some(_) => qualified_rule(&mut s, nested, None).map(RawRule::Qualified),
    };
    let rule = rule.ok_or(SyntaxError)?;
    s.discard_whitespace();
    if !s.at_end() {
        return Err(SyntaxError);
    }
    Ok(rule)
}

/// "Parse a block's contents" (the `style` attribute and `cssText` of
/// declaration blocks, where the rules are then dropped).
pub fn parse_block_contents(text: &str) -> Vec<BlockItem> {
    let mut s = Stream::new(tokenize(text));
    let mut items = Vec::new();
    // At the top level a `}` is not the end of a block: it is skipped.
    loop {
        items.extend(block_contents(&mut s));
        if s.at_end() {
            return items;
        }
        s.discard();
    }
}

/// The declarations of a block's contents, in order.
pub fn declarations_of(items: &[BlockItem]) -> Vec<&RawDeclaration> {
    items
        .iter()
        .filter_map(|i| match i {
            BlockItem::Declarations(d) => Some(d),
            BlockItem::Rule(_) => None,
        })
        .flatten()
        .collect()
}

/// "Parse a list of component values".
pub fn parse_component_values(text: &str) -> Vec<ComponentValue> {
    let mut s = Stream::new(tokenize(text));
    component_values(&mut s, None, false)
}

/// "Parse a comma-separated list of component values".
pub fn parse_comma_separated(text: &str) -> Vec<Vec<ComponentValue>> {
    split_commas(&parse_component_values(text))
        .into_iter()
        .map(<[ComponentValue]>::to_vec)
        .collect()
}

/// "Parse a declaration" (`@supports` and `CSS.supports`): a whole input
/// that is one declaration.
pub fn parse_declaration(text: &str) -> Option<RawDeclaration> {
    let mut s = Stream::new(tokenize(text));
    s.discard_whitespace();
    if !matches!(s.next(), Some(Token::Ident(_))) {
        return None;
    }
    let d = declaration(&mut s, false)?;
    if !s.at_end() {
        return None;
    }
    Some(d)
}

/// "Parse a declaration" from component values.
pub fn declaration_from_values(values: &[ComponentValue]) -> Option<RawDeclaration> {
    let mut s = Stream::new(to_tokens(values));
    s.discard_whitespace();
    if !matches!(s.next(), Some(Token::Ident(_))) {
        return None;
    }
    let d = declaration(&mut s, false)?;
    if !s.at_end() {
        return None;
    }
    Some(d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules_and_nested_rules() {
        let rules = parse_stylesheet("a { color: red; b { x: y } z: w } @media screen { c {} }");
        assert_eq!(rules.len(), 2);
        let RawRule::Qualified(q) = &rules[0] else {
            panic!("a qualified rule");
        };
        assert_eq!(q.block.len(), 3);
        assert!(matches!(
            &q.block[1],
            BlockItem::Rule(RawRule::Qualified(_))
        ));
        let RawRule::At(m) = &rules[1] else {
            panic!("an at-rule");
        };
        assert_eq!(m.name, "media");
        assert_eq!(m.block.as_ref().map(Vec::len), Some(1));
    }

    #[test]
    fn declarations_recover_from_errors() {
        let items = parse_block_contents("color: red; ;; 12px; width: 1px !IMPORTANT; }  x: y");
        let d = declarations_of(&items);
        assert_eq!(d.len(), 3);
        assert!(d[1].important);
        assert_eq!(d[2].name, "x");
    }

    #[test]
    fn parse_a_rule_wants_exactly_one() {
        assert!(parse_rule("a {} b {}", false).is_err());
        assert!(parse_rule("  ", false).is_err());
        assert!(parse_rule("@import 'x';", false).is_ok());
    }
}
