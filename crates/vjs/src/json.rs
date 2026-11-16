//! ParseJSON (ECMA-262 §25.5.1.1 without the reviver): JSON text (RFC 8259)
//! to a value of the current realm. JSON modules use it; `JSON.parse`
//! can build on it.

use crate::agent::Agent;
use crate::completion::JsResult;
use crate::object::ObjectKind;
use crate::ops;
use crate::realm::Intrinsic;
use crate::value::Value;

struct JsonParser<'a> {
    text: &'a [u16],
    pos: usize,
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

    fn value(&mut self, agent: &mut Agent, depth: usize) -> Parsed<Value> {
        if depth > 10_000 {
            return self.error("nesting too deep");
        }
        self.skip_ws();
        match self.peek() {
            Some(0x7B) => self.object(agent, depth),
            Some(0x5B) => self.array(agent, depth),
            Some(0x22) => {
                let units = self.string()?;
                Ok(Value::String(agent.heap.alloc_string(&units)))
            }
            Some(0x74) => self.literal("true").map(|_| Value::Boolean(true)),
            Some(0x66) => self.literal("false").map(|_| Value::Boolean(false)),
            Some(0x6E) => self.literal("null").map(|_| Value::Null),
            Some(c) if c == u16::from(b'-') || (0x30..=0x39).contains(&c) => self.number(),
            Some(_) => self.error("unexpected character"),
            None => self.error("unexpected end of input"),
        }
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

    fn array(&mut self, agent: &mut Agent, depth: usize) -> Parsed<Value> {
        self.expect(b'[')?;
        let mut items = Vec::new();
        self.skip_ws();
        if self.peek() == Some(u16::from(b']')) {
            self.pos += 1;
        } else {
            loop {
                items.push(self.value(agent, depth + 1)?);
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
        Ok(Value::Object(ops::create_array_from_list(agent, &items)))
    }

    fn object(&mut self, agent: &mut Agent, depth: usize) -> Parsed<Value> {
        self.expect(b'{')?;
        let proto = agent.intrinsic(Intrinsic::ObjectPrototype);
        let o = agent.heap.create_object(Some(proto), ObjectKind::Ordinary);
        self.skip_ws();
        if self.peek() == Some(u16::from(b'}')) {
            self.pos += 1;
            return Ok(Value::Object(o));
        }
        loop {
            self.skip_ws();
            let key = self.string()?;
            self.skip_ws();
            self.expect(b':')?;
            let v = self.value(agent, depth + 1)?;
            let key = agent.heap.key_from_units(&key);
            // A fresh extensible ordinary object accepts every key.
            let _ = ops::create_data_property(agent, o, key, v);
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
        Ok(Value::Object(o))
    }
}

/// Parses JSON text; a malformed text throws a SyntaxError.
pub fn parse_json(agent: &mut Agent, text: &[u16]) -> JsResult<Value> {
    let mut p = JsonParser { text, pos: 0 };
    let result = p.value(agent, 0).and_then(|v| {
        p.skip_ws();
        if p.pos == text.len() {
            Ok(v)
        } else {
            p.error("unexpected text after the value")
        }
    });
    match result {
        Ok(v) => Ok(v),
        Err(msg) => agent.syntax_error(&msg),
    }
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
}
