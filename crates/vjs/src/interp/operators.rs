//! Operators (ECMA-262 §13.15.3 ApplyStringOrNumericBinaryOperator and
//! the Number and BigInt operations of §6.1.6).

use crate::agent::Agent;
use crate::bigint::BigInt;
use crate::bytecode::Op;
use crate::completion::JsResult;
use crate::number::{f64_to_int32, f64_to_uint32};
use crate::ops::{self, Numeric, PreferredType};
use crate::value::{PropertyKey, Value};

/// Number::exponentiate.
pub(crate) fn number_exp(base: f64, exponent: f64) -> f64 {
    if exponent.is_nan() {
        return f64::NAN;
    }
    if exponent == 0.0 {
        return 1.0;
    }
    if base.is_nan() {
        return f64::NAN;
    }
    if base.abs() == 1.0 && exponent.is_infinite() {
        return f64::NAN;
    }
    base.powf(exponent)
}

fn number_op(op: Op, a: f64, b: f64) -> f64 {
    match op {
        Op::Add => a + b,
        Op::Sub => a - b,
        Op::Mul => a * b,
        Op::Div => a / b,
        Op::Rem => a % b,
        Op::Exp => number_exp(a, b),
        Op::Shl => f64::from(f64_to_int32(a).wrapping_shl(f64_to_uint32(b) & 31)),
        Op::Shr => f64::from(f64_to_int32(a) >> (f64_to_uint32(b) & 31)),
        Op::UShr => f64::from(f64_to_uint32(a) >> (f64_to_uint32(b) & 31)),
        Op::BitAnd => f64::from(f64_to_int32(a) & f64_to_int32(b)),
        Op::BitOr => f64::from(f64_to_int32(a) | f64_to_int32(b)),
        Op::BitXor => f64::from(f64_to_int32(a) ^ f64_to_int32(b)),
        _ => f64::NAN,
    }
}

fn bigint_op(agent: &mut Agent, op: Op, a: &BigInt, b: &BigInt) -> JsResult<BigInt> {
    Ok(match op {
        Op::Add => a.add(b),
        Op::Sub => a.sub(b),
        Op::Mul => a.mul(b),
        Op::Div => match a.div_rem(b) {
            Some((q, _)) => q,
            None => return agent.range_error("division by zero"),
        },
        Op::Rem => match a.div_rem(b) {
            Some((_, r)) => r,
            None => return agent.range_error("division by zero"),
        },
        Op::Exp => {
            if b.is_negative() {
                return agent.range_error("exponent must be non-negative");
            }
            if b.bit_length() > 64 {
                if a.is_zero() || a.cmp_bigint(&BigInt::from_i64(1)).is_eq() {
                    return Ok(a.clone());
                }
                return agent.range_error("maximum BigInt size exceeded");
            }
            match a.pow(b.to_u64_wrapping()) {
                Some(r) => r,
                None => return agent.range_error("maximum BigInt size exceeded"),
            }
        }
        Op::Shl => match a.shift_left(b) {
            Some(r) => r,
            None => return agent.range_error("maximum BigInt size exceeded"),
        },
        Op::Shr => match a.shift_left(&b.neg()) {
            Some(r) => r,
            None => return agent.range_error("maximum BigInt size exceeded"),
        },
        Op::UShr => {
            return agent.type_error("BigInts have no unsigned right shift, use >> instead");
        }
        Op::BitAnd => a.bitand(b),
        Op::BitOr => a.bitor(b),
        Op::BitXor => a.bitxor(b),
        _ => return agent.type_error("invalid BigInt operation"),
    })
}

/// A numeric operator on two values already converted to primitives.
fn numeric(agent: &mut Agent, op: Op, a: Value, b: Value) -> JsResult<Value> {
    let x = ops::to_numeric(agent, a)?;
    let y = ops::to_numeric(agent, b)?;
    match (x, y) {
        (Numeric::Number(x), Numeric::Number(y)) => Ok(Value::Number(number_op(op, x, y))),
        (Numeric::BigInt(x), Numeric::BigInt(y)) => {
            let (x, y) = (agent.heap.bigint(x).clone(), agent.heap.bigint(y).clone());
            let r = bigint_op(agent, op, &x, &y)?;
            Ok(Value::BigInt(agent.heap.alloc_bigint(r)))
        }
        _ => agent.type_error("cannot mix BigInt and other types, use explicit conversions"),
    }
}

/// ApplyStringOrNumericBinaryOperator for the arithmetic and bitwise
/// operators.
pub(crate) fn binary(agent: &mut Agent, op: Op, a: Value, b: Value) -> JsResult<Value> {
    if let (Value::Number(x), Value::Number(y)) = (a, b) {
        return Ok(Value::Number(number_op(op, x, y)));
    }
    if matches!(op, Op::Add) {
        if let (Value::String(x), Value::String(y)) = (a, b) {
            return concat(agent, x, y);
        }
        let pa = ops::to_primitive(agent, a, PreferredType::Default)?;
        let pb = ops::to_primitive(agent, b, PreferredType::Default)?;
        if matches!(pa, Value::String(_)) || matches!(pb, Value::String(_)) {
            let sa = ops::to_string(agent, pa)?;
            let sb = ops::to_string(agent, pb)?;
            return concat(agent, sa, sb);
        }
        return numeric(agent, op, pa, pb);
    }
    numeric(agent, op, a, b)
}

