//! The `Math` object (ECMA-262 §21.3), with `Math.f16round` and
//! `Math.sumPrecise`. The transcendental functions come from glibc's libm
//! through `f64`'s methods.

use std::cell::Cell;

use crate::agent::Agent;
use crate::bigint::BigInt;
use crate::builtins::{intrinsic, method, plain_object, set_intrinsic, to_string_tag, value};
use crate::completion::JsResult;
use crate::function::{NativeCall, NativeFn};
use crate::gc::Realm;
use crate::interp::operators::number_exp;
use crate::number::{f16_bits_to_f64, f64_to_f16_bits};
use crate::numconv::{decompose, round_to_f64};
use crate::ops;
use crate::realm::Intrinsic;
use crate::value::Value;

/// Defines a one-argument Math function from `fn(f64) -> f64`.
macro_rules! unary {
    ($($name:ident => $f:expr;)*) => {
        $(
            fn $name(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
                let x = ops::to_number(agent, call.arg(0))?;
                let f: fn(f64) -> f64 = $f;
                Ok(Value::Number(f(x)))
            }
        )*
    };
}

unary! {
    abs => f64::abs;
    acos => f64::acos;
    acosh => f64::acosh;
    asin => f64::asin;
    asinh => asinh_exact_zero;
    atan => f64::atan;
    atanh => f64::atanh;
    cbrt => f64::cbrt;
    ceil => f64::ceil;
    cos => f64::cos;
    cosh => f64::cosh;
    exp => f64::exp;
    expm1 => f64::exp_m1;
    f16round => |x| f16_bits_to_f64(f64_to_f16_bits(x));
    floor => f64::floor;
    fround => |x| f64::from(x as f32);
    log => f64::ln;
    log1p => f64::ln_1p;
    log10 => f64::log10;
    log2 => f64::log2;
    round => round_number;
    sign => sign_number;
    sin => f64::sin;
    sinh => f64::sinh;
    sqrt => f64::sqrt;
    tan => f64::tan;
    tanh => f64::tanh;
    trunc => f64::trunc;
}

/// `asinh` keeping `-0` and the infinities.
fn asinh_exact_zero(x: f64) -> f64 {
    if x == 0.0 || x.is_infinite() {
        x
    } else {
        x.asinh()
    }
}

/// `Math.round(x)` (§21.3.2.28): half rounds up, `-0.5 ≤ x < 0` gives -0.
fn round_number(x: f64) -> f64 {
    if !x.is_finite() || x.trunc() == x {
        return x;
    }
    if x < 0.0 && x >= -0.5 {
        return -0.0;
    }
    let f = x.floor();
    // `x - f` is exact.
    if x - f >= 0.5 { f + 1.0 } else { f }
}

fn sign_number(x: f64) -> f64 {
    if x.is_nan() || x == 0.0 {
        x
    } else {
        x.signum()
    }
}

