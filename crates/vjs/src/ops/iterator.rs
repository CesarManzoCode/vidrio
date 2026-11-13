//! Operations on iterator objects (ECMA-262 §7.4).

use crate::agent::Agent;
use crate::completion::{JsResult, Throw};
use crate::gc::{Object, Trace, Tracer};
use crate::object::{Attributes, Slot};
use crate::ops::conversion::to_boolean;
use crate::ops::objects::{call, get, get_method, object_create};
use crate::value::{PropertyKey, Value};

/// Iterator Record.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct IteratorRecord {
    pub iterator: Object,
    pub next_method: Value,
    pub done: bool,
}

impl Trace for IteratorRecord {
    fn trace(&self, tracer: &mut Tracer) {
        tracer.object(self.iterator);
        tracer.value(self.next_method);
    }
}

/// GetIteratorDirect(obj).
pub fn get_iterator_direct(agent: &mut Agent, obj: Object) -> JsResult<IteratorRecord> {
    let key = PropertyKey::String(agent.heap.names.next);
    let next_method = get(agent, obj, key)?;
    Ok(IteratorRecord {
        iterator: obj,
        next_method,
        done: false,
    })
}

/// GetIteratorFromMethod(obj, method).
pub fn get_iterator_from_method(
    agent: &mut Agent,
    obj: Value,
    method: Value,
) -> JsResult<IteratorRecord> {
    let Value::Object(iterator) = call(agent, method, obj, &[])? else {
        return agent.type_error("iterator is not an object");
    };
    get_iterator_direct(agent, iterator)
}

/// GetIterator(obj, sync).
pub fn get_iterator(agent: &mut Agent, obj: Value) -> JsResult<IteratorRecord> {
    let key = PropertyKey::Symbol(agent.symbols.iterator);
    let Some(method) = get_method(agent, obj, key)? else {
        return agent.type_error("value is not iterable");
    };
    get_iterator_from_method(agent, obj, method)
}

/// IteratorNext(iteratorRecord[, value]).
pub fn iterator_next(
    agent: &mut Agent,
    record: &mut IteratorRecord,
    value: Option<Value>,
) -> JsResult<Object> {
    let args: &[Value] = match &value {
        Some(v) => std::slice::from_ref(v),
        None => &[],
    };
    let result = match call(
        agent,
        record.next_method,
        Value::Object(record.iterator),
        args,
    ) {
        Ok(r) => r,
        Err(e) => {
            record.done = true;
            return Err(e);
        }
    };
    match result {
        Value::Object(o) => Ok(o),
        _ => {
            record.done = true;
            agent.type_error("iterator result is not an object")
        }
    }
}

/// IteratorComplete(iteratorResult).
pub fn iterator_complete(agent: &mut Agent, result: Object) -> JsResult<bool> {
    let key = PropertyKey::String(agent.heap.names.done);
    let done = get(agent, result, key)?;
    Ok(to_boolean(agent, done))
}

/// IteratorValue(iteratorResult).
pub fn iterator_value(agent: &mut Agent, result: Object) -> JsResult<Value> {
    let key = PropertyKey::String(agent.heap.names.value);
    get(agent, result, key)
}

/// IteratorStep(iteratorRecord): the result object, or `None` when done.
pub fn iterator_step(agent: &mut Agent, record: &mut IteratorRecord) -> JsResult<Option<Object>> {
    let result = iterator_next(agent, record, None)?;
    let done = match iterator_complete(agent, result) {
        Ok(d) => d,
        Err(e) => {
            record.done = true;
            return Err(e);
        }
    };
    if done {
        record.done = true;
        return Ok(None);
    }
    Ok(Some(result))
}

/// IteratorStepValue(iteratorRecord): the next value, or `None` when done.
pub fn iterator_step_value(
    agent: &mut Agent,
    record: &mut IteratorRecord,
) -> JsResult<Option<Value>> {
    let Some(result) = iterator_step(agent, record)? else {
        return Ok(None);
    };
    match iterator_value(agent, result) {
        Ok(v) => Ok(Some(v)),
        Err(e) => {
            record.done = true;
            Err(e)
        }
    }
}

/// IteratorClose(iteratorRecord, completion).
pub fn iterator_close<T>(
    agent: &mut Agent,
    record: &IteratorRecord,
    completion: JsResult<T>,
) -> JsResult<T> {
    let iterator = Value::Object(record.iterator);
    let key = PropertyKey::String(agent.heap.names.return_);
    let inner: Result<Value, Throw> = match get_method(agent, iterator, key) {
        Ok(None) => return completion,
        Ok(Some(ret)) => call(agent, ret, iterator, &[]),
        Err(e) => Err(e),
    };
    let value = completion?;
    let inner = inner?;
    if !inner.is_object() {
        return agent.type_error("iterator return() result is not an object");
    }
    Ok(value)
}

/// CreateIteratorResultObject(value, done).
pub fn create_iter_result_object(agent: &mut Agent, value: Value, done: bool) -> Object {
    let o = object_create(agent);
    let (value_key, done_key) = (agent.heap.names.value, agent.heap.names.done);
    agent.heap.set_own(
        o,
        PropertyKey::String(value_key),
        Attributes::ALL,
        Slot::Data(value),
    );
    agent.heap.set_own(
        o,
        PropertyKey::String(done_key),
        Attributes::ALL,
        Slot::Data(Value::Boolean(done)),
    );
    o
}

/// IteratorToList(iteratorRecord).
pub fn iterator_to_list(agent: &mut Agent, record: &mut IteratorRecord) -> JsResult<Vec<Value>> {
    let mut values = Vec::new();
    while let Some(v) = iterator_step_value(agent, record)? {
        values.push(v);
    }
    Ok(values)
}
