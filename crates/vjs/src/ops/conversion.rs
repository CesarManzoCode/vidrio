//! Type conversion (ECMA-262 §7.1).

use crate::agent::Agent;
use crate::bigint::BigInt;
use crate::completion::JsResult;
use crate::gc::{HeapBigInt, JsString, Object};
use crate::internal::string_create;
use crate::number::{
    f64_to_int8, f64_to_int16, f64_to_int32, f64_to_uint8, f64_to_uint8_clamp, f64_to_uint16,
    f64_to_uint32, non_decimal_digits, number_to_string, string_to_number, trim_str_whitespace,
};
use crate::object::ObjectKind;
use crate::ops::{call, get, get_method, is_callable};
use crate::realm::Intrinsic;
use crate::string::utf16;
use crate::value::{MAX_ARRAY_INDEX, PropertyKey, Value};

/// The hint of ToPrimitive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreferredType {
    Default,
    String,
    Number,
}

/// ToPrimitive(input, preferredType).
pub fn to_primitive(agent: &mut Agent, input: Value, preferred: PreferredType) -> JsResult<Value> {
    let Value::Object(o) = input else {
        return Ok(input);
    };
    let key = PropertyKey::Symbol(agent.symbols.to_primitive);
    if let Some(exotic) = get_method(agent, input, key)? {
        let hint = match preferred {
            PreferredType::Default => "default",
            PreferredType::String => "string",
            PreferredType::Number => "number",
        };
        let hint = Value::String(agent.heap.alloc_str(hint));
        let result = call(agent, exotic, input, &[hint])?;
        if result.is_object() {
            return agent.type_error("Symbol.toPrimitive returned an object");
        }
        return Ok(result);
    }
    let hint = match preferred {
        PreferredType::Default => PreferredType::Number,
        other => other,
    };
    ordinary_to_primitive(agent, o, hint)
}

/// OrdinaryToPrimitive(O, hint).
pub fn ordinary_to_primitive(agent: &mut Agent, o: Object, hint: PreferredType) -> JsResult<Value> {
    let (to_string, value_of) = (agent.heap.names.to_string, agent.heap.names.value_of);
    let names = if hint == PreferredType::String {
        [to_string, value_of]
    } else {
        [value_of, to_string]
    };
    for name in names {
        let method = get(agent, o, PropertyKey::String(name))?;
        if is_callable(agent, method) {
            let result = call(agent, method, Value::Object(o), &[])?;
            if !result.is_object() {
                return Ok(result);
            }
        }
    }
    agent.type_error("cannot convert object to primitive value")
}

fn is_htmldda(agent: &Agent, o: Object) -> bool {
    match &agent.heap.object(o).kind {
        ObjectKind::Host(h) => h.is_htmldda(),
        _ => false,
    }
}

/// ToBoolean(argument).
pub fn to_boolean(agent: &Agent, v: Value) -> bool {
    match v {
        Value::Undefined | Value::Null => false,
        Value::Boolean(b) => b,
        Value::Number(n) => !(n == 0.0 || n.is_nan()),
        Value::String(s) => agent.heap.string_len(s) != 0,
        Value::Symbol(_) => true,
        Value::BigInt(b) => !agent.heap.bigint(b).is_zero(),
        Value::Object(o) => !is_htmldda(agent, o),
    }
}

/// Whether `v` is an object with `[[IsHTMLDDA]]`.
pub fn is_htmldda_value(agent: &Agent, v: Value) -> bool {
    matches!(v, Value::Object(o) if is_htmldda(agent, o))
}

/// A Number or a BigInt.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Numeric {
    Number(f64),
    BigInt(HeapBigInt),
}

/// ToNumeric(value).
pub fn to_numeric(agent: &mut Agent, v: Value) -> JsResult<Numeric> {
    let prim = to_primitive(agent, v, PreferredType::Number)?;
    if let Value::BigInt(b) = prim {
        return Ok(Numeric::BigInt(b));
    }
    Ok(Numeric::Number(to_number(agent, prim)?))
}