pub(crate) fn install(agent: &mut Agent, realm: Realm) {
    let object_proto = intrinsic(agent, realm, Intrinsic::ObjectPrototype);
    let math = plain_object(agent, object_proto);
    set_intrinsic(agent, realm, Intrinsic::Math, math);
    let constants = [
        ("E", std::f64::consts::E),
        ("LN10", std::f64::consts::LN_10),
        ("LN2", std::f64::consts::LN_2),
        ("LOG10E", std::f64::consts::LOG10_E),
        ("LOG2E", std::f64::consts::LOG2_E),
        ("PI", std::f64::consts::PI),
        ("SQRT1_2", std::f64::consts::FRAC_1_SQRT_2),
        ("SQRT2", std::f64::consts::SQRT_2),
    ];
    for (name, n) in constants {
        let key = agent.heap.key_from_str(name);
        value(agent, math, key, Value::Number(n), false, false, false);
    }
    let functions: [(&str, NativeFn, u32); 37] = [
        ("abs", abs, 1),
        ("acos", acos, 1),
        ("acosh", acosh, 1),
        ("asin", asin, 1),
        ("asinh", asinh, 1),
        ("atan", atan, 1),
        ("atanh", atanh, 1),
        ("atan2", atan2, 2),
        ("cbrt", cbrt, 1),
        ("ceil", ceil, 1),
        ("clz32", clz32, 1),
        ("cos", cos, 1),
        ("cosh", cosh, 1),
        ("exp", exp, 1),
        ("expm1", expm1, 1),
        ("f16round", f16round, 1),
        ("floor", floor, 1),
        ("fround", fround, 1),
        ("hypot", hypot, 2),
        ("imul", imul, 2),
        ("log", log, 1),
        ("log1p", log1p, 1),
        ("log10", log10, 1),
        ("log2", log2, 1),
        ("max", max, 2),
        ("min", min, 2),
        ("pow", pow, 2),
        ("random", random, 0),
        ("round", round, 1),
        ("sign", sign, 1),
        ("sin", sin, 1),
        ("sinh", sinh, 1),
        ("sqrt", sqrt, 1),
        ("sumPrecise", sum_precise, 1),
        ("tan", tan, 1),
        ("tanh", tanh, 1),
        ("trunc", trunc, 1),
    ];
    for (name, f, length) in functions {
        method(agent, realm, math, name, f, length);
    }
    to_string_tag(agent, math, "Math");
}

fn atan2(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let y = ops::to_number(agent, call.arg(0))?;
    let x = ops::to_number(agent, call.arg(1))?;
    Ok(Value::Number(y.atan2(x)))
}

fn clz32(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let n = ops::to_uint32(agent, call.arg(0))?;
    Ok(Value::Number(f64::from(n.leading_zeros())))
}

fn imul(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let a = ops::to_uint32(agent, call.arg(0))?;
    let b = ops::to_uint32(agent, call.arg(1))?;
    Ok(Value::Number(f64::from(a.wrapping_mul(b) as i32)))
}

fn pow(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let base = ops::to_number(agent, call.arg(0))?;
    let exponent = ops::to_number(agent, call.arg(1))?;
    Ok(Value::Number(number_exp(base, exponent)))
}

/// Every argument through ToNumber, in order, before any is inspected.
fn coerced_arguments(agent: &mut Agent, call: &NativeCall<'_>) -> JsResult<Vec<f64>> {
    let mut out = Vec::with_capacity(call.args.len());
    for &v in call.args {
        out.push(ops::to_number(agent, v)?);
    }
    Ok(out)
}

/// `Math.hypot(...args)` (§21.3.2.18): infinities win over NaN; the sum of
/// squares is scaled by the largest magnitude and compensated.
fn hypot(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let xs = coerced_arguments(agent, &call)?;
    if xs.iter().any(|x| x.is_infinite()) {
        return Ok(Value::Number(f64::INFINITY));
    }
    if xs.iter().any(|x| x.is_nan()) {
        return Ok(Value::Number(f64::NAN));
    }
    let largest = xs.iter().fold(0.0f64, |m, x| m.max(x.abs()));
    if largest == 0.0 {
        return Ok(Value::Number(0.0));
    }
    let mut sum = 0.0f64;
    let mut compensation = 0.0f64;
    for x in xs {
        let scaled = x / largest;
        let term = scaled * scaled - compensation;
        let next = sum + term;
        compensation = (next - sum) - term;
        sum = next;
    }
    Ok(Value::Number(sum.sqrt() * largest))
}

/// `Math.max` and `Math.min`: NaN wins, and +0 is larger than -0.
fn extremum(agent: &mut Agent, call: &NativeCall<'_>, want_max: bool) -> JsResult<Value> {
    let xs = coerced_arguments(agent, call)?;
    let mut best = if want_max {
        f64::NEG_INFINITY
    } else {
        f64::INFINITY
    };
    for x in xs {
        if x.is_nan() {
            return Ok(Value::Number(f64::NAN));
        }
        let better = if x == best {
            // Only the zeros tie with a different sign.
            if want_max {
                best.is_sign_negative() && x.is_sign_positive()
            } else {
                best.is_sign_positive() && x.is_sign_negative()
            }
        } else if want_max {
            x > best
        } else {
            x < best
        };
        if better {
            best = x;
        }
    }
    Ok(Value::Number(best))
}

