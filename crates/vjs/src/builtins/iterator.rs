//! Iterator objects (ECMA-262 §27.1): the `Iterator` constructor with
//! `Iterator.from`, `Iterator.concat` (feature `iterator-sequencing`) and
//! `Iterator.zip`/`Iterator.zipKeyed` (`joint-iteration`);
//! `%Iterator.prototype%` with the helpers (`map`, `filter`, `take`,
//! `drop`, `flatMap`, `reduce`, `toArray`, `forEach`, `some`, `every`,
//! `find`), `includes` (`iterator-includes`), `chunks` and `windows`
//! (`iterator-chunking`) and `@@dispose`; `%IteratorHelperPrototype%` and
//! `%WrapForValidIteratorPrototype%`.
//!
//! The specification writes the lazy helpers as generator closures. Here
//! each one is a state machine: [`HelperKind`] holds the variables of its
//! closure and `step` runs it up to the next `Yield`. A `return()` while
//! suspended at a `Yield` runs what the closure does with the abrupt
//! completion of that `Yield` (`abort`).

use std::collections::VecDeque;

use crate::agent::{Agent, ErrorKind};
use crate::builtins::{
    accessor, constructor, intrinsic, method, method_key, plain_object, set_intrinsic,
    to_string_tag,
};
use crate::completion::JsResult;
use crate::function::{NativeCall, NativeFn};
use crate::gc::{Object, Realm, Trace, Tracer};
use crate::internal;
use crate::object::ObjectKind;
use crate::ops::{self, IteratorRecord, PrimitiveHandling};
use crate::realm::Intrinsic;
use crate::value::{PropertyKey, Value};

const MAX_SAFE_INTEGER: f64 = 9007199254740991.0;

/// `[[GeneratorState]]` of an iterator helper.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HelperState {
    SuspendedStart,
    SuspendedYield,
    Executing,
    Completed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ZipMode {
    Shortest,
    Longest,
    Strict,
}

/// The variables of a helper's closure.
enum HelperKind {
    /// Placeholder while the closure runs or after it completed.
    Empty,
    Map {
        mapper: Value,
        counter: f64,
    },
    Filter {
        predicate: Value,
        counter: f64,
    },
    Take {
        remaining: f64,
    },
    Drop {
        remaining: f64,
    },
    FlatMap {
        mapper: Value,
        counter: f64,
        inner: Option<IteratorRecord>,
    },
    Chunks {
        size: usize,
        /// Suspended at the yield of the last, partial chunk.
        finishing: bool,
    },
    Windows {
        size: usize,
        allow_partial: bool,
        buffer: VecDeque<Value>,
        finishing: bool,
    },
    Concat {
        /// `[[OpenMethod]]` and `[[Iterable]]` of each argument.
        iterables: Vec<(Value, Value)>,
        next: usize,
        inner: Option<IteratorRecord>,
    },
    Zip {
        iters: Vec<IteratorRecord>,
        /// Whether `iters[i]` is still in `openIters` (in "longest" mode a
        /// closed one is `null` and yields its padding).
        open: Vec<bool>,
        mode: ZipMode,
        padding: Vec<Value>,
        /// The keys of `Iterator.zipKeyed`; `None` for `Iterator.zip`.
        keys: Option<Vec<PropertyKey>>,
    },
}

impl Trace for HelperKind {
    fn trace(&self, tracer: &mut Tracer) {
        match self {
            HelperKind::Empty
            | HelperKind::Take { .. }
            | HelperKind::Drop { .. }
            | HelperKind::Chunks { .. } => {}
            HelperKind::Map { mapper, .. } => {
                tracer.value(*mapper);
            }
            HelperKind::Filter { predicate, .. } => {
                tracer.value(*predicate);
            }
            HelperKind::FlatMap { mapper, inner, .. } => {
                tracer.value(*mapper);
                if let Some(r) = inner {
                    r.trace(tracer);
                }
            }
            HelperKind::Windows { buffer, .. } => {
                for v in buffer {
                    tracer.value(*v);
                }
            }
            HelperKind::Concat {
                iterables, inner, ..
            } => {
                for (m, i) in iterables {
                    tracer.value(*m);
                    tracer.value(*i);
                }
                if let Some(r) = inner {
                    r.trace(tracer);
                }
            }
            HelperKind::Zip {
                iters,
                padding,
                keys,
                ..
            } => {
                for r in iters {
                    r.trace(tracer);
                }
                for v in padding {
                    tracer.value(*v);
                }
                for k in keys.iter().flatten() {
                    tracer.key(*k);
                }
            }
        }
    }
}

/// The internal slots of an iterator helper: `[[GeneratorState]]`,
/// `[[UnderlyingIterator]]` (`iterated`) and the closure.
pub struct HelperData {
    state: HelperState,
    iterated: Option<IteratorRecord>,
    kind: HelperKind,
}

impl Trace for HelperData {
    fn trace(&self, tracer: &mut Tracer) {
        if let Some(r) = &self.iterated {
            r.trace(tracer);
        }
        self.kind.trace(tracer);
    }
}

// ----- installation -----

