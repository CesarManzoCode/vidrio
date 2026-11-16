//! `Number` (ECMA-262 §21.1) and `BigInt` (§21.2).

use crate::agent::Agent;
use crate::bigint::BigInt;
use crate::builtins::{
    constructor, intrinsic, method, set_intrinsic, str_value, to_string_tag, value,
};
use crate::completion::JsResult;
use crate::function::NativeCall;
use crate::gc::{HeapBigInt, Realm};
use crate::numconv;
use crate::object::ObjectKind;
use crate::ops::{self, Numeric, PreferredType};
use crate::realm::Intrinsic;
use crate::value::Value;

pub(crate) fn install(agent: &mut Agent, realm: Realm) {
    install_number(agent, realm);
    install_bigint(agent, realm);
}

fn install_number(agent: &mut Agent, realm: Realm) {
    let proto = intrinsic(agent, realm, Intrinsic::NumberPrototype);
    let ctor = constructor(agent, realm, "Number", number_constructor, 1, proto, None);
    set_intrinsic(agent, realm, Intrinsic::Number, ctor);
    let constants = [
        ("EPSILON", f64::EPSILON),
        ("MAX_SAFE_INTEGER", 9007199254740991.0),
        ("MAX_VALUE", f64::MAX),
        ("MIN_SAFE_INTEGER", -9007199254740991.0),
        ("MIN_VALUE", 5e-324),
        ("NaN", f64::NAN),
        ("NEGATIVE_INFINITY", f64::NEG_INFINITY),
        ("POSITIVE_INFINITY", f64::INFINITY),
    ];
    for (name, n) in constants {
        let key = agent.heap.key_from_str(name);
        value(agent, ctor, key, Value::Number(n), false, false, false);
    }
    method(agent, realm, ctor, "isFinite", is_finite, 1);
    method(agent, realm, ctor, "isInteger", is_integer, 1);
    method(agent, realm, ctor, "isNaN", is_nan, 1);
    method(agent, realm, ctor, "isSafeInteger", is_safe_integer, 1);
    // Number.parseFloat and Number.parseInt are the global functions.
    for (name, which) in [
        ("parseFloat", Intrinsic::ParseFloat),
        ("parseInt", Intrinsic::ParseInt),
    ] {
        let f = intrinsic(agent, realm, which);
        let key = agent.heap.key_from_str(name);
        value(agent, ctor, key, Value::Object(f), true, false, true);
    }

    method(agent, realm, proto, "toExponential", to_exponential, 1);
    method(agent, realm, proto, "toFixed", to_fixed, 1);
    method(
        agent,
        realm,
        proto,
        "toLocaleString",
        number_to_locale_string,
        0,
    );
    method(agent, realm, proto, "toPrecision", to_precision, 1);
    method(agent, realm, proto, "toString", number_to_string, 1);
    method(agent, realm, proto, "valueOf", number_value_of, 0);
}

fn install_bigint(agent: &mut Agent, realm: Realm) {
    let proto = intrinsic(agent, realm, Intrinsic::BigIntPrototype);
    let ctor = constructor(agent, realm, "BigInt", bigint_constructor, 1, proto, None);
    set_intrinsic(agent, realm, Intrinsic::BigInt, ctor);
    method(agent, realm, ctor, "asIntN", as_int_n, 2);
    method(agent, realm, ctor, "asUintN", as_uint_n, 2);
    method(
        agent,
        realm,
        proto,
        "toLocaleString",
        bigint_to_locale_string,
        0,
    );
    method(agent, realm, proto, "toString", bigint_to_string, 0);
    method(agent, realm, proto, "valueOf", bigint_value_of, 0);
    to_string_tag(agent, proto, "BigInt");
}

// ----- Number -----

