//! CSS declaration blocks (CSSOM §6.7): the parser of a list of
//! declarations of CSS Syntax 3 §5.4.5 and the serialization of a
//! block, for the `style` attribute and its `CSSStyleProperties`.
//!
//! Values are kept as their component values (tokens) and serialized
//! back from them; the property table that validates and canonicalizes
//! each value comes with the CSS values sprint (LIMITATIONS.md).

use super::tokenizer::{Numeric, Token, tokenize};

/// One declaration: a property name (lowercase unless custom), its value
/// and its importance.
#[derive(Clone, Debug, PartialEq)]
pub struct Declaration {
    pub name: String,
    pub value: Vec<Token>,
    pub important: bool,
}

/// A CSS declaration block.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DeclarationBlock {
    pub declarations: Vec<Declaration>,
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

/// The closing token of a block opened by `t`, if `t` opens one.
fn closer(t: &Token) -> Option<Token> {
    match t {
        Token::Function(_) | Token::LeftParen => Some(Token::RightParen),
        Token::LeftBracket => Some(Token::RightBracket),
        Token::LeftBrace => Some(Token::RightBrace),
        _ => None,
    }
}

/// Splits `tokens` at the semicolons outside nested blocks.
fn split_top_level(tokens: &[Token]) -> Vec<&[Token]> {
    let mut out = Vec::new();
    let mut stack: Vec<Token> = Vec::new();
    let mut start = 0;
    for (i, t) in tokens.iter().enumerate() {
        if let Some(c) = closer(t) {
            stack.push(c);
        } else if stack.last() == Some(t) {
            stack.pop();
        } else if stack.is_empty() && *t == Token::Semicolon {
            out.push(&tokens[start..i]);
            start = i + 1;
        }
    }
    out.push(&tokens[start..]);
    out
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
/// custom, not empty and without `!` (CSSOM's "parse a CSS value"
/// without the property grammar).
fn valid_value(name: &str, value: &[Token]) -> bool {
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
            _ => {}
        }
    }
    stack.is_empty()
}

/// "Consume a declaration" (CSS Syntax §5.4.6) from the tokens of one
/// declaration, or `None` if they are not one.
fn consume_declaration(tokens: &[Token]) -> Option<Declaration> {
    let tokens = trim_whitespace(tokens);
    let (Token::Ident(name), rest) = tokens.split_first()? else {
        return None;
    };
    let rest = trim_whitespace(rest);
    let (Token::Colon, value) = rest.split_first()? else {
        return None;
    };
    let mut value = trim_whitespace(value);
    let mut important = false;
    let n = value.len();
    if n >= 2 {
        if let (Token::Delim('!'), Token::Ident(word)) = (&value[n - 2], &value[n - 1]) {
            if word.eq_ignore_ascii_case("important") {
                important = true;
                value = trim_whitespace(&value[..n - 2]);
            }
        }
    }
    if !important && n >= 3 {
        if let (Token::Delim('!'), Token::Whitespace, Token::Ident(word)) =
            (&value[n - 3], &value[n - 2], &value[n - 1])
        {
            if word.eq_ignore_ascii_case("important") {
                important = true;
                value = trim_whitespace(&value[..n - 3]);
            }
        }
    }
    let name = normalize_property(name);
    if !valid_value(&name, value) {
        return None;
    }
    Some(Declaration {
        name,
        value: value.to_vec(),
        important,
    })
}

impl DeclarationBlock {
    /// Parses `text` as a list of declarations (the `style` attribute or
    /// `cssText`). A later declaration of a property replaces an earlier
    /// one unless only the earlier one is important.
    pub fn parse(text: &str) -> DeclarationBlock {
        let tokens = tokenize(text);
        let mut block = DeclarationBlock::default();
        for part in split_top_level(&tokens) {
            let Some(d) = consume_declaration(part) else {
                continue;
            };
            if let Some(i) = block.index_of(&d.name) {
                if block.declarations[i].important && !d.important {
                    continue;
                }
                block.declarations.remove(i);
            }
            block.declarations.push(d);
        }
        block
    }

    fn index_of(&self, name: &str) -> Option<usize> {
        self.declarations.iter().position(|d| d.name == name)
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

    /// `getPropertyValue(property)`.
    pub fn property_value(&self, property: &str) -> String {
        let name = normalize_property(property);
        self.index_of(&name)
            .map(|i| serialize_tokens(&self.declarations[i].value))
            .unwrap_or_default()
    }

    /// `getPropertyPriority(property)`.
    pub fn property_priority(&self, property: &str) -> &'static str {
        let name = normalize_property(property);
        match self.index_of(&name) {
            Some(i) if self.declarations[i].important => "important",
            _ => "",
        }
    }

    /// `setProperty(property, value, priority)`; true if the block
    /// changed (the `style` attribute is then updated).
    pub fn set_property(&mut self, property: &str, value: &str, priority: &str) -> bool {
        let name = normalize_property(property);
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
        let tokens = trim_whitespace(&tokens);
        if !valid_value(&name, tokens) {
            return false;
        }
        let d = Declaration {
            name,
            value: tokens.to_vec(),
            important,
        };
        match self.index_of(&d.name) {
            Some(i) => {
                if self.declarations[i] == d {
                    return false;
                }
                self.declarations[i] = d;
            }
            None => self.declarations.push(d),
        }
        true
    }

    /// `removeProperty(property)`: the removed declaration's value, or
    /// `None` if the block had no such property.
    pub fn remove_property(&mut self, property: &str) -> Option<String> {
        let name = normalize_property(property);
        let i = self.index_of(&name)?;
        let d = self.declarations.remove(i);
        Some(serialize_tokens(&d.value))
    }

    /// Serializes the block (CSSOM §6.7.2, without shorthands).
    pub fn serialize(&self) -> String {
        let mut out = String::new();
        for d in &self.declarations {
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(&d.name);
            out.push_str(": ");
            out.push_str(&serialize_tokens(&d.value));
            if d.important {
                out.push_str(" !important");
            }
            out.push(';');
        }
        out
    }
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
    fn set_and_remove_property() {
        let mut b = DeclarationBlock::default();
        assert!(b.set_property("display", "none", ""));
        assert!(!b.set_property("display", "none", ""));
        assert!(!b.set_property("color", "red", "bogus"));
        assert!(!b.set_property("color", "red;", ""));
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
    fn serializes_tokens_so_they_round_trip() {
        let t = tokenize("1em url(ab) 'x\"' a/**/b #f00 1e3 50% rgb(0, 0, 0)");
        assert_eq!(
            serialize_tokens(&t),
            "1em url(\"ab\") \"x\\\"\" a/**/b #f00 1000 50% rgb(0, 0, 0)"
        );
    }
}
