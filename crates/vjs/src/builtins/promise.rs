//! The rest of `Promise` (ECMA-262 §27.2) on top of the core in
//! `interp::promise`: the static methods `all`, `allSettled`, `any`,
//! `race`, `reject`, `resolve`, `try` and `withResolvers`, `get
//! [Symbol.species]`, and `Promise.prototype.catch` and `finally`.
//!
//! The state shared by the element functions of `all`, `allSettled` and
//! `any` lives in their slots, so it is traced while their jobs wait:
//! `[[Values]]`/`[[Errors]]` is an ordinary object without prototype keyed
//! by index, and `[[RemainingElements]]` and `[[AlreadyCalled]]` are
//! `Number` and `Boolean` cells.

use crate::agent::Agent;
use crate::builtins::{intrinsic, method, species_getter};
use crate::completion::{JsResult, Throw};
use crate::function::{NativeCall, create_native_function, native_slots};
use crate::gc::{Object, Realm};
use crate::interp::promise::{self, Capability};
use crate::object::{ObjectKind, PropertyDescriptor};
use crate::ops::{self, IteratorRecord};
use crate::realm::Intrinsic;
use crate::value::{PropertyKey, Value};

pub(crate) fn install(agent: &mut Agent, realm: Realm) {
    let ctor = intrinsic(agent, realm, Intrinsic::Promise);
    let proto = intrinsic(agent, realm, Intrinsic::PromisePrototype);
    method(agent, realm, ctor, "all", all, 1);
    method(agent, realm, ctor, "allSettled", all_settled, 1);
    method(agent, realm, ctor, "any", any, 1);
    method(agent, realm, ctor, "race", race, 1);
    method(agent, realm, ctor, "reject", reject, 1);
    method(agent, realm, ctor, "resolve", resolve, 1);
    method(agent, realm, ctor, "try", try_, 1);
    method(agent, realm, ctor, "withResolvers", with_resolvers, 0);
    species_getter(agent, realm, ctor);
    method(agent, realm, proto, "catch", catch, 1);
    method(agent, realm, proto, "finally", finally, 1);
}

/// IfAbruptRejectPromise(value, capability): the promise of the
/// capability, rejected with the thrown value.
fn reject_capability(agent: &mut Agent, cap: &Capability, e: Value) -> JsResult<Value> {
    ops::call(agent, cap.reject, Value::Undefined, &[e])?;
    Ok(cap.promise)
}