/// `Number(value)` (§21.1.1.1).
fn number_constructor(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let n = if call.args.is_empty() {
        0.0
    } else {
        match ops::to_numeric(agent, call.arg(0))? {
            Numeric::Number(n) => n,
            Numeric::BigInt(b) => agent.heap.bigint(b).to_f64(),
        }
    };
    let Some(new_target) = call.new_target else {
        return Ok(Value::Number(n));
    };
    let o = ops::ordinary_create_from_constructor(
        agent,
        new_target,
        Intrinsic::NumberPrototype,
        ObjectKind::Number(n),
    )?;
    Ok(Value::Object(o))
}

fn is_finite(_agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    Ok(Value::Boolean(
        matches!(call.arg(0), Value::Number(n) if n.is_finite()),
    ))
}

fn is_integer(_agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    Ok(Value::Boolean(ops::is_integral_number(call.arg(0))))
}

fn is_nan(_agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    Ok(Value::Boolean(
        matches!(call.arg(0), Value::Number(n) if n.is_nan()),
    ))
}

fn is_safe_integer(_agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let v = call.arg(0);
    Ok(Value::Boolean(
        ops::is_integral_number(v)
            && matches!(v, Value::Number(n) if n.abs() <= 9007199254740991.0),
    ))
}

/// ThisNumberValue(value).
fn this_number_value(agent: &mut Agent, v: Value) -> JsResult<f64> {
    match v {
        Value::Number(n) => Ok(n),
        Value::Object(o) => {
            let inner = match &agent.heap.object(o).kind {
                ObjectKind::Number(n) => Some(*n),
                _ => None,
            };
            match inner {
                Some(n) => Ok(n),
                None => agent.type_error("not a Number object"),
            }
        }
        _ => agent.type_error("not a number"),
    }
}

/// A digit count argument checked against `min..=max` after the finite
/// check (`what` names the method in the RangeError).
fn digit_count(agent: &mut Agent, n: f64, min: f64, max: f64, what: &str) -> JsResult<u32> {
    if !(min..=max).contains(&n) {
        return agent.range_error(&format!("{what} argument must be between {min} and {max}"));
    }
    Ok(n as u32)
}

/// `Number.prototype.toExponential(fractionDigits)` (§21.1.3.2).
fn to_exponential(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let x = this_number_value(agent, call.this)?;
    let fd = call.arg(0);
    let f = ops::to_integer_or_infinity(agent, fd)?;
    if !x.is_finite() {
        return Ok(str_value(agent, &numconv::number_to_string(x)));
    }
    let f = digit_count(agent, f, 0.0, 100.0, "toExponential()")?;
    let digits = if fd.is_undefined() { None } else { Some(f) };
    Ok(str_value(agent, &numconv::to_exponential(x, digits)))
}

/// `Number.prototype.toFixed(fractionDigits)` (§21.1.3.3).
fn to_fixed(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let x = this_number_value(agent, call.this)?;
    let f = ops::to_integer_or_infinity(agent, call.arg(0))?;
    let f = digit_count(agent, f, 0.0, 100.0, "toFixed()")?;
    if !x.is_finite() {
        return Ok(str_value(agent, &numconv::number_to_string(x)));
    }
    Ok(str_value(agent, &numconv::to_fixed(x, f)))
}

fn number_to_locale_string(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let x = this_number_value(agent, call.this)?;
    Ok(str_value(agent, &numconv::number_to_string(x)))
}

/// `Number.prototype.toPrecision(precision)` (§21.1.3.5).
fn to_precision(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let x = this_number_value(agent, call.this)?;
    let precision = call.arg(0);
    if precision.is_undefined() {
        return Ok(str_value(agent, &numconv::number_to_string(x)));
    }
    let p = ops::to_integer_or_infinity(agent, precision)?;
    if !x.is_finite() {
        return Ok(str_value(agent, &numconv::number_to_string(x)));
    }
    let p = digit_count(agent, p, 1.0, 100.0, "toPrecision()")?;
    Ok(str_value(agent, &numconv::to_precision(x, p)))
}

