//! The CSS tokenizer of CSS Syntax Level 3 §4: input preprocessing and
//! every token type. The selector parser reads its tokens; the stylesheet
//! parser of K4 will reuse it.

/// A numeric value with its type flag.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Numeric {
    pub value: f64,
    /// The type flag is "integer".
    pub integer: bool,
    /// The number was written with a leading `+` or `-`.
    pub signed: bool,
}

/// A CSS token.
#[derive(Clone, Debug, PartialEq)]
pub enum Token {
    Ident(String),
    Function(String),
    AtKeyword(String),
    /// A hash token; `id` is the "id" type flag.
    Hash {
        value: String,
        id: bool,
    },
    String(String),
    BadString,
    Url(String),
    BadUrl,
    Delim(char),
    Number(Numeric),
    Percentage(Numeric),
    Dimension(Numeric, String),
    Whitespace,
    Cdo,
    Cdc,
    Colon,
    Semicolon,
    Comma,
    LeftBracket,
    RightBracket,
    LeftParen,
    RightParen,
    LeftBrace,
    RightBrace,
}

/// Preprocesses `input` (CSS Syntax §3.3): newlines normalized, NUL
/// replaced.
fn preprocess(input: &str) -> Vec<char> {
    let mut out = Vec::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push('\n');
            }
            '\u{C}' => out.push('\n'),
            '\0' => out.push('\u{FFFD}'),
            c => out.push(c),
        }
    }
    out
}

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_' || c >= '\u{80}'
}

fn is_ident_char(c: char) -> bool {
    is_ident_start(c) || c.is_ascii_digit() || c == '-'
}

fn is_whitespace(c: char) -> bool {
    matches!(c, '\n' | '\t' | ' ')
}

fn is_non_printable(c: char) -> bool {
    matches!(c, '\0'..='\u{8}' | '\u{B}' | '\u{E}'..='\u{1F}' | '\u{7F}')
}

/// Tokenizes `input` into a list of tokens (without the EOF token).
pub fn tokenize(input: &str) -> Vec<Token> {
    let mut t = Tokenizer {
        chars: preprocess(input),
        pos: 0,
    };
    let mut out = Vec::new();
    while let Some(tok) = t.next_token() {
        out.push(tok);
    }
    out
}

struct Tokenizer {
    chars: Vec<char>,
    pos: usize,
}

impl Tokenizer {
    fn peek(&self, n: usize) -> Option<char> {
        self.chars.get(self.pos + n).copied()
    }

    fn valid_escape(a: Option<char>, b: Option<char>) -> bool {
        a == Some('\\') && b != Some('\n') && b.is_some()
    }

    fn starts_ident(a: Option<char>, b: Option<char>, c: Option<char>) -> bool {
        match a {
            Some('-') => {
                b.is_some_and(|b| is_ident_start(b) || b == '-') || Self::valid_escape(b, c)
            }
            Some(x) if is_ident_start(x) => true,
            Some('\\') => Self::valid_escape(a, b),
            _ => false,
        }
    }

    fn starts_number(a: Option<char>, b: Option<char>, c: Option<char>) -> bool {
        match a {
            Some('+') | Some('-') => {
                b.is_some_and(|b| b.is_ascii_digit())
                    || (b == Some('.') && c.is_some_and(|c| c.is_ascii_digit()))
            }
            Some('.') => b.is_some_and(|b| b.is_ascii_digit()),
            Some(x) => x.is_ascii_digit(),
            None => false,
        }
    }

    fn consume_comments(&mut self) {
        while self.peek(0) == Some('/') && self.peek(1) == Some('*') {
            self.pos += 2;
            loop {
                match self.peek(0) {
                    None => return,
                    Some('*') if self.peek(1) == Some('/') => {
                        self.pos += 2;
                        break;
                    }
                    _ => self.pos += 1,
                }
            }
        }
    }

