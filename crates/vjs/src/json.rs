//! ParseJSON (ECMA-262 §25.5.1.1): JSON text (RFC 8259) to a value of the
//! current realm, plus the JSON Parse Records of the source text access
//! (`json-parse-with-source`) that `JSON.parse` hands to a reviver. JSON
//! modules use [`parse_json`]; `JSON.parse` uses [`parse_json_with_records`].

use crate::agent::Agent;
use crate::completion::JsResult;
use crate::object::ObjectKind;
use crate::ops;
use crate::realm::Intrinsic;
use crate::value::{PropertyKey, Value};

/// JSON Parse Record: the value a piece of the text produced, the range
/// of its source text if it is a primitive, and the records of its
/// elements (arrays) or entries (objects, in source order; with duplicate
/// keys the last one is the property's).
#[derive(Debug)]
pub struct ParseRecord {
    pub value: Value,
    pub source: Option<(usize, usize)>,
    pub elements: Vec<ParseRecord>,
    pub entries: Vec<(PropertyKey, ParseRecord)>,
}

impl ParseRecord {
    fn new(value: Value) -> ParseRecord {
        ParseRecord {
            value,
            source: None,
            elements: Vec::new(),
            entries: Vec::new(),
        }
    }

    /// The record of the entry `key` (the last one with that key).
    pub fn entry(&self, key: PropertyKey) -> Option<&ParseRecord> {
        self.entries
            .iter()
            .rev()
            .find(|(k, _)| *k == key)
            .map(|(_, r)| r)
    }
}

struct JsonParser<'a> {
    text: &'a [u16],
    pos: usize,
    /// Whether to keep the records of elements and entries.
    records: bool,
}

type Parsed<T> = Result<T, String>;

