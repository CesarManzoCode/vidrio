//! The `JSON` object (ECMA-262 §25.5): `JSON.parse` with a reviver and
//! the source text access of `json-parse-with-source` (the `context`
//! argument, `JSON.rawJSON` and `JSON.isRawJSON`), and `JSON.stringify`
//! with replacer functions and property lists, gaps, `toJSON`, cycle
//! detection and well-formed output (lone surrogates are escaped).

use crate::agent::Agent;
use crate::builtins::{intrinsic, method, plain_object, set_intrinsic, to_string_tag};
use crate::completion::JsResult;
use crate::function::NativeCall;
use crate::gc::{HeapBigInt, Object, Realm};
use crate::internal;
use crate::json::{ParseRecord, parse_json_with_records};
use crate::object::ObjectKind;
use crate::ops::{self, EnumerableKind, IntegrityLevel};
use crate::realm::Intrinsic;
use crate::string::utf16;
use crate::value::{PropertyKey, Value};

pub(crate) fn install(agent: &mut Agent, realm: Realm) {
    let object_proto = intrinsic(agent, realm, Intrinsic::ObjectPrototype);
    let json = plain_object(agent, object_proto);
    method(agent, realm, json, "isRawJSON", is_raw_json, 1);
    method(agent, realm, json, "parse", parse, 2);
    method(agent, realm, json, "rawJSON", raw_json, 1);
    method(agent, realm, json, "stringify", stringify, 3);
    to_string_tag(agent, json, "JSON");
    set_intrinsic(agent, realm, Intrinsic::Json, json);
}

// ----- parse -----

/// `JSON.parse(text [, reviver])`.
fn parse(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let s = ops::to_string(agent, call.arg(0))?;
    let text = agent.heap.string_units(s).to_vec();
    let record = parse_json_with_records(agent, &text)?;
    let unfiltered = record.value;
    let reviver = call.arg(1);
    if !ops::is_callable(agent, reviver) {
        return Ok(unfiltered);
    }
    let root = ops::object_create(agent);
    let root_name = PropertyKey::String(agent.heap.names.empty);
    ops::create_data_property_or_throw(agent, root, root_name, unfiltered)?;
    let mut ctx = Reviver {
        text: &text,
        reviver,
    };
    ctx.internalize(agent, root, root_name, Some(&record))
}

struct Reviver<'a> {
    text: &'a [u16],
    reviver: Value,
}

impl Reviver<'_> {
    /// InternalizeJSONProperty(holder, name, reviver, valRecord).
    fn internalize(
        &mut self,
        agent: &mut Agent,
        holder: Object,
        name: PropertyKey,
        record: Option<&ParseRecord>,
    ) -> JsResult<Value> {
        let val = ops::get(agent, holder, name)?;
        let context = ops::object_create(agent);
        // The record only describes the value if the reviver did not
        // replace it.
        let record = match record {
            Some(r) if ops::same_value(agent, r.value, val) => Some(r),
            _ => None,
        };
        if let Some(r) = record
            && !val.is_object()
            && let Some((start, end)) = r.source
        {
            let source = Value::String(agent.heap.alloc_string(&self.text[start..end]));
            let key = agent.heap.key_from_str("source");
            ops::create_data_property_or_throw(agent, context, key, source)?;
        }
        if let Value::Object(o) = val {
            if ops::is_array(agent, val)? {
                let len = ops::length_of_array_like(agent, o)? as u64;
                for i in 0..len {
                    let key = ops::index_key(agent, i);
                    let element = record.and_then(|r| r.elements.get(i as usize));
                    let new = self.internalize(agent, o, key, element)?;
                    self.replace(agent, o, key, new)?;
                }
            } else {
                let keys = ops::enumerable_own_properties(agent, o, EnumerableKind::Key)?;
                for k in keys {
                    let key = ops::to_property_key(agent, k)?;
                    let entry = record.and_then(|r| r.entry(key));
                    let new = self.internalize(agent, o, key, entry)?;
                    self.replace(agent, o, key, new)?;
                }
            }
        }
        let name_value = agent.heap.key_to_value(name);
        ops::call(
            agent,
            self.reviver,
            Value::Object(holder),
            &[name_value, val, Value::Object(context)],
        )
    }

    fn replace(
        &mut self,
        agent: &mut Agent,
        o: Object,
        key: PropertyKey,
        new: Value,
    ) -> JsResult<()> {
        if new.is_undefined() {
            internal::delete(agent, o, key)?;
        } else {
            ops::create_data_property(agent, o, key, new)?;
        }
        Ok(())
    }
}

