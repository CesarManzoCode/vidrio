//! `Atomics` (ECMA-262 §25.4): the read-modify-write operations,
//! `load`/`store`, `isLockFree`, `wait`/`notify` between agents (threads)
//! over a `SharedArrayBuffer`, `waitAsync` and `pause`.
//!
//! On a shared buffer each operation is one compare-and-swap loop on the
//! 64-bit word that holds the element ([`crate::buffer::SharedBlock`]); on
//! a non-shared buffer only this agent sees the bytes, so a plain read and
//! write is atomic.

use crate::agent::Agent;
use crate::buffer::{self, ElementValue, TypedArrayKind};
use crate::builtins::{intrinsic, method, plain_object, set_intrinsic, to_string_tag};
use crate::completion::JsResult;
use crate::function::{NativeCall, NativeFn};
use crate::gc::{Object, Realm};
use crate::interp::promise;
use crate::object::ObjectKind;
use crate::ops;
use crate::realm::Intrinsic;
use crate::value::Value;
use crate::waiter;

pub(crate) fn install(agent: &mut Agent, realm: Realm) {
    let object_proto = intrinsic(agent, realm, Intrinsic::ObjectPrototype);
    let atomics = plain_object(agent, object_proto);
    set_intrinsic(agent, realm, Intrinsic::Atomics, atomics);
    let methods: &[(&str, NativeFn, u32)] = &[
        ("add", add, 3),
        ("and", and, 3),
        ("compareExchange", compare_exchange, 4),
        ("exchange", exchange, 3),
        ("isLockFree", is_lock_free, 1),
        ("load", load, 2),
        ("notify", notify, 3),
        ("or", or, 3),
        ("pause", pause, 0),
        ("store", store, 3),
        ("sub", sub, 3),
        ("wait", wait, 4),
        ("waitAsync", wait_async, 4),
        ("xor", xor, 3),
    ];
    for &(name, f, length) in methods {
        method(agent, realm, atomics, name, f, length);
    }
    to_string_tag(agent, atomics, "Atomics");
}

/// ValidateIntegerTypedArray(typedArray, waitable): the array and its kind.
fn validate_integer_typed_array(
    agent: &mut Agent,
    v: Value,
    waitable: bool,
) -> JsResult<(Object, TypedArrayKind)> {
    let kind = match v {
        Value::Object(o) => match &agent.heap.object(o).kind {
            ObjectKind::TypedArray(t) => Some((o, t.kind)),
            _ => None,
        },
        _ => None,
    };
    let Some((o, kind)) = kind else {
        return agent.type_error("Atomics: not a TypedArray");
    };
    if buffer::typed_array_length(agent, o).is_none() {
        return agent.type_error("Atomics: the TypedArray is detached or out of bounds");
    }
    let ok = if waitable {
        matches!(kind, TypedArrayKind::Int32 | TypedArrayKind::BigInt64)
    } else {
        kind.is_unclamped_integer() || kind.is_bigint()
    };
    if !ok {
        return agent.type_error(&format!(
            "Atomics: {} is not a valid array type",
            kind.name()
        ));
    }
    Ok((o, kind))
}

/// ValidateAtomicAccess(taRecord, requestIndex): the byte index.
fn validate_atomic_access(
    agent: &mut Agent,
    o: Object,
    kind: TypedArrayKind,
    index: Value,
) -> JsResult<usize> {
    let len = buffer::typed_array_length(agent, o).unwrap_or(0);
    let access = ops::to_index(agent, index)?;
    if access >= len as u64 {
        return agent.range_error("Atomics: index out of range");
    }
    let offset = buffer::typed_array_slots(agent, o).map_or(0, |s| s.2);
    Ok(access as usize * kind.element_size() + offset)
}

/// RevalidateAtomicAccess(typedArray, byteIndexInBuffer).
fn revalidate(agent: &mut Agent, o: Object, byte_index: usize) -> JsResult<Object> {
    if buffer::typed_array_length(agent, o).is_none() {
        return agent.type_error("Atomics: the TypedArray is detached or out of bounds");
    }
    let Some((_, buf, _, _)) = buffer::typed_array_slots(agent, o) else {
        return agent.type_error("Atomics: not a TypedArray");
    };
    if byte_index >= buffer::buffer_byte_length(agent, buf) {
        return agent.range_error("Atomics: index out of range");
    }
    Ok(buf)
}