pub(crate) fn install(agent: &mut Agent, realm: Realm) {
    let proto = intrinsic(agent, realm, Intrinsic::IteratorPrototype);
    let ctor = constructor(
        agent,
        realm,
        "Iterator",
        iterator_constructor,
        0,
        proto,
        None,
    );
    set_intrinsic(agent, realm, Intrinsic::Iterator, ctor);
    method(agent, realm, ctor, "concat", iterator_concat, 0);
    method(agent, realm, ctor, "from", iterator_from, 1);
    method(agent, realm, ctor, "zip", iterator_zip, 1);
    method(agent, realm, ctor, "zipKeyed", iterator_zip_keyed, 1);

    let methods: &[(&str, NativeFn, u32)] = &[
        ("chunks", proto_chunks, 1),
        ("drop", proto_drop, 1),
        ("every", proto_every, 1),
        ("filter", proto_filter, 1),
        ("find", proto_find, 1),
        ("flatMap", proto_flat_map, 1),
        ("forEach", proto_for_each, 1),
        ("includes", proto_includes, 1),
        ("map", proto_map, 1),
        ("reduce", proto_reduce, 1),
        ("some", proto_some, 1),
        ("take", proto_take, 1),
        ("toArray", proto_to_array, 0),
        ("windows", proto_windows, 1),
    ];
    for &(name, f, length) in methods {
        method(agent, realm, proto, name, f, length);
    }
    let key = PropertyKey::Symbol(agent.symbols.dispose);
    method_key(agent, realm, proto, key, proto_dispose, 0);
    // `constructor` and `@@toStringTag` are accessors whose setters ignore
    // assignments to the prototype itself.
    let key = PropertyKey::String(agent.heap.names.constructor);
    accessor(
        agent,
        realm,
        proto,
        key,
        Some(get_constructor),
        Some(set_constructor),
    );
    let key = PropertyKey::Symbol(agent.symbols.to_string_tag);
    accessor(
        agent,
        realm,
        proto,
        key,
        Some(get_to_string_tag),
        Some(set_to_string_tag),
    );

    let helper_proto = plain_object(agent, proto);
    method(agent, realm, helper_proto, "next", helper_next, 0);
    method(agent, realm, helper_proto, "return", helper_return, 0);
    to_string_tag(agent, helper_proto, "Iterator Helper");
    set_intrinsic(
        agent,
        realm,
        Intrinsic::IteratorHelperPrototype,
        helper_proto,
    );

    let wrap_proto = plain_object(agent, proto);
    method(agent, realm, wrap_proto, "next", wrap_next, 0);
    method(agent, realm, wrap_proto, "return", wrap_return, 0);
    set_intrinsic(
        agent,
        realm,
        Intrinsic::WrapForValidIteratorPrototype,
        wrap_proto,
    );
}

// ----- the constructor and %Iterator.prototype% accessors -----

/// `Iterator()` (§27.1.3.1): an abstract class.
fn iterator_constructor(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    match call.new_target {
        Some(nt) if nt != call.callee => {
            let o = ops::ordinary_create_from_constructor(
                agent,
                nt,
                Intrinsic::IteratorPrototype,
                ObjectKind::Ordinary,
            )?;
            Ok(Value::Object(o))
        }
        _ => agent.type_error("Iterator is an abstract class and cannot be constructed directly"),
    }
}

/// SetterThatIgnoresPrototypeProperties(thisValue, home, p, v).
fn setter_that_ignores_prototype_properties(
    agent: &mut Agent,
    this: Value,
    home: Object,
    key: PropertyKey,
    v: Value,
) -> JsResult<Value> {
    let Value::Object(o) = this else {
        return agent.type_error("setter called on a non-object");
    };
    if o == home {
        return agent.type_error("cannot assign to this property of Iterator.prototype");
    }
    if internal::get_own_property(agent, o, key)?.is_none() {
        ops::create_data_property_or_throw(agent, o, key, v)?;
    } else {
        ops::set(agent, o, key, v, true)?;
    }
    Ok(Value::Undefined)
}

fn get_constructor(agent: &mut Agent, _call: NativeCall<'_>) -> JsResult<Value> {
    Ok(Value::Object(agent.intrinsic(Intrinsic::Iterator)))
}

fn set_constructor(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let home = agent.intrinsic(Intrinsic::IteratorPrototype);
    let key = PropertyKey::String(agent.heap.names.constructor);
    setter_that_ignores_prototype_properties(agent, call.this, home, key, call.arg(0))
}

fn get_to_string_tag(agent: &mut Agent, _call: NativeCall<'_>) -> JsResult<Value> {
    Ok(Value::String(agent.heap.alloc_str("Iterator")))
}

fn set_to_string_tag(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let home = agent.intrinsic(Intrinsic::IteratorPrototype);
    let key = PropertyKey::Symbol(agent.symbols.to_string_tag);
    setter_that_ignores_prototype_properties(agent, call.this, home, key, call.arg(0))
}

/// `%Iterator.prototype%[@@dispose]()`.
fn proto_dispose(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let key = PropertyKey::String(agent.heap.names.return_);
    if let Some(ret) = ops::get_method(agent, call.this, key)? {
        ops::call(agent, ret, call.this, &[])?;
    }
    Ok(Value::Undefined)
}

// ----- Iterator.from and %WrapForValidIteratorPrototype% -----