// ----- rawJSON and isRawJSON -----

/// `JSON.rawJSON(text)`.
fn raw_json(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let s = ops::to_string(agent, call.arg(0))?;
    let text = agent.heap.string_units(s).to_vec();
    let is_ws = |c: u16| matches!(c, 0x09 | 0x0A | 0x0D | 0x20);
    match (text.first(), text.last()) {
        (Some(&first), Some(&last)) if !is_ws(first) && !is_ws(last) => {}
        _ => return agent.syntax_error("JSON.rawJSON: empty text or surrounding whitespace"),
    }
    if matches!(text[0], 0x5B | 0x7B) {
        return agent.syntax_error("JSON.rawJSON: arrays and objects are not allowed");
    }
    // Validates the text as a JSON primitive.
    crate::json::parse_json(agent, &text)?;
    let o = agent.heap.create_object(None, ObjectKind::RawJson);
    let key = agent.heap.key_from_str("rawJSON");
    ops::create_data_property_or_throw(agent, o, key, Value::String(s))?;
    ops::set_integrity_level(agent, o, IntegrityLevel::Frozen)?;
    Ok(Value::Object(o))
}

fn is_raw_json_object(agent: &Agent, v: Value) -> bool {
    matches!(v, Value::Object(o) if matches!(agent.heap.object(o).kind, ObjectKind::RawJson))
}

/// `JSON.isRawJSON(O)`.
fn is_raw_json(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    Ok(Value::Boolean(is_raw_json_object(agent, call.arg(0))))
}

// ----- stringify -----

/// The internal slot of an object that serialization looks at.
enum Boxed {
    Number,
    String,
    Boolean(bool),
    BigInt(HeapBigInt),
    RawJson,
    Other,
}

fn boxed_kind(agent: &Agent, o: Object) -> Boxed {
    match &agent.heap.object(o).kind {
        ObjectKind::Number(_) => Boxed::Number,
        ObjectKind::String(_) => Boxed::String,
        ObjectKind::Boolean(b) => Boxed::Boolean(*b),
        ObjectKind::BigInt(b) => Boxed::BigInt(*b),
        ObjectKind::RawJson => Boxed::RawJson,
        _ => Boxed::Other,
    }
}

fn wraps_string_or_number(agent: &Agent, o: Object) -> bool {
    matches!(boxed_kind(agent, o), Boxed::Number | Boxed::String)
}

/// JSON Serialization Record.
struct Serializer {
    replacer: Option<Value>,
    property_list: Option<Vec<PropertyKey>>,
    gap: Vec<u16>,
    indent: Vec<u16>,
    stack: Vec<Object>,
}

/// `JSON.stringify(value [, replacer [, space]])`.
fn stringify(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let (value, replacer, space) = (call.arg(0), call.arg(1), call.arg(2));
    let mut state = Serializer {
        replacer: None,
        property_list: None,
        gap: Vec::new(),
        indent: Vec::new(),
        stack: Vec::new(),
    };
    if let Value::Object(r) = replacer {
        if ops::is_callable(agent, replacer) {
            state.replacer = Some(replacer);
        } else if ops::is_array(agent, replacer)? {
            let mut list: Vec<PropertyKey> = Vec::new();
            let len = ops::length_of_array_like(agent, r)? as u64;
            for k in 0..len {
                let key = ops::index_key(agent, k);
                let v = ops::get(agent, r, key)?;
                let item = match v {
                    Value::String(_) | Value::Number(_) => Some(ops::to_string(agent, v)?),
                    Value::Object(o) if wraps_string_or_number(agent, o) => {
                        Some(ops::to_string(agent, v)?)
                    }
                    _ => None,
                };
                if let Some(item) = item {
                    let key = agent.heap.key_from_string(item);
                    if !list.contains(&key) {
                        list.push(key);
                    }
                }
            }
            state.property_list = Some(list);
        }
    }
    let space = match space {
        Value::Object(o) => match boxed_kind(agent, o) {
            Boxed::Number => Value::Number(ops::to_number(agent, space)?),
            Boxed::String => Value::String(ops::to_string(agent, space)?),
            _ => space,
        },
        _ => space,
    };
    state.gap = match space {
        Value::Number(n) => {
            let n = ops::f64_to_integer_or_infinity(n).min(10.0);
            if n < 1.0 {
                Vec::new()
            } else {
                vec![0x20; n as usize]
            }
        }
        Value::String(s) => {
            let units = agent.heap.string_units(s);
            units[..units.len().min(10)].to_vec()
        }
        _ => Vec::new(),
    };
    let wrapper = ops::object_create(agent);
    let empty = PropertyKey::String(agent.heap.names.empty);
    ops::create_data_property_or_throw(agent, wrapper, empty, value)?;
    match state.property(agent, empty, wrapper)? {
        Some(units) => Ok(Value::String(agent.heap.alloc_string_owned(units))),
        None => Ok(Value::Undefined),
    }
}

