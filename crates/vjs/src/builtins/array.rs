//! `Array` (ECMA-262 §23.1): the constructor with `Array.from`,
//! `Array.of`, `Array.isArray`, `Array.fromAsync` and `@@species`, and
//! every method of `%Array.prototype%` (`values` and `@@iterator` are
//! installed with the array iterator by the interpreter's intrinsics) with
//! `@@unscopables`.
//!
//! The methods follow the generic algorithms of the specification on any
//! object. Where a step only asks `HasProperty` of an index, the loop
//! skips indices that no object of the prototype chain stores when every
//! object of the chain is ordinary (then skipping is not observable), so
//! sparse arrays with a huge `length` do not visit every index.

use crate::agent::{Agent, ErrorKind};
use crate::builtins::{constructor, intrinsic, method, set_intrinsic, species_getter, value};
use crate::completion::{JsResult, Throw};
use crate::function::{NativeCall, NativeFn, create_native_function, native_slots};
use crate::gc::{Object, Realm, Trace, Tracer};
use crate::internal;
use crate::interp::iter::{ArrayIterKind, create_array_iterator, create_async_from_sync_iterator};
use crate::interp::promise::{self, Capability};
use crate::object::ObjectKind;
use crate::ops::{self, IteratorRecord};
use crate::realm::Intrinsic;
use crate::string::MAX_STRING_LENGTH;
use crate::value::{PropertyKey, Value};

const MAX_SAFE_INTEGER: f64 = 9007199254740991.0;

/// 2^32 - 1: the first integer that is not an array index.
const INDEX_LIMIT: f64 = 4294967295.0;

pub(crate) fn install(agent: &mut Agent, realm: Realm) {
    let proto = intrinsic(agent, realm, Intrinsic::ArrayPrototype);
    let ctor = constructor(agent, realm, "Array", array_constructor, 1, proto, None);
    set_intrinsic(agent, realm, Intrinsic::Array, ctor);
    method(agent, realm, ctor, "from", array_from, 1);
    method(agent, realm, ctor, "fromAsync", array_from_async, 1);
    method(agent, realm, ctor, "isArray", array_is_array, 1);
    method(agent, realm, ctor, "of", array_of, 0);
    species_getter(agent, realm, ctor);

    let methods: &[(&str, NativeFn, u32)] = &[
        ("at", at, 1),
        ("concat", concat, 1),
        ("copyWithin", copy_within, 2),
        ("entries", entries, 0),
        ("every", every, 1),
        ("fill", fill, 1),
        ("filter", filter, 1),
        ("find", find, 1),
        ("findIndex", find_index, 1),
        ("findLast", find_last, 1),
        ("findLastIndex", find_last_index, 1),
        ("flat", flat, 0),
        ("flatMap", flat_map, 1),
        ("forEach", for_each, 1),
        ("includes", includes, 1),
        ("indexOf", index_of, 1),
        ("join", join, 1),
        ("keys", keys, 0),
        ("lastIndexOf", last_index_of, 1),
        ("map", map, 1),
        ("pop", pop, 0),
        ("push", push, 1),
        ("reduce", reduce, 1),
        ("reduceRight", reduce_right, 1),
        ("reverse", reverse, 0),
        ("shift", shift, 0),
        ("slice", slice, 2),
        ("some", some, 1),
        ("sort", sort, 1),
        ("splice", splice, 2),
        ("toLocaleString", to_locale_string, 0),
        ("toReversed", to_reversed, 0),
        ("toSorted", to_sorted, 1),
        ("toSpliced", to_spliced, 2),
        ("unshift", unshift, 1),
        ("with", with, 2),
    ];
    for &(name, f, length) in methods {
        method(agent, realm, proto, name, f, length);
    }
    // `toString` falls back to %Object.prototype.toString%, kept in its
    // slot.
    let object_proto = intrinsic(agent, realm, Intrinsic::ObjectPrototype);
    let key = PropertyKey::String(agent.heap.names.to_string);
    let object_to_string = match agent.heap.get_own(object_proto, key) {
        Some((_, crate::object::Slot::Data(v))) => v,
        _ => Value::Undefined,
    };
    let f = method(agent, realm, proto, "toString", to_string, 0);
    if let Some(slots) = crate::function::native_slots_mut(agent, f) {
        slots.push(object_to_string);
    }

    // %Array.prototype%[@@unscopables].
    let unscopables = agent.heap.create_object(None, ObjectKind::Ordinary);
    for name in [
        "at",
        "copyWithin",
        "entries",
        "fill",
        "find",
        "findIndex",
        "findLast",
        "findLastIndex",
        "flat",
        "flatMap",
        "includes",
        "keys",
        "toReversed",
        "toSorted",
        "toSpliced",
        "values",
    ] {
        let key = agent.heap.key_from_str(name);
        value(
            agent,
            unscopables,
            key,
            Value::Boolean(true),
            true,
            true,
            true,
        );
    }
    let key = PropertyKey::Symbol(agent.symbols.unscopables);
    value(
        agent,
        proto,
        key,
        Value::Object(unscopables),
        false,
        false,
        true,
    );
}

// ----- helpers -----

fn key(agent: &mut Agent, i: f64) -> PropertyKey {
    ops::index_key(agent, i as u64)
}

fn length_key(agent: &Agent) -> PropertyKey {
    PropertyKey::String(agent.heap.names.length)
}

/// `O = ? ToObject(this)` and `len = ? LengthOfArrayLike(O)`.
fn this_and_length(agent: &mut Agent, this: Value) -> JsResult<(Object, f64)> {
    let o = ops::to_object(agent, this)?;
    let len = ops::length_of_array_like(agent, o)?;
    Ok((o, len))
}

fn set_length(agent: &mut Agent, o: Object, len: f64) -> JsResult<()> {
    let key = length_key(agent);
    ops::set(agent, o, key, Value::Number(len), true)
}

/// A relative index (`start`, `end`, `target`) resolved against `len`;
/// `undefined` gives `default`.
fn relative(agent: &mut Agent, v: Value, len: f64, default: Option<f64>) -> JsResult<f64> {
    if let (Value::Undefined, Some(d)) = (v, default) {
        return Ok(d);
    }
    let rel = ops::to_integer_or_infinity(agent, v)?;
    Ok(if rel == f64::NEG_INFINITY {
        0.0
    } else if rel < 0.0 {
        (len + rel).max(0.0)
    } else {
        rel.min(len)
    })
}

fn callable(agent: &mut Agent, f: Value, name: &str) -> JsResult<Value> {
    if ops::is_callable(agent, f) {
        Ok(f)
    } else {
        agent.type_error(&format!(
            "Array.prototype.{name}: the callback is not a function"
        ))
    }
}

/// Whether every object of the prototype chain of `o` has ordinary
/// `[[HasProperty]]`, `[[Get]]` and `[[GetOwnProperty]]`, so that absent
/// indices can be skipped unobservably.
fn plain_chain(agent: &Agent, o: Object) -> bool {
    let mut cur = Some(o);
    while let Some(c) = cur {
        if !internal::is_plain(agent, c) {
            return false;
        }
        cur = agent.heap.object(c).proto();
    }
    true
}

