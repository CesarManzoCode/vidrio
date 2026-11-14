//! Iterators the language needs: the `for-in` iterator (CreateForInIterator
//! and `%ForInIteratorPrototype%.next`), the array iterator behind
//! `Array.prototype.values` and `arguments[Symbol.iterator]`, and
//! `%AsyncFromSyncIteratorPrototype%`.

use std::collections::VecDeque;

use crate::agent::Agent;
use crate::completion::{JsResult, Throw};
use crate::function::{NativeCall, create_native_function, native_slots};
use crate::gc::{Object, Trace, Tracer};
use crate::internal;
use crate::interp::promise::{self, Capability};
use crate::object::ObjectKind;
use crate::ops::{self, IteratorRecord};
use crate::realm::Intrinsic;
use crate::value::{PropertyKey, Value};

/// The state of a `for-in` iteration (§14.7.5.10).
pub struct ForInData {
    pub object: Option<Object>,
    pub object_was_visited: bool,
    pub visited: Vec<PropertyKey>,
    pub remaining: VecDeque<PropertyKey>,
}

impl Trace for ForInData {
    fn trace(&self, tracer: &mut Tracer) {
        if let Some(o) = self.object {
            tracer.object(o);
        }
        for k in self.visited.iter().chain(self.remaining.iter()) {
            tracer.key(*k);
        }
    }
}

/// CreateForInIterator(object).
pub fn create_for_in_iterator(agent: &mut Agent, object: Object) -> Object {
    let proto = agent.intrinsic(Intrinsic::ForInIteratorPrototype);
    agent.heap.create_object(
        Some(proto),
        ObjectKind::ForInIterator(Box::new(ForInData {
            object: Some(object),
            object_was_visited: false,
            visited: Vec::new(),
            remaining: VecDeque::new(),
        })),
    )
}

fn for_in_data(agent: &mut Agent, it: Object) -> Option<&mut ForInData> {
    match &mut agent.heap.object_mut(it).kind {
        ObjectKind::ForInIterator(d) => Some(d),
        _ => None,
    }
}

/// `%ForInIteratorPrototype%.next()` as an engine operation: the next key
/// or `None` when done.
pub fn for_in_next(agent: &mut Agent, it: Object) -> JsResult<Option<Value>> {
    loop {
        let Some(d) = for_in_data(agent, it) else {
            return Ok(None);
        };
        let Some(o) = d.object else {
            return Ok(None);
        };
        if !d.object_was_visited {
            let keys = internal::own_property_keys(agent, o)?;
            let d = for_in_data(agent, it).expect("for-in data");
            for k in keys {
                if !k.is_symbol() {
                    d.remaining.push_back(k);
                }
            }
            d.object_was_visited = true;
        }
        loop {
            let d = for_in_data(agent, it).expect("for-in data");
            let Some(r) = d.remaining.pop_front() else {
                break;
            };
            if d.visited.contains(&r) {
                continue;
            }
            let desc = internal::get_own_property(agent, o, r)?;
            if let Some(desc) = desc {
                for_in_data(agent, it).expect("for-in data").visited.push(r);
                if desc.enumerable == Some(true) {
                    return Ok(Some(agent.heap.key_to_value(r)));
                }
            }
        }
        let proto = internal::get_prototype_of(agent, o)?;
        let d = for_in_data(agent, it).expect("for-in data");
        d.object = proto;
        d.object_was_visited = false;
        if proto.is_none() {
            return Ok(None);
        }
    }
}

pub(crate) fn for_in_iterator_next(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let Value::Object(it) = call.this else {
        return agent.type_error("not a for-in iterator");
    };
    let r = for_in_next(agent, it)?;
    let done = r.is_none();
    let o = ops::create_iter_result_object(agent, r.unwrap_or(Value::Undefined), done);
    Ok(Value::Object(o))
}

// ----- array iterators -----

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArrayIterKind {
    Keys,
    Values,
    Entries,
}

pub struct ArrayIteratorData {
    /// `None` once the iterator is exhausted.
    pub object: Option<Object>,
    pub index: u64,
    pub kind: ArrayIterKind,
}

impl Trace for ArrayIteratorData {
    fn trace(&self, tracer: &mut Tracer) {
        if let Some(o) = self.object {
            tracer.object(o);
        }
    }
}

/// CreateArrayIterator(array, kind).
pub fn create_array_iterator(agent: &mut Agent, array: Object, kind: ArrayIterKind) -> Object {
    let proto = agent.intrinsic(Intrinsic::ArrayIteratorPrototype);
    agent.heap.create_object(
        Some(proto),
        ObjectKind::ArrayIterator(Box::new(ArrayIteratorData {
            object: Some(array),
            index: 0,
            kind,
        })),
    )
}