impl Serializer {
    /// SerializeJSONProperty(state, key, holder); `None` is undefined.
    fn property(
        &mut self,
        agent: &mut Agent,
        key: PropertyKey,
        holder: Object,
    ) -> JsResult<Option<Vec<u16>>> {
        let mut value = ops::get(agent, holder, key)?;
        if matches!(value, Value::Object(_) | Value::BigInt(_)) {
            let to_json = agent.heap.key_from_str("toJSON");
            let f = ops::get_v(agent, value, to_json)?;
            if ops::is_callable(agent, f) {
                let k = agent.heap.key_to_value(key);
                value = ops::call(agent, f, value, &[k])?;
            }
        }
        if let Some(replacer) = self.replacer {
            let k = agent.heap.key_to_value(key);
            value = ops::call(agent, replacer, Value::Object(holder), &[k, value])?;
        }
        if let Value::Object(o) = value {
            value = match boxed_kind(agent, o) {
                Boxed::Number => Value::Number(ops::to_number(agent, value)?),
                Boxed::String => Value::String(ops::to_string(agent, value)?),
                Boxed::Boolean(b) => Value::Boolean(b),
                Boxed::BigInt(b) => Value::BigInt(b),
                Boxed::RawJson => {
                    let raw = agent.heap.key_from_str("rawJSON");
                    let v = ops::get(agent, o, raw)?;
                    let s = ops::to_string(agent, v)?;
                    return Ok(Some(agent.heap.string_units(s).to_vec()));
                }
                _ => value,
            };
        }
        Ok(match value {
            Value::Null => Some(utf16("null")),
            Value::Boolean(true) => Some(utf16("true")),
            Value::Boolean(false) => Some(utf16("false")),
            Value::String(s) => {
                let mut out = Vec::new();
                quote(&mut out, agent.heap.string_units(s));
                Some(out)
            }
            Value::Number(n) if n.is_finite() => {
                let s = ops::number_to_js_string(agent, n);
                Some(agent.heap.string_units(s).to_vec())
            }
            Value::Number(_) => Some(utf16("null")),
            Value::BigInt(_) => {
                return agent.type_error("BigInt value can't be serialized in JSON");
            }
            Value::Object(o) if !ops::is_callable(agent, value) => {
                if ops::is_array(agent, value)? {
                    Some(self.array(agent, o)?)
                } else {
                    Some(self.object(agent, o)?)
                }
            }
            _ => None,
        })
    }

    fn enter(&mut self, agent: &mut Agent, o: Object) -> JsResult<Vec<u16>> {
        if self.stack.contains(&o) {
            return agent.type_error("cyclic object value");
        }
        self.stack.push(o);
        let stepback = self.indent.clone();
        self.indent.extend_from_slice(&self.gap);
        Ok(stepback)
    }

    fn leave(&mut self, stepback: Vec<u16>) {
        self.stack.pop();
        self.indent = stepback;
    }

    /// The members joined with the gap's line breaks and indentation.
    fn join(&self, open: u16, close: u16, partial: &[Vec<u16>], stepback: &[u16]) -> Vec<u16> {
        let mut out = vec![open];
        if partial.is_empty() {
            out.push(close);
            return out;
        }
        for (i, member) in partial.iter().enumerate() {
            if i > 0 {
                out.push(u16::from(b','));
            }
            if !self.gap.is_empty() {
                out.push(0x0A);
                out.extend_from_slice(&self.indent);
            }
            out.extend_from_slice(member);
        }
        if !self.gap.is_empty() {
            out.push(0x0A);
            out.extend_from_slice(stepback);
        }
        out.push(close);
        out
    }