/// The lowest index in `k..len` that some object of the chain of `o` may
/// have (`len` if none); `k` itself when skipping is not safe.
fn next_present(agent: &Agent, o: Object, k: f64, len: f64) -> f64 {
    if k >= len || k >= INDEX_LIMIT || !plain_chain(agent, o) {
        return k;
    }
    let end = len.min(INDEX_LIMIT) as u32;
    let mut best: Option<u32> = None;
    let mut cur = Some(o);
    while let Some(c) = cur {
        let to = best.unwrap_or(end);
        if let Some(i) = agent.heap.next_element_index(c, k as u32, to) {
            best = Some(i);
        }
        cur = agent.heap.object(c).proto();
    }
    match best {
        Some(i) => f64::from(i),
        None => len.min(INDEX_LIMIT),
    }
}

/// The highest index at most `k` that some object of the chain of `o`
/// may have (`-1` if none); `k` itself when skipping is not safe.
fn prev_present(agent: &Agent, o: Object, k: f64) -> f64 {
    if k < 0.0 || k >= INDEX_LIMIT || !plain_chain(agent, o) {
        return k;
    }
    let mut best: Option<u32> = None;
    let mut cur = Some(o);
    while let Some(c) = cur {
        if let Some(i) = agent.heap.prev_element_index(c, k as u32) {
            best = Some(best.map_or(i, |b| b.max(i)));
        }
        cur = agent.heap.object(c).proto();
    }
    best.map_or(-1.0, f64::from)
}

/// ArraySpeciesCreate(originalArray, length).
fn array_species_create(agent: &mut Agent, original: Object, length: f64) -> JsResult<Object> {
    if !ops::is_array(agent, Value::Object(original))? {
        return internal::array_create(agent, length, None);
    }
    let ckey = PropertyKey::String(agent.heap.names.constructor);
    let mut c = ops::get(agent, original, ckey)?;
    if let Value::Object(co) = c
        && agent.heap.object(co).is_constructor()
    {
        let this_realm = agent.current_realm();
        let realm_c = ops::get_function_realm(agent, co)?;
        if this_realm != realm_c && agent.heap.realm(realm_c).intrinsics.get(Intrinsic::Array) == co
        {
            c = Value::Undefined;
        }
    }
    if let Value::Object(co) = c {
        let species = PropertyKey::Symbol(agent.symbols.species);
        c = ops::get(agent, co, species)?;
        if c.is_null() {
            c = Value::Undefined;
        }
    }
    match c {
        Value::Undefined => internal::array_create(agent, length, None),
        Value::Object(co) if agent.heap.object(co).is_constructor() => {
            ops::construct(agent, co, &[Value::Number(length)], None)
        }
        _ => agent.type_error("the species of the array is not a constructor"),
    }
}

/// `IsConstructor(C) ? Construct(C, args) : ArrayCreate(len)`.
fn construct_or_array(agent: &mut Agent, c: Value, len: Option<f64>) -> JsResult<Object> {
    match c {
        Value::Object(co) if agent.heap.object(co).is_constructor() => {
            let args: Vec<Value> = len.map(Value::Number).into_iter().collect();
            ops::construct(agent, co, &args, None)
        }
        _ => internal::array_create(agent, len.unwrap_or(0.0), None),
    }
}

// ----- the constructor -----

/// `Array(...values)` (§23.1.1.1).
fn array_constructor(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let new_target = call.new_target.unwrap_or(call.callee);
    let proto = ops::get_prototype_from_constructor(agent, new_target, Intrinsic::ArrayPrototype)?;
    match call.args.len() {
        0 => Ok(Value::Object(internal::array_create(
            agent,
            0.0,
            Some(proto),
        )?)),
        1 => {
            let len = call.arg(0);
            let array = internal::array_create(agent, 0.0, Some(proto))?;
            let int_len = match len {
                Value::Number(n) => {
                    let int_len = f64::from(ops::to_uint32(agent, len)?);
                    if int_len != n {
                        return agent.range_error("invalid array length");
                    }
                    int_len
                }
                _ => {
                    ops::create_data_property_or_throw(agent, array, PropertyKey::Index(0), len)?;
                    1.0
                }
            };
            set_length(agent, array, int_len)?;
            Ok(Value::Object(array))
        }
        n => {
            let array = internal::array_create(agent, n as f64, Some(proto))?;
            for (i, v) in call.args.iter().enumerate() {
                ops::create_data_property_or_throw(agent, array, PropertyKey::Index(i as u32), *v)?;
            }
            Ok(Value::Object(array))
        }
    }
}

/// `Array.from(items[, mapfn[, thisArg]])` (§23.1.2.1).
fn array_from(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let c = call.this;
    let (items, mapfn, this_arg) = (call.arg(0), call.arg(1), call.arg(2));
    let mapping = match mapfn {
        Value::Undefined => false,
        f if ops::is_callable(agent, f) => true,
        _ => return agent.type_error("Array.from: the mapper is not a function"),
    };
    let iterator_key = PropertyKey::Symbol(agent.symbols.iterator);
    if let Some(using_iterator) = ops::get_method(agent, items, iterator_key)? {
        let a = construct_or_array(agent, c, None)?;
        let mut record = ops::get_iterator_from_method(agent, items, using_iterator)?;
        let mut k = 0.0;
        loop {
            if k >= MAX_SAFE_INTEGER {
                let error = agent.type_error("Array.from: too many elements");
                return ops::iterator_close(agent, &record, error);
            }
            let pk = key(agent, k);
            let Some(next) = ops::iterator_step_value(agent, &mut record)? else {
                set_length(agent, a, k)?;
                return Ok(Value::Object(a));
            };
            let mapped = if mapping {
                let r = ops::call(agent, mapfn, this_arg, &[next, Value::Number(k)]);
                ops::if_abrupt_close_iterator(agent, &record, r)?
            } else {
                next
            };
            let status = ops::create_data_property_or_throw(agent, a, pk, mapped);
            ops::if_abrupt_close_iterator(agent, &record, status)?;
            k += 1.0;
        }
    }
    let array_like = ops::to_object(agent, items)?;
    let len = ops::length_of_array_like(agent, array_like)?;
    let a = construct_or_array(agent, c, Some(len))?;
    let mut k = 0.0;
    while k < len {
        let pk = key(agent, k);
        let k_value = ops::get(agent, array_like, pk)?;
        let mapped = if mapping {
            ops::call(agent, mapfn, this_arg, &[k_value, Value::Number(k)])?
        } else {
            k_value
        };
        ops::create_data_property_or_throw(agent, a, pk, mapped)?;
        k += 1.0;
    }
    set_length(agent, a, len)?;
    Ok(Value::Object(a))
}

fn array_is_array(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    Ok(Value::Boolean(ops::is_array(agent, call.arg(0))?))
}

/// `Array.of(...items)` (§23.1.2.3).
fn array_of(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let len = call.args.len() as f64;
    let a = construct_or_array(agent, call.this, Some(len))?;
    for (k, v) in call.args.iter().enumerate() {
        let pk = key(agent, k as f64);
        ops::create_data_property_or_throw(agent, a, pk, *v)?;
    }
    set_length(agent, a, len)?;
    Ok(Value::Object(a))
}

// ----- Array.fromAsync -----

enum FromAsyncSource {
    Iterator(IteratorRecord),
    ArrayLike { object: Object, len: f64 },
}

/// Where the closure of `Array.fromAsync` is suspended.
#[derive(Clone, Copy, PartialEq)]
enum FromAsyncStage {
    Start,
    /// Awaiting the result of `next()`.
    AwaitNext,
    /// Awaiting the mapped value of an iterator value.
    AwaitMapped,
    /// Awaiting an element of the array-like.
    AwaitValue,
    /// Awaiting the mapped element of the array-like.
    AwaitMappedElement,
    /// Awaiting `return()` of AsyncIteratorClose; then rethrows the value.
    AwaitClose(Value),
    Done,
}