/// `Iterator.from(O)` (§27.1.3.2.1).
fn iterator_from(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let record = ops::get_iterator_flattenable(
        agent,
        call.arg(0),
        PrimitiveHandling::IterateStringPrimitives,
    )?;
    let iterator_ctor = Value::Object(agent.intrinsic(Intrinsic::Iterator));
    if ops::ordinary_has_instance(agent, iterator_ctor, Value::Object(record.iterator))? {
        return Ok(Value::Object(record.iterator));
    }
    let proto = agent.intrinsic(Intrinsic::WrapForValidIteratorPrototype);
    let wrapper = agent.heap.create_object(
        Some(proto),
        ObjectKind::WrapForValidIterator(Box::new(record)),
    );
    Ok(Value::Object(wrapper))
}

fn wrapped(agent: &mut Agent, this: Value) -> JsResult<IteratorRecord> {
    if let Value::Object(o) = this
        && let ObjectKind::WrapForValidIterator(r) = &agent.heap.object(o).kind
    {
        return Ok(**r);
    }
    agent.type_error("not an iterator wrapped by Iterator.from")
}

/// `%WrapForValidIteratorPrototype%.next()`.
fn wrap_next(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let record = wrapped(agent, call.this)?;
    ops::call(
        agent,
        record.next_method,
        Value::Object(record.iterator),
        &[],
    )
}

/// `%WrapForValidIteratorPrototype%.return()`.
fn wrap_return(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let record = wrapped(agent, call.this)?;
    let iterator = Value::Object(record.iterator);
    let key = PropertyKey::String(agent.heap.names.return_);
    match ops::get_method(agent, iterator, key)? {
        None => Ok(Value::Object(ops::create_iter_result_object(
            agent,
            Value::Undefined,
            true,
        ))),
        Some(ret) => ops::call(agent, ret, iterator, &[]),
    }
}

// ----- %IteratorHelperPrototype% -----

fn create_helper(agent: &mut Agent, iterated: Option<IteratorRecord>, kind: HelperKind) -> Value {
    let proto = agent.intrinsic(Intrinsic::IteratorHelperPrototype);
    let o = agent.heap.create_object(
        Some(proto),
        ObjectKind::IteratorHelper(Box::new(HelperData {
            state: HelperState::SuspendedStart,
            iterated,
            kind,
        })),
    );
    Value::Object(o)
}

fn helper_data(agent: &mut Agent, o: Object) -> Option<&mut HelperData> {
    match &mut agent.heap.object_mut(o).kind {
        ObjectKind::IteratorHelper(h) => Some(h),
        _ => None,
    }
}

/// RequireInternalSlot(this, [[UnderlyingIterator]]): the helper and its
/// state.
fn require_helper(agent: &mut Agent, this: Value) -> JsResult<(Object, HelperState)> {
    if let Value::Object(o) = this
        && let Some(h) = helper_data(agent, o)
    {
        return Ok((o, h.state));
    }
    agent.type_error("not an Iterator Helper object")
}

/// Takes the closure out while it runs; `finish` puts it back.
fn take_closure(agent: &mut Agent, o: Object) -> (Option<IteratorRecord>, HelperKind) {
    match helper_data(agent, o) {
        Some(h) => {
            h.state = HelperState::Executing;
            (
                h.iterated,
                std::mem::replace(&mut h.kind, HelperKind::Empty),
            )
        }
        None => (None, HelperKind::Empty),
    }
}

fn finish(agent: &mut Agent, o: Object, state: HelperState, kind: HelperKind) {
    if let Some(h) = helper_data(agent, o) {
        h.state = state;
        if state == HelperState::Completed {
            h.iterated = None;
        } else {
            h.kind = kind;
        }
    }
}

fn done_result(agent: &mut Agent) -> Value {
    Value::Object(ops::create_iter_result_object(
        agent,
        Value::Undefined,
        true,
    ))
}

/// `%IteratorHelperPrototype%.next()`: GeneratorResume.
fn helper_next(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let (o, state) = require_helper(agent, call.this)?;
    match state {
        HelperState::Executing => {
            return agent.type_error("the iterator helper is already running");
        }
        HelperState::Completed => return Ok(done_result(agent)),
        _ => {}
    }
    let (mut iterated, mut kind) = take_closure(agent, o);
    let result = step(agent, &mut iterated, &mut kind);
    match result {
        Ok(Some(v)) => {
            finish(agent, o, HelperState::SuspendedYield, kind);
            Ok(Value::Object(ops::create_iter_result_object(
                agent, v, false,
            )))
        }
        Ok(None) => {
            finish(agent, o, HelperState::Completed, kind);
            Ok(done_result(agent))
        }
        Err(e) => {
            finish(agent, o, HelperState::Completed, kind);
            Err(e)
        }
    }
}

/// The iterators a helper closes when returned before it started
/// (`[[UnderlyingIterators]]`).
fn start_iterators(iterated: &Option<IteratorRecord>, kind: &HelperKind) -> Vec<IteratorRecord> {
    match kind {
        HelperKind::Zip { iters, open, .. } => iters
            .iter()
            .zip(open)
            .filter(|(_, o)| **o)
            .map(|(r, _)| *r)
            .collect(),
        HelperKind::Concat { .. } => Vec::new(),
        _ => iterated.iter().copied().collect(),
    }
}