/// The operand of an atomic operation: ToBigInt, or ToIntegerOrInfinity
/// as a Number.
fn operand(agent: &mut Agent, kind: TypedArrayKind, v: Value) -> JsResult<ElementValue> {
    if kind.is_bigint() {
        buffer::to_element_value(agent, kind, v)
    } else {
        Ok(ElementValue::Number(ops::to_integer_or_infinity(agent, v)?))
    }
}

fn mask(kind: TypedArrayKind) -> u64 {
    match kind.element_size() {
        8 => u64::MAX,
        size => (1u64 << (size * 8)) - 1,
    }
}

/// AtomicReadModifyWrite(typedArray, index, value, op).
fn read_modify_write(
    agent: &mut Agent,
    call: &NativeCall<'_>,
    op: fn(u64, u64) -> u64,
) -> JsResult<Value> {
    let (o, kind) = validate_integer_typed_array(agent, call.arg(0), false)?;
    let byte_index = validate_atomic_access(agent, o, kind, call.arg(1))?;
    let v = operand(agent, kind, call.arg(2))?;
    let buf = revalidate(agent, o, byte_index)?;
    let bits = buffer::numeric_to_bits(kind, &v);
    let m = mask(kind);
    let old =
        buffer::modify_value_in_buffer(agent, buf, byte_index, kind, |old| Some(op(old, bits) & m));
    let r = buffer::bits_to_numeric(kind, old);
    Ok(buffer::element_to_value(agent, r))
}

fn add(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    read_modify_write(agent, &call, u64::wrapping_add)
}

fn sub(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    read_modify_write(agent, &call, u64::wrapping_sub)
}

fn and(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    read_modify_write(agent, &call, |a, b| a & b)
}

fn or(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    read_modify_write(agent, &call, |a, b| a | b)
}

fn xor(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    read_modify_write(agent, &call, |a, b| a ^ b)
}

fn exchange(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    read_modify_write(agent, &call, |_, b| b)
}

fn compare_exchange(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let (o, kind) = validate_integer_typed_array(agent, call.arg(0), false)?;
    let byte_index = validate_atomic_access(agent, o, kind, call.arg(1))?;
    let expected = operand(agent, kind, call.arg(2))?;
    let replacement = operand(agent, kind, call.arg(3))?;
    let buf = revalidate(agent, o, byte_index)?;
    let expected = buffer::numeric_to_bits(kind, &expected);
    let replacement = buffer::numeric_to_bits(kind, &replacement);
    let old = buffer::modify_value_in_buffer(agent, buf, byte_index, kind, |old| {
        (old == expected).then_some(replacement)
    });
    let r = buffer::bits_to_numeric(kind, old);
    Ok(buffer::element_to_value(agent, r))
}

fn load(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let (o, kind) = validate_integer_typed_array(agent, call.arg(0), false)?;
    let byte_index = validate_atomic_access(agent, o, kind, call.arg(1))?;
    let buf = revalidate(agent, o, byte_index)?;
    Ok(buffer::get_value_from_buffer(
        agent, buf, byte_index, kind, true,
    ))
}

fn store(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let (o, kind) = validate_integer_typed_array(agent, call.arg(0), false)?;
    let byte_index = validate_atomic_access(agent, o, kind, call.arg(1))?;
    let v = operand(agent, kind, call.arg(2))?;
    let buf = revalidate(agent, o, byte_index)?;
    buffer::set_value_in_buffer(agent, buf, byte_index, kind, &v, true);
    Ok(buffer::element_to_value(agent, v))
}

fn is_lock_free(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let n = ops::to_integer_or_infinity(agent, call.arg(0))?;
    Ok(Value::Boolean(n == 1.0 || n == 2.0 || n == 4.0 || n == 8.0))
}

/// The most iterations `Atomics.pause` spins.
const MAX_PAUSE: f64 = 1024.0;

fn pause(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let n = call.arg(0);
    if !n.is_undefined() && !ops::is_integral_number(n) {
        return agent.type_error("Atomics.pause: the argument is not an integral Number");
    }
    let spins = match n {
        Value::Number(x) => x.clamp(0.0, MAX_PAUSE) as u32,
        _ => 1,
    };
    for _ in 0..spins.max(1) {
        std::hint::spin_loop();
    }
    Ok(Value::Undefined)
}

/// The validated arguments of DoWait.
struct WaitArgs {
    buffer: Object,
    byte_index: usize,
    kind: TypedArrayKind,
    expected: u64,
    timeout: f64,
}