/// The variables of the closure of `Array.fromAsync`.
pub struct FromAsyncData {
    capability: Capability,
    c: Value,
    items: Value,
    mapper: Value,
    this_arg: Value,
    source: Option<FromAsyncSource>,
    target: Option<Object>,
    k: f64,
    stage: FromAsyncStage,
}

impl Trace for FromAsyncData {
    fn trace(&self, tracer: &mut Tracer) {
        self.capability.trace(tracer);
        for v in [self.c, self.items, self.mapper, self.this_arg] {
            tracer.value(v);
        }
        match &self.source {
            Some(FromAsyncSource::Iterator(r)) => r.trace(tracer),
            Some(FromAsyncSource::ArrayLike { object, .. }) => {
                tracer.object(*object);
            }
            None => {}
        }
        if let Some(t) = self.target {
            tracer.object(t);
        }
        if let FromAsyncStage::AwaitClose(v) = self.stage {
            tracer.value(v);
        }
    }
}

/// What the closure does next.
enum Flow {
    Await(Value),
    Return(Value),
}

/// `Array.fromAsync(asyncItems[, mapper[, thisArg]])`.
fn array_from_async(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let realm = agent.current_realm();
    let capability = promise::new_intrinsic_capability(agent, realm);
    let state = agent.heap.create_object(
        None,
        ObjectKind::FromAsync(Box::new(FromAsyncData {
            capability,
            c: call.this,
            items: call.arg(0),
            mapper: call.arg(1),
            this_arg: call.arg(2),
            source: None,
            target: None,
            k: 0.0,
            stage: FromAsyncStage::Start,
        })),
    );
    let flow = resume(agent, state, Ok(Value::Undefined));
    drive(agent, state, flow)?;
    Ok(capability.promise)
}

fn from_async(agent: &mut Agent, state: Object) -> &mut FromAsyncData {
    match &mut agent.heap.object_mut(state).kind {
        ObjectKind::FromAsync(d) => d,
        _ => unreachable!("Array.fromAsync state object"),
    }
}

/// Runs the closure from its suspension point until the next `Await` or
/// its end, then settles the promise or subscribes to the awaited value.
fn drive(agent: &mut Agent, state: Object, mut flow: JsResult<Flow>) -> JsResult<()> {
    loop {
        let capability = from_async(agent, state).capability;
        match flow {
            Ok(Flow::Return(v)) => {
                from_async(agent, state).stage = FromAsyncStage::Done;
                ops::call(agent, capability.resolve, Value::Undefined, &[v])?;
                return Ok(());
            }
            Err(Throw(e)) => {
                from_async(agent, state).stage = FromAsyncStage::Done;
                ops::call(agent, capability.reject, Value::Undefined, &[e])?;
                return Ok(());
            }
            Ok(Flow::Await(v)) => {
                let ctor = agent.intrinsic(Intrinsic::Promise);
                match promise::promise_resolve(agent, ctor, v) {
                    Ok(Value::Object(p)) => {
                        let slots = vec![Value::Object(state)];
                        let on_fulfilled = create_native_function(
                            agent,
                            await_fulfilled,
                            1,
                            "",
                            slots.clone(),
                            None,
                        );
                        let on_rejected =
                            create_native_function(agent, await_rejected, 1, "", slots, None);
                        promise::perform_promise_then(
                            agent,
                            p,
                            Value::Object(on_fulfilled),
                            Value::Object(on_rejected),
                            None,
                        );
                        return Ok(());
                    }
                    Ok(v) => flow = resume(agent, state, Ok(v)),
                    Err(e) => flow = resume(agent, state, Err(e)),
                }
            }
        }
    }
}

fn await_fulfilled(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let Value::Object(state) = native_slots(agent, call.callee)[0] else {
        return Ok(Value::Undefined);
    };
    let flow = resume(agent, state, Ok(call.arg(0)));
    drive(agent, state, flow)?;
    Ok(Value::Undefined)
}

fn await_rejected(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let Value::Object(state) = native_slots(agent, call.callee)[0] else {
        return Ok(Value::Undefined);
    };
    let flow = resume(agent, state, Err(Throw(call.arg(0))));
    drive(agent, state, flow)?;
    Ok(Value::Undefined)
}

/// AsyncIteratorClose(iteratorRecord, throwCompletion): awaits `return()`
/// if there is one, then rethrows `error`.
fn async_close(
    agent: &mut Agent,
    state: Object,
    record: IteratorRecord,
    error: Throw,
) -> JsResult<Flow> {
    let iterator = Value::Object(record.iterator);
    let key = PropertyKey::String(agent.heap.names.return_);
    let ret = match ops::get_method(agent, iterator, key) {
        Ok(Some(r)) => r,
        Ok(None) | Err(_) => return Err(error),
    };
    match ops::call(agent, ret, iterator, &[]) {
        Ok(result) => {
            from_async(agent, state).stage = FromAsyncStage::AwaitClose(error.0);
            Ok(Flow::Await(result))
        }
        Err(_) => Err(error),
    }
}

/// Resumes the closure of `Array.fromAsync` with the completion of the
/// `Await` it is suspended at.
fn resume(agent: &mut Agent, state: Object, input: JsResult<Value>) -> JsResult<Flow> {
    let d = from_async(agent, state);
    let stage = d.stage;
    let mapper = d.mapper;
    let this_arg = d.this_arg;
    let mapping = !mapper.is_undefined();
    match stage {
        FromAsyncStage::Done => Ok(Flow::Return(Value::Undefined)),
        FromAsyncStage::Start => {
            let (c, items) = (d.c, d.items);
            if mapping && !ops::is_callable(agent, mapper) {
                return agent.type_error("Array.fromAsync: the mapper is not a function");
            }
            let key = PropertyKey::Symbol(agent.symbols.async_iterator);
            let using_async = ops::get_method(agent, items, key)?;
            let using_sync = match using_async {
                Some(_) => None,
                None => {
                    let key = PropertyKey::Symbol(agent.symbols.iterator);
                    ops::get_method(agent, items, key)?
                }
            };
            let record = match (using_async, using_sync) {
                (Some(m), _) => Some(ops::get_iterator_from_method(agent, items, m)?),
                (None, Some(m)) => {
                    let sync = ops::get_iterator_from_method(agent, items, m)?;
                    Some(create_async_from_sync_iterator(agent, sync))
                }
                (None, None) => None,
            };
            match record {
                Some(record) => {
                    let a = construct_or_array(agent, c, None)?;
                    let d = from_async(agent, state);
                    d.source = Some(FromAsyncSource::Iterator(record));
                    d.target = Some(a);
                    iterator_step(agent, state)
                }
                None => {
                    let array_like = ops::to_object(agent, items)?;
                    let len = ops::length_of_array_like(agent, array_like)?;
                    let a = construct_or_array(agent, c, Some(len))?;
                    let d = from_async(agent, state);
                    d.source = Some(FromAsyncSource::ArrayLike {
                        object: array_like,
                        len,
                    });
                    d.target = Some(a);
                    array_like_step(agent, state)
                }
            }
        }
        FromAsyncStage::AwaitNext => {
            let next_result = input?;
            let Value::Object(next_result) = next_result else {
                return agent.type_error("iterator result is not an object");
            };
            let (record, a, k) = iterator_state(agent, state);
            if ops::iterator_complete(agent, next_result)? {
                set_length(agent, a, k)?;
                return Ok(Flow::Return(Value::Object(a)));
            }
            let next_value = ops::iterator_value(agent, next_result)?;
            if mapping {
                match ops::call(agent, mapper, this_arg, &[next_value, Value::Number(k)]) {
                    Ok(mapped) => {
                        from_async(agent, state).stage = FromAsyncStage::AwaitMapped;
                        Ok(Flow::Await(mapped))
                    }
                    Err(e) => async_close(agent, state, record, e),
                }
            } else {
                iterator_define(agent, state, next_value)
            }
        }
        FromAsyncStage::AwaitMapped => match input {
            Ok(v) => iterator_define(agent, state, v),
            Err(e) => {
                let (record, _, _) = iterator_state(agent, state);
                async_close(agent, state, record, e)
            }
        },
        FromAsyncStage::AwaitValue => {
            let k_value = input?;
            if mapping {
                let k = from_async(agent, state).k;
                let mapped = ops::call(agent, mapper, this_arg, &[k_value, Value::Number(k)])?;
                from_async(agent, state).stage = FromAsyncStage::AwaitMappedElement;
                Ok(Flow::Await(mapped))
            } else {
                array_like_define(agent, state, k_value)
            }
        }
        FromAsyncStage::AwaitMappedElement => {
            let v = input?;
            array_like_define(agent, state, v)
        }
        FromAsyncStage::AwaitClose(error) => Err(Throw(error)),
    }
}