/// `%IteratorHelperPrototype%.return()`.
fn helper_return(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let (o, state) = require_helper(agent, call.this)?;
    match state {
        HelperState::SuspendedStart => {
            let (iterated, kind) = take_closure(agent, o);
            let iters = start_iterators(&iterated, &kind);
            finish(agent, o, HelperState::Completed, kind);
            ops::iterator_close_all(agent, &iters, Ok(()))?;
            Ok(done_result(agent))
        }
        HelperState::Executing => agent.type_error("the iterator helper is already running"),
        HelperState::Completed => Ok(done_result(agent)),
        HelperState::SuspendedYield => {
            let (iterated, mut kind) = take_closure(agent, o);
            let result = abort(agent, &iterated, &mut kind);
            finish(agent, o, HelperState::Completed, kind);
            result?;
            Ok(done_result(agent))
        }
    }
}

/// What a closure does when its `Yield` returns a return completion.
fn abort(
    agent: &mut Agent,
    iterated: &Option<IteratorRecord>,
    kind: &mut HelperKind,
) -> JsResult<()> {
    match kind {
        HelperKind::Chunks {
            finishing: true, ..
        }
        | HelperKind::Windows {
            finishing: true, ..
        } => Ok(()),
        HelperKind::FlatMap { inner, .. } => {
            let Some(outer) = iterated else {
                return Ok(());
            };
            let backup = match inner {
                Some(inner) => ops::iterator_close(agent, inner, Ok(())),
                None => Ok(()),
            };
            match backup {
                Err(e) => ops::iterator_close(agent, outer, Err(e)),
                Ok(()) => ops::iterator_close(agent, outer, Ok(())),
            }
        }
        HelperKind::Concat { inner, .. } => match inner {
            Some(inner) => ops::iterator_close(agent, inner, Ok(())),
            None => Ok(()),
        },
        HelperKind::Zip { iters, open, .. } => close_open(agent, iters, open, Ok(())),
        _ => match iterated {
            Some(r) => ops::iterator_close(agent, r, Ok(())),
            None => Ok(()),
        },
    }
}

/// IteratorCloseAll(openIters, completion).
fn close_open<T>(
    agent: &mut Agent,
    iters: &[IteratorRecord],
    open: &[bool],
    completion: JsResult<T>,
) -> JsResult<T> {
    let open_iters: Vec<IteratorRecord> = iters
        .iter()
        .zip(open)
        .filter(|(_, o)| **o)
        .map(|(r, _)| *r)
        .collect();
    ops::iterator_close_all(agent, &open_iters, completion)
}

fn array_of(agent: &mut Agent, values: &[Value]) -> Value {
    Value::Object(ops::create_array_from_list(agent, values))
}

