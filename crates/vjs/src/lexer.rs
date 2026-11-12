//! Lexical grammar of ECMA-262 §12 plus Annex B.1 (HTML-like comments and
//! legacy octal literals and escapes).
//!
//! The lexer is driven by the parser: it always reads the `InputElementDiv`
//! goal, and the parser asks it to re-read a `/` or `/=` as a regular
//! expression literal ([`Lexer::read_regexp`]) and a `}` as the continuation
//! of a template ([`Lexer::read_template_continuation`]). Checks that depend
//! on strict mode (legacy octals) are left to the parser through flags on the
//! token.

use crate::ast::{Span, SyntaxError, Utf16, line_column};
use crate::unicode::{is_id_continue, is_id_start, is_line_terminator, is_whitespace};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum T {
    Eof,
    /// IdentifierName, including reserved words.
    Name,
    /// `#name`.
    PrivateName,
    Number,
    BigInt,
    String,
    /// NoSubstitutionTemplate, TemplateHead, TemplateMiddle or TemplateTail.
    Template,
    RegExp,
    LBrace,
    RBrace,
    LParen,
    RParen,
    LBracket,
    RBracket,
    Dot,
    Ellipsis,
    Semi,
    Comma,
    Question,
    QuestionDot,
    Colon,
    Arrow,
    Lt,
    Gt,
    LtEq,
    GtEq,
    Eq,
    NotEq,
    StrictEq,
    StrictNotEq,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    StarStar,
    Inc,
    Dec,
    Shl,
    Shr,
    UShr,
    Amp,
    Pipe,
    Caret,
    Bang,
    Tilde,
    AndAnd,
    OrOr,
    Nullish,
    Assign,
    PlusAssign,
    MinusAssign,
    StarAssign,
    SlashAssign,
    PercentAssign,
    StarStarAssign,
    ShlAssign,
    ShrAssign,
    UShrAssign,
    AmpAssign,
    PipeAssign,
    CaretAssign,
    AndAndAssign,
    OrOrAssign,
    NullishAssign,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    None,
    /// StringValue of an IdentifierName or PrivateIdentifier (without `#`).
    Name(String),
    Number(f64),
    BigInt(String),
    String(Utf16),
    Template {
        /// Err holds the offset and message of the first invalid escape.
        cooked: Result<Utf16, (u32, &'static str)>,
        raw: String,
        /// Ends with a backtick (NoSubstitutionTemplate or TemplateTail)
        /// rather than `${`.
        tail: bool,
    },
    RegExp {
        pattern: String,
        flags: String,
    },
}

#[derive(Clone, Debug)]
pub struct Token {
    pub kind: T,
    pub span: Span,
    /// A line terminator (possibly inside a comment) precedes the token.
    pub nl_before: bool,
    /// The IdentifierName contains a Unicode escape.
    pub escaped: bool,
    /// Legacy octal or NonOctalDecimal integer literal, or a string literal
    /// with a legacy octal or `\8`/`\9` escape: a strict mode error.
    pub legacy_octal: bool,
    pub value: Value,
}

impl Token {
    pub fn name(&self) -> &str {
        match &self.value {
            Value::Name(name) => name,
            _ => "",
        }
    }
}

enum Escape {
    Ok,
    LegacyOctal,
    Invalid(u32, &'static str),
}

#[derive(Clone)]
pub struct Lexer<'a> {
    src: &'a str,
    pos: usize,
    module: bool,
    seen_token: bool,
    allow_hashbang: bool,
}

impl<'a> Lexer<'a> {
    /// `module` disables the HTML-like comments of Annex B.1.1.
    pub fn new(src: &'a str, module: bool, allow_hashbang: bool) -> Lexer<'a> {
        Lexer {
            src,
            pos: 0,
            module,
            seen_token: false,
            allow_hashbang,
        }
    }

    pub fn source(&self) -> &'a str {
        self.src
    }

    pub fn error(&self, start: usize, end: usize, message: &str) -> SyntaxError {
        let (line, column) = line_column(self.src, start as u32);
        SyntaxError {
            message: message.to_string(),
            span: Span::new(start as u32, end.max(start) as u32),
            line,
            column,
        }
    }

    fn at(&self, offset: usize) -> u8 {
        *self.src.as_bytes().get(self.pos + offset).unwrap_or(&0)
    }

    fn peek(&self) -> Option<char> {
        self.src[self.pos..].chars().next()
    }

    fn rest(&self) -> &'a str {
        &self.src[self.pos..]
    }

    fn skip_line(&mut self) {
        while let Some(c) = self.peek() {
            if is_line_terminator(c) {
                break;
            }
            self.pos += c.len_utf8();
        }
    }

    /// Skips white space, line terminators and comments; returns whether a
    /// line terminator was seen.
    fn skip_trivia(&mut self) -> Result<bool, SyntaxError> {
        let mut nl = false;
        if self.pos == 0 && self.allow_hashbang && self.src.starts_with("#!") {
            self.skip_line();
        }
        while let Some(c) = self.peek() {
            if is_line_terminator(c) {
                nl = true;
                self.pos += c.len_utf8();
            } else if is_whitespace(c) {
                self.pos += c.len_utf8();
            } else if c == '/' && self.at(1) == b'/' {
                self.skip_line();
            } else if c == '/' && self.at(1) == b'*' {
                let start = self.pos;
                let Some(end) = self.src[start + 2..].find("*/") else {
                    return Err(self.error(start, self.src.len(), "unterminated comment"));
                };
                let body = &self.src[start + 2..start + 2 + end];
                if body.chars().any(is_line_terminator) {
                    nl = true;
                }
                self.pos = start + 2 + end + 2;
            } else if !self.module && c == '<' && self.rest().starts_with("<!--") {
                self.skip_line();
            } else if !self.module
                && c == '-'
                && (nl || !self.seen_token)
                && self.rest().starts_with("-->")
            {
                self.skip_line();
            } else {
                break;
            }
        }
        Ok(nl)
    }

    pub fn next_token(&mut self) -> Result<Token, SyntaxError> {
        let nl = self.skip_trivia()?;
        let start = self.pos;
        let mut tok = Token {
            kind: T::Eof,
            span: Span::new(start as u32, start as u32),
            nl_before: nl,
            escaped: false,
            legacy_octal: false,
            value: Value::None,
        };
        let Some(c) = self.peek() else {
            return Ok(tok);
        };
        self.seen_token = true;
        let b1 = self.at(1);
        let b2 = self.at(2);
        let b3 = self.at(3);
        let (kind, len) = match c {
            '{' => (T::LBrace, 1),
            '}' => (T::RBrace, 1),
            '(' => (T::LParen, 1),
            ')' => (T::RParen, 1),
            '[' => (T::LBracket, 1),
            ']' => (T::RBracket, 1),
            ';' => (T::Semi, 1),
            ',' => (T::Comma, 1),
            ':' => (T::Colon, 1),
            '~' => (T::Tilde, 1),
            '.' if b1.is_ascii_digit() => {
                self.read_number(&mut tok)?;
                (tok.kind, 0)
            }
            '.' if b1 == b'.' && b2 == b'.' => (T::Ellipsis, 3),
            '.' => (T::Dot, 1),
            '?' if b1 == b'?' && b2 == b'=' => (T::NullishAssign, 3),
            '?' if b1 == b'?' => (T::Nullish, 2),
            '?' if b1 == b'.' && !b2.is_ascii_digit() => (T::QuestionDot, 2),
            '?' => (T::Question, 1),
            '<' if b1 == b'<' && b2 == b'=' => (T::ShlAssign, 3),
            '<' if b1 == b'<' => (T::Shl, 2),
            '<' if b1 == b'=' => (T::LtEq, 2),
            '<' => (T::Lt, 1),
            '>' if b1 == b'>' && b2 == b'>' && b3 == b'=' => (T::UShrAssign, 4),
            '>' if b1 == b'>' && b2 == b'>' => (T::UShr, 3),
            '>' if b1 == b'>' && b2 == b'=' => (T::ShrAssign, 3),
            '>' if b1 == b'>' => (T::Shr, 2),
            '>' if b1 == b'=' => (T::GtEq, 2),
            '>' => (T::Gt, 1),
            '=' if b1 == b'=' && b2 == b'=' => (T::StrictEq, 3),
            '=' if b1 == b'=' => (T::Eq, 2),
            '=' if b1 == b'>' => (T::Arrow, 2),
            '=' => (T::Assign, 1),
            '!' if b1 == b'=' && b2 == b'=' => (T::StrictNotEq, 3),
            '!' if b1 == b'=' => (T::NotEq, 2),
            '!' => (T::Bang, 1),
            '+' if b1 == b'+' => (T::Inc, 2),
            '+' if b1 == b'=' => (T::PlusAssign, 2),
            '+' => (T::Plus, 1),
            '-' if b1 == b'-' => (T::Dec, 2),
            '-' if b1 == b'=' => (T::MinusAssign, 2),
            '-' => (T::Minus, 1),
            '*' if b1 == b'*' && b2 == b'=' => (T::StarStarAssign, 3),
            '*' if b1 == b'*' => (T::StarStar, 2),
            '*' if b1 == b'=' => (T::StarAssign, 2),
            '*' => (T::Star, 1),
            '/' if b1 == b'=' => (T::SlashAssign, 2),
            '/' => (T::Slash, 1),
            '%' if b1 == b'=' => (T::PercentAssign, 2),
            '%' => (T::Percent, 1),
            '&' if b1 == b'&' && b2 == b'=' => (T::AndAndAssign, 3),
            '&' if b1 == b'&' => (T::AndAnd, 2),
            '&' if b1 == b'=' => (T::AmpAssign, 2),
            '&' => (T::Amp, 1),
            '|' if b1 == b'|' && b2 == b'=' => (T::OrOrAssign, 3),
            '|' if b1 == b'|' => (T::OrOr, 2),
            '|' if b1 == b'=' => (T::PipeAssign, 2),
            '|' => (T::Pipe, 1),
            '^' if b1 == b'=' => (T::CaretAssign, 2),
            '^' => (T::Caret, 1),
            '0'..='9' => {
                self.read_number(&mut tok)?;
                (tok.kind, 0)
            }
            '"' | '\'' => {
                let value = self.read_string(c, &mut tok)?;
                tok.value = Value::String(value);
                (T::String, 0)
            }
            '`' => {
                self.pos += 1;
                self.read_template_chars(&mut tok)?;
                (T::Template, 0)
            }
            '#' => {
                self.pos += 1;
                match self.peek() {
                    Some(n) if n == '\\' || is_id_start(n) => {}
                    _ => return Err(self.error(start, self.pos, "invalid character '#'")),
                }
                let (name, escaped) = self.read_name()?;
                tok.escaped = escaped;
                tok.value = Value::Name(name);
                (T::PrivateName, 0)
            }
            c if c == '\\' || is_id_start(c) => {
                let (name, escaped) = self.read_name()?;
                tok.escaped = escaped;
                tok.value = Value::Name(name);
                (T::Name, 0)
            }
            _ => {
                return Err(self.error(
                    start,
                    start + c.len_utf8(),
                    &format!("unexpected character {c:?}"),
                ));
            }
        };
        self.pos += len;
        tok.kind = kind;
        tok.span.end = self.pos as u32;
        Ok(tok)
    }

    /// Reads IdentifierStart IdentifierPart*; returns the StringValue and
    /// whether an escape was used.
    fn read_name(&mut self) -> Result<(String, bool), SyntaxError> {
        let mut name = String::new();
        let mut escaped = false;
        while let Some(c) = self.peek() {
            let first = name.is_empty();
            let ch = if c == '\\' {
                let start = self.pos;
                if self.at(1) != b'u' {
                    return Err(self.error(start, start + 2, "invalid escape in identifier"));
                }
                self.pos += 2;
                let ch = self.read_unicode_escape_body().and_then(char::from_u32);
                let valid = match ch {
                    Some(ch) if first => is_id_start(ch),
                    Some(ch) => is_id_continue(ch),
                    None => false,
                };
                if !valid {
                    return Err(self.error(
                        start,
                        self.pos,
                        "invalid Unicode escape in identifier",
                    ));
                }
                escaped = true;
                ch.unwrap()
            } else {
                let valid = if first {
                    is_id_start(c)
                } else {
                    is_id_continue(c)
                };
                if !valid {
                    break;
                }
                self.pos += c.len_utf8();
                c
            };
            name.push(ch);
        }
        Ok((name, escaped))
    }

    /// After `\u`: `XXXX` or `{X...}` (at most 10FFFF). `None` if malformed;
    /// the position is then somewhere inside the escape.
    fn read_unicode_escape_body(&mut self) -> Option<u32> {
        if self.at(0) == b'{' {
            self.pos += 1;
            let mut value: u32 = 0;
            let mut digits = 0;
            while let Some(d) = (self.at(0) as char).to_digit(16) {
                value = value.saturating_mul(16).saturating_add(d);
                digits += 1;
                self.pos += 1;
            }
            if digits == 0 || self.at(0) != b'}' || value > 0x10FFFF {
                return None;
            }
            self.pos += 1;
            Some(value)
        } else {
            let mut value = 0;
            for _ in 0..4 {
                let d = (self.at(0) as char).to_digit(16)?;
                value = value * 16 + d;
                self.pos += 1;
            }
            Some(value)
        }
    }

    /// Reads digits of `radix` with optional NumericLiteralSeparators.
    /// Returns the digits without separators (possibly empty).
    fn read_digits(&mut self, radix: u32, separators: bool) -> Result<String, SyntaxError> {
        let mut digits = String::new();
        loop {
            let b = self.at(0);
            if (b as char).is_digit(radix) {
                digits.push(b as char);
                self.pos += 1;
            } else if b == b'_' && separators {
                let next = self.at(1) as char;
                if digits.is_empty() || !next.is_digit(radix) {
                    return Err(self.error(self.pos, self.pos + 1, "invalid numeric separator"));
                }
                self.pos += 1;
            } else {
                return Ok(digits);
            }
        }
    }

    fn read_number(&mut self, tok: &mut Token) -> Result<(), SyntaxError> {
        let start = self.pos;
        let b0 = self.at(0);
        let b1 = self.at(1) | 0x20;
        if b0 == b'0' && matches!(b1, b'x' | b'o' | b'b') {
            let radix = match b1 {
                b'x' => 16,
                b'o' => 8,
                _ => 2,
            };
            self.pos += 2;
            if self.at(0) == b'_' {
                return Err(self.error(self.pos, self.pos + 1, "invalid numeric separator"));
            }
            let digits = self.read_digits(radix, true)?;
            if digits.is_empty() {
                return Err(self.error(start, self.pos, "missing digits after radix prefix"));
            }
            if self.at(0) == b'n' {
                self.pos += 1;
                tok.kind = T::BigInt;
                tok.value = Value::BigInt(format!("0{}{}", b1 as char, digits));
            } else {
                tok.kind = T::Number;
                tok.value = Value::Number(radix_to_f64(&digits, radix));
            }
        } else if b0 == b'0' && self.at(1).is_ascii_digit() {
            // LegacyOctalIntegerLiteral or NonOctalDecimalIntegerLiteral.
            tok.legacy_octal = true;
            self.pos += 1;
            let digits = self.read_digits(10, false)?;
            tok.kind = T::Number;
            if digits.bytes().all(|b| b < b'8') {
                tok.value = Value::Number(radix_to_f64(&digits, 8));
            } else {
                let value = self.read_decimal_tail(format!("0{digits}"))?;
                tok.value = Value::Number(value);
            }
        } else {
            if b0 == b'0' && self.at(1) == b'_' {
                return Err(self.error(start + 1, start + 2, "invalid numeric separator"));
            }
            let int = if b0 == b'.' {
                String::new()
            } else {
                self.read_digits(10, true)?
            };
            let is_integer = !matches!(self.at(0), b'.' | b'e' | b'E');
            if self.at(0) == b'n' && is_integer {
                if int.len() > 1 && int.starts_with('0') {
                    return Err(self.error(start, self.pos, "invalid BigInt literal"));
                }
                self.pos += 1;
                tok.kind = T::BigInt;
                tok.value = Value::BigInt(int);
            } else {
                let value = self.read_decimal_tail(int)?;
                tok.kind = T::Number;
                tok.value = Value::Number(value);
            }
        }
        if let Some(c) = self.peek() {
            if c == '\\' || is_id_start(c) || c.is_ascii_digit() {
                return Err(self.error(
                    start,
                    self.pos + c.len_utf8(),
                    "identifier starts immediately after numeric literal",
                ));
            }
        }
        Ok(())
    }

    /// Reads the optional fraction and exponent after the integer digits.
    fn read_decimal_tail(&mut self, int: String) -> Result<f64, SyntaxError> {
        let mut text = if int.is_empty() { "0".to_string() } else { int };
        if self.at(0) == b'.' {
            self.pos += 1;
            if self.at(0) == b'_' {
                return Err(self.error(self.pos, self.pos + 1, "invalid numeric separator"));
            }
            let fraction = self.read_digits(10, true)?;
            text.push('.');
            text.push_str(if fraction.is_empty() { "0" } else { &fraction });
        }
        if matches!(self.at(0), b'e' | b'E') {
            self.pos += 1;
            text.push('e');
            if matches!(self.at(0), b'+' | b'-') {
                text.push(self.at(0) as char);
                self.pos += 1;
            }
            if self.at(0) == b'_' {
                return Err(self.error(self.pos, self.pos + 1, "invalid numeric separator"));
            }
            let exponent = self.read_digits(10, true)?;
            if exponent.is_empty() {
                return Err(self.error(self.pos, self.pos + 1, "missing exponent"));
            }
            text.push_str(&exponent);
        }
        Ok(text.parse::<f64>().unwrap_or(f64::NAN))
    }

    fn read_string(&mut self, quote: char, tok: &mut Token) -> Result<Utf16, SyntaxError> {
        let start = self.pos;
        self.pos += 1;
        let mut out = Vec::new();
        loop {
            let Some(c) = self.peek() else {
                return Err(self.error(start, self.pos, "unterminated string literal"));
            };
            if c == quote {
                self.pos += 1;
                return Ok(out);
            }
            match c {
                '\\' => {
                    self.pos += 1;
                    if self.peek().is_none() {
                        return Err(self.error(start, self.pos, "unterminated string literal"));
                    }
                    match self.read_escape(&mut out, false) {
                        Escape::Ok => {}
                        Escape::LegacyOctal => tok.legacy_octal = true,
                        Escape::Invalid(at, message) => {
                            return Err(self.error(at as usize, self.pos, message));
                        }
                    }
                }
                '\n' | '\r' => {
                    return Err(self.error(start, self.pos, "unterminated string literal"));
                }
                _ => {
                    push_char(&mut out, c);
                    self.pos += c.len_utf8();
                }
            }
        }
    }

    /// Reads one escape sequence; the position is just after the backslash.
    fn read_escape(&mut self, out: &mut Utf16, template: bool) -> Escape {
        let start = (self.pos - 1) as u32;
        let Some(c) = self.peek() else {
            return Escape::Invalid(start, "unterminated escape sequence");
        };
        self.pos += c.len_utf8();
        match c {
            '\r' => {
                if self.at(0) == b'\n' {
                    self.pos += 1;
                }
            }
            '\n' | '\u{2028}' | '\u{2029}' => {}
            'b' => out.push(8),
            't' => out.push(9),
            'n' => out.push(10),
            'v' => out.push(11),
            'f' => out.push(12),
            'r' => out.push(13),
            '0' if !self.at(0).is_ascii_digit() => out.push(0),
            '0'..='7' => {
                if template {
                    return Escape::Invalid(
                        start,
                        "octal escape sequences are not allowed in templates",
                    );
                }
                let mut value = c as u32 - '0' as u32;
                let max = if c <= '3' { 3 } else { 2 };
                let mut len = 1;
                while len < max && (b'0'..=b'7').contains(&self.at(0)) {
                    value = value * 8 + (self.at(0) - b'0') as u32;
                    self.pos += 1;
                    len += 1;
                }
                out.push(value as u16);
                return Escape::LegacyOctal;
            }
            '8' | '9' => {
                if template {
                    return Escape::Invalid(start, "\\8 and \\9 are not allowed in templates");
                }
                out.push(c as u16);
                return Escape::LegacyOctal;
            }
            'x' => {
                let hi = (self.at(0) as char).to_digit(16);
                let lo = (self.at(1) as char).to_digit(16);
                match (hi, lo) {
                    (Some(hi), Some(lo)) => {
                        self.pos += 2;
                        out.push((hi * 16 + lo) as u16);
                    }
                    _ => return Escape::Invalid(start, "invalid hexadecimal escape sequence"),
                }
            }
            'u' => match self.read_unicode_escape_body() {
                Some(cp) => push_code_point(out, cp),
                None => return Escape::Invalid(start, "invalid Unicode escape sequence"),
            },
            _ => push_char(out, c),
        }
        Escape::Ok
    }

    /// Reads template characters after a backtick or `}` up to and including
    /// the closing backtick or `${`.
    fn read_template_chars(&mut self, tok: &mut Token) -> Result<(), SyntaxError> {
        let start = tok.span.start as usize;
        let content_start = self.pos;
        let mut cooked = Vec::new();
        let mut invalid = None;
        let (content_end, tail) = loop {
            let Some(c) = self.peek() else {
                return Err(self.error(start, self.pos, "unterminated template literal"));
            };
            match c {
                '`' => {
                    let end = self.pos;
                    self.pos += 1;
                    break (end, true);
                }
                '$' if self.at(1) == b'{' => {
                    let end = self.pos;
                    self.pos += 2;
                    break (end, false);
                }
                '\\' => {
                    self.pos += 1;
                    if let Escape::Invalid(at, message) = self.read_escape(&mut cooked, true) {
                        invalid.get_or_insert((at, message));
                    }
                }
                '\r' => {
                    self.pos += 1;
                    if self.at(0) == b'\n' {
                        self.pos += 1;
                    }
                    cooked.push(10);
                }
                _ => {
                    push_char(&mut cooked, c);
                    self.pos += c.len_utf8();
                }
            }
        };
        let raw = self.src[content_start..content_end]
            .replace("\r\n", "\n")
            .replace('\r', "\n");
        tok.kind = T::Template;
        tok.value = Value::Template {
            cooked: match invalid {
                Some(error) => Err(error),
                None => Ok(cooked),
            },
            raw,
            tail,
        };
        Ok(())
    }

    /// Re-reads the token that starts at `start` (a `}`) as a TemplateMiddle
    /// or TemplateTail.
    pub fn read_template_continuation(
        &mut self,
        start: u32,
        nl_before: bool,
    ) -> Result<Token, SyntaxError> {
        self.pos = start as usize + 1;
        let mut tok = Token {
            kind: T::Template,
            span: Span::new(start, start),
            nl_before,
            escaped: false,
            legacy_octal: false,
            value: Value::None,
        };
        self.read_template_chars(&mut tok)?;
        tok.span.end = self.pos as u32;
        Ok(tok)
    }

    /// Re-reads the token that starts at `start` (a `/` or `/=`) as a
    /// RegularExpressionLiteral. Only the body and flags are delimited here;
    /// the pattern is validated by the parser's RegExp hook.
    pub fn read_regexp(&mut self, start: u32, nl_before: bool) -> Result<Token, SyntaxError> {
        let start = start as usize;
        self.pos = start + 1;
        let mut in_class = false;
        loop {
            let Some(c) = self.peek() else {
                return Err(self.error(start, self.pos, "unterminated regular expression"));
            };
            if is_line_terminator(c) {
                return Err(self.error(start, self.pos, "unterminated regular expression"));
            }
            self.pos += c.len_utf8();
            match c {
                '\\' => match self.peek() {
                    Some(d) if !is_line_terminator(d) => self.pos += d.len_utf8(),
                    _ => {
                        return Err(self.error(start, self.pos, "unterminated regular expression"));
                    }
                },
                '[' => in_class = true,
                ']' => in_class = false,
                '/' if !in_class => break,
                _ => {}
            }
        }
        let pattern = self.src[start + 1..self.pos - 1].to_string();
        let flags_start = self.pos;
        while let Some(c) = self.peek() {
            if c == '\\' {
                return Err(self.error(
                    self.pos,
                    self.pos + 1,
                    "escape in regular expression flags",
                ));
            }
            if !is_id_continue(c) {
                break;
            }
            self.pos += c.len_utf8();
        }
        let flags = self.src[flags_start..self.pos].to_string();
        Ok(Token {
            kind: T::RegExp,
            span: Span::new(start as u32, self.pos as u32),
            nl_before,
            escaped: false,
            legacy_octal: false,
            value: Value::RegExp { pattern, flags },
        })
    }
}