    fn next_token(&mut self) -> Option<Token> {
        self.consume_comments();
        let c = self.peek(0)?;
        if is_whitespace(c) {
            while self.peek(0).is_some_and(is_whitespace) {
                self.pos += 1;
            }
            return Some(Token::Whitespace);
        }
        let (n1, n2) = (self.peek(1), self.peek(2));
        let tok = match c {
            '"' | '\'' => {
                self.pos += 1;
                self.consume_string(c)
            }
            '#' => {
                if n1.is_some_and(is_ident_char) || Self::valid_escape(n1, n2) {
                    self.pos += 1;
                    let id = Self::starts_ident(self.peek(0), self.peek(1), self.peek(2));
                    let value = self.consume_ident_sequence();
                    Token::Hash { value, id }
                } else {
                    self.pos += 1;
                    Token::Delim('#')
                }
            }
            '(' => self.single(Token::LeftParen),
            ')' => self.single(Token::RightParen),
            ',' => self.single(Token::Comma),
            ':' => self.single(Token::Colon),
            ';' => self.single(Token::Semicolon),
            '[' => self.single(Token::LeftBracket),
            ']' => self.single(Token::RightBracket),
            '{' => self.single(Token::LeftBrace),
            '}' => self.single(Token::RightBrace),
            '+' | '.' if Self::starts_number(Some(c), n1, n2) => self.consume_numeric(),
            '-' => {
                if Self::starts_number(Some(c), n1, n2) {
                    self.consume_numeric()
                } else if n1 == Some('-') && n2 == Some('>') {
                    self.pos += 3;
                    Token::Cdc
                } else if Self::starts_ident(Some(c), n1, n2) {
                    self.consume_ident_like()
                } else {
                    self.single(Token::Delim('-'))
                }
            }
            '<' if n1 == Some('!') && n2 == Some('-') && self.peek(3) == Some('-') => {
                self.pos += 4;
                Token::Cdo
            }
            '@' => {
                if Self::starts_ident(n1, n2, self.peek(3)) {
                    self.pos += 1;
                    Token::AtKeyword(self.consume_ident_sequence())
                } else {
                    self.single(Token::Delim('@'))
                }
            }
            '\\' => {
                if Self::valid_escape(Some(c), n1) {
                    self.consume_ident_like()
                } else {
                    self.single(Token::Delim('\\'))
                }
            }
            d if d.is_ascii_digit() => self.consume_numeric(),
            s if is_ident_start(s) => self.consume_ident_like(),
            other => self.single(Token::Delim(other)),
        };
        Some(tok)
    }

    fn single(&mut self, tok: Token) -> Token {
        self.pos += 1;
        tok
    }

    /// Consumes an escaped code point; the backslash is already consumed.
    fn consume_escape(&mut self) -> char {
        let Some(c) = self.peek(0) else {
            return '\u{FFFD}';
        };
        self.pos += 1;
        if c.is_ascii_hexdigit() {
            let mut value = c.to_digit(16).unwrap_or(0);
            let mut n = 1;
            while n < 6 {
                match self.peek(0) {
                    Some(h) if h.is_ascii_hexdigit() => {
                        value = value * 16 + h.to_digit(16).unwrap_or(0);
                        self.pos += 1;
                        n += 1;
                    }
                    _ => break,
                }
            }
            if self.peek(0).is_some_and(is_whitespace) {
                self.pos += 1;
            }
            if value == 0 || (0xD800..=0xDFFF).contains(&value) || value > 0x10FFFF {
                return '\u{FFFD}';
            }
            return char::from_u32(value).unwrap_or('\u{FFFD}');
        }
        c
    }

    fn consume_ident_sequence(&mut self) -> String {
        let mut out = String::new();
        loop {
            match self.peek(0) {
                Some(c) if is_ident_char(c) => {
                    out.push(c);
                    self.pos += 1;
                }
                Some('\\') if Self::valid_escape(Some('\\'), self.peek(1)) => {
                    self.pos += 1;
                    out.push(self.consume_escape());
                }
                _ => return out,
            }
        }
    }

    fn consume_ident_like(&mut self) -> Token {
        let name = self.consume_ident_sequence();
        if name.eq_ignore_ascii_case("url") && self.peek(0) == Some('(') {
            self.pos += 1;
            while self.peek(0).is_some_and(is_whitespace) && self.peek(1).is_some_and(is_whitespace)
            {
                self.pos += 1;
            }
            let quote = |c: Option<char>| matches!(c, Some('"') | Some('\''));
            if quote(self.peek(0))
                || (self.peek(0).is_some_and(is_whitespace) && quote(self.peek(1)))
            {
                return Token::Function(name);
            }
            return self.consume_url();
        }
        if self.peek(0) == Some('(') {
            self.pos += 1;
            return Token::Function(name);
        }
        Token::Ident(name)
    }

    fn consume_string(&mut self, ending: char) -> Token {
        let mut out = String::new();
        loop {
            match self.peek(0) {
                None => return Token::String(out),
                Some(c) if c == ending => {
                    self.pos += 1;
                    return Token::String(out);
                }
                Some('\n') => return Token::BadString,
                Some('\\') => {
                    self.pos += 1;
                    match self.peek(0) {
                        None => {}
                        Some('\n') => self.pos += 1,
                        Some(_) => out.push(self.consume_escape()),
                    }
                }
                Some(c) => {
                    out.push(c);
                    self.pos += 1;
                }
            }
        }
    }