impl JsonParser<'_> {
    fn peek(&self) -> Option<u16> {
        self.text.get(self.pos).copied()
    }

    fn skip_ws(&mut self) {
        while let Some(c) = self.peek() {
            if c == 0x20 || c == 0x09 || c == 0x0A || c == 0x0D {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    fn error<T>(&self, what: &str) -> Parsed<T> {
        Err(format!("JSON: {what} at position {}", self.pos))
    }

    fn expect(&mut self, c: u8) -> Parsed<()> {
        if self.peek() == Some(u16::from(c)) {
            self.pos += 1;
            Ok(())
        } else {
            self.error(&format!("expected '{}'", c as char))
        }
    }

    fn literal(&mut self, word: &str) -> Parsed<()> {
        for b in word.bytes() {
            self.expect(b)?;
        }
        Ok(())
    }

    fn value(&mut self, agent: &mut Agent, depth: usize) -> Parsed<ParseRecord> {
        if depth > 10_000 {
            return self.error("nesting too deep");
        }
        self.skip_ws();
        let start = self.pos;
        let v = match self.peek() {
            Some(0x7B) => return self.object(agent, depth),
            Some(0x5B) => return self.array(agent, depth),
            Some(0x22) => {
                let units = self.string()?;
                Value::String(agent.heap.alloc_string(&units))
            }
            Some(0x74) => self.literal("true").map(|_| Value::Boolean(true))?,
            Some(0x66) => self.literal("false").map(|_| Value::Boolean(false))?,
            Some(0x6E) => self.literal("null").map(|_| Value::Null)?,
            Some(c) if c == u16::from(b'-') || (0x30..=0x39).contains(&c) => self.number()?,
            Some(_) => return self.error("unexpected character"),
            None => return self.error("unexpected end of input"),
        };
        let mut r = ParseRecord::new(v);
        r.source = Some((start, self.pos));
        Ok(r)
    }

    fn digits(&mut self) -> usize {
        let start = self.pos;
        while matches!(self.peek(), Some(c) if (0x30..=0x39).contains(&c)) {
            self.pos += 1;
        }
        self.pos - start
    }

    fn number(&mut self) -> Parsed<Value> {
        let start = self.pos;
        if self.peek() == Some(u16::from(b'-')) {
            self.pos += 1;
        }
        if self.peek() == Some(u16::from(b'0')) {
            self.pos += 1;
        } else if self.digits() == 0 {
            return self.error("invalid number");
        }
        if self.peek() == Some(u16::from(b'.')) {
            self.pos += 1;
            if self.digits() == 0 {
                return self.error("invalid number");
            }
        }
        if matches!(self.peek(), Some(0x65 | 0x45)) {
            self.pos += 1;
            if matches!(self.peek(), Some(0x2B | 0x2D)) {
                self.pos += 1;
            }
            if self.digits() == 0 {
                return self.error("invalid number");
            }
        }
        let text: String = self.text[start..self.pos]
            .iter()
            .map(|&u| u as u8 as char)
            .collect();
        match crate::numconv::parse_decimal(text.as_bytes()) {
            Some(n) => Ok(Value::Number(n)),
            None => self.error("invalid number"),
        }
    }

    fn hex4(&mut self) -> Parsed<u16> {
        let mut v: u16 = 0;
        for _ in 0..4 {
            let Some(c) = self.peek() else {
                return self.error("unexpected end of input");
            };
            let d = match c {
                0x30..=0x39 => c - 0x30,
                0x41..=0x46 => c - 0x41 + 10,
                0x61..=0x66 => c - 0x61 + 10,
                _ => return self.error("invalid unicode escape"),
            };
            v = v * 16 + d;
            self.pos += 1;
        }
        Ok(v)
    }

    fn string(&mut self) -> Parsed<Vec<u16>> {
        self.expect(b'"')?;
        let mut out = Vec::new();
        loop {
            let Some(c) = self.peek() else {
                return self.error("unterminated string");
            };
            self.pos += 1;
            match c {
                0x22 => return Ok(out),
                0x5C => {
                    let Some(e) = self.peek() else {
                        return self.error("unterminated string");
                    };
                    self.pos += 1;
                    let unit = match e {
                        0x22 => 0x22,
                        0x5C => 0x5C,
                        0x2F => 0x2F,
                        0x62 => 0x08,
                        0x66 => 0x0C,
                        0x6E => 0x0A,
                        0x72 => 0x0D,
                        0x74 => 0x09,
                        0x75 => self.hex4()?,
                        _ => return self.error("invalid escape"),
                    };
                    out.push(unit);
                }
                c if c < 0x20 => return self.error("control character in string"),
                c => out.push(c),
            }
        }
    }

    fn array(&mut self, agent: &mut Agent, depth: usize) -> Parsed<ParseRecord> {
        self.expect(b'[')?;
        let mut items = Vec::new();
        let mut elements = Vec::new();
        self.skip_ws();
        if self.peek() == Some(u16::from(b']')) {
            self.pos += 1;
        } else {
            loop {
                let r = self.value(agent, depth + 1)?;
                items.push(r.value);
                if self.records {
                    elements.push(r);
                }
                self.skip_ws();
                match self.peek() {
                    Some(0x2C) => self.pos += 1,
                    Some(0x5D) => {
                        self.pos += 1;
                        break;
                    }
                    _ => return self.error("expected ',' or ']'"),
                }
            }
        }
        let a = ops::create_array_from_list(agent, &items);
        let mut r = ParseRecord::new(Value::Object(a));
        r.elements = elements;
        Ok(r)
    }

    fn object(&mut self, agent: &mut Agent, depth: usize) -> Parsed<ParseRecord> {
        self.expect(b'{')?;
        let proto = agent.intrinsic(Intrinsic::ObjectPrototype);
        let o = agent.heap.create_object(Some(proto), ObjectKind::Ordinary);
        let mut r = ParseRecord::new(Value::Object(o));
        self.skip_ws();
        if self.peek() == Some(u16::from(b'}')) {
            self.pos += 1;
            return Ok(r);
        }
        loop {
            self.skip_ws();
            let key = self.string()?;
            self.skip_ws();
            self.expect(b':')?;
            let v = self.value(agent, depth + 1)?;
            let key = agent.heap.key_from_units(&key);
            // A fresh extensible ordinary object accepts every key.
            let _ = ops::create_data_property(agent, o, key, v.value);
            if self.records {
                r.entries.push((key, v));
            }
            self.skip_ws();
            match self.peek() {
                Some(0x2C) => self.pos += 1,
                Some(0x7D) => {
                    self.pos += 1;
                    break;
                }
                _ => return self.error("expected ',' or '}'"),
            }
        }
        Ok(r)
    }
}

fn parse(agent: &mut Agent, text: &[u16], records: bool) -> JsResult<ParseRecord> {
    let mut p = JsonParser {
        text,
        pos: 0,
        records,
    };
    let result = p.value(agent, 0).and_then(|r| {
        p.skip_ws();
        if p.pos == text.len() {
            Ok(r)
        } else {
            p.error("unexpected text after the value")
        }
    });
    match result {
        Ok(r) => Ok(r),
        Err(msg) => agent.syntax_error(&msg),
    }
}

/// Parses JSON text; a malformed text throws a SyntaxError.
pub fn parse_json(agent: &mut Agent, text: &[u16]) -> JsResult<Value> {
    parse(agent, text, false).map(|r| r.value)
}

/// Parses JSON text and keeps the JSON Parse Record of every value (the
/// snapshot CreateJSONParseRecord takes: the values are those of the
/// fresh objects, which no code has touched yet).
pub fn parse_json_with_records(agent: &mut Agent, text: &[u16]) -> JsResult<ParseRecord> {
    parse(agent, text, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(agent: &mut Agent, s: &str) -> JsResult<Value> {
        let units: Vec<u16> = s.encode_utf16().collect();
        parse_json(agent, &units)
    }

    #[test]
    fn scalars_and_errors() {
        let mut agent = Agent::new();
        agent.initialize_host_defined_realm();
        assert!(matches!(parse(&mut agent, " 1.5e2 "), Ok(Value::Number(n)) if n == 150.0));
        assert!(matches!(
            parse(&mut agent, "true"),
            Ok(Value::Boolean(true))
        ));
        assert!(matches!(parse(&mut agent, "null"), Ok(Value::Null)));
        assert!(matches!(
            parse(&mut agent, "[1, {\"a\": \"\\u0041\"}]"),
            Ok(Value::Object(_))
        ));
        for bad in ["", "01", "[1,]", "{\"a\" 1}", "'x'", "1 2", "\"\t\""] {
            assert!(parse(&mut agent, bad).is_err(), "{bad:?} should fail");
        }
    }

    #[test]
    fn records_keep_source_ranges() {
        let mut agent = Agent::new();
        agent.initialize_host_defined_realm();
        let units: Vec<u16> = "{\"a\": [1.50, \"x\"], \"a\": -0}".encode_utf16().collect();
        let r = parse_json_with_records(&mut agent, &units).expect("valid JSON");
        assert_eq!(r.source, None);
        assert_eq!(r.entries.len(), 2);
        let key = agent.heap.key_from_str("a");
        let last = r.entry(key).expect("entry a");
        let (s, e) = last.source.expect("primitive source");
        assert_eq!(String::from_utf16_lossy(&units[s..e]), "-0");
        let first = &r.entries[0].1;
        assert_eq!(first.elements.len(), 2);
        let (s, e) = first.elements[0].source.expect("number source");
        assert_eq!(String::from_utf16_lossy(&units[s..e]), "1.50");
    }
}