fn push_char(out: &mut Utf16, c: char) {
    let mut buf = [0u16; 2];
    out.extend_from_slice(c.encode_utf16(&mut buf));
}

fn push_code_point(out: &mut Utf16, cp: u32) {
    match char::from_u32(cp) {
        Some(c) => push_char(out, c),
        // A lone surrogate written as an escape.
        None => out.push(cp as u16),
    }
}

/// Converts digits of a power-of-two radix (or 10) to the nearest double,
/// rounding ties to even.
pub fn radix_to_f64(digits: &str, radix: u32) -> f64 {
    if radix == 10 {
        return digits.parse().unwrap_or(f64::NAN);
    }
    let bits_per_digit = radix.trailing_zeros();
    let mut bits: Vec<bool> = Vec::with_capacity(digits.len() * bits_per_digit as usize);
    for c in digits.chars() {
        let d = c.to_digit(radix).unwrap_or(0);
        for i in (0..bits_per_digit).rev() {
            let bit = (d >> i) & 1 == 1;
            if bit || !bits.is_empty() {
                bits.push(bit);
            }
        }
    }
    if bits.len() <= 53 {
        let mut value: u64 = 0;
        for bit in &bits {
            value = value << 1 | *bit as u64;
        }
        return value as f64;
    }
    let mut mantissa: u64 = 0;
    for bit in &bits[..53] {
        mantissa = mantissa << 1 | *bit as u64;
    }
    let round = bits[53];
    let sticky = bits[54..].iter().any(|b| *b);
    if round && (sticky || mantissa & 1 == 1) {
        mantissa += 1;
    }
    let exponent = (bits.len() - 53) as i32;
    if exponent > 1100 {
        return f64::INFINITY;
    }
    mantissa as f64 * 2f64.powi(exponent)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(src: &str, module: bool) -> Vec<Token> {
        let mut lexer = Lexer::new(src, module, true);
        let mut out = Vec::new();
        loop {
            let tok = lexer.next_token().unwrap();
            if tok.kind == T::Eof {
                return out;
            }
            out.push(tok);
        }
    }

    fn kinds(src: &str) -> Vec<T> {
        tokens(src, false).iter().map(|t| t.kind).collect()
    }

    fn number(src: &str) -> f64 {
        match tokens(src, false)[0].value {
            Value::Number(n) => n,
            ref other => panic!("{other:?}"),
        }
    }

    fn lex_error(src: &str) -> bool {
        let mut lexer = Lexer::new(src, false, true);
        loop {
            match lexer.next_token() {
                Err(_) => return true,
                Ok(tok) if tok.kind == T::Eof => return false,
                Ok(_) => {}
            }
        }
    }

    #[test]
    fn punctuators_use_longest_match() {
        assert_eq!(
            kinds(">>>= ?. ?.5 ??= **="),
            [
                T::UShrAssign,
                T::QuestionDot,
                T::Question,
                T::Number,
                T::NullishAssign,
                T::StarStarAssign
            ]
        );
        assert_eq!(kinds("a...b"), [T::Name, T::Ellipsis, T::Name]);
    }

    #[test]
    fn numbers() {
        assert_eq!(number("0x1F"), 31.0);
        assert_eq!(number("0b1010"), 10.0);
        assert_eq!(number("0o17"), 15.0);
        assert_eq!(number("1_000_000"), 1e6);
        assert_eq!(number(".5e1"), 5.0);
        assert_eq!(number("5."), 5.0);
        assert_eq!(number("010"), 8.0);
        assert_eq!(number("019"), 19.0);
        assert_eq!(number("09.5"), 9.5);
        assert_eq!(number("0x20000000000001"), 9007199254740992.0);
        assert_eq!(number("0x20000000000003"), 9007199254740996.0);
        assert!(tokens("010", false)[0].legacy_octal);
        assert_eq!(
            tokens("0x1fn", false)[0].value,
            Value::BigInt("0x1f".into())
        );
        assert_eq!(tokens("0n", false)[0].kind, T::BigInt);
        for bad in [
            "1__0", "1_", "0_1", "1._5", "1e_1", "0x", "0b2", "01n", "1.5n", "1e3n", "3in", "0x_1",
            "08_1", "1\\u0061",
        ] {
            assert!(lex_error(bad), "{bad}");
        }
    }

    #[test]
    fn strings_and_escapes() {
        let toks = tokens(
            r#"'a\x41B\u{1F600}\
b' "\0" "\101" "\8""#,
            false,
        );
        assert_eq!(
            toks[0].value,
            Value::String("aAB😀b".encode_utf16().collect())
        );
        assert!(!toks[0].legacy_octal);
        assert_eq!(toks[1].value, Value::String(vec![0]));
        assert!(!toks[1].legacy_octal);
        assert_eq!(toks[2].value, Value::String(vec![0x41]));
        assert!(toks[2].legacy_octal && toks[3].legacy_octal);
        assert_eq!(
            tokens(r"'\uD800'", false)[0].value,
            Value::String(vec![0xD800])
        );
        assert_eq!(
            tokens("'\u{2028}'", false)[0].value,
            Value::String(vec![0x2028])
        );
        for bad in ["'abc", "'a\nb'", r"'\x4'", r"'\u{110000}'", r"'\u12'"] {
            assert!(lex_error(bad), "{bad}");
        }
    }

    #[test]
    fn templates_keep_raw_and_invalid_escapes() {
        let toks = tokens("`a\\u{41}\r\n${", false);
        match &toks[0].value {
            Value::Template { cooked, raw, tail } => {
                assert_eq!(
                    cooked.as_ref().unwrap(),
                    &"aA\n".encode_utf16().collect::<Vec<_>>()
                );
                assert_eq!(raw, "a\\u{41}\n");
                assert!(!tail);
            }
            other => panic!("{other:?}"),
        }
        let toks = tokens(r"`\unicode and \01`", false);
        match &toks[0].value {
            Value::Template { cooked, tail, .. } => assert!(cooked.is_err() && *tail),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn identifiers_with_escapes_and_unicode() {
        let toks = tokens("\\u0061b c\\u{64} λ𝐀 #priv", false);
        assert_eq!(toks[0].name(), "ab");
        assert!(toks[0].escaped && toks[1].escaped && !toks[2].escaped);
        assert_eq!(toks[1].name(), "cd");
        assert_eq!(toks[2].name(), "λ𝐀");
        assert_eq!(toks[3].kind, T::PrivateName);
        assert_eq!(toks[3].name(), "priv");
        for bad in ["\\u0031", "a\\u002D", "\\x61", "\\uD800", "# a"] {
            assert!(lex_error(bad), "{bad}");
        }
    }

    #[test]
    fn comments_hashbang_and_line_terminators() {
        let toks = tokens("#!/usr/bin/env node\na /* x\n */ b // c\nc", false);
        assert_eq!(toks.len(), 3);
        assert!(toks[1].nl_before && toks[2].nl_before);
        // Annex B HTML-like comments in scripts only.
        assert_eq!(tokens("a <!-- b\n--> c\nd", false).len(), 2);
        assert_eq!(tokens("--> first line\na", false).len(), 1);
        assert_eq!(tokens("/*\n*/ --> c\nd", false).len(), 1);
        assert_eq!(kinds("a --> b"), [T::Name, T::Dec, T::Gt, T::Name]);
        assert_eq!(tokens("a <!-- b", true).len(), 5);
        assert!(lex_error("/* unterminated"));
        assert!(Lexer::new("#!x", false, false).next_token().is_err());
    }

    #[test]
    fn regexp_rescan() {
        let mut lexer = Lexer::new("/[/]\\//gi.x", false, true);
        let slash = lexer.next_token().unwrap();
        assert_eq!(slash.kind, T::Slash);
        let re = lexer.read_regexp(slash.span.start, false).unwrap();
        assert_eq!(
            re.value,
            Value::RegExp {
                pattern: "[/]\\/".into(),
                flags: "gi".into()
            }
        );
        assert_eq!(lexer.next_token().unwrap().kind, T::Dot);
        let mut lexer = Lexer::new("/a\n/", false, true);
        let slash = lexer.next_token().unwrap();
        assert!(lexer.read_regexp(slash.span.start, false).is_err());
    }
}