fn max(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    extremum(agent, &call, true)
}

fn min(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    extremum(agent, &call, false)
}

thread_local! {
    static RANDOM_STATE: Cell<u64> = const { Cell::new(0) };
}

/// xorshift64* seeded from the clock and a stack address.
fn next_random() -> u64 {
    RANDOM_STATE.with(|state| {
        let mut x = state.get();
        if x == 0 {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos() as u64);
            let local = 0u8;
            x = nanos ^ ((&local as *const u8 as u64).rotate_left(32)) | 1;
        }
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        state.set(x);
        x.wrapping_mul(0x2545_f491_4f6c_dd1d)
    })
}

fn random(_agent: &mut Agent, _call: NativeCall<'_>) -> JsResult<Value> {
    let bits = next_random() >> 11;
    Ok(Value::Number(bits as f64 / 9007199254740992.0))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SumState {
    MinusZero,
    Finite,
    PlusInfinity,
    MinusInfinity,
    NotANumber,
}

/// `Math.sumPrecise(items)`: the exact sum, rounded once. Every finite
/// double is an integer multiple of 2^-1074, so the sum is kept exactly as
/// a BigInt count of those units.
fn sum_precise(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let items = call.arg(0);
    ops::require_object_coercible(agent, items)?;
    let mut record = ops::get_iterator(agent, items)?;
    let mut state = SumState::MinusZero;
    let mut sum = BigInt::zero();
    let mut count: u64 = 0;
    while let Some(v) = ops::iterator_step_value(agent, &mut record)? {
        count += 1;
        if count >= 1 << 53 {
            let error = agent.range_error("too many values for Math.sumPrecise");
            return ops::iterator_close(agent, &record, error);
        }
        let Value::Number(n) = v else {
            let error = agent.type_error("Math.sumPrecise values must be numbers");
            return ops::iterator_close(agent, &record, error);
        };
        if state == SumState::NotANumber {
            continue;
        }
        if n.is_nan() {
            state = SumState::NotANumber;
        } else if n == f64::INFINITY {
            state = if state == SumState::MinusInfinity {
                SumState::NotANumber
            } else {
                SumState::PlusInfinity
            };
        } else if n == f64::NEG_INFINITY {
            state = if state == SumState::PlusInfinity {
                SumState::NotANumber
            } else {
                SumState::MinusInfinity
            };
        } else if !(n == 0.0 && n.is_sign_negative())
            && matches!(state, SumState::MinusZero | SumState::Finite)
        {
            state = SumState::Finite;
            let (f, e) = decompose(n.abs());
            let units = BigInt::from_u64(f).shl((e + 1074) as u64);
            sum = if n < 0.0 {
                sum.sub(&units)
            } else {
                sum.add(&units)
            };
        }
    }
    let result = match state {
        SumState::NotANumber => f64::NAN,
        SumState::PlusInfinity => f64::INFINITY,
        SumState::MinusInfinity => f64::NEG_INFINITY,
        SumState::MinusZero => -0.0,
        SumState::Finite => {
            if sum.is_zero() {
                0.0
            } else if sum.is_negative() {
                -round_to_f64(&sum.neg(), -1074, false)
            } else {
                round_to_f64(&sum, -1074, false)
            }
        }
    };
    Ok(Value::Number(result))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_and_sign_edges() {
        assert_eq!(round_number(0.49999999999999994), 0.0);
        assert!(round_number(-0.5).is_sign_negative());
        assert_eq!(round_number(-0.6), -1.0);
        assert_eq!(round_number(2.5), 3.0);
        assert_eq!(round_number(-2.5), -2.0);
        assert!(sign_number(-0.0).is_sign_negative());
        assert!(asinh_exact_zero(-0.0).is_sign_negative());
    }
}