/// `Array.prototype.values()`.
pub(crate) fn array_values(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let o = ops::to_object(agent, call.this)?;
    Ok(Value::Object(create_array_iterator(
        agent,
        o,
        ArrayIterKind::Values,
    )))
}

/// `%ArrayIteratorPrototype%.next()`.
pub(crate) fn array_iterator_next(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let Value::Object(it) = call.this else {
        return agent.type_error("not an array iterator");
    };
    let (object, index, kind) = match &agent.heap.object(it).kind {
        ObjectKind::ArrayIterator(d) => (d.object, d.index, d.kind),
        _ => return agent.type_error("not an array iterator"),
    };
    let Some(a) = object else {
        let r = ops::create_iter_result_object(agent, Value::Undefined, true);
        return Ok(Value::Object(r));
    };
    let len = ops::length_of_array_like(agent, a)? as u64;
    if index >= len {
        if let ObjectKind::ArrayIterator(d) = &mut agent.heap.object_mut(it).kind {
            d.object = None;
        }
        let r = ops::create_iter_result_object(agent, Value::Undefined, true);
        return Ok(Value::Object(r));
    }
    if let ObjectKind::ArrayIterator(d) = &mut agent.heap.object_mut(it).kind {
        d.index = index + 1;
    }
    let key_value = Value::Number(index as f64);
    let result = match kind {
        ArrayIterKind::Keys => key_value,
        ArrayIterKind::Values => {
            let k = ops::index_key(agent, index);
            ops::get(agent, a, k)?
        }
        ArrayIterKind::Entries => {
            let k = ops::index_key(agent, index);
            let v = ops::get(agent, a, k)?;
            Value::Object(ops::create_array_from_list(agent, &[key_value, v]))
        }
    };
    Ok(Value::Object(ops::create_iter_result_object(
        agent, result, false,
    )))
}

/// `%IteratorPrototype%[Symbol.iterator]` and
/// `%AsyncIteratorPrototype%[Symbol.asyncIterator]`.
pub(crate) fn return_this(_agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    Ok(call.this)
}

// ----- async-from-sync iterators -----

/// CreateAsyncFromSyncIterator(syncIteratorRecord).
pub fn create_async_from_sync_iterator(
    agent: &mut Agent,
    record: IteratorRecord,
) -> IteratorRecord {
    let proto = agent.intrinsic(Intrinsic::AsyncFromSyncIteratorPrototype);
    let o = agent.heap.create_object(
        Some(proto),
        ObjectKind::AsyncFromSyncIterator(Box::new(record)),
    );
    let next_key = PropertyKey::String(agent.heap.names.next);
    let next = match agent.heap.get_own(proto, next_key) {
        Some((_, crate::object::Slot::Data(v))) => v,
        _ => Value::Undefined,
    };
    IteratorRecord {
        iterator: o,
        next_method: next,
        done: false,
    }
}

fn sync_record(agent: &Agent, this: Value) -> Option<IteratorRecord> {
    match this {
        Value::Object(o) => match &agent.heap.object(o).kind {
            ObjectKind::AsyncFromSyncIterator(r) => Some(**r),
            _ => None,
        },
        _ => None,
    }
}

fn reject_with(agent: &mut Agent, cap: &Capability, e: Value) -> JsResult<Value> {
    ops::call(agent, cap.reject, Value::Undefined, &[e])?;
    Ok(cap.promise)
}

/// AsyncFromSyncIteratorContinuation(result, promiseCapability,
/// syncIteratorRecord, closeOnRejection).
fn continuation(
    agent: &mut Agent,
    result: Object,
    cap: Capability,
    record: IteratorRecord,
    close_on_rejection: bool,
) -> JsResult<Value> {
    let done = match ops::iterator_complete(agent, result) {
        Ok(d) => d,
        Err(Throw(e)) => return reject_with(agent, &cap, e),
    };
    let value = match ops::iterator_value(agent, result) {
        Ok(v) => v,
        Err(Throw(e)) => return reject_with(agent, &cap, e),
    };
    let ctor = agent.intrinsic(Intrinsic::Promise);
    let wrapper = match promise::promise_resolve(agent, ctor, value) {
        Ok(Value::Object(p)) => p,
        Ok(_) => return Ok(cap.promise),
        Err(Throw(e)) => {
            let e = if !done && close_on_rejection {
                match ops::iterator_close::<()>(agent, &record, Err(Throw(e))) {
                    Err(Throw(x)) => x,
                    Ok(()) => e,
                }
            } else {
                e
            };
            return reject_with(agent, &cap, e);
        }
    };
    let on_fulfilled = create_native_function(
        agent,
        unwrap_fulfilled,
        1,
        "",
        vec![Value::Boolean(done)],
        None,
    );
    let on_rejected = if done || !close_on_rejection {
        Value::Undefined
    } else {
        Value::Object(create_native_function(
            agent,
            close_rejected,
            1,
            "",
            vec![Value::Object(record.iterator), record.next_method],
            None,
        ))
    };
    promise::perform_promise_then(
        agent,
        wrapper,
        Value::Object(on_fulfilled),
        on_rejected,
        Some(cap),
    );
    Ok(cap.promise)
}