    fn consume_url(&mut self) -> Token {
        let mut out = String::new();
        while self.peek(0).is_some_and(is_whitespace) {
            self.pos += 1;
        }
        loop {
            match self.peek(0) {
                None => return Token::Url(out),
                Some(')') => {
                    self.pos += 1;
                    return Token::Url(out);
                }
                Some(c) if is_whitespace(c) => {
                    while self.peek(0).is_some_and(is_whitespace) {
                        self.pos += 1;
                    }
                    match self.peek(0) {
                        None => return Token::Url(out),
                        Some(')') => {
                            self.pos += 1;
                            return Token::Url(out);
                        }
                        _ => {
                            self.consume_bad_url();
                            return Token::BadUrl;
                        }
                    }
                }
                Some('"') | Some('\'') | Some('(') => {
                    self.consume_bad_url();
                    return Token::BadUrl;
                }
                Some(c) if is_non_printable(c) => {
                    self.consume_bad_url();
                    return Token::BadUrl;
                }
                Some('\\') => {
                    if Self::valid_escape(Some('\\'), self.peek(1)) {
                        self.pos += 1;
                        out.push(self.consume_escape());
                    } else {
                        self.consume_bad_url();
                        return Token::BadUrl;
                    }
                }
                Some(c) => {
                    out.push(c);
                    self.pos += 1;
                }
            }
        }
    }

    fn consume_bad_url(&mut self) {
        loop {
            match self.peek(0) {
                None => return,
                Some(')') => {
                    self.pos += 1;
                    return;
                }
                Some('\\') if Self::valid_escape(Some('\\'), self.peek(1)) => {
                    self.pos += 1;
                    self.consume_escape();
                }
                _ => self.pos += 1,
            }
        }
    }

    fn consume_number(&mut self) -> Numeric {
        let mut repr = String::new();
        let mut integer = true;
        let mut signed = false;
        if let Some(s @ ('+' | '-')) = self.peek(0) {
            repr.push(s);
            signed = true;
            self.pos += 1;
        }
        let digits = |t: &mut Tokenizer, repr: &mut String| {
            while let Some(d) = t.peek(0).filter(char::is_ascii_digit) {
                repr.push(d);
                t.pos += 1;
            }
        };
        digits(self, &mut repr);
        if self.peek(0) == Some('.') && self.peek(1).is_some_and(|c| c.is_ascii_digit()) {
            repr.push('.');
            self.pos += 1;
            integer = false;
            digits(self, &mut repr);
        }
        if let Some('e' | 'E') = self.peek(0) {
            let (n1, n2) = (self.peek(1), self.peek(2));
            let exponent = n1.is_some_and(|c| c.is_ascii_digit())
                || (matches!(n1, Some('+') | Some('-')) && n2.is_some_and(|c| c.is_ascii_digit()));
            if exponent {
                repr.push('e');
                self.pos += 1;
                if let Some(s @ ('+' | '-')) = self.peek(0) {
                    repr.push(s);
                    self.pos += 1;
                }
                integer = false;
                digits(self, &mut repr);
            }
        }
        let value = repr.parse::<f64>().unwrap_or(0.0);
        Numeric {
            value,
            integer,
            signed,
        }
    }

    fn consume_numeric(&mut self) -> Token {
        let number = self.consume_number();
        if Self::starts_ident(self.peek(0), self.peek(1), self.peek(2)) {
            let unit = self.consume_ident_sequence();
            return Token::Dimension(number, unit);
        }
        if self.peek(0) == Some('%') {
            self.pos += 1;
            return Token::Percentage(number);
        }
        Token::Number(number)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selector_like_input() {
        let toks = tokenize("a#b.c[d|='e'] > :nth-child(2n+1 of .x)/*c*/");
        assert_eq!(toks[0], Token::Ident("a".into()));
        assert_eq!(
            toks[1],
            Token::Hash {
                value: "b".into(),
                id: true
            }
        );
        assert_eq!(toks[2], Token::Delim('.'));
        assert_eq!(toks[5], Token::Ident("d".into()));
        assert_eq!(toks[6], Token::Delim('|'));
        assert_eq!(toks[8], Token::String("e".into()));
        assert!(toks.contains(&Token::Function("nth-child".into())));
        assert!(
            toks.iter()
                .any(|t| matches!(t, Token::Dimension(n, u) if n.value == 2.0 && u == "n"))
        );
        assert!(
            toks.iter()
                .any(|t| matches!(t, Token::Number(n) if n.signed && n.value == 1.0))
        );
        assert_eq!(toks.last(), Some(&Token::RightParen));
    }

    #[test]
    fn escapes_and_numbers() {
        assert_eq!(tokenize("\\31 23"), vec![Token::Ident("123".into())]);
        assert_eq!(tokenize("-n-1"), vec![Token::Ident("-n-1".into())]);
        assert!(
            matches!(tokenize("1.5e3")[0], Token::Number(n) if n.value == 1500.0 && !n.integer)
        );
        assert_eq!(tokenize("url( a b )"), vec![Token::BadUrl]);
        assert_eq!(tokenize("url(x)"), vec![Token::Url("x".into())]);
        assert_eq!(tokenize("'a\nb'")[0], Token::BadString);
    }
}
