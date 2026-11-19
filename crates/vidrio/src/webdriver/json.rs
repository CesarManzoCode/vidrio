//! The JSON of the WebDriver wire protocol (RFC 8259): a value type, a
//! parser and a serializer. Object members keep their order.

use std::fmt::Write as _;

/// A JSON value.
#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<Json>),
    Object(Vec<(String, Json)>),
}

/// The deepest nesting the parser accepts.
const MAX_DEPTH: usize = 512;

impl Json {
    /// An object from `(name, value)` pairs.
    pub fn object<const N: usize>(members: [(&str, Json); N]) -> Json {
        Json::Object(
            members
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
        )
    }

    pub fn str(s: &str) -> Json {
        Json::String(s.to_string())
    }

    /// Member `name` of an object.
    pub fn get(&self, name: &str) -> Option<&Json> {
        match self {
            Json::Object(m) => m.iter().find(|(k, _)| k == name).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Json::Number(n) => Some(*n),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[Json]> {
        match self {
            Json::Array(a) => Some(a),
            _ => None,
        }
    }

    pub fn as_object(&self) -> Option<&[(String, Json)]> {
        match self {
            Json::Object(m) => Some(m),
            _ => None,
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Json::Null)
    }

    /// Parses a JSON text (surrounding whitespace allowed).
    pub fn parse(text: &str) -> Result<Json, String> {
        let mut p = Parser {
            s: text.as_bytes(),
            i: 0,
            depth: 0,
        };
        p.ws();
        let v = p.value()?;
        p.ws();
        if p.i != p.s.len() {
            return Err(format!("unexpected data at offset {}", p.i));
        }
        Ok(v)
    }

    /// Serializes the value without whitespace.
    pub fn serialize(&self) -> String {
        let mut out = String::new();
        self.write(&mut out);
        out
    }

    fn write(&self, out: &mut String) {
        match self {
            Json::Null => out.push_str("null"),
            Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Json::Number(n) => write_number(*n, out),
            Json::String(s) => write_string(s, out),
            Json::Array(a) => {
                out.push('[');
                for (i, v) in a.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    v.write(out);
                }
                out.push(']');
            }
            Json::Object(m) => {
                out.push('{');
                for (i, (k, v)) in m.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write_string(k, out);
                    out.push(':');
                    v.write(out);
                }
                out.push('}');
            }
        }
    }
}

/// Numbers: integers without a fraction; NaN and infinities (which JSON
/// cannot express) as `null`.
fn write_number(n: f64, out: &mut String) {
    if !n.is_finite() {
        out.push_str("null");
    } else if n == n.trunc() && n.abs() < 9_007_199_254_740_992.0 {
        if n == 0.0 {
            out.push('0');
        } else {
            let _ = write!(out, "{}", n as i64);
        }
    } else {
        let _ = write!(out, "{n}");
    }
}

fn write_string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
    depth: usize,
}