/// GetPromiseResolve(promiseConstructor).
fn get_promise_resolve(agent: &mut Agent, c: Value) -> JsResult<Value> {
    let key = agent.heap.key_from_str("resolve");
    let resolve = ops::get_v(agent, c, key)?;
    if !ops::is_callable(agent, resolve) {
        return agent.type_error("the promise constructor's 'resolve' is not callable");
    }
    Ok(resolve)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Combinator {
    All,
    AllSettled,
    Any,
    Race,
}

/// The common steps of `Promise.all`, `allSettled`, `any` and `race`:
/// capability, GetPromiseResolve, GetIterator, the loop, and closing the
/// iterator on an abrupt completion that did not come from it.
fn combinator(agent: &mut Agent, call: &NativeCall<'_>, kind: Combinator) -> JsResult<Value> {
    let c = call.this;
    let cap = promise::new_promise_capability(agent, c)?;
    let promise_resolve = match get_promise_resolve(agent, c) {
        Ok(r) => r,
        Err(Throw(e)) => return reject_capability(agent, &cap, e),
    };
    let mut record = match ops::get_iterator(agent, call.arg(0)) {
        Ok(r) => r,
        Err(Throw(e)) => return reject_capability(agent, &cap, e),
    };
    let result = match kind {
        Combinator::Race => perform_race(agent, &mut record, c, &cap, promise_resolve),
        _ => perform_all(agent, &mut record, c, &cap, promise_resolve, kind),
    };
    match result {
        Ok(v) => Ok(v),
        Err(Throw(e)) => {
            let e = if record.done {
                e
            } else {
                match ops::iterator_close::<()>(agent, &record, Err(Throw(e))) {
                    Err(Throw(x)) => x,
                    Ok(()) => e,
                }
            };
            reject_capability(agent, &cap, e)
        }
    }
}

/// A mutable cell kept in a slot.
fn number_cell(agent: &mut Agent, n: f64) -> Object {
    agent.heap.create_object(None, ObjectKind::Number(n))
}

/// Adds `delta` to a `Number` cell and returns the new count.
fn bump(agent: &mut Agent, cell: Value, delta: f64) -> f64 {
    let Value::Object(o) = cell else {
        return 0.0;
    };
    match &mut agent.heap.object_mut(o).kind {
        ObjectKind::Number(n) => {
            *n += delta;
            *n
        }
        _ => 0.0,
    }
}

/// Sets a `Boolean` cell and returns its previous value.
fn take_flag(agent: &mut Agent, cell: Value) -> bool {
    let Value::Object(o) = cell else {
        return true;
    };
    match &mut agent.heap.object_mut(o).kind {
        ObjectKind::Boolean(b) => std::mem::replace(b, true),
        _ => true,
    }
}

/// The values collected so far, as a fresh array.
fn list_to_array(agent: &mut Agent, list: Value, len: u64) -> JsResult<Object> {
    let Value::Object(o) = list else {
        return Ok(ops::create_array_from_list(agent, &[]));
    };
    let mut values = Vec::with_capacity(len as usize);
    for i in 0..len {
        let key = ops::index_key(agent, i);
        values.push(ops::get(agent, o, key)?);
    }
    Ok(ops::create_array_from_list(agent, &values))
}

fn list_set(agent: &mut Agent, list: Value, index: u64, v: Value) {
    if let Value::Object(o) = list {
        let key = ops::index_key(agent, index);
        // An extensible ordinary object without prototype accepts every key.
        let _ = ops::create_data_property(agent, o, key, v);
    }
}

/// `Promise.any` rejects with an AggregateError whose `errors` are the
/// collected reasons.
fn aggregate_error(agent: &mut Agent, errors: Object) -> JsResult<Value> {
    let proto = agent.intrinsic(Intrinsic::AggregateErrorPrototype);
    let e = agent.heap.create_object(Some(proto), ObjectKind::Error);
    let key = PropertyKey::String(agent.heap.names.errors);
    ops::define_property_or_throw(
        agent,
        e,
        key,
        PropertyDescriptor::data(Value::Object(errors), true, false, true),
    )?;
    Ok(Value::Object(e))
}

/// Settles the combined promise once no element is left.
fn finish(
    agent: &mut Agent,
    kind: Combinator,
    list: Value,
    len: u64,
    settle: Value,
) -> JsResult<Value> {
    let array = list_to_array(agent, list, len)?;
    if kind == Combinator::Any {
        let e = aggregate_error(agent, array)?;
        return Err(Throw(e));
    }
    ops::call(agent, settle, Value::Undefined, &[Value::Object(array)])
}

// Slots of the element functions.
const EL_KIND: usize = 0;
const EL_ALREADY: usize = 1;
const EL_INDEX: usize = 2;
const EL_LIST: usize = 3;
const EL_SETTLE: usize = 4;
const EL_REMAINING: usize = 5;
const EL_LEN: usize = 6;

fn kind_code(kind: Combinator, rejected: bool) -> f64 {
    match (kind, rejected) {
        (Combinator::AllSettled, false) => 1.0,
        (Combinator::AllSettled, true) => 2.0,
        (Combinator::Any, _) => 3.0,
        _ => 0.0,
    }
}

/// PerformPromiseAll, PerformPromiseAllSettled and PerformPromiseAny.
fn perform_all(
    agent: &mut Agent,
    record: &mut IteratorRecord,
    c: Value,
    cap: &Capability,
    promise_resolve: Value,
    kind: Combinator,
) -> JsResult<Value> {
    let list = Value::Object(agent.heap.create_object(None, ObjectKind::Ordinary));
    let remaining = Value::Object(number_cell(agent, 1.0));
    // The number of elements, read by the element functions when the
    // count reaches zero.
    let len_cell = Value::Object(number_cell(agent, 0.0));
    // `all` and `allSettled` resolve; `any` rejects.
    let settle = if kind == Combinator::Any {
        cap.reject
    } else {
        cap.resolve
    };
    let mut index: u64 = 0;
    loop {
        let Some(next) = ops::iterator_step_value(agent, record)? else {
            if bump(agent, remaining, -1.0) == 0.0 {
                finish(agent, kind, list, index, settle)?;
            }
            return Ok(cap.promise);
        };
        list_set(agent, list, index, Value::Undefined);
        bump(agent, len_cell, 1.0);
        let next_promise = ops::call(agent, promise_resolve, c, &[next])?;
        let already = Value::Object(agent.heap.create_object(None, ObjectKind::Boolean(false)));
        let make = |agent: &mut Agent, rejected: bool| {
            let slots = vec![
                Value::Number(kind_code(kind, rejected)),
                already,
                Value::Number(index as f64),
                list,
                settle,
                remaining,
                len_cell,
            ];
            Value::Object(create_native_function(
                agent,
                element_function,
                1,
                "",
                slots,
                None,
            ))
        };
        let (on_fulfilled, on_rejected) = match kind {
            Combinator::AllSettled => (make(agent, false), make(agent, true)),
            Combinator::Any => (cap.resolve, make(agent, true)),
            _ => (make(agent, false), cap.reject),
        };
        bump(agent, remaining, 1.0);
        let then = PropertyKey::String(agent.heap.names.then);
        ops::invoke(agent, next_promise, then, &[on_fulfilled, on_rejected])?;
        index += 1;
    }
}

/// `Promise.all` Resolve Element Functions, `Promise.allSettled` Resolve
/// and Reject Element Functions, and `Promise.any` Reject Element
/// Functions.
fn element_function(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let slots = native_slots(agent, call.callee).to_vec();
    if take_flag(agent, slots[EL_ALREADY]) {
        return Ok(Value::Undefined);
    }
    let code = slots[EL_KIND].as_number().unwrap_or(0.0);
    let index = slots[EL_INDEX].as_number().unwrap_or(0.0) as u64;
    let x = call.arg(0);
    let (kind, stored) = if code == 1.0 || code == 2.0 {
        let proto = agent.intrinsic(Intrinsic::ObjectPrototype);
        let o = agent.heap.create_object(Some(proto), ObjectKind::Ordinary);
        let (status, field) = if code == 1.0 {
            ("fulfilled", "value")
        } else {
            ("rejected", "reason")
        };
        let key = agent.heap.key_from_str("status");
        let status = Value::String(agent.heap.alloc_str(status));
        ops::create_data_property_or_throw(agent, o, key, status)?;
        let key = agent.heap.key_from_str(field);
        ops::create_data_property_or_throw(agent, o, key, x)?;
        (Combinator::AllSettled, Value::Object(o))
    } else if code == 3.0 {
        (Combinator::Any, x)
    } else {
        (Combinator::All, x)
    };
    list_set(agent, slots[EL_LIST], index, stored);
    if bump(agent, slots[EL_REMAINING], -1.0) == 0.0 {
        let len = bump(agent, slots[EL_LEN], 0.0) as u64;
        return match finish(agent, kind, slots[EL_LIST], len, slots[EL_SETTLE]) {
            Err(Throw(e)) if kind == Combinator::Any => {
                ops::call(agent, slots[EL_SETTLE], Value::Undefined, &[e])
            }
            r => r,
        };
    }
    Ok(Value::Undefined)
}

/// PerformPromiseRace.
fn perform_race(
    agent: &mut Agent,
    record: &mut IteratorRecord,
    c: Value,
    cap: &Capability,
    promise_resolve: Value,
) -> JsResult<Value> {
    loop {
        let Some(next) = ops::iterator_step_value(agent, record)? else {
            return Ok(cap.promise);
        };
        let next_promise = ops::call(agent, promise_resolve, c, &[next])?;
        let then = PropertyKey::String(agent.heap.names.then);
        ops::invoke(agent, next_promise, then, &[cap.resolve, cap.reject])?;
    }
}

/// `Promise.all(iterable)`.
fn all(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    combinator(agent, &call, Combinator::All)
}

/// `Promise.allSettled(iterable)`.
fn all_settled(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    combinator(agent, &call, Combinator::AllSettled)
}

/// `Promise.any(iterable)`.
fn any(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    combinator(agent, &call, Combinator::Any)
}

/// `Promise.race(iterable)`.
fn race(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    combinator(agent, &call, Combinator::Race)
}

/// `Promise.reject(r)`.
fn reject(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let cap = promise::new_promise_capability(agent, call.this)?;
    ops::call(agent, cap.reject, Value::Undefined, &[call.arg(0)])?;
    Ok(cap.promise)
}

/// `Promise.resolve(x)`.
fn resolve(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let Value::Object(c) = call.this else {
        return agent.type_error("Promise.resolve called on a non-object");
    };
    promise::promise_resolve(agent, c, call.arg(0))
}

/// `Promise.try(callbackfn, ...args)`. A normal result goes through
/// PromiseResolve, so a promise of the receiver comes back unwrapped.
fn try_(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let Value::Object(c) = call.this else {
        return agent.type_error("Promise.try called on a non-object");
    };
    let args = call.args.get(1..).unwrap_or(&[]);
    match ops::call(agent, call.arg(0), Value::Undefined, args) {
        Ok(v) => promise::promise_resolve(agent, c, v),
        Err(Throw(e)) => {
            let cap = promise::new_promise_capability(agent, call.this)?;
            ops::call(agent, cap.reject, Value::Undefined, &[e])?;
            Ok(cap.promise)
        }
    }
}

/// `Promise.withResolvers()`.
fn with_resolvers(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let cap = promise::new_promise_capability(agent, call.this)?;
    let proto = agent.intrinsic(Intrinsic::ObjectPrototype);
    let o = agent.heap.create_object(Some(proto), ObjectKind::Ordinary);
    for (name, v) in [
        ("promise", cap.promise),
        ("resolve", cap.resolve),
        ("reject", cap.reject),
    ] {
        let key = agent.heap.key_from_str(name);
        ops::create_data_property_or_throw(agent, o, key, v)?;
    }
    Ok(Value::Object(o))
}

/// `Promise.prototype.catch(onRejected)`.
fn catch(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let then = PropertyKey::String(agent.heap.names.then);
    ops::invoke(agent, call.this, then, &[Value::Undefined, call.arg(0)])
}

/// `Promise.prototype.finally(onFinally)`.
fn finally(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let Value::Object(p) = call.this else {
        return agent.type_error("Promise.prototype.finally called on a non-object");
    };
    let default = agent.intrinsic(Intrinsic::Promise);
    let c = ops::species_constructor(agent, p, default)?;
    let on_finally = call.arg(0);
    let (then_finally, catch_finally) = if ops::is_callable(agent, on_finally) {
        let slots = vec![on_finally, Value::Object(c)];
        let t = create_native_function(agent, then_finally_function, 1, "", slots.clone(), None);
        let k = create_native_function(agent, catch_finally_function, 1, "", slots, None);
        (Value::Object(t), Value::Object(k))
    } else {
        (on_finally, on_finally)
    };
    let then = PropertyKey::String(agent.heap.names.then);
    ops::invoke(agent, call.this, then, &[then_finally, catch_finally])
}

/// The common part of the Then Finally and Catch Finally functions: calls
/// `onFinally`, resolves its result with `C` and chains `thunk`.
fn finally_step(
    agent: &mut Agent,
    callee: Object,
    thunk: fn(&mut Agent, NativeCall<'_>) -> JsResult<Value>,
    value: Value,
) -> JsResult<Value> {
    let slots = native_slots(agent, callee).to_vec();
    let (on_finally, c) = (slots[0], slots[1]);
    let result = ops::call(agent, on_finally, Value::Undefined, &[])?;
    let Value::Object(c) = c else {
        return agent.type_error("Promise.prototype.finally: no constructor");
    };
    let p = promise::promise_resolve(agent, c, result)?;
    let f = create_native_function(agent, thunk, 0, "", vec![value], None);
    let then = PropertyKey::String(agent.heap.names.then);
    ops::invoke(agent, p, then, &[Value::Object(f)])
}

fn then_finally_function(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    finally_step(agent, call.callee, return_value, call.arg(0))
}

fn catch_finally_function(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    finally_step(agent, call.callee, throw_reason, call.arg(0))
}

fn return_value(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    Ok(native_slots(agent, call.callee)[0])
}

fn throw_reason(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    Err(Throw(native_slots(agent, call.callee)[0]))
}

#[cfg(test)]
mod tests {
    use crate::agent::Agent;
    use crate::interp::evaluate_script;
    use crate::value::Value;

    fn run(src: &str) -> String {
        let mut agent = Agent::new();
        agent.initialize_host_defined_realm();
        evaluate_script(&mut agent, src).expect("script");
        assert!(agent.run_jobs().is_empty(), "jobs threw");
        let v = evaluate_script(&mut agent, "log").expect("log");
        match v {
            Value::String(s) => agent.heap.string_to_rust(s),
            _ => String::new(),
        }
    }

    #[test]
    fn statics_settle_in_order() {
        assert_eq!(
            run(
                "var log = ''; Promise.all([1, Promise.resolve(2)]).then(function (v) { log += v.join(); });"
            ),
            "1,2"
        );
        assert_eq!(
            run(
                "var log = ''; Promise.any([Promise.reject(1)]).catch(function (e) { log += e.constructor.name + e.errors; });"
            ),
            "AggregateError1"
        );
        assert_eq!(
            run(
                "var log = ''; Promise.race([new Promise(function () {}), 3]).then(function (v) { log += v; });"
            ),
            "3"
        );
    }
}
