//! Testing and comparison operations (ECMA-262 §7.2).

use std::cmp::Ordering;

use crate::agent::Agent;
use crate::completion::JsResult;
use crate::object::ObjectKind;
use crate::ops::conversion::{
    Numeric, PreferredType, is_htmldda_value, string_to_bigint, to_number, to_numeric, to_primitive,
};
use crate::proxy::proxy_target;
use crate::value::Value;

/// RequireObjectCoercible(argument).
pub fn require_object_coercible(agent: &mut Agent, v: Value) -> JsResult<Value> {
    if v.is_nullish() {
        return agent.type_error("cannot convert undefined or null to an object");
    }
    Ok(v)
}

/// IsArray(argument).
pub fn is_array(agent: &mut Agent, v: Value) -> JsResult<bool> {
    let Value::Object(mut o) = v else {
        return Ok(false);
    };
    loop {
        let is_proxy = match &agent.heap.object(o).kind {
            ObjectKind::Array => return Ok(true),
            ObjectKind::Proxy(_) => true,
            _ => false,
        };
        if !is_proxy {
            return Ok(false);
        }
        match proxy_target(agent, o) {
            Some(t) => o = t,
            None => return agent.type_error("IsArray on a revoked Proxy"),
        }
    }
}

/// IsIntegralNumber(argument).
pub fn is_integral_number(v: Value) -> bool {
    match v {
        Value::Number(n) => n.is_finite() && n.trunc() == n,
        _ => false,
    }
}

/// IsPropertyKey(argument).
pub fn is_property_key(v: Value) -> bool {
    matches!(v, Value::String(_) | Value::Symbol(_))
}

/// SameValueNonNumber(x, y) for values of the same type.
pub fn same_value_non_number(agent: &mut Agent, x: Value, y: Value) -> bool {
    match (x, y) {
        (Value::Undefined, Value::Undefined) | (Value::Null, Value::Null) => true,
        (Value::Boolean(a), Value::Boolean(b)) => a == b,
        (Value::String(a), Value::String(b)) => agent.heap.string_eq(a, b),
        (Value::Symbol(a), Value::Symbol(b)) => a == b,
        (Value::BigInt(a), Value::BigInt(b)) => {
            a == b || agent.heap.bigint(a) == agent.heap.bigint(b)
        }
        (Value::Object(a), Value::Object(b)) => a == b,
        _ => false,
    }
}

/// SameValue(x, y).
pub fn same_value(agent: &mut Agent, x: Value, y: Value) -> bool {
    match (x, y) {
        (Value::Number(a), Value::Number(b)) => {
            (a.is_nan() && b.is_nan()) || (a == b && a.is_sign_negative() == b.is_sign_negative())
        }
        _ => same_value_non_number(agent, x, y),
    }
}

/// SameValueZero(x, y).
pub fn same_value_zero(agent: &mut Agent, x: Value, y: Value) -> bool {
    match (x, y) {
        (Value::Number(a), Value::Number(b)) => (a.is_nan() && b.is_nan()) || a == b,
        _ => same_value_non_number(agent, x, y),
    }
}

/// IsStrictlyEqual(x, y).
pub fn is_strictly_equal(agent: &mut Agent, x: Value, y: Value) -> bool {
    match (x, y) {
        (Value::Number(a), Value::Number(b)) => a == b,
        _ => same_value_non_number(agent, x, y),
    }
}

fn bigint_equals_number(agent: &Agent, b: crate::gc::HeapBigInt, n: f64) -> bool {
    agent.heap.bigint(b).cmp_f64(n) == Some(Ordering::Equal)
}