pub(crate) fn concat(
    agent: &mut Agent,
    a: crate::gc::JsString,
    b: crate::gc::JsString,
) -> JsResult<Value> {
    match agent.heap.concat(a, b) {
        Some(s) => Ok(Value::String(s)),
        None => agent.range_error("invalid string length"),
    }
}

/// Unary minus.
pub(crate) fn negate(agent: &mut Agent, v: Value) -> JsResult<Value> {
    match ops::to_numeric(agent, v)? {
        Numeric::Number(n) => Ok(Value::Number(-n)),
        Numeric::BigInt(b) => {
            let r = agent.heap.bigint(b).neg();
            Ok(Value::BigInt(agent.heap.alloc_bigint(r)))
        }
    }
}

/// `~v`.
pub(crate) fn bit_not(agent: &mut Agent, v: Value) -> JsResult<Value> {
    match ops::to_numeric(agent, v)? {
        Numeric::Number(n) => Ok(Value::Number(f64::from(!f64_to_int32(n)))),
        Numeric::BigInt(b) => {
            let r = agent.heap.bigint(b).bitnot();
            Ok(Value::BigInt(agent.heap.alloc_bigint(r)))
        }
    }
}

/// `v + 1` or `v - 1` of a numeric value (Number::add / BigInt::add).
pub(crate) fn step(agent: &mut Agent, v: Value, delta: i64) -> JsResult<Value> {
    match v {
        Value::Number(n) => Ok(Value::Number(n + delta as f64)),
        Value::BigInt(b) => {
            let r = agent.heap.bigint(b).add(&BigInt::from_i64(delta));
            Ok(Value::BigInt(agent.heap.alloc_bigint(r)))
        }
        _ => {
            let n = ops::to_numeric(agent, v)?;
            let v = match n {
                Numeric::Number(n) => Value::Number(n),
                Numeric::BigInt(b) => Value::BigInt(b),
            };
            step(agent, v, delta)
        }
    }
}

/// ToNumeric as a value.
pub(crate) fn to_numeric_value(agent: &mut Agent, v: Value) -> JsResult<Value> {
    if let Value::Number(_) | Value::BigInt(_) = v {
        return Ok(v);
    }
    Ok(match ops::to_numeric(agent, v)? {
        Numeric::Number(n) => Value::Number(n),
        Numeric::BigInt(b) => Value::BigInt(b),
    })
}

/// The relational and equality operators.
pub(crate) fn compare(agent: &mut Agent, op: Op, a: Value, b: Value) -> JsResult<Value> {
    if let (Value::Number(x), Value::Number(y)) = (a, b) {
        let r = match op {
            Op::Lt => x < y,
            Op::Gt => x > y,
            Op::LtEq => x <= y,
            Op::GtEq => x >= y,
            Op::Eq | Op::StrictEq => x == y,
            Op::NotEq | Op::StrictNotEq => x != y,
            _ => false,
        };
        return Ok(Value::Boolean(r));
    }
    let r = match op {
        Op::Lt => ops::is_less_than(agent, a, b, true)? == Some(true),
        Op::Gt => ops::is_less_than(agent, b, a, false)? == Some(true),
        Op::LtEq => ops::is_less_than(agent, b, a, false)? == Some(false),
        Op::GtEq => ops::is_less_than(agent, a, b, true)? == Some(false),
        Op::Eq => ops::is_loosely_equal(agent, a, b)?,
        Op::NotEq => !ops::is_loosely_equal(agent, a, b)?,
        Op::StrictEq => ops::is_strictly_equal(agent, a, b),
        Op::StrictNotEq => !ops::is_strictly_equal(agent, a, b),
        _ => false,
    };
    Ok(Value::Boolean(r))
}

/// `key in obj`.
pub(crate) fn has_in(agent: &mut Agent, key: Value, obj: Value) -> JsResult<Value> {
    let Value::Object(o) = obj else {
        return agent.type_error("cannot use 'in' operator to search for a key in a non-object");
    };
    let k: PropertyKey = ops::to_property_key(agent, key)?;
    Ok(Value::Boolean(ops::has_property(agent, o, k)?))
}

/// `typeof v`.
pub(crate) fn type_of(agent: &mut Agent, v: Value) -> Value {
    let s = match v {
        Value::Undefined => "undefined",
        Value::Null => "object",
        Value::Boolean(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Symbol(_) => "symbol",
        Value::BigInt(_) => "bigint",
        Value::Object(o) => {
            if ops::is_htmldda_value(agent, v) {
                "undefined"
            } else if agent.heap.object(o).is_callable() {
                "function"
            } else {
                "object"
            }
        }
    };
    Value::String(agent.heap.alloc_str(s))
}