fn iterator_state(agent: &mut Agent, state: Object) -> (IteratorRecord, Object, f64) {
    let d = from_async(agent, state);
    let record = match &d.source {
        Some(FromAsyncSource::Iterator(r)) => *r,
        _ => unreachable!("Array.fromAsync iterator state"),
    };
    (record, d.target.expect("Array.fromAsync target"), d.k)
}

/// The top of the iterator loop: calls `next()` and awaits its result.
fn iterator_step(agent: &mut Agent, state: Object) -> JsResult<Flow> {
    let (record, _, k) = iterator_state(agent, state);
    if k >= MAX_SAFE_INTEGER {
        let error = agent.throw_error(ErrorKind::Type, "Array.fromAsync: too many elements");
        return async_close(agent, state, record, error);
    }
    let next_result = ops::call(
        agent,
        record.next_method,
        Value::Object(record.iterator),
        &[],
    )?;
    from_async(agent, state).stage = FromAsyncStage::AwaitNext;
    Ok(Flow::Await(next_result))
}

fn iterator_define(agent: &mut Agent, state: Object, v: Value) -> JsResult<Flow> {
    let (record, a, k) = iterator_state(agent, state);
    let pk = key(agent, k);
    if let Err(e) = ops::create_data_property_or_throw(agent, a, pk, v) {
        return async_close(agent, state, record, e);
    }
    from_async(agent, state).k = k + 1.0;
    iterator_step(agent, state)
}

fn array_like_state(agent: &mut Agent, state: Object) -> (Object, f64, Object, f64) {
    let d = from_async(agent, state);
    let (object, len) = match &d.source {
        Some(FromAsyncSource::ArrayLike { object, len }) => (*object, *len),
        _ => unreachable!("Array.fromAsync array-like state"),
    };
    (object, len, d.target.expect("Array.fromAsync target"), d.k)
}

/// The top of the array-like loop: reads element `k` and awaits it.
fn array_like_step(agent: &mut Agent, state: Object) -> JsResult<Flow> {
    let (object, len, a, k) = array_like_state(agent, state);
    if k < len {
        let pk = key(agent, k);
        let k_value = ops::get(agent, object, pk)?;
        from_async(agent, state).stage = FromAsyncStage::AwaitValue;
        return Ok(Flow::Await(k_value));
    }
    set_length(agent, a, len)?;
    Ok(Flow::Return(Value::Object(a)))
}

fn array_like_define(agent: &mut Agent, state: Object, v: Value) -> JsResult<Flow> {
    let (_, _, a, k) = array_like_state(agent, state);
    let pk = key(agent, k);
    ops::create_data_property_or_throw(agent, a, pk, v)?;
    from_async(agent, state).k = k + 1.0;
    array_like_step(agent, state)
}

// ----- %Array.prototype% -----

fn at(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let (o, len) = this_and_length(agent, call.this)?;
    let rel = ops::to_integer_or_infinity(agent, call.arg(0))?;
    let k = if rel >= 0.0 { rel } else { len + rel };
    if k < 0.0 || k >= len {
        return Ok(Value::Undefined);
    }
    let pk = key(agent, k);
    ops::get(agent, o, pk)
}

/// IsConcatSpreadable(O).
fn is_concat_spreadable(agent: &mut Agent, v: Value) -> JsResult<bool> {
    let Value::Object(o) = v else {
        return Ok(false);
    };
    let key = PropertyKey::Symbol(agent.symbols.is_concat_spreadable);
    let spreadable = ops::get(agent, o, key)?;
    if !spreadable.is_undefined() {
        return Ok(ops::to_boolean(agent, spreadable));
    }
    ops::is_array(agent, v)
}

fn concat(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let o = ops::to_object(agent, call.this)?;
    let a = array_species_create(agent, o, 0.0)?;
    let mut n = 0.0;
    let items = std::iter::once(Value::Object(o)).chain(call.args.iter().copied());
    for e in items {
        if is_concat_spreadable(agent, e)? {
            let Value::Object(e) = e else { continue };
            let len = ops::length_of_array_like(agent, e)?;
            if n + len > MAX_SAFE_INTEGER {
                return agent.type_error("Array.prototype.concat: the result is too long");
            }
            let mut k = 0.0;
            while k < len {
                let next = next_present(agent, e, k, len);
                n += next - k;
                k = next;
                if k >= len {
                    break;
                }
                let pk = key(agent, k);
                if ops::has_property(agent, e, pk)? {
                    let sub = ops::get(agent, e, pk)?;
                    let nk = key(agent, n);
                    ops::create_data_property_or_throw(agent, a, nk, sub)?;
                }
                n += 1.0;
                k += 1.0;
            }
        } else {
            if n >= MAX_SAFE_INTEGER {
                return agent.type_error("Array.prototype.concat: the result is too long");
            }
            let nk = key(agent, n);
            ops::create_data_property_or_throw(agent, a, nk, e)?;
            n += 1.0;
        }
    }
    set_length(agent, a, n)?;
    Ok(Value::Object(a))
}

fn copy_within(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let (o, len) = this_and_length(agent, call.this)?;
    let mut to = relative(agent, call.arg(0), len, None)?;
    let mut from = relative(agent, call.arg(1), len, None)?;
    let fin = relative(agent, call.arg(2), len, Some(len))?;
    let mut count = (fin - from).min(len - to);
    let direction = if from < to && to < from + count {
        from += count - 1.0;
        to += count - 1.0;
        -1.0
    } else {
        1.0
    };
    while count > 0.0 {
        let from_key = key(agent, from);
        let to_key = key(agent, to);
        if ops::has_property(agent, o, from_key)? {
            let v = ops::get(agent, o, from_key)?;
            ops::set(agent, o, to_key, v, true)?;
        } else {
            ops::delete_property_or_throw(agent, o, to_key)?;
        }
        from += direction;
        to += direction;
        count -= 1.0;
    }
    Ok(Value::Object(o))
}

fn entries(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let o = ops::to_object(agent, call.this)?;
    Ok(Value::Object(create_array_iterator(
        agent,
        o,
        ArrayIterKind::Entries,
    )))
}