/// Runs the closure up to its next `Yield`: `Some(value)` for a yield,
/// `None` when it returns.
fn step(
    agent: &mut Agent,
    iterated: &mut Option<IteratorRecord>,
    kind: &mut HelperKind,
) -> JsResult<Option<Value>> {
    match kind {
        HelperKind::Empty => Ok(None),
        HelperKind::Map { mapper, counter } => {
            let Some(it) = iterated else { return Ok(None) };
            let Some(value) = ops::iterator_step_value(agent, it)? else {
                return Ok(None);
            };
            let mapped = ops::call(
                agent,
                *mapper,
                Value::Undefined,
                &[value, Value::Number(*counter)],
            );
            let mapped = ops::if_abrupt_close_iterator(agent, it, mapped)?;
            *counter += 1.0;
            Ok(Some(mapped))
        }
        HelperKind::Filter { predicate, counter } => {
            let Some(it) = iterated else { return Ok(None) };
            loop {
                let Some(value) = ops::iterator_step_value(agent, it)? else {
                    return Ok(None);
                };
                let selected = ops::call(
                    agent,
                    *predicate,
                    Value::Undefined,
                    &[value, Value::Number(*counter)],
                );
                let selected = ops::if_abrupt_close_iterator(agent, it, selected)?;
                *counter += 1.0;
                if ops::to_boolean(agent, selected) {
                    return Ok(Some(value));
                }
            }
        }
        HelperKind::Take { remaining } => {
            let Some(it) = iterated else { return Ok(None) };
            if *remaining == 0.0 {
                ops::iterator_close(agent, it, Ok(()))?;
                return Ok(None);
            }
            if *remaining != f64::INFINITY {
                *remaining -= 1.0;
            }
            ops::iterator_step_value(agent, it)
        }
        HelperKind::Drop { remaining } => {
            let Some(it) = iterated else { return Ok(None) };
            while *remaining > 0.0 {
                if *remaining != f64::INFINITY {
                    *remaining -= 1.0;
                }
                if ops::iterator_step(agent, it)?.is_none() {
                    return Ok(None);
                }
            }
            ops::iterator_step_value(agent, it)
        }
        HelperKind::FlatMap {
            mapper,
            counter,
            inner,
        } => {
            let Some(it) = iterated else { return Ok(None) };
            loop {
                if let Some(inner_rec) = inner {
                    let r = ops::iterator_step_value(agent, inner_rec);
                    match ops::if_abrupt_close_iterator(agent, it, r)? {
                        Some(v) => return Ok(Some(v)),
                        None => *inner = None,
                    }
                }
                let Some(value) = ops::iterator_step_value(agent, it)? else {
                    return Ok(None);
                };
                let mapped = ops::call(
                    agent,
                    *mapper,
                    Value::Undefined,
                    &[value, Value::Number(*counter)],
                );
                let mapped = ops::if_abrupt_close_iterator(agent, it, mapped)?;
                let inner_rec = ops::get_iterator_flattenable(
                    agent,
                    mapped,
                    PrimitiveHandling::RejectPrimitives,
                );
                *inner = Some(ops::if_abrupt_close_iterator(agent, it, inner_rec)?);
                *counter += 1.0;
            }
        }
        HelperKind::Chunks { size, finishing } => {
            let Some(it) = iterated else { return Ok(None) };
            if *finishing {
                return Ok(None);
            }
            let mut buffer = Vec::new();
            loop {
                let Some(value) = ops::iterator_step_value(agent, it)? else {
                    if buffer.is_empty() {
                        return Ok(None);
                    }
                    *finishing = true;
                    return Ok(Some(array_of(agent, &buffer)));
                };
                buffer.push(value);
                if buffer.len() == *size {
                    return Ok(Some(array_of(agent, &buffer)));
                }
            }
        }
        HelperKind::Windows {
            size,
            allow_partial,
            buffer,
            finishing,
        } => {
            let Some(it) = iterated else { return Ok(None) };
            if *finishing {
                return Ok(None);
            }
            loop {
                let Some(value) = ops::iterator_step_value(agent, it)? else {
                    if *allow_partial && !buffer.is_empty() && buffer.len() < *size {
                        *finishing = true;
                        let values: Vec<Value> = buffer.iter().copied().collect();
                        return Ok(Some(array_of(agent, &values)));
                    }
                    return Ok(None);
                };
                if buffer.len() == *size {
                    buffer.pop_front();
                }
                buffer.push_back(value);
                if buffer.len() == *size {
                    let values: Vec<Value> = buffer.iter().copied().collect();
                    return Ok(Some(array_of(agent, &values)));
                }
            }
        }
        HelperKind::Concat {
            iterables,
            next,
            inner,
        } => loop {
            if let Some(rec) = inner {
                match ops::iterator_step_value(agent, rec)? {
                    Some(v) => return Ok(Some(v)),
                    None => *inner = None,
                }
            }
            let Some(&(open_method, iterable)) = iterables.get(*next) else {
                return Ok(None);
            };
            *next += 1;
            let Value::Object(iter) = ops::call(agent, open_method, iterable, &[])? else {
                return agent.type_error("Iterator.concat: the iterator is not an object");
            };
            *inner = Some(ops::get_iterator_direct(agent, iter)?);
        },
        HelperKind::Zip {
            iters,
            open,
            mode,
            padding,
            keys,
        } => {
            let count = iters.len();
            if count == 0 {
                return Ok(None);
            }
            let mut results = Vec::with_capacity(count);
            for i in 0..count {
                if !open[i] {
                    results.push(padding[i]);
                    continue;
                }
                let result = match ops::iterator_step_value(agent, &mut iters[i]) {
                    Err(e) => {
                        open[i] = false;
                        return close_open(agent, iters, open, Err(e));
                    }
                    Ok(r) => r,
                };
                let value = match result {
                    Some(v) => v,
                    None => {
                        open[i] = false;
                        match mode {
                            ZipMode::Shortest => {
                                close_open(agent, iters, open, Ok(()))?;
                                return Ok(None);
                            }
                            ZipMode::Strict => {
                                if i != 0 {
                                    let e = agent.throw_error(
                                        ErrorKind::Type,
                                        "Iterator.zip: the iterators have different lengths",
                                    );
                                    return close_open(agent, iters, open, Err(e));
                                }
                                for k in 1..count {
                                    match ops::iterator_step(agent, &mut iters[k]) {
                                        Err(e) => {
                                            open[k] = false;
                                            return close_open(agent, iters, open, Err(e));
                                        }
                                        Ok(None) => open[k] = false,
                                        Ok(Some(_)) => {
                                            let e = agent.throw_error(
                                                ErrorKind::Type,
                                                "Iterator.zip: the iterators have different lengths",
                                            );
                                            return close_open(agent, iters, open, Err(e));
                                        }
                                    }
                                }
                                return Ok(None);
                            }
                            ZipMode::Longest => {
                                if !open.iter().any(|o| *o) {
                                    return Ok(None);
                                }
                                padding[i]
                            }
                        }
                    }
                };
                results.push(value);
            }
            let out = match keys {
                None => array_of(agent, &results),
                Some(keys) => {
                    let obj = ops::ordinary_object_create(agent, None);
                    for (k, v) in keys.iter().zip(results) {
                        ops::create_data_property_or_throw(agent, obj, *k, v)?;
                    }
                    Value::Object(obj)
                }
            };
            Ok(Some(out))
        }
    }
}

// ----- %Iterator.prototype% methods -----

fn this_object(agent: &mut Agent, this: Value) -> JsResult<Object> {
    match this {
        Value::Object(o) => Ok(o),
        _ => agent.type_error("Iterator.prototype method called on a non-object"),
    }
}