impl Parser<'_> {
    fn ws(&mut self) {
        while self.i < self.s.len() && matches!(self.s[self.i], b' ' | b'\t' | b'\n' | b'\r') {
            self.i += 1;
        }
    }

    fn err<T>(&self, what: &str) -> Result<T, String> {
        Err(format!("{what} at offset {}", self.i))
    }

    fn literal(&mut self, word: &str, v: Json) -> Result<Json, String> {
        if self.s[self.i..].starts_with(word.as_bytes()) {
            self.i += word.len();
            Ok(v)
        } else {
            self.err("invalid literal")
        }
    }

    fn value(&mut self) -> Result<Json, String> {
        let Some(&c) = self.s.get(self.i) else {
            return self.err("unexpected end");
        };
        match c {
            b'n' => self.literal("null", Json::Null),
            b't' => self.literal("true", Json::Bool(true)),
            b'f' => self.literal("false", Json::Bool(false)),
            b'"' => Ok(Json::String(self.string()?)),
            b'[' | b'{' => {
                self.depth += 1;
                if self.depth > MAX_DEPTH {
                    return self.err("nesting too deep");
                }
                let v = if c == b'[' {
                    self.array()
                } else {
                    self.object()
                };
                self.depth -= 1;
                v
            }
            b'-' | b'0'..=b'9' => self.number(),
            _ => self.err("unexpected character"),
        }
    }

    fn array(&mut self) -> Result<Json, String> {
        self.i += 1;
        let mut out = Vec::new();
        self.ws();
        if self.s.get(self.i) == Some(&b']') {
            self.i += 1;
            return Ok(Json::Array(out));
        }
        loop {
            self.ws();
            out.push(self.value()?);
            self.ws();
            match self.s.get(self.i) {
                Some(b',') => self.i += 1,
                Some(b']') => {
                    self.i += 1;
                    return Ok(Json::Array(out));
                }
                _ => return self.err("expected ',' or ']'"),
            }
        }
    }

    fn object(&mut self) -> Result<Json, String> {
        self.i += 1;
        let mut out: Vec<(String, Json)> = Vec::new();
        self.ws();
        if self.s.get(self.i) == Some(&b'}') {
            self.i += 1;
            return Ok(Json::Object(out));
        }
        loop {
            self.ws();
            if self.s.get(self.i) != Some(&b'"') {
                return self.err("expected a member name");
            }
            let k = self.string()?;
            self.ws();
            if self.s.get(self.i) != Some(&b':') {
                return self.err("expected ':'");
            }
            self.i += 1;
            self.ws();
            let v = self.value()?;
            // A repeated name keeps the last value, as JSON.parse does.
            out.retain(|(x, _)| *x != k);
            out.push((k, v));
            self.ws();
            match self.s.get(self.i) {
                Some(b',') => self.i += 1,
                Some(b'}') => {
                    self.i += 1;
                    return Ok(Json::Object(out));
                }
                _ => return self.err("expected ',' or '}'"),
            }
        }
    }

    fn number(&mut self) -> Result<Json, String> {
        let start = self.i;
        if self.s[self.i] == b'-' {
            self.i += 1;
        }
        let digits = |p: &mut Self| {
            let s = p.i;
            while p.i < p.s.len() && p.s[p.i].is_ascii_digit() {
                p.i += 1;
            }
            p.i - s
        };
        let int_start = self.i;
        if digits(self) == 0 {
            return self.err("expected a digit");
        }
        if self.s[int_start] == b'0' && self.i - int_start > 1 {
            return self.err("leading zero");
        }
        if self.s.get(self.i) == Some(&b'.') {
            self.i += 1;
            if digits(self) == 0 {
                return self.err("expected a digit");
            }
        }
        if matches!(self.s.get(self.i), Some(b'e' | b'E')) {
            self.i += 1;
            if matches!(self.s.get(self.i), Some(b'+' | b'-')) {
                self.i += 1;
            }
            if digits(self) == 0 {
                return self.err("expected a digit");
            }
        }
        let text = std::str::from_utf8(&self.s[start..self.i]).expect("ASCII");
        text.parse::<f64>()
            .map(Json::Number)
            .or_else(|_| self.err("invalid number"))
    }

    fn hex4(&mut self) -> Result<u32, String> {
        let Some(h) = self.s.get(self.i..self.i + 4) else {
            return self.err("short \\u escape");
        };
        let h = std::str::from_utf8(h).map_err(|_| "invalid \\u escape".to_string())?;
        let v = u32::from_str_radix(h, 16).or_else(|_| self.err("invalid \\u escape"))?;
        self.i += 4;
        Ok(v)
    }

    fn string(&mut self) -> Result<String, String> {
        self.i += 1;
        let mut out = String::new();
        loop {
            let Some(&c) = self.s.get(self.i) else {
                return self.err("unterminated string");
            };
            match c {
                b'"' => {
                    self.i += 1;
                    return Ok(out);
                }
                b'\\' => {
                    self.i += 1;
                    let Some(&e) = self.s.get(self.i) else {
                        return self.err("unterminated string");
                    };
                    self.i += 1;
                    match e {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let hi = self.hex4()?;
                            let code = if (0xD800..0xDC00).contains(&hi)
                                && self.s[self.i..].starts_with(b"\\u")
                            {
                                let save = self.i;
                                self.i += 2;
                                let lo = self.hex4()?;
                                if (0xDC00..0xE000).contains(&lo) {
                                    0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)
                                } else {
                                    self.i = save;
                                    0xFFFD
                                }
                            } else {
                                hi
                            };
                            // A lone surrogate cannot be a Rust char.
                            out.push(char::from_u32(code).unwrap_or('\u{FFFD}'));
                        }
                        _ => return self.err("invalid escape"),
                    }
                }
                0x00..=0x1F => return self.err("control character in string"),
                _ => {
                    // Copy one UTF-8 sequence (the input is a &str).
                    let len = match c {
                        0x00..=0x7F => 1,
                        0xC0..=0xDF => 2,
                        0xE0..=0xEF => 3,
                        _ => 4,
                    };
                    let end = (self.i + len).min(self.s.len());
                    out.push_str(std::str::from_utf8(&self.s[self.i..end]).unwrap_or("\u{FFFD}"));
                    self.i = end;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_serializes_round_trip() {
        let text = r#"{"a":[1,-2.5,true,false,null],"b":{"c":"x\"y\\z\n"},"e":1e3}"#;
        let v = Json::parse(text).unwrap();
        assert_eq!(v.get("e"), Some(&Json::Number(1000.0)));
        assert_eq!(
            v.serialize(),
            r#"{"a":[1,-2.5,true,false,null],"b":{"c":"x\"y\\z\n"},"e":1000}"#
        );
    }

    #[test]
    fn unicode_escapes_and_surrogate_pairs() {
        let v = Json::parse(r#""é😀\u0001""#).unwrap();
        assert_eq!(v, Json::str("é😀\u{1}"));
        assert_eq!(v.serialize(), "\"é😀\\u0001\"");
        assert_eq!(Json::parse(r#""\ud800""#).unwrap(), Json::str("\u{FFFD}"));
    }

    #[test]
    fn rejects_malformed_text() {
        for bad in [
            "",
            "{",
            "[1,]",
            "01",
            "1.",
            "\"a",
            "{\"a\" 1}",
            "tru",
            "[1] x",
            "\"\t\"",
        ] {
            assert!(Json::parse(bad).is_err(), "{bad:?} should not parse");
        }
    }

    #[test]
    fn numbers_that_json_cannot_express_become_null() {
        assert_eq!(Json::Number(f64::NAN).serialize(), "null");
        assert_eq!(Json::Number(-0.0).serialize(), "0");
        assert_eq!(Json::Number(0.1).serialize(), "0.1");
    }
}