fn keys(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let o = ops::to_object(agent, call.this)?;
    Ok(Value::Object(create_array_iterator(
        agent,
        o,
        ArrayIterKind::Keys,
    )))
}

/// Calls `f` for every present index of `0..len` in ascending order with
/// the value of that index; stops when `f` returns `Some`.
fn for_present<T>(
    agent: &mut Agent,
    o: Object,
    len: f64,
    mut f: impl FnMut(&mut Agent, f64, Value) -> JsResult<Option<T>>,
) -> JsResult<Option<T>> {
    let mut k = 0.0;
    while k < len {
        k = next_present(agent, o, k, len);
        if k >= len {
            break;
        }
        let pk = key(agent, k);
        if ops::has_property(agent, o, pk)? {
            let v = ops::get(agent, o, pk)?;
            if let Some(r) = f(agent, k, v)? {
                return Ok(Some(r));
            }
        }
        k += 1.0;
    }
    Ok(None)
}

fn every(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let (o, len) = this_and_length(agent, call.this)?;
    let f = callable(agent, call.arg(0), "every")?;
    let this_arg = call.arg(1);
    let failed = for_present(agent, o, len, |agent, k, v| {
        let r = ops::call(agent, f, this_arg, &[v, Value::Number(k), Value::Object(o)])?;
        Ok((!ops::to_boolean(agent, r)).then_some(()))
    })?;
    Ok(Value::Boolean(failed.is_none()))
}

fn some(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let (o, len) = this_and_length(agent, call.this)?;
    let f = callable(agent, call.arg(0), "some")?;
    let this_arg = call.arg(1);
    let found = for_present(agent, o, len, |agent, k, v| {
        let r = ops::call(agent, f, this_arg, &[v, Value::Number(k), Value::Object(o)])?;
        Ok(ops::to_boolean(agent, r).then_some(()))
    })?;
    Ok(Value::Boolean(found.is_some()))
}

fn for_each(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let (o, len) = this_and_length(agent, call.this)?;
    let f = callable(agent, call.arg(0), "forEach")?;
    let this_arg = call.arg(1);
    for_present::<()>(agent, o, len, |agent, k, v| {
        ops::call(agent, f, this_arg, &[v, Value::Number(k), Value::Object(o)])?;
        Ok(None)
    })?;
    Ok(Value::Undefined)
}

fn map(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let (o, len) = this_and_length(agent, call.this)?;
    let f = callable(agent, call.arg(0), "map")?;
    let this_arg = call.arg(1);
    let a = array_species_create(agent, o, len)?;
    for_present::<()>(agent, o, len, |agent, k, v| {
        let mapped = ops::call(agent, f, this_arg, &[v, Value::Number(k), Value::Object(o)])?;
        let pk = key(agent, k);
        ops::create_data_property_or_throw(agent, a, pk, mapped)?;
        Ok(None)
    })?;
    Ok(Value::Object(a))
}

fn filter(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let (o, len) = this_and_length(agent, call.this)?;
    let f = callable(agent, call.arg(0), "filter")?;
    let this_arg = call.arg(1);
    let a = array_species_create(agent, o, 0.0)?;
    let mut to = 0.0;
    for_present::<()>(agent, o, len, |agent, k, v| {
        let selected = ops::call(agent, f, this_arg, &[v, Value::Number(k), Value::Object(o)])?;
        if ops::to_boolean(agent, selected) {
            let pk = key(agent, to);
            ops::create_data_property_or_throw(agent, a, pk, v)?;
            to += 1.0;
        }
        Ok(None)
    })?;
    Ok(Value::Object(a))
}

fn fill(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let (o, len) = this_and_length(agent, call.this)?;
    let v = call.arg(0);
    let mut k = relative(agent, call.arg(1), len, None)?;
    let fin = relative(agent, call.arg(2), len, Some(len))?;
    while k < fin {
        let pk = key(agent, k);
        ops::set(agent, o, pk, v, true)?;
        k += 1.0;
    }
    Ok(Value::Object(o))
}

/// FindViaPredicate(O, len, direction, predicate, thisArg): the index and
/// value found.
fn find_via_predicate(
    agent: &mut Agent,
    call: &NativeCall<'_>,
    descending: bool,
    name: &str,
) -> JsResult<(f64, Value)> {
    let (o, len) = this_and_length(agent, call.this)?;
    let predicate = callable(agent, call.arg(0), name)?;
    let this_arg = call.arg(1);
    let mut i = 0.0;
    while i < len {
        let k = if descending { len - 1.0 - i } else { i };
        let pk = key(agent, k);
        let v = ops::get(agent, o, pk)?;
        let r = ops::call(
            agent,
            predicate,
            this_arg,
            &[v, Value::Number(k), Value::Object(o)],
        )?;
        if ops::to_boolean(agent, r) {
            return Ok((k, v));
        }
        i += 1.0;
    }
    Ok((-1.0, Value::Undefined))
}

fn find(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    Ok(find_via_predicate(agent, &call, false, "find")?.1)
}

fn find_index(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    Ok(Value::Number(
        find_via_predicate(agent, &call, false, "findIndex")?.0,
    ))
}

fn find_last(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    Ok(find_via_predicate(agent, &call, true, "findLast")?.1)
}

fn find_last_index(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    Ok(Value::Number(
        find_via_predicate(agent, &call, true, "findLastIndex")?.0,
    ))
}

/// FlattenIntoArray(target, source, sourceLen, start, depth[, mapper,
/// thisArg]) with an explicit stack instead of recursion.
fn flatten_into_array(
    agent: &mut Agent,
    target: Object,
    source: Object,
    source_len: f64,
    depth: f64,
    mapper: Option<(Value, Value)>,
) -> JsResult<()> {
    struct Frame {
        source: Object,
        len: f64,
        index: f64,
        depth: f64,
    }
    let mut stack = vec![Frame {
        source,
        len: source_len,
        index: 0.0,
        depth,
    }];
    let mut target_index = 0.0;
    while let Some(frame) = stack.last_mut() {
        if frame.index >= frame.len {
            stack.pop();
            continue;
        }
        let (src, len, depth) = (frame.source, frame.len, frame.depth);
        let mut index = frame.index;
        index = next_present(agent, src, index, len);
        let top = stack.len() - 1;
        stack[top].index = index + 1.0;
        if index >= len {
            continue;
        }
        let p = key(agent, index);
        if !ops::has_property(agent, src, p)? {
            continue;
        }
        let mut element = ops::get(agent, src, p)?;
        // The mapper only applies to the elements of the outer source.
        if top == 0
            && let Some((f, this_arg)) = mapper
        {
            element = ops::call(
                agent,
                f,
                this_arg,
                &[element, Value::Number(index), Value::Object(src)],
            )?;
        }
        let should_flatten = depth > 0.0 && ops::is_array(agent, element)?;
        if should_flatten {
            let Value::Object(e) = element else { continue };
            let new_depth = if depth == f64::INFINITY {
                f64::INFINITY
            } else {
                depth - 1.0
            };
            let element_len = ops::length_of_array_like(agent, e)?;
            stack.push(Frame {
                source: e,
                len: element_len,
                index: 0.0,
                depth: new_depth,
            });
        } else {
            if target_index >= MAX_SAFE_INTEGER {
                return agent.type_error("Array.prototype.flat: the result is too long");
            }
            let tk = key(agent, target_index);
            ops::create_data_property_or_throw(agent, target, tk, element)?;
            target_index += 1.0;
        }
    }
    Ok(())
}