/// Steps 1–7 of DoWait(mode, typedArray, index, value, timeout).
fn wait_args(agent: &mut Agent, call: &NativeCall<'_>) -> JsResult<WaitArgs> {
    let (o, kind) = validate_integer_typed_array(agent, call.arg(0), true)?;
    let buf = buffer::typed_array_slots(agent, o).map(|s| s.1);
    let Some(buf) = buf.filter(|b| buffer::is_shared(agent, *b)) else {
        return agent.type_error("Atomics.wait: the array does not view a SharedArrayBuffer");
    };
    let byte_index = validate_atomic_access(agent, o, kind, call.arg(1))?;
    let expected = if kind == TypedArrayKind::BigInt64 {
        ops::to_bigint64(agent, call.arg(2))? as u64
    } else {
        u64::from(ops::to_int32(agent, call.arg(2))? as u32)
    };
    let q = ops::to_number(agent, call.arg(3))?;
    let timeout = if q.is_nan() || q == f64::INFINITY {
        f64::INFINITY
    } else if q == f64::NEG_INFINITY {
        0.0
    } else {
        q.max(0.0)
    };
    Ok(WaitArgs {
        buffer: buf,
        byte_index,
        kind,
        expected,
        timeout,
    })
}

/// `Atomics.wait(typedArray, index, value, timeout)`.
fn wait(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let args = wait_args(agent, &call)?;
    if !agent.can_block {
        return agent.type_error("Atomics.wait: this agent cannot block");
    }
    let Some(block) = buffer::buffer_data(agent, args.buffer)
        .and_then(|d| d.shared_block())
        .cloned()
    else {
        return agent.type_error("Atomics.wait: not a SharedArrayBuffer");
    };
    let id = agent.async_waits.agent_id();
    let cs = waiter::enter_critical_section();
    let current = block.load(args.byte_index, args.kind.element_size());
    let result = if current != args.expected {
        drop(cs);
        "not-equal"
    } else if cs.suspend(&block, args.byte_index, id, args.timeout) {
        "ok"
    } else {
        "timed-out"
    };
    Ok(Value::String(agent.heap.alloc_str(result)))
}

/// `Atomics.waitAsync(typedArray, index, value, timeout)`.
fn wait_async(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let args = wait_args(agent, &call)?;
    let Some(block) = buffer::buffer_data(agent, args.buffer)
        .and_then(|d| d.shared_block())
        .cloned()
    else {
        return agent.type_error("Atomics.waitAsync: not a SharedArrayBuffer");
    };
    let realm = agent.current_realm();
    let capability = promise::new_intrinsic_capability(agent, realm);
    let proto = agent.intrinsic(Intrinsic::ObjectPrototype);
    let result = plain_object(agent, proto);
    let mut cs = waiter::enter_critical_section();
    let current = block.load(args.byte_index, args.kind.element_size());
    let (is_async, value) = if current != args.expected {
        drop(cs);
        (false, Value::String(agent.heap.alloc_str("not-equal")))
    } else if args.timeout == 0.0 {
        drop(cs);
        (false, Value::String(agent.heap.alloc_str("timed-out")))
    } else {
        waiter::add_async_waiter(
            agent,
            &mut cs,
            &block,
            args.byte_index,
            args.timeout,
            capability,
        );
        drop(cs);
        (true, capability.promise)
    };
    let key = agent.heap.key_from_str("async");
    ops::create_data_property_or_throw(agent, result, key, Value::Boolean(is_async))?;
    let key = agent.heap.key_from_str("value");
    ops::create_data_property_or_throw(agent, result, key, value)?;
    Ok(Value::Object(result))
}

/// `Atomics.notify(typedArray, index, count)`.
fn notify(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let (o, kind) = validate_integer_typed_array(agent, call.arg(0), true)?;
    let byte_index = validate_atomic_access(agent, o, kind, call.arg(1))?;
    let count = if call.arg(2).is_undefined() {
        f64::INFINITY
    } else {
        ops::to_integer_or_infinity(agent, call.arg(2))?.max(0.0)
    };
    let block = buffer::typed_array_slots(agent, o)
        .and_then(|s| buffer::buffer_data(agent, s.1))
        .and_then(|d| d.shared_block())
        .cloned();
    let Some(block) = block else {
        return Ok(Value::Number(0.0));
    };
    let mut cs = waiter::enter_critical_section();
    let n = cs.notify(&block, byte_index, count);
    drop(cs);
    Ok(Value::Number(n as f64))
}