    /// SerializeJSONObject(state, value).
    fn object(&mut self, agent: &mut Agent, o: Object) -> JsResult<Vec<u16>> {
        let stepback = self.enter(agent, o)?;
        let result = self.object_members(agent, o, &stepback);
        self.leave(stepback);
        result
    }

    fn object_members(
        &mut self,
        agent: &mut Agent,
        o: Object,
        stepback: &[u16],
    ) -> JsResult<Vec<u16>> {
        let keys = match &self.property_list {
            Some(list) => list.clone(),
            None => {
                let values = ops::enumerable_own_properties(agent, o, EnumerableKind::Key)?;
                let mut keys = Vec::with_capacity(values.len());
                for v in values {
                    keys.push(ops::to_property_key(agent, v)?);
                }
                keys
            }
        };
        let mut partial = Vec::new();
        for key in keys {
            let Some(str_p) = self.property(agent, key, o)? else {
                continue;
            };
            let mut member = Vec::new();
            let name = agent.heap.key_units(key).unwrap_or_default();
            quote(&mut member, &name);
            member.push(u16::from(b':'));
            if !self.gap.is_empty() {
                member.push(0x20);
            }
            member.extend_from_slice(&str_p);
            partial.push(member);
        }
        Ok(self.join(u16::from(b'{'), u16::from(b'}'), &partial, stepback))
    }

    /// SerializeJSONArray(state, value).
    fn array(&mut self, agent: &mut Agent, o: Object) -> JsResult<Vec<u16>> {
        let stepback = self.enter(agent, o)?;
        let result = self.array_elements(agent, o, &stepback);
        self.leave(stepback);
        result
    }

    fn array_elements(
        &mut self,
        agent: &mut Agent,
        o: Object,
        stepback: &[u16],
    ) -> JsResult<Vec<u16>> {
        let len = ops::length_of_array_like(agent, o)? as u64;
        let mut partial = Vec::new();
        for i in 0..len {
            let key = ops::index_key(agent, i);
            let s = self.property(agent, key, o)?;
            partial.push(s.unwrap_or_else(|| utf16("null")));
        }
        Ok(self.join(u16::from(b'['), u16::from(b']'), &partial, stepback))
    }
}

/// QuoteJSONString(value): escapes the short forms, control characters
/// and lone surrogates (well-formed `JSON.stringify`).
pub(crate) fn quote(out: &mut Vec<u16>, units: &[u16]) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    out.push(u16::from(b'"'));
    let mut i = 0;
    while i < units.len() {
        let c = units[i];
        let short = match c {
            0x08 => Some(b'b'),
            0x09 => Some(b't'),
            0x0A => Some(b'n'),
            0x0C => Some(b'f'),
            0x0D => Some(b'r'),
            0x22 => Some(b'"'),
            0x5C => Some(b'\\'),
            _ => None,
        };
        if let Some(e) = short {
            out.push(u16::from(b'\\'));
            out.push(u16::from(e));
            i += 1;
            continue;
        }
        let is_high = (0xD800..0xDC00).contains(&c);
        let is_low = (0xDC00..0xE000).contains(&c);
        if is_high
            && units
                .get(i + 1)
                .is_some_and(|n| (0xDC00..0xE000).contains(n))
        {
            out.push(c);
            out.push(units[i + 1]);
            i += 2;
            continue;
        }
        if c < 0x20 || is_high || is_low {
            out.extend_from_slice(&utf16("\\u"));
            for shift in [12, 8, 4, 0] {
                out.push(u16::from(HEX[usize::from((c >> shift) & 0xF)]));
            }
        } else {
            out.push(c);
        }
        i += 1;
    }
    out.push(u16::from(b'"'));
}

#[cfg(test)]
mod tests {
    use super::quote;
    use crate::string::utf16;

    fn q(units: &[u16]) -> String {
        let mut out = Vec::new();
        quote(&mut out, units);
        String::from_utf16_lossy(&out)
    }

    #[test]
    fn quote_escapes_controls_and_lone_surrogates() {
        assert_eq!(q(&utf16("a\"b\\\n\u{1}")), "\"a\\\"b\\\\\\n\\u0001\"");
        assert_eq!(q(&[0xD800]), "\"\\ud800\"");
        assert_eq!(q(&[0xDC00, 0x61]), "\"\\udc00a\"");
        assert_eq!(q(&[0xD83D, 0xDE00]), "\"\u{1F600}\"");
    }
}