/// IsLooselyEqual(x, y).
pub fn is_loosely_equal(agent: &mut Agent, x: Value, y: Value) -> JsResult<bool> {
    if x.value_type() == y.value_type() {
        return Ok(is_strictly_equal(agent, x, y));
    }
    if x.is_nullish() && y.is_nullish() {
        return Ok(true);
    }
    if (is_htmldda_value(agent, x) && y.is_nullish())
        || (x.is_nullish() && is_htmldda_value(agent, y))
    {
        return Ok(true);
    }
    match (x, y) {
        (Value::Number(a), Value::String(_)) => {
            let b = to_number(agent, y)?;
            Ok(a == b)
        }
        (Value::String(_), Value::Number(b)) => {
            let a = to_number(agent, x)?;
            Ok(a == b)
        }
        (Value::BigInt(a), Value::String(s)) | (Value::String(s), Value::BigInt(a)) => {
            let parsed = string_to_bigint(agent.heap.string_units(s));
            Ok(parsed.is_some_and(|n| *agent.heap.bigint(a) == n))
        }
        (Value::Boolean(_), _) => {
            let n = to_number(agent, x)?;
            is_loosely_equal(agent, Value::Number(n), y)
        }
        (_, Value::Boolean(_)) => {
            let n = to_number(agent, y)?;
            is_loosely_equal(agent, x, Value::Number(n))
        }
        (
            Value::String(_) | Value::Number(_) | Value::BigInt(_) | Value::Symbol(_),
            Value::Object(_),
        ) => {
            let p = to_primitive(agent, y, PreferredType::Default)?;
            is_loosely_equal(agent, x, p)
        }
        (
            Value::Object(_),
            Value::String(_) | Value::Number(_) | Value::BigInt(_) | Value::Symbol(_),
        ) => {
            let p = to_primitive(agent, x, PreferredType::Default)?;
            is_loosely_equal(agent, p, y)
        }
        (Value::BigInt(a), Value::Number(n)) | (Value::Number(n), Value::BigInt(a)) => {
            Ok(bigint_equals_number(agent, a, n))
        }
        _ => Ok(false),
    }
}

/// IsLessThan(x, y, LeftFirst): `None` is the specification's undefined.
pub fn is_less_than(
    agent: &mut Agent,
    x: Value,
    y: Value,
    left_first: bool,
) -> JsResult<Option<bool>> {
    let (px, py) = if left_first {
        let px = to_primitive(agent, x, PreferredType::Number)?;
        let py = to_primitive(agent, y, PreferredType::Number)?;
        (px, py)
    } else {
        let py = to_primitive(agent, y, PreferredType::Number)?;
        let px = to_primitive(agent, x, PreferredType::Number)?;
        (px, py)
    };
    match (px, py) {
        (Value::String(a), Value::String(b)) => {
            return Ok(Some(agent.heap.string_cmp(a, b) == Ordering::Less));
        }
        (Value::BigInt(a), Value::String(s)) => {
            let Some(n) = string_to_bigint(agent.heap.string_units(s)) else {
                return Ok(None);
            };
            return Ok(Some(agent.heap.bigint(a).cmp_bigint(&n) == Ordering::Less));
        }
        (Value::String(s), Value::BigInt(b)) => {
            let Some(n) = string_to_bigint(agent.heap.string_units(s)) else {
                return Ok(None);
            };
            return Ok(Some(n.cmp_bigint(agent.heap.bigint(b)) == Ordering::Less));
        }
        _ => {}
    }
    let nx = to_numeric(agent, px)?;
    let ny = to_numeric(agent, py)?;
    Ok(match (nx, ny) {
        (Numeric::Number(a), Numeric::Number(b)) => {
            if a.is_nan() || b.is_nan() {
                None
            } else {
                Some(a < b)
            }
        }
        (Numeric::BigInt(a), Numeric::BigInt(b)) => {
            Some(agent.heap.bigint(a).cmp_bigint(agent.heap.bigint(b)) == Ordering::Less)
        }
        (Numeric::BigInt(a), Numeric::Number(b)) => {
            agent.heap.bigint(a).cmp_f64(b).map(|o| o == Ordering::Less)
        }
        (Numeric::Number(a), Numeric::BigInt(b)) => agent
            .heap
            .bigint(b)
            .cmp_f64(a)
            .map(|o| o == Ordering::Greater),
    })
}