fn flat(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let (o, len) = this_and_length(agent, call.this)?;
    let depth = match call.arg(0) {
        Value::Undefined => 1.0,
        d => ops::to_integer_or_infinity(agent, d)?.max(0.0),
    };
    let a = array_species_create(agent, o, 0.0)?;
    flatten_into_array(agent, a, o, len, depth, None)?;
    Ok(Value::Object(a))
}

fn flat_map(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let (o, len) = this_and_length(agent, call.this)?;
    let f = callable(agent, call.arg(0), "flatMap")?;
    let a = array_species_create(agent, o, 0.0)?;
    flatten_into_array(agent, a, o, len, 1.0, Some((f, call.arg(1))))?;
    Ok(Value::Object(a))
}

fn includes(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let (o, len) = this_and_length(agent, call.this)?;
    if len == 0.0 {
        return Ok(Value::Boolean(false));
    }
    let search = call.arg(0);
    let n = ops::to_integer_or_infinity(agent, call.arg(1))?;
    if n == f64::INFINITY {
        return Ok(Value::Boolean(false));
    }
    let mut k = if n >= 0.0 { n } else { (len + n).max(0.0) };
    while k < len {
        // An absent element reads as `undefined`.
        if !search.is_undefined() {
            k = next_present(agent, o, k, len);
            if k >= len {
                break;
            }
        }
        let pk = key(agent, k);
        let element = ops::get(agent, o, pk)?;
        if ops::same_value_zero(agent, search, element) {
            return Ok(Value::Boolean(true));
        }
        k += 1.0;
    }
    Ok(Value::Boolean(false))
}

fn index_of(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let (o, len) = this_and_length(agent, call.this)?;
    if len == 0.0 {
        return Ok(Value::Number(-1.0));
    }
    let search = call.arg(0);
    let n = ops::to_integer_or_infinity(agent, call.arg(1))?;
    if n == f64::INFINITY {
        return Ok(Value::Number(-1.0));
    }
    let mut k = if n >= 0.0 { n } else { (len + n).max(0.0) };
    while k < len {
        k = next_present(agent, o, k, len);
        if k >= len {
            break;
        }
        let pk = key(agent, k);
        if ops::has_property(agent, o, pk)? {
            let element = ops::get(agent, o, pk)?;
            if ops::is_strictly_equal(agent, search, element) {
                return Ok(Value::Number(k));
            }
        }
        k += 1.0;
    }
    Ok(Value::Number(-1.0))
}

fn last_index_of(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let (o, len) = this_and_length(agent, call.this)?;
    if len == 0.0 {
        return Ok(Value::Number(-1.0));
    }
    let search = call.arg(0);
    let n = if call.args.len() > 1 {
        ops::to_integer_or_infinity(agent, call.arg(1))?
    } else {
        len - 1.0
    };
    if n == f64::NEG_INFINITY {
        return Ok(Value::Number(-1.0));
    }
    let mut k = if n >= 0.0 { n.min(len - 1.0) } else { len + n };
    while k >= 0.0 {
        k = prev_present(agent, o, k);
        if k < 0.0 {
            break;
        }
        let pk = key(agent, k);
        if ops::has_property(agent, o, pk)? {
            let element = ops::get(agent, o, pk)?;
            if ops::is_strictly_equal(agent, search, element) {
                return Ok(Value::Number(k));
            }
        }
        k -= 1.0;
    }
    Ok(Value::Number(-1.0))
}

/// Appends the code units of `s` to `out`, checking the string length.
fn append(agent: &mut Agent, out: &mut Vec<u16>, s: crate::gc::JsString) -> JsResult<()> {
    let units = agent.heap.string_units(s);
    if out.len() + units.len() > MAX_STRING_LENGTH {
        return agent.range_error("invalid string length");
    }
    out.extend_from_slice(units);
    Ok(())
}

fn join(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let (o, len) = this_and_length(agent, call.this)?;
    let sep = match call.arg(0) {
        Value::Undefined => agent.heap.alloc_str(","),
        s => ops::to_string(agent, s)?,
    };
    let mut out: Vec<u16> = Vec::new();
    let mut k = 0.0;
    while k < len {
        if k > 0.0 {
            append(agent, &mut out, sep)?;
        }
        let pk = key(agent, k);
        let element = ops::get(agent, o, pk)?;
        if !element.is_nullish() {
            let s = ops::to_string(agent, element)?;
            append(agent, &mut out, s)?;
        }
        k += 1.0;
    }
    Ok(Value::String(agent.heap.alloc_string_owned(out)))
}

fn to_locale_string(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let (o, len) = this_and_length(agent, call.this)?;
    let sep = agent.heap.alloc_str(",");
    let method = agent.heap.key_from_str("toLocaleString");
    let mut out: Vec<u16> = Vec::new();
    let mut k = 0.0;
    while k < len {
        if k > 0.0 {
            append(agent, &mut out, sep)?;
        }
        let pk = key(agent, k);
        let element = ops::get(agent, o, pk)?;
        if !element.is_nullish() {
            let r = ops::invoke(agent, element, method, &[])?;
            let s = ops::to_string(agent, r)?;
            append(agent, &mut out, s)?;
        }
        k += 1.0;
    }
    Ok(Value::String(agent.heap.alloc_string_owned(out)))
}

fn to_string(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let array = ops::to_object(agent, call.this)?;
    let key = agent.heap.key_from_str("join");
    let mut f = ops::get(agent, array, key)?;
    if !ops::is_callable(agent, f) {
        f = native_slots(agent, call.callee)
            .first()
            .copied()
            .unwrap_or(Value::Undefined);
    }
    ops::call(agent, f, Value::Object(array), &[])
}

fn pop(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let (o, len) = this_and_length(agent, call.this)?;
    if len == 0.0 {
        set_length(agent, o, 0.0)?;
        return Ok(Value::Undefined);
    }
    let new_len = len - 1.0;
    let index = key(agent, new_len);
    let element = ops::get(agent, o, index)?;
    ops::delete_property_or_throw(agent, o, index)?;
    set_length(agent, o, new_len)?;
    Ok(element)
}

fn push(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let (o, mut len) = this_and_length(agent, call.this)?;
    if len + call.args.len() as f64 > MAX_SAFE_INTEGER {
        return agent.type_error("Array.prototype.push: the array is too long");
    }
    for &e in call.args {
        let pk = key(agent, len);
        ops::set(agent, o, pk, e, true)?;
        len += 1.0;
    }
    set_length(agent, o, len)?;
    Ok(Value::Number(len))
}

fn reduce_impl(agent: &mut Agent, call: &NativeCall<'_>, right: bool) -> JsResult<Value> {
    let name = if right { "reduceRight" } else { "reduce" };
    let (o, len) = this_and_length(agent, call.this)?;
    let f = callable(agent, call.arg(0), name)?;
    let has_initial = call.args.len() > 1;
    if len == 0.0 && !has_initial {
        return agent.type_error(&format!(
            "Array.prototype.{name} of an empty array with no initial value"
        ));
    }
    // `k` walks from the start (or the end) towards the other side.
    let mut k = if right { len - 1.0 } else { 0.0 };
    let advance = |agent: &Agent, k: f64| {
        if right {
            prev_present(agent, o, k)
        } else {
            next_present(agent, o, k, len)
        }
    };
    let in_range = |k: f64| if right { k >= 0.0 } else { k < len };
    let step = if right { -1.0 } else { 1.0 };
    let mut accumulator = Value::Undefined;
    if has_initial {
        accumulator = call.arg(1);
    } else {
        let mut present = false;
        while !present && in_range(k) {
            k = advance(agent, k);
            if !in_range(k) {
                break;
            }
            let pk = key(agent, k);
            present = ops::has_property(agent, o, pk)?;
            if present {
                accumulator = ops::get(agent, o, pk)?;
            }
            k += step;
        }
        if !present {
            return agent.type_error(&format!(
                "Array.prototype.{name} of an empty array with no initial value"
            ));
        }
    }
    while in_range(k) {
        k = advance(agent, k);
        if !in_range(k) {
            break;
        }
        let pk = key(agent, k);
        if ops::has_property(agent, o, pk)? {
            let v = ops::get(agent, o, pk)?;
            accumulator = ops::call(
                agent,
                f,
                Value::Undefined,
                &[accumulator, v, Value::Number(k), Value::Object(o)],
            )?;
        }
        k += step;
    }
    Ok(accumulator)
}