/// The radix argument of the `toString` methods: 10 when undefined,
/// otherwise an integer in 2..=36.
fn radix_argument(agent: &mut Agent, v: Value) -> JsResult<u32> {
    if v.is_undefined() {
        return Ok(10);
    }
    let r = ops::to_integer_or_infinity(agent, v)?;
    if !(2.0..=36.0).contains(&r) {
        return agent.range_error("toString() radix must be between 2 and 36");
    }
    Ok(r as u32)
}

/// `Number.prototype.toString([radix])` (§21.1.3.6).
fn number_to_string(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let x = this_number_value(agent, call.this)?;
    let radix = radix_argument(agent, call.arg(0))?;
    Ok(str_value(agent, &numconv::number_to_string_radix(x, radix)))
}

fn number_value_of(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    Ok(Value::Number(this_number_value(agent, call.this)?))
}

// ----- BigInt -----

/// `BigInt(value)` (§21.2.1.1).
fn bigint_constructor(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    if call.new_target.is_some() {
        return agent.type_error("BigInt is not a constructor");
    }
    let prim = ops::to_primitive(agent, call.arg(0), PreferredType::Number)?;
    if let Value::Number(n) = prim {
        // NumberToBigInt.
        return match BigInt::from_f64(n) {
            Some(b) => Ok(Value::BigInt(agent.heap.alloc_bigint(b))),
            None => agent.range_error("cannot convert a non-integral Number to a BigInt"),
        };
    }
    Ok(Value::BigInt(ops::to_bigint(agent, prim)?))
}

/// The `bits` and `bigint` arguments of `asIntN`/`asUintN`.
fn as_n_arguments(agent: &mut Agent, call: &NativeCall<'_>) -> JsResult<(u64, BigInt)> {
    let bits = ops::to_index(agent, call.arg(0))?;
    let b = ops::to_bigint(agent, call.arg(1))?;
    Ok((bits, agent.heap.bigint(b).clone()))
}

/// `BigInt.asIntN(bits, bigint)` (§21.2.2.1).
fn as_int_n(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let (bits, b) = as_n_arguments(agent, &call)?;
    match b.as_int_n(bits) {
        Some(r) => Ok(Value::BigInt(agent.heap.alloc_bigint(r))),
        None => agent.range_error("maximum BigInt size exceeded"),
    }
}

/// `BigInt.asUintN(bits, bigint)` (§21.2.2.2).
fn as_uint_n(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let (bits, b) = as_n_arguments(agent, &call)?;
    match b.as_uint_n(bits) {
        Some(r) => Ok(Value::BigInt(agent.heap.alloc_bigint(r))),
        None => agent.range_error("maximum BigInt size exceeded"),
    }
}

/// ThisBigIntValue(value).
fn this_bigint_value(agent: &mut Agent, v: Value) -> JsResult<HeapBigInt> {
    match v {
        Value::BigInt(b) => Ok(b),
        Value::Object(o) => {
            let inner = match &agent.heap.object(o).kind {
                ObjectKind::BigInt(b) => Some(*b),
                _ => None,
            };
            match inner {
                Some(b) => Ok(b),
                None => agent.type_error("not a BigInt object"),
            }
        }
        _ => agent.type_error("not a BigInt"),
    }
}

fn bigint_to_locale_string(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let b = this_bigint_value(agent, call.this)?;
    let s = agent.heap.bigint(b).to_string_radix(10);
    Ok(str_value(agent, &s))
}

/// `BigInt.prototype.toString([radix])` (§21.2.3.3).
fn bigint_to_string(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let b = this_bigint_value(agent, call.this)?;
    let radix = radix_argument(agent, call.arg(0))?;
    let s = agent.heap.bigint(b).to_string_radix(radix);
    Ok(str_value(agent, &s))
}

fn bigint_value_of(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    Ok(Value::BigInt(this_bigint_value(agent, call.this)?))
}