/// Closes `o` (an Iterator Record whose `[[NextMethod]]` is not read yet)
/// with a new error.
fn close_with_error<T>(
    agent: &mut Agent,
    o: Object,
    kind: ErrorKind,
    message: &str,
) -> JsResult<T> {
    let record = IteratorRecord {
        iterator: o,
        next_method: Value::Undefined,
        done: false,
    };
    let e = agent.throw_error(kind, message);
    ops::iterator_close(agent, &record, Err(e))
}

/// The common start of the helpers that take a callback: the receiver,
/// the callable check (closing the receiver if it fails) and
/// GetIteratorDirect.
fn with_callback(
    agent: &mut Agent,
    call: &NativeCall<'_>,
    name: &str,
) -> JsResult<(IteratorRecord, Value)> {
    let o = this_object(agent, call.this)?;
    let f = call.arg(0);
    if !ops::is_callable(agent, f) {
        return close_with_error(
            agent,
            o,
            ErrorKind::Type,
            &format!("Iterator.prototype.{name}: the argument is not a function"),
        );
    }
    let record = ops::get_iterator_direct(agent, o)?;
    Ok((record, f))
}

/// The limit of `take` and `drop`, closing the receiver if it is invalid.
fn limit_argument(agent: &mut Agent, o: Object, limit: Value) -> JsResult<f64> {
    let record = IteratorRecord {
        iterator: o,
        next_method: Value::Undefined,
        done: false,
    };
    let num = ops::to_number(agent, limit);
    let num = ops::if_abrupt_close_iterator(agent, &record, num)?;
    if num.is_nan() {
        return close_with_error(agent, o, ErrorKind::Range, "the limit is NaN");
    }
    let int = ops::f64_to_integer_or_infinity(num);
    if int < 0.0 {
        return close_with_error(agent, o, ErrorKind::Range, "the limit is negative");
    }
    Ok(int)
}

fn proto_map(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let (record, mapper) = with_callback(agent, &call, "map")?;
    Ok(create_helper(
        agent,
        Some(record),
        HelperKind::Map {
            mapper,
            counter: 0.0,
        },
    ))
}

fn proto_filter(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let (record, predicate) = with_callback(agent, &call, "filter")?;
    Ok(create_helper(
        agent,
        Some(record),
        HelperKind::Filter {
            predicate,
            counter: 0.0,
        },
    ))
}

fn proto_take(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let o = this_object(agent, call.this)?;
    let remaining = limit_argument(agent, o, call.arg(0))?;
    let record = ops::get_iterator_direct(agent, o)?;
    Ok(create_helper(
        agent,
        Some(record),
        HelperKind::Take { remaining },
    ))
}

fn proto_drop(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let o = this_object(agent, call.this)?;
    let remaining = limit_argument(agent, o, call.arg(0))?;
    let record = ops::get_iterator_direct(agent, o)?;
    Ok(create_helper(
        agent,
        Some(record),
        HelperKind::Drop { remaining },
    ))
}

fn proto_flat_map(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let (record, mapper) = with_callback(agent, &call, "flatMap")?;
    Ok(create_helper(
        agent,
        Some(record),
        HelperKind::FlatMap {
            mapper,
            counter: 0.0,
            inner: None,
        },
    ))
}

/// The size argument of `chunks` and `windows`: an integral Number in
/// 1..=2^32-1, not coerced.
fn size_argument(agent: &mut Agent, o: Object, v: Value) -> JsResult<usize> {
    let Value::Number(n) = v else {
        return close_with_error(agent, o, ErrorKind::Type, "the size is not a number");
    };
    if !ops::is_integral_number(v) {
        return close_with_error(agent, o, ErrorKind::Type, "the size is not an integer");
    }
    if !(1.0..=4294967295.0).contains(&n) {
        return close_with_error(agent, o, ErrorKind::Range, "the size is out of range");
    }
    Ok(n as usize)
}

fn proto_chunks(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let o = this_object(agent, call.this)?;
    let size = size_argument(agent, o, call.arg(0))?;
    let record = ops::get_iterator_direct(agent, o)?;
    Ok(create_helper(
        agent,
        Some(record),
        HelperKind::Chunks {
            size,
            finishing: false,
        },
    ))
}

fn proto_windows(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let o = this_object(agent, call.this)?;
    let size = size_argument(agent, o, call.arg(0))?;
    let undersized = call.arg(1);
    let allow_partial = match undersized {
        Value::Undefined => false,
        Value::String(s) => match agent.heap.string_to_rust(s).as_str() {
            "only-full" => false,
            "allow-partial" => true,
            _ => {
                return close_with_error(
                    agent,
                    o,
                    ErrorKind::Type,
                    "undersized must be \"only-full\" or \"allow-partial\"",
                );
            }
        },
        _ => {
            return close_with_error(
                agent,
                o,
                ErrorKind::Type,
                "undersized must be \"only-full\" or \"allow-partial\"",
            );
        }
    };
    let record = ops::get_iterator_direct(agent, o)?;
    Ok(create_helper(
        agent,
        Some(record),
        HelperKind::Windows {
            size,
            allow_partial,
            buffer: VecDeque::new(),
            finishing: false,
        },
    ))
}