fn reduce(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    reduce_impl(agent, &call, false)
}

fn reduce_right(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    reduce_impl(agent, &call, true)
}

fn reverse(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let (o, len) = this_and_length(agent, call.this)?;
    let middle = (len / 2.0).floor();
    let mut lower = 0.0;
    while lower != middle {
        let upper = len - lower - 1.0;
        let upper_p = key(agent, upper);
        let lower_p = key(agent, lower);
        let lower_exists = ops::has_property(agent, o, lower_p)?;
        let lower_value = if lower_exists {
            ops::get(agent, o, lower_p)?
        } else {
            Value::Undefined
        };
        let upper_exists = ops::has_property(agent, o, upper_p)?;
        let upper_value = if upper_exists {
            ops::get(agent, o, upper_p)?
        } else {
            Value::Undefined
        };
        match (lower_exists, upper_exists) {
            (true, true) => {
                ops::set(agent, o, lower_p, upper_value, true)?;
                ops::set(agent, o, upper_p, lower_value, true)?;
            }
            (false, true) => {
                ops::set(agent, o, lower_p, upper_value, true)?;
                ops::delete_property_or_throw(agent, o, upper_p)?;
            }
            (true, false) => {
                ops::delete_property_or_throw(agent, o, lower_p)?;
                ops::set(agent, o, upper_p, lower_value, true)?;
            }
            (false, false) => {}
        }
        lower += 1.0;
    }
    Ok(Value::Object(o))
}

fn shift(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let (o, len) = this_and_length(agent, call.this)?;
    if len == 0.0 {
        set_length(agent, o, 0.0)?;
        return Ok(Value::Undefined);
    }
    let first = ops::get(agent, o, PropertyKey::Index(0))?;
    let mut k = 1.0;
    while k < len {
        let from = key(agent, k);
        let to = key(agent, k - 1.0);
        if ops::has_property(agent, o, from)? {
            let v = ops::get(agent, o, from)?;
            ops::set(agent, o, to, v, true)?;
        } else {
            ops::delete_property_or_throw(agent, o, to)?;
        }
        k += 1.0;
    }
    let last = key(agent, len - 1.0);
    ops::delete_property_or_throw(agent, o, last)?;
    set_length(agent, o, len - 1.0)?;
    Ok(first)
}

fn unshift(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let (o, len) = this_and_length(agent, call.this)?;
    let arg_count = call.args.len() as f64;
    if arg_count > 0.0 {
        if len + arg_count > MAX_SAFE_INTEGER {
            return agent.type_error("Array.prototype.unshift: the array is too long");
        }
        let mut k = len;
        while k > 0.0 {
            let from = key(agent, k - 1.0);
            let to = key(agent, k + arg_count - 1.0);
            if ops::has_property(agent, o, from)? {
                let v = ops::get(agent, o, from)?;
                ops::set(agent, o, to, v, true)?;
            } else {
                ops::delete_property_or_throw(agent, o, to)?;
            }
            k -= 1.0;
        }
        for (j, &e) in call.args.iter().enumerate() {
            let pk = key(agent, j as f64);
            ops::set(agent, o, pk, e, true)?;
        }
    }
    set_length(agent, o, len + arg_count)?;
    Ok(Value::Number(len + arg_count))
}

fn slice(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let (o, len) = this_and_length(agent, call.this)?;
    let mut k = relative(agent, call.arg(0), len, None)?;
    let fin = relative(agent, call.arg(1), len, Some(len))?;
    let count = (fin - k).max(0.0);
    let a = array_species_create(agent, o, count)?;
    let mut n = 0.0;
    while k < fin {
        let next = next_present(agent, o, k, fin);
        n += next - k;
        k = next;
        if k >= fin {
            break;
        }
        let pk = key(agent, k);
        if ops::has_property(agent, o, pk)? {
            let v = ops::get(agent, o, pk)?;
            let nk = key(agent, n);
            ops::create_data_property_or_throw(agent, a, nk, v)?;
        }
        k += 1.0;
        n += 1.0;
    }
    set_length(agent, a, n)?;
    Ok(Value::Object(a))
}

/// CompareArrayElements(x, y, comparefn): negative, zero or positive.
fn compare_array_elements(
    agent: &mut Agent,
    x: Value,
    y: Value,
    comparefn: Value,
) -> JsResult<f64> {
    match (x, y) {
        (Value::Undefined, Value::Undefined) => return Ok(0.0),
        (Value::Undefined, _) => return Ok(1.0),
        (_, Value::Undefined) => return Ok(-1.0),
        _ => {}
    }
    if !comparefn.is_undefined() {
        let r = ops::call(agent, comparefn, Value::Undefined, &[x, y])?;
        let v = ops::to_number(agent, r)?;
        return Ok(if v.is_nan() { 0.0 } else { v });
    }
    let xs = ops::to_string(agent, x)?;
    let ys = ops::to_string(agent, y)?;
    Ok(match agent.heap.string_cmp(xs, ys) {
        std::cmp::Ordering::Less => -1.0,
        std::cmp::Ordering::Equal => 0.0,
        std::cmp::Ordering::Greater => 1.0,
    })
}

/// A stable merge sort whose comparisons may throw (the first throw stops
/// the sort).
fn sort_values(agent: &mut Agent, items: Vec<Value>, comparefn: Value) -> JsResult<Vec<Value>> {
    let n = items.len();
    let mut src = items;
    let mut dst = src.clone();
    let mut width = 1;
    while width < n {
        let mut lo = 0;
        while lo < n {
            let mid = (lo + width).min(n);
            let hi = (lo + 2 * width).min(n);
            let (mut l, mut r, mut out) = (lo, mid, lo);
            while l < mid && r < hi {
                // Equal elements keep their order: the right one goes
                // first only if it is strictly smaller.
                if compare_array_elements(agent, src[l], src[r], comparefn)? > 0.0 {
                    dst[out] = src[r];
                    r += 1;
                } else {
                    dst[out] = src[l];
                    l += 1;
                }
                out += 1;
            }
            dst[out..out + (mid - l)].copy_from_slice(&src[l..mid]);
            out += mid - l;
            dst[out..out + (hi - r)].copy_from_slice(&src[r..hi]);
            lo = hi;
        }
        std::mem::swap(&mut src, &mut dst);
        width *= 2;
    }
    Ok(src)
}