fn unwrap_fulfilled(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let done = matches!(native_slots(agent, call.callee)[0], Value::Boolean(true));
    Ok(Value::Object(ops::create_iter_result_object(
        agent,
        call.arg(0),
        done,
    )))
}

fn close_rejected(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let slots = native_slots(agent, call.callee);
    let (Value::Object(iterator), next_method) = (slots[0], slots[1]) else {
        return Err(Throw(call.arg(0)));
    };
    let record = IteratorRecord {
        iterator,
        next_method,
        done: false,
    };
    ops::iterator_close::<Value>(agent, &record, Err(Throw(call.arg(0))))
}

pub(crate) fn async_from_sync_next(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let realm = agent.current_realm();
    let cap = promise::new_intrinsic_capability(agent, realm);
    let Some(mut record) = sync_record(agent, call.this) else {
        return agent.type_error("not an async-from-sync iterator");
    };
    let value = if call.args.is_empty() {
        None
    } else {
        Some(call.arg(0))
    };
    let result = match ops::iterator_next(agent, &mut record, value) {
        Ok(r) => r,
        Err(Throw(e)) => return reject_with(agent, &cap, e),
    };
    continuation(agent, result, cap, record, true)
}

pub(crate) fn async_from_sync_return(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let realm = agent.current_realm();
    let cap = promise::new_intrinsic_capability(agent, realm);
    let Some(record) = sync_record(agent, call.this) else {
        return agent.type_error("not an async-from-sync iterator");
    };
    let iterator = Value::Object(record.iterator);
    let key = PropertyKey::String(agent.heap.names.return_);
    let method = match ops::get_method(agent, iterator, key) {
        Ok(m) => m,
        Err(Throw(e)) => return reject_with(agent, &cap, e),
    };
    let Some(method) = method else {
        let r = ops::create_iter_result_object(agent, call.arg(0), true);
        ops::call(agent, cap.resolve, Value::Undefined, &[Value::Object(r)])?;
        return Ok(cap.promise);
    };
    let args: Vec<Value> = call.args.iter().take(1).copied().collect();
    let result = match ops::call(agent, method, iterator, &args) {
        Ok(r) => r,
        Err(Throw(e)) => return reject_with(agent, &cap, e),
    };
    let Value::Object(result) = result else {
        let e = agent.throw_error(
            crate::agent::ErrorKind::Type,
            "iterator result is not an object",
        );
        return reject_with(agent, &cap, e.0);
    };
    continuation(agent, result, cap, record, false)
}

pub(crate) fn async_from_sync_throw(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let realm = agent.current_realm();
    let cap = promise::new_intrinsic_capability(agent, realm);
    let Some(record) = sync_record(agent, call.this) else {
        return agent.type_error("not an async-from-sync iterator");
    };
    let iterator = Value::Object(record.iterator);
    let key = PropertyKey::String(agent.heap.names.throw);
    let method = match ops::get_method(agent, iterator, key) {
        Ok(m) => m,
        Err(Throw(e)) => return reject_with(agent, &cap, e),
    };
    let Some(method) = method else {
        if let Err(Throw(e)) = ops::iterator_close(agent, &record, Ok(())) {
            return reject_with(agent, &cap, e);
        }
        let e = agent.throw_error(
            crate::agent::ErrorKind::Type,
            "the iterator does not have a 'throw' method",
        );
        return reject_with(agent, &cap, e.0);
    };
    let args: Vec<Value> = call.args.iter().take(1).copied().collect();
    let result = match ops::call(agent, method, iterator, &args) {
        Ok(r) => r,
        Err(Throw(e)) => return reject_with(agent, &cap, e),
    };
    let Value::Object(result) = result else {
        let e = agent.throw_error(
            crate::agent::ErrorKind::Type,
            "iterator result is not an object",
        );
        return reject_with(agent, &cap, e.0);
    };
    continuation(agent, result, cap, record, true)
}