fn proto_reduce(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let (mut record, reducer) = with_callback(agent, &call, "reduce")?;
    let (mut accumulator, mut counter) = if call.args.len() < 2 {
        match ops::iterator_step_value(agent, &mut record)? {
            Some(v) => (v, 1.0),
            None => {
                return agent.type_error("reduce of an empty iterator with no initial value");
            }
        }
    } else {
        (call.arg(1), 0.0)
    };
    while let Some(value) = ops::iterator_step_value(agent, &mut record)? {
        let r = ops::call(
            agent,
            reducer,
            Value::Undefined,
            &[accumulator, value, Value::Number(counter)],
        );
        accumulator = ops::if_abrupt_close_iterator(agent, &record, r)?;
        counter += 1.0;
    }
    Ok(accumulator)
}

fn proto_to_array(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let o = this_object(agent, call.this)?;
    let mut record = ops::get_iterator_direct(agent, o)?;
    let items = ops::iterator_to_list(agent, &mut record)?;
    Ok(array_of(agent, &items))
}

fn proto_for_each(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let (mut record, f) = with_callback(agent, &call, "forEach")?;
    let mut counter = 0.0;
    while let Some(value) = ops::iterator_step_value(agent, &mut record)? {
        let r = ops::call(agent, f, Value::Undefined, &[value, Value::Number(counter)]);
        ops::if_abrupt_close_iterator(agent, &record, r)?;
        counter += 1.0;
    }
    Ok(Value::Undefined)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Search {
    Some,
    Every,
    Find,
}

/// `some`, `every` and `find`.
fn search(agent: &mut Agent, call: &NativeCall<'_>, which: Search) -> JsResult<Value> {
    let name = match which {
        Search::Some => "some",
        Search::Every => "every",
        Search::Find => "find",
    };
    let (mut record, predicate) = with_callback(agent, call, name)?;
    let mut counter = 0.0;
    while let Some(value) = ops::iterator_step_value(agent, &mut record)? {
        let r = ops::call(
            agent,
            predicate,
            Value::Undefined,
            &[value, Value::Number(counter)],
        );
        let r = ops::if_abrupt_close_iterator(agent, &record, r)?;
        let hit = ops::to_boolean(agent, r);
        let result = match which {
            Search::Some if hit => Some(Value::Boolean(true)),
            Search::Every if !hit => Some(Value::Boolean(false)),
            Search::Find if hit => Some(value),
            _ => None,
        };
        if let Some(result) = result {
            return ops::iterator_close(agent, &record, Ok(result));
        }
        counter += 1.0;
    }
    Ok(match which {
        Search::Some => Value::Boolean(false),
        Search::Every => Value::Boolean(true),
        Search::Find => Value::Undefined,
    })
}

fn proto_some(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    search(agent, &call, Search::Some)
}

fn proto_every(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    search(agent, &call, Search::Every)
}

fn proto_find(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    search(agent, &call, Search::Find)
}

/// `Iterator.prototype.includes(searchElement[, skippedElements])`.
fn proto_includes(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let o = this_object(agent, call.this)?;
    let (search_element, skipped) = (call.arg(0), call.arg(1));
    let to_skip = match skipped {
        Value::Undefined => 0.0,
        Value::Number(n) if n.is_infinite() || ops::is_integral_number(skipped) => n,
        _ => {
            return close_with_error(
                agent,
                o,
                ErrorKind::Type,
                "skippedElements is not an integer",
            );
        }
    };
    if to_skip < 0.0 {
        return close_with_error(agent, o, ErrorKind::Range, "skippedElements is negative");
    }
    if to_skip.is_finite() && to_skip > MAX_SAFE_INTEGER {
        return close_with_error(agent, o, ErrorKind::Range, "skippedElements is too large");
    }
    let mut record = ops::get_iterator_direct(agent, o)?;
    let mut skipped_count = 0.0;
    while let Some(value) = ops::iterator_step_value(agent, &mut record)? {
        if skipped_count < to_skip {
            skipped_count += 1.0;
        } else if ops::same_value_zero(agent, value, search_element) {
            return ops::iterator_close(agent, &record, Ok(Value::Boolean(true)));
        }
    }
    Ok(Value::Boolean(false))
}

// ----- Iterator.concat, Iterator.zip, Iterator.zipKeyed -----

/// `Iterator.concat(...items)`.
fn iterator_concat(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let mut iterables = Vec::with_capacity(call.args.len());
    for &item in call.args {
        if !item.is_object() {
            return agent.type_error("Iterator.concat: an argument is not an object");
        }
        let key = PropertyKey::Symbol(agent.symbols.iterator);
        let Some(method) = ops::get_method(agent, item, key)? else {
            return agent.type_error("Iterator.concat: an argument is not iterable");
        };
        iterables.push((method, item));
    }
    Ok(create_helper(
        agent,
        None,
        HelperKind::Concat {
            iterables,
            next: 0,
            inner: None,
        },
    ))
}

/// GetOptionsObject, then the `mode` and `padding` options.
fn zip_options(agent: &mut Agent, options: Value) -> JsResult<(ZipMode, Value)> {
    let options = match options {
        Value::Undefined => ops::ordinary_object_create(agent, None),
        Value::Object(o) => o,
        _ => return agent.type_error("the options argument is not an object"),
    };
    let key = agent.heap.key_from_str("mode");
    let mode = match ops::get(agent, options, key)? {
        Value::Undefined => ZipMode::Shortest,
        Value::String(s) => match agent.heap.string_to_rust(s).as_str() {
            "shortest" => ZipMode::Shortest,
            "longest" => ZipMode::Longest,
            "strict" => ZipMode::Strict,
            _ => return agent.type_error("mode must be \"shortest\", \"longest\" or \"strict\""),
        },
        _ => return agent.type_error("mode must be \"shortest\", \"longest\" or \"strict\""),
    };
    let mut padding = Value::Undefined;
    if mode == ZipMode::Longest {
        let key = agent.heap.key_from_str("padding");
        padding = ops::get(agent, options, key)?;
        if !padding.is_undefined() && !padding.is_object() {
            return agent.type_error("the padding option is not an object");
        }
    }
    Ok((mode, padding))
}

fn zip_helper(
    agent: &mut Agent,
    iters: Vec<IteratorRecord>,
    mode: ZipMode,
    padding: Vec<Value>,
    keys: Option<Vec<PropertyKey>>,
) -> Value {
    let open = vec![true; iters.len()];
    create_helper(
        agent,
        None,
        HelperKind::Zip {
            iters,
            open,
            mode,
            padding,
            keys,
        },
    )
}

/// `Iterator.zip(iterables[, options])`.
fn iterator_zip(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let iterables = call.arg(0);
    if !iterables.is_object() {
        return agent.type_error("Iterator.zip: the iterables argument is not an object");
    }
    let (mode, padding_option) = zip_options(agent, call.arg(1))?;
    let mut iters: Vec<IteratorRecord> = Vec::new();
    let mut input = ops::get_iterator(agent, iterables)?;
    loop {
        let next = match ops::iterator_step_value(agent, &mut input) {
            Ok(n) => n,
            Err(e) => return ops::iterator_close_all(agent, &iters, Err(e)),
        };
        let Some(next) = next else {
            break;
        };
        match ops::get_iterator_flattenable(agent, next, PrimitiveHandling::RejectPrimitives) {
            Ok(iter) => iters.push(iter),
            Err(e) => {
                let mut all = vec![input];
                all.extend(iters.iter().copied());
                return ops::iterator_close_all(agent, &all, Err(e));
            }
        }
    }
    let count = iters.len();
    let mut padding = Vec::with_capacity(count);
    if mode == ZipMode::Longest {
        if padding_option.is_undefined() {
            padding.resize(count, Value::Undefined);
        } else {
            let mut padding_iter = match ops::get_iterator(agent, padding_option) {
                Ok(p) => p,
                Err(e) => return ops::iterator_close_all(agent, &iters, Err(e)),
            };
            let mut using_iterator = true;
            for _ in 0..count {
                if using_iterator {
                    match ops::iterator_step_value(agent, &mut padding_iter) {
                        Err(e) => return ops::iterator_close_all(agent, &iters, Err(e)),
                        Ok(None) => using_iterator = false,
                        Ok(Some(v)) => padding.push(v),
                    }
                }
                if !using_iterator {
                    padding.push(Value::Undefined);
                }
            }
            if using_iterator && let Err(e) = ops::iterator_close(agent, &padding_iter, Ok(())) {
                return ops::iterator_close_all(agent, &iters, Err(e));
            }
        }
    }
    Ok(zip_helper(agent, iters, mode, padding, None))
}

/// `Iterator.zipKeyed(iterables[, options])`.
fn iterator_zip_keyed(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let Value::Object(iterables) = call.arg(0) else {
        return agent.type_error("Iterator.zipKeyed: the iterables argument is not an object");
    };
    let (mode, padding_option) = zip_options(agent, call.arg(1))?;
    let mut iters: Vec<IteratorRecord> = Vec::new();
    let mut keys: Vec<PropertyKey> = Vec::new();
    let all_keys = internal::own_property_keys(agent, iterables)?;
    for key in all_keys {
        let desc = match internal::get_own_property(agent, iterables, key) {
            Ok(d) => d,
            Err(e) => return ops::iterator_close_all(agent, &iters, Err(e)),
        };
        if desc.is_none_or(|d| d.enumerable != Some(true)) {
            continue;
        }
        let value = match ops::get(agent, iterables, key) {
            Ok(v) => v,
            Err(e) => return ops::iterator_close_all(agent, &iters, Err(e)),
        };
        if value.is_undefined() {
            continue;
        }
        keys.push(key);
        match ops::get_iterator_flattenable(agent, value, PrimitiveHandling::RejectPrimitives) {
            Ok(iter) => iters.push(iter),
            Err(e) => return ops::iterator_close_all(agent, &iters, Err(e)),
        }
    }
    let count = iters.len();
    let mut padding = Vec::with_capacity(count);
    if mode == ZipMode::Longest {
        match padding_option {
            Value::Object(p) => {
                for &key in &keys {
                    match ops::get(agent, p, key) {
                        Ok(v) => padding.push(v),
                        Err(e) => return ops::iterator_close_all(agent, &iters, Err(e)),
                    }
                }
            }
            _ => padding.resize(count, Value::Undefined),
        }
    }
    Ok(zip_helper(agent, iters, mode, padding, Some(keys)))
}