/// SortIndexedProperties(obj, len, SortCompare, holes).
fn sort_indexed_properties(
    agent: &mut Agent,
    o: Object,
    len: f64,
    comparefn: Value,
    skip_holes: bool,
) -> JsResult<Vec<Value>> {
    let mut items = Vec::new();
    let mut k = 0.0;
    while k < len {
        if skip_holes {
            k = next_present(agent, o, k, len);
            if k >= len {
                break;
            }
        }
        let pk = key(agent, k);
        let read = !skip_holes || ops::has_property(agent, o, pk)?;
        if read {
            items.push(ops::get(agent, o, pk)?);
        }
        k += 1.0;
    }
    sort_values(agent, items, comparefn)
}

fn comparefn_argument(agent: &mut Agent, v: Value, name: &str) -> JsResult<Value> {
    if v.is_undefined() || ops::is_callable(agent, v) {
        Ok(v)
    } else {
        agent.type_error(&format!(
            "Array.prototype.{name}: the comparison function is not a function"
        ))
    }
}

fn sort(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let comparefn = comparefn_argument(agent, call.arg(0), "sort")?;
    let (o, len) = this_and_length(agent, call.this)?;
    let sorted = sort_indexed_properties(agent, o, len, comparefn, true)?;
    let count = sorted.len() as f64;
    for (j, v) in sorted.into_iter().enumerate() {
        let pk = key(agent, j as f64);
        ops::set(agent, o, pk, v, true)?;
    }
    // Delete the holes left at the end; deleting an absent own property of
    // an ordinary object has no effect, so those are skipped.
    let mut j = count;
    while j < len {
        if internal::is_plain(agent, o) {
            let next = if j < INDEX_LIMIT {
                agent
                    .heap
                    .next_element_index(o, j as u32, len.min(INDEX_LIMIT) as u32)
                    .map_or(len.min(INDEX_LIMIT), f64::from)
            } else {
                j
            };
            j = next;
            if j >= len {
                break;
            }
        }
        let pk = key(agent, j);
        ops::delete_property_or_throw(agent, o, pk)?;
        j += 1.0;
    }
    Ok(Value::Object(o))
}

fn splice(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let (o, len) = this_and_length(agent, call.this)?;
    let actual_start = relative(agent, call.arg(0), len, None)?;
    let items: &[Value] = call.args.get(2..).unwrap_or(&[]);
    let item_count = items.len() as f64;
    let actual_delete_count = match call.args.len() {
        0 => 0.0,
        1 => len - actual_start,
        _ => {
            let dc = ops::to_integer_or_infinity(agent, call.arg(1))?;
            dc.max(0.0).min(len - actual_start)
        }
    };
    if len + item_count - actual_delete_count > MAX_SAFE_INTEGER {
        return agent.type_error("Array.prototype.splice: the array is too long");
    }
    let a = array_species_create(agent, o, actual_delete_count)?;
    let mut k = 0.0;
    while k < actual_delete_count {
        let from = key(agent, actual_start + k);
        if ops::has_property(agent, o, from)? {
            let v = ops::get(agent, o, from)?;
            let pk = key(agent, k);
            ops::create_data_property_or_throw(agent, a, pk, v)?;
        }
        k += 1.0;
    }
    set_length(agent, a, actual_delete_count)?;
    if item_count < actual_delete_count {
        let mut k = actual_start;
        while k < len - actual_delete_count {
            let from = key(agent, k + actual_delete_count);
            let to = key(agent, k + item_count);
            if ops::has_property(agent, o, from)? {
                let v = ops::get(agent, o, from)?;
                ops::set(agent, o, to, v, true)?;
            } else {
                ops::delete_property_or_throw(agent, o, to)?;
            }
            k += 1.0;
        }
        let mut k = len;
        while k > len - actual_delete_count + item_count {
            let pk = key(agent, k - 1.0);
            ops::delete_property_or_throw(agent, o, pk)?;
            k -= 1.0;
        }
    } else if item_count > actual_delete_count {
        let mut k = len - actual_delete_count;
        while k > actual_start {
            let from = key(agent, k + actual_delete_count - 1.0);
            let to = key(agent, k + item_count - 1.0);
            if ops::has_property(agent, o, from)? {
                let v = ops::get(agent, o, from)?;
                ops::set(agent, o, to, v, true)?;
            } else {
                ops::delete_property_or_throw(agent, o, to)?;
            }
            k -= 1.0;
        }
    }
    for (i, &e) in items.iter().enumerate() {
        let pk = key(agent, actual_start + i as f64);
        ops::set(agent, o, pk, e, true)?;
    }
    set_length(agent, o, len - actual_delete_count + item_count)?;
    Ok(Value::Object(a))
}

fn to_reversed(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let (o, len) = this_and_length(agent, call.this)?;
    let a = internal::array_create(agent, len, None)?;
    let mut k = 0.0;
    while k < len {
        let from = key(agent, len - k - 1.0);
        let v = ops::get(agent, o, from)?;
        let pk = key(agent, k);
        ops::create_data_property_or_throw(agent, a, pk, v)?;
        k += 1.0;
    }
    Ok(Value::Object(a))
}

fn to_sorted(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let comparefn = comparefn_argument(agent, call.arg(0), "toSorted")?;
    let (o, len) = this_and_length(agent, call.this)?;
    let a = internal::array_create(agent, len, None)?;
    let sorted = sort_indexed_properties(agent, o, len, comparefn, false)?;
    for (j, v) in sorted.into_iter().enumerate() {
        ops::create_data_property_or_throw(agent, a, PropertyKey::Index(j as u32), v)?;
    }
    Ok(Value::Object(a))
}

fn to_spliced(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let (o, len) = this_and_length(agent, call.this)?;
    let actual_start = relative(agent, call.arg(0), len, None)?;
    let items: &[Value] = call.args.get(2..).unwrap_or(&[]);
    let insert_count = items.len() as f64;
    let actual_skip_count = match call.args.len() {
        0 => 0.0,
        1 => len - actual_start,
        _ => {
            let sc = ops::to_integer_or_infinity(agent, call.arg(1))?;
            sc.max(0.0).min(len - actual_start)
        }
    };
    let new_len = len + insert_count - actual_skip_count;
    if new_len > MAX_SAFE_INTEGER {
        return agent.type_error("Array.prototype.toSpliced: the array is too long");
    }
    let a = internal::array_create(agent, new_len, None)?;
    let mut i = 0.0;
    let mut r = actual_start + actual_skip_count;
    while i < actual_start {
        let pi = key(agent, i);
        let v = ops::get(agent, o, pi)?;
        ops::create_data_property_or_throw(agent, a, pi, v)?;
        i += 1.0;
    }
    for &e in items {
        let pi = key(agent, i);
        ops::create_data_property_or_throw(agent, a, pi, e)?;
        i += 1.0;
    }
    while i < new_len {
        let pi = key(agent, i);
        let from = key(agent, r);
        let v = ops::get(agent, o, from)?;
        ops::create_data_property_or_throw(agent, a, pi, v)?;
        i += 1.0;
        r += 1.0;
    }
    Ok(Value::Object(a))
}

fn with(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let (o, len) = this_and_length(agent, call.this)?;
    let rel = ops::to_integer_or_infinity(agent, call.arg(0))?;
    let actual = if rel >= 0.0 { rel } else { len + rel };
    if actual >= len || actual < 0.0 {
        return agent.range_error("Array.prototype.with: the index is out of range");
    }
    let a = internal::array_create(agent, len, None)?;
    let mut k = 0.0;
    while k < len {
        let pk = key(agent, k);
        let v = if k == actual {
            call.arg(1)
        } else {
            ops::get(agent, o, pk)?
        };
        ops::create_data_property_or_throw(agent, a, pk, v)?;
        k += 1.0;
    }
    Ok(Value::Object(a))
}