/// ToNumber(argument).
pub fn to_number(agent: &mut Agent, v: Value) -> JsResult<f64> {
    match v {
        Value::Undefined => Ok(f64::NAN),
        Value::Null => Ok(0.0),
        Value::Boolean(b) => Ok(if b { 1.0 } else { 0.0 }),
        Value::Number(n) => Ok(n),
        Value::String(s) => Ok(string_to_number(agent.heap.string_units(s))),
        Value::Symbol(_) => agent.type_error("cannot convert a Symbol value to a number"),
        Value::BigInt(_) => agent.type_error("cannot convert a BigInt value to a number"),
        Value::Object(_) => {
            let prim = to_primitive(agent, v, PreferredType::Number)?;
            to_number(agent, prim)
        }
    }
}

/// The ToIntegerOrInfinity of a Number.
pub fn f64_to_integer_or_infinity(n: f64) -> f64 {
    if n.is_nan() || n == 0.0 {
        return 0.0;
    }
    if n.is_infinite() {
        return n;
    }
    let t = n.trunc();
    if t == 0.0 { 0.0 } else { t }
}

/// ToIntegerOrInfinity(argument).
pub fn to_integer_or_infinity(agent: &mut Agent, v: Value) -> JsResult<f64> {
    Ok(f64_to_integer_or_infinity(to_number(agent, v)?))
}

pub fn to_int32(agent: &mut Agent, v: Value) -> JsResult<i32> {
    Ok(f64_to_int32(to_number(agent, v)?))
}

pub fn to_uint32(agent: &mut Agent, v: Value) -> JsResult<u32> {
    Ok(f64_to_uint32(to_number(agent, v)?))
}

pub fn to_int16(agent: &mut Agent, v: Value) -> JsResult<i16> {
    Ok(f64_to_int16(to_number(agent, v)?))
}

pub fn to_uint16(agent: &mut Agent, v: Value) -> JsResult<u16> {
    Ok(f64_to_uint16(to_number(agent, v)?))
}

pub fn to_int8(agent: &mut Agent, v: Value) -> JsResult<i8> {
    Ok(f64_to_int8(to_number(agent, v)?))
}

pub fn to_uint8(agent: &mut Agent, v: Value) -> JsResult<u8> {
    Ok(f64_to_uint8(to_number(agent, v)?))
}

pub fn to_uint8_clamp(agent: &mut Agent, v: Value) -> JsResult<u8> {
    Ok(f64_to_uint8_clamp(to_number(agent, v)?))
}

/// StringToBigInt(str): `None` where the specification returns undefined.
pub fn string_to_bigint(units: &[u16]) -> Option<BigInt> {
    let trimmed = trim_str_whitespace(units);
    if trimmed.is_empty() {
        return Some(BigInt::zero());
    }
    let mut text = Vec::with_capacity(trimmed.len());
    for &u in trimmed {
        if u > 0x7f {
            return None;
        }
        text.push(u as u8);
    }
    if let Some((digits, radix)) = non_decimal_digits(&text) {
        return BigInt::parse_radix(digits, radix);
    }
    let (negative, digits) = match text[0] {
        b'+' => (false, &text[1..]),
        b'-' => (true, &text[1..]),
        _ => (false, &text[..]),
    };
    if !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let b = BigInt::parse_radix(digits, 10)?;
    Some(if negative { b.neg() } else { b })
}

/// ToBigInt(argument).
pub fn to_bigint(agent: &mut Agent, v: Value) -> JsResult<HeapBigInt> {
    let prim = to_primitive(agent, v, PreferredType::Number)?;
    match prim {
        Value::Boolean(b) => Ok(agent.heap.alloc_bigint(BigInt::from_i64(i64::from(b)))),
        Value::BigInt(b) => Ok(b),
        Value::String(s) => {
            let parsed = string_to_bigint(agent.heap.string_units(s));
            match parsed {
                Some(b) => Ok(agent.heap.alloc_bigint(b)),
                None => agent.syntax_error("cannot convert string to a BigInt"),
            }
        }
        Value::Number(_) => agent.type_error("cannot convert a Number to a BigInt"),
        Value::Symbol(_) => agent.type_error("cannot convert a Symbol to a BigInt"),
        Value::Undefined | Value::Null | Value::Object(_) => {
            agent.type_error("cannot convert value to a BigInt")
        }
    }
}

/// ToBigInt64(argument).
pub fn to_bigint64(agent: &mut Agent, v: Value) -> JsResult<i64> {
    let b = to_bigint(agent, v)?;
    Ok(agent.heap.bigint(b).to_i64_wrapping())
}

/// ToBigUint64(argument).
pub fn to_biguint64(agent: &mut Agent, v: Value) -> JsResult<u64> {
    let b = to_bigint(agent, v)?;
    Ok(agent.heap.bigint(b).to_u64_wrapping())
}

/// Number::toString(x, 10) as a heap string.
pub fn number_to_js_string(agent: &mut Agent, n: f64) -> JsString {
    agent.heap.alloc_str(&number_to_string(n))
}

/// ToString(argument).
pub fn to_string(agent: &mut Agent, v: Value) -> JsResult<JsString> {
    match v {
        Value::Undefined => Ok(agent.heap.alloc_str("undefined")),
        Value::Null => Ok(agent.heap.alloc_str("null")),
        Value::Boolean(b) => Ok(agent.heap.alloc_str(if b { "true" } else { "false" })),
        Value::Number(n) => Ok(number_to_js_string(agent, n)),
        Value::String(s) => Ok(s),
        Value::Symbol(_) => agent.type_error("cannot convert a Symbol value to a string"),
        Value::BigInt(b) => {
            let text = agent.heap.bigint(b).to_string_radix(10);
            Ok(agent.heap.alloc_str(&text))
        }
        Value::Object(_) => {
            let prim = to_primitive(agent, v, PreferredType::String)?;
            to_string(agent, prim)
        }
    }
}

/// ToObject(argument).
pub fn to_object(agent: &mut Agent, v: Value) -> JsResult<Object> {
    let (proto, kind) = match v {
        Value::Object(o) => return Ok(o),
        Value::Undefined | Value::Null => {
            return agent.type_error("cannot convert undefined or null to an object");
        }
        Value::String(s) => return Ok(string_create(agent, s, None)),
        Value::Boolean(b) => (Intrinsic::BooleanPrototype, ObjectKind::Boolean(b)),
        Value::Number(n) => (Intrinsic::NumberPrototype, ObjectKind::Number(n)),
        Value::Symbol(s) => (Intrinsic::SymbolPrototype, ObjectKind::Symbol(s)),
        Value::BigInt(b) => (Intrinsic::BigIntPrototype, ObjectKind::BigInt(b)),
    };
    let proto = agent.intrinsic(proto);
    Ok(agent.heap.create_object(Some(proto), kind))
}

/// ToPropertyKey(argument).
pub fn to_property_key(agent: &mut Agent, v: Value) -> JsResult<PropertyKey> {
    match v {
        Value::Number(n) if n.trunc() == n && n >= 0.0 && n <= f64::from(MAX_ARRAY_INDEX) => {
            return Ok(PropertyKey::Index(n as u32));
        }
        Value::String(s) => return Ok(agent.heap.key_from_string(s)),
        Value::Symbol(s) => return Ok(PropertyKey::Symbol(s)),
        _ => {}
    }
    let key = to_primitive(agent, v, PreferredType::String)?;
    if let Value::Symbol(s) = key {
        return Ok(PropertyKey::Symbol(s));
    }
    let s = to_string(agent, key)?;
    Ok(agent.heap.key_from_string(s))
}

/// ToLength(argument).
pub fn to_length(agent: &mut Agent, v: Value) -> JsResult<f64> {
    let len = to_integer_or_infinity(agent, v)?;
    if len <= 0.0 {
        return Ok(0.0);
    }
    Ok(len.min(9007199254740991.0))
}

/// ToIndex(value).
pub fn to_index(agent: &mut Agent, v: Value) -> JsResult<u64> {
    let integer = to_integer_or_infinity(agent, v)?;
    if !(0.0..=9007199254740991.0).contains(&integer) {
        return agent.range_error("index out of range");
    }
    Ok(integer as u64)
}

/// CanonicalNumericIndexString(argument) on the code units of a string.
pub fn canonical_numeric_index_string(units: &[u16]) -> Option<f64> {
    if units == [u16::from(b'-'), u16::from(b'0')] {
        return Some(-0.0);
    }
    let first = *units.first()?;
    let plausible = (u16::from(b'0')..=u16::from(b'9')).contains(&first)
        || first == u16::from(b'-')
        || first == u16::from(b'I')
        || first == u16::from(b'N');
    if !plausible {
        return None;
    }
    let n = string_to_number(units);
    if utf16(&number_to_string(n)) == units {
        Some(n)
    } else {
        None
    }
}
