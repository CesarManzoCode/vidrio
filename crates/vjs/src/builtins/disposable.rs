//! Explicit resource management (ECMA-262 §27.3–27.4): `DisposableStack`,
//! `AsyncDisposableStack` and `%AsyncIteratorPrototype%[@@asyncDispose]`.
//! `%Iterator.prototype%[@@dispose]` lives with the iterators and the
//! `using` declarations in the interpreter.
//!
//! A stack keeps its DisposableResource records in order; disposal walks
//! them from the newest and folds the errors into `SuppressedError`s.
//! `disposeAsync` runs as a chain of promise reactions over a separate
//! state object that takes the resources out of the stack.

use crate::agent::Agent;
use crate::builtins::{
    accessor, constructor, intrinsic, method, method_key, plain_object, set_intrinsic,
    to_string_tag, value,
};
use crate::completion::{JsResult, Throw};
use crate::function::{NativeCall, create_native_function, native_slots};
use crate::gc::{Object, Realm, Trace, Tracer};
use crate::interp::exec::suppressed_error;
use crate::interp::promise::{
    Capability, new_intrinsic_capability, perform_promise_then, promise_resolve,
};
use crate::object::ObjectKind;
use crate::ops;
use crate::realm::Intrinsic;
use crate::value::{PropertyKey, Value};

/// How a resource is disposed (the `[[Hint]]` of DisposableResource).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hint {
    Sync,
    Async,
}

/// A DisposableResource Record: `[[ResourceValue]]`, `[[Hint]]` and
/// `[[DisposeMethod]]` (`undefined` for the `null`/`undefined` values of
/// an async stack, which only await).
#[derive(Clone, Copy, Debug)]
pub struct Resource {
    pub value: Value,
    pub hint: Hint,
    pub method: Value,
}

pub struct DisposableStackData {
    pub is_async: bool,
    /// `[[DisposableState]]` is disposed.
    pub disposed: bool,
    /// `[[DisposeCapability]].[[DisposableResourceStack]]`.
    pub resources: Vec<Resource>,
    /// The completion of a running `disposeAsync`: `Some(error)` once a
    /// dispose method threw.
    pub error: Option<Value>,
}

impl Trace for DisposableStackData {
    fn trace(&self, tracer: &mut Tracer) {
        for r in &self.resources {
            tracer.value(r.value);
            tracer.value(r.method);
        }
        if let Some(e) = self.error {
            tracer.value(e);
        }
    }
}

pub(crate) fn install(agent: &mut Agent, realm: Realm) {
    let object_proto = intrinsic(agent, realm, Intrinsic::ObjectPrototype);

    // DisposableStack.
    let proto = plain_object(agent, object_proto);
    set_intrinsic(agent, realm, Intrinsic::DisposableStackPrototype, proto);
    let ctor = constructor(
        agent,
        realm,
        "DisposableStack",
        disposable_stack_constructor,
        0,
        proto,
        None,
    );
    set_intrinsic(agent, realm, Intrinsic::DisposableStack, ctor);
    method(agent, realm, proto, "adopt", sync_adopt, 2);
    method(agent, realm, proto, "defer", sync_defer, 1);
    let dispose = method(agent, realm, proto, "dispose", sync_dispose, 0);
    let key = agent.heap.key_from_str("disposed");
    accessor(agent, realm, proto, key, Some(sync_disposed), None);
    method(agent, realm, proto, "move", sync_move, 0);
    method(agent, realm, proto, "use", sync_use, 1);
    let key = PropertyKey::Symbol(agent.symbols.dispose);
    value(agent, proto, key, Value::Object(dispose), true, false, true);
    to_string_tag(agent, proto, "DisposableStack");

    // AsyncDisposableStack.
    let proto = plain_object(agent, object_proto);
    set_intrinsic(
        agent,
        realm,
        Intrinsic::AsyncDisposableStackPrototype,
        proto,
    );
    let ctor = constructor(
        agent,
        realm,
        "AsyncDisposableStack",
        async_disposable_stack_constructor,
        0,
        proto,
        None,
    );
    set_intrinsic(agent, realm, Intrinsic::AsyncDisposableStack, ctor);
    method(agent, realm, proto, "adopt", async_adopt, 2);
    method(agent, realm, proto, "defer", async_defer, 1);
    let dispose = method(agent, realm, proto, "disposeAsync", async_dispose_async, 0);
    let key = agent.heap.key_from_str("disposed");
    accessor(agent, realm, proto, key, Some(async_disposed), None);
    method(agent, realm, proto, "move", async_move, 0);
    method(agent, realm, proto, "use", async_use, 1);
    let key = PropertyKey::Symbol(agent.symbols.async_dispose);
    value(agent, proto, key, Value::Object(dispose), true, false, true);
    to_string_tag(agent, proto, "AsyncDisposableStack");

    // %AsyncIteratorPrototype%[@@asyncDispose] (§27.1.3.1).
    let async_iterator_proto = intrinsic(agent, realm, Intrinsic::AsyncIteratorPrototype);
    let key = PropertyKey::Symbol(agent.symbols.async_dispose);
    method_key(
        agent,
        realm,
        async_iterator_proto,
        key,
        async_iterator_async_dispose,
        0,
    );
}

fn new_data(is_async: bool) -> ObjectKind {
    ObjectKind::DisposableStack(Box::new(DisposableStackData {
        is_async,
        disposed: false,
        resources: Vec::new(),
        error: None,
    }))
}

fn data(agent: &Agent, o: Object) -> Option<&DisposableStackData> {
    match &agent.heap.object(o).kind {
        ObjectKind::DisposableStack(d) => Some(d),
        _ => None,
    }
}

fn data_mut(agent: &mut Agent, o: Object) -> &mut DisposableStackData {
    match &mut agent.heap.object_mut(o).kind {
        ObjectKind::DisposableStack(d) => d,
        _ => unreachable!("checked by this_stack"),
    }
}

/// RequireInternalSlot(this, [[DisposableState]]) for a stack of the given
/// kind.
fn this_stack(agent: &mut Agent, this: Value, is_async: bool) -> JsResult<Object> {
    if let Value::Object(o) = this
        && data(agent, o).is_some_and(|d| d.is_async == is_async)
    {
        return Ok(o);
    }
    let name = if is_async {
        "AsyncDisposableStack"
    } else {
        "DisposableStack"
    };
    agent.type_error(&format!("this is not a {name}"))
}

/// The checks of `adopt`, `defer`, `move` and `use`: a stack of the kind
/// that is not disposed yet.
fn pending_stack(agent: &mut Agent, this: Value, is_async: bool) -> JsResult<Object> {
    let o = this_stack(agent, this, is_async)?;
    if data(agent, o).is_some_and(|d| d.disposed) {
        return agent.reference_error("the stack is already disposed");
    }
    Ok(o)
}

fn push(agent: &mut Agent, o: Object, resource: Resource) {
    data_mut(agent, o).resources.push(resource);
}

// ----- The constructors -----

fn create_stack(agent: &mut Agent, call: &NativeCall<'_>, is_async: bool) -> JsResult<Value> {
    let Some(new_target) = call.new_target else {
        return agent.type_error("constructor requires 'new'");
    };
    let proto = if is_async {
        Intrinsic::AsyncDisposableStackPrototype
    } else {
        Intrinsic::DisposableStackPrototype
    };
    let o = ops::ordinary_create_from_constructor(agent, new_target, proto, new_data(is_async))?;
    Ok(Value::Object(o))
}

/// `DisposableStack()` (§27.3.1.1).
fn disposable_stack_constructor(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    create_stack(agent, &call, false)
}

/// `AsyncDisposableStack()` (§27.4.1.1).
fn async_disposable_stack_constructor(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    create_stack(agent, &call, true)
}

// ----- Shared methods -----

/// The closure of `adopt`: calls `onDispose(value)`. Slots: onDispose,
/// value.
fn adopt_closure(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let slots = native_slots(agent, call.callee);
    let (on_dispose, v) = (slots[0], slots[1]);
    ops::call(agent, on_dispose, Value::Undefined, &[v])
}

fn adopt(agent: &mut Agent, call: &NativeCall<'_>, is_async: bool) -> JsResult<Value> {
    let o = pending_stack(agent, call.this, is_async)?;
    let (v, on_dispose) = (call.arg(0), call.arg(1));
    if !ops::is_callable(agent, on_dispose) {
        return agent.type_error("onDispose is not callable");
    }
    let f = create_native_function(agent, adopt_closure, 0, "", vec![on_dispose, v], None);
    let hint = if is_async { Hint::Async } else { Hint::Sync };
    push(
        agent,
        o,
        Resource {
            value: Value::Undefined,
            hint,
            method: Value::Object(f),
        },
    );
    Ok(v)
}

fn defer(agent: &mut Agent, call: &NativeCall<'_>, is_async: bool) -> JsResult<Value> {
    let o = pending_stack(agent, call.this, is_async)?;
    let on_dispose = call.arg(0);
    if !ops::is_callable(agent, on_dispose) {
        return agent.type_error("onDispose is not callable");
    }
    let hint = if is_async { Hint::Async } else { Hint::Sync };
    push(
        agent,
        o,
        Resource {
            value: Value::Undefined,
            hint,
            method: on_dispose,
        },
    );
    Ok(Value::Undefined)
}

fn disposed(agent: &mut Agent, call: &NativeCall<'_>, is_async: bool) -> JsResult<Value> {
    let o = this_stack(agent, call.this, is_async)?;
    Ok(Value::Boolean(data(agent, o).is_some_and(|d| d.disposed)))
}

fn move_stack(agent: &mut Agent, call: &NativeCall<'_>, is_async: bool) -> JsResult<Value> {
    let o = pending_stack(agent, call.this, is_async)?;
    let proto = if is_async {
        Intrinsic::AsyncDisposableStackPrototype
    } else {
        Intrinsic::DisposableStackPrototype
    };
    let proto = agent.intrinsic(proto);
    let d = data_mut(agent, o);
    let resources = std::mem::take(&mut d.resources);
    d.disposed = true;
    let fresh = agent.heap.create_object(Some(proto), new_data(is_async));
    data_mut(agent, fresh).resources = resources;
    Ok(Value::Object(fresh))
}

/// GetDisposeMethod(V, hint) (§7.5.3.4 in the explicit resource
/// management section): for the async hint, a sync `@@dispose` is wrapped
/// so that its result is not awaited.
fn get_dispose_method(agent: &mut Agent, v: Value, hint: Hint) -> JsResult<Option<Value>> {
    if hint == Hint::Async {
        let key = PropertyKey::Symbol(agent.symbols.async_dispose);
        if let Some(m) = ops::get_method(agent, v, key)? {
            return Ok(Some(m));
        }
        let key = PropertyKey::Symbol(agent.symbols.dispose);
        return Ok(match ops::get_method(agent, v, key)? {
            Some(m) => Some(Value::Object(create_native_function(
                agent,
                sync_dispose_as_async,
                0,
                "",
                vec![m],
                None,
            ))),
            None => None,
        });
    }
    let key = PropertyKey::Symbol(agent.symbols.dispose);
    ops::get_method(agent, v, key)
}

/// The closure GetDisposeMethod makes around a sync `@@dispose` for the
/// async hint: a promise for `undefined`, rejected if the method throws.
fn sync_dispose_as_async(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let method = native_slots(agent, call.callee)[0];
    let realm = agent.current_realm();
    let cap = new_intrinsic_capability(agent, realm);
    match ops::call(agent, method, call.this, &[]) {
        Ok(_) => ops::call(agent, cap.resolve, Value::Undefined, &[Value::Undefined])?,
        Err(Throw(e)) => ops::call(agent, cap.reject, Value::Undefined, &[e])?,
    };
    Ok(cap.promise)
}

fn use_resource(agent: &mut Agent, call: &NativeCall<'_>, is_async: bool) -> JsResult<Value> {
    let o = pending_stack(agent, call.this, is_async)?;
    let v = call.arg(0);
    let hint = if is_async { Hint::Async } else { Hint::Sync };
    // AddDisposableResource(disposeCapability, value, hint).
    if v.is_nullish() {
        if is_async {
            push(
                agent,
                o,
                Resource {
                    value: Value::Undefined,
                    hint,
                    method: Value::Undefined,
                },
            );
        }
        return Ok(v);
    }
    if !v.is_object() {
        return agent.type_error("the value is not an object");
    }
    let Some(method) = get_dispose_method(agent, v, hint)? else {
        return agent.type_error("the value is not disposable");
    };
    push(
        agent,
        o,
        Resource {
            value: v,
            hint,
            method,
        },
    );
    Ok(v)
}

/// The completion after a dispose method threw `e` while the completion
/// was `previous`.
fn fold_error(agent: &mut Agent, previous: Option<Value>, e: Value) -> Value {
    match previous {
        Some(p) => Value::Object(suppressed_error(agent, e, p)),
        None => e,
    }
}

// ----- DisposableStack.prototype -----

fn sync_adopt(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    adopt(agent, &call, false)
}

fn sync_defer(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    defer(agent, &call, false)
}

/// `dispose()` and `[@@dispose]()`: DisposeResources with a normal
/// completion.
fn sync_dispose(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let o = this_stack(agent, call.this, false)?;
    let d = data_mut(agent, o);
    if d.disposed {
        return Ok(Value::Undefined);
    }
    d.disposed = true;
    let resources = std::mem::take(&mut d.resources);
    let mut error = None;
    for r in resources.iter().rev() {
        if r.method.is_undefined() {
            continue;
        }
        if let Err(Throw(e)) = ops::call(agent, r.method, r.value, &[]) {
            error = Some(fold_error(agent, error, e));
        }
    }
    match error {
        Some(e) => Err(Throw(e)),
        None => Ok(Value::Undefined),
    }
}

fn sync_disposed(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    disposed(agent, &call, false)
}

fn sync_move(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    move_stack(agent, &call, false)
}

fn sync_use(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    use_resource(agent, &call, false)
}

// ----- AsyncDisposableStack.prototype -----

fn async_adopt(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    adopt(agent, &call, true)
}

fn async_defer(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    defer(agent, &call, true)
}

fn async_disposed(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    disposed(agent, &call, true)
}

fn async_move(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    move_stack(agent, &call, true)
}

fn async_use(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    use_resource(agent, &call, true)
}

/// `disposeAsync()` and `[@@asyncDispose]()` (§27.4.3.4).
fn async_dispose_async(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let realm = agent.current_realm();
    let cap = new_intrinsic_capability(agent, realm);
    let o = match this_stack(agent, call.this, true) {
        Ok(o) => o,
        Err(Throw(e)) => {
            ops::call(agent, cap.reject, Value::Undefined, &[e])?;
            return Ok(cap.promise);
        }
    };
    let d = data_mut(agent, o);
    if d.disposed {
        ops::call(agent, cap.resolve, Value::Undefined, &[Value::Undefined])?;
        return Ok(cap.promise);
    }
    d.disposed = true;
    let resources = std::mem::take(&mut d.resources);
    // The state of this disposal: the remaining resources and the error so
    // far, in a stack object that code never sees.
    let state = agent.heap.create_object(None, new_data(true));
    data_mut(agent, state).resources = resources;
    let slots = vec![Value::Object(state), cap.promise, cap.resolve, cap.reject];
    async_dispose_continue(agent, &slots)?;
    Ok(cap.promise)
}

/// Disposes resources of a running `disposeAsync` until one needs an
/// await (then the reactions resume here) or none is left (then the
/// promise settles). `slots`: state, promise, resolve, reject.
fn async_dispose_continue(agent: &mut Agent, slots: &[Value]) -> JsResult<()> {
    let Value::Object(state) = slots[0] else {
        return Ok(());
    };
    loop {
        let Some(r) = data_mut(agent, state).resources.pop() else {
            let error = data_mut(agent, state).error.take();
            match error {
                Some(e) => ops::call(agent, slots[3], Value::Undefined, &[e])?,
                None => ops::call(agent, slots[2], Value::Undefined, &[Value::Undefined])?,
            };
            return Ok(());
        };
        let result = if r.method.is_undefined() {
            Ok(Value::Undefined)
        } else {
            ops::call(agent, r.method, r.value, &[])
        };
        let awaited = result.and_then(|v| {
            if r.hint == Hint::Async {
                let promise_ctor = agent.intrinsic(Intrinsic::Promise);
                promise_resolve(agent, promise_ctor, v).map(Some)
            } else {
                Ok(None)
            }
        });
        match awaited {
            Ok(Some(Value::Object(p))) => {
                let on_fulfilled = create_native_function(
                    agent,
                    async_dispose_fulfilled,
                    1,
                    "",
                    slots.to_vec(),
                    None,
                );
                let on_rejected = create_native_function(
                    agent,
                    async_dispose_rejected,
                    1,
                    "",
                    slots.to_vec(),
                    None,
                );
                perform_promise_then(
                    agent,
                    p,
                    Value::Object(on_fulfilled),
                    Value::Object(on_rejected),
                    None::<Capability>,
                );
                return Ok(());
            }
            Ok(_) => {}
            Err(Throw(e)) => {
                let previous = data_mut(agent, state).error.take();
                let folded = fold_error(agent, previous, e);
                data_mut(agent, state).error = Some(folded);
            }
        }
    }
}

fn async_dispose_fulfilled(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let slots = native_slots(agent, call.callee).to_vec();
    async_dispose_continue(agent, &slots)?;
    Ok(Value::Undefined)
}

fn async_dispose_rejected(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let slots = native_slots(agent, call.callee).to_vec();
    if let Value::Object(state) = slots[0] {
        let previous = data_mut(agent, state).error.take();
        let folded = fold_error(agent, previous, call.arg(0));
        data_mut(agent, state).error = Some(folded);
    }
    async_dispose_continue(agent, &slots)?;
    Ok(Value::Undefined)
}

// ----- %AsyncIteratorPrototype%[@@asyncDispose] -----

/// `%AsyncIteratorPrototype%[@@asyncDispose]()`: awaits `this.return()`
/// if there is one and fulfills with `undefined`.
fn async_iterator_async_dispose(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let realm = agent.current_realm();
    let cap = new_intrinsic_capability(agent, realm);
    let o = call.this;
    let result = (|| -> JsResult<Option<Value>> {
        let key = PropertyKey::String(agent.heap.names.return_);
        let Some(ret) = ops::get_method(agent, o, key)? else {
            return Ok(None);
        };
        let r = ops::call(agent, ret, o, &[Value::Undefined])?;
        let promise_ctor = agent.intrinsic(Intrinsic::Promise);
        promise_resolve(agent, promise_ctor, r).map(Some)
    })();
    match result {
        Ok(None) => {
            ops::call(agent, cap.resolve, Value::Undefined, &[Value::Undefined])?;
        }
        Ok(Some(Value::Object(p))) => {
            let on_fulfilled =
                create_native_function(agent, resolve_undefined, 1, "", vec![cap.resolve], None);
            perform_promise_then(
                agent,
                p,
                Value::Object(on_fulfilled),
                cap.reject,
                None::<Capability>,
            );
        }
        Ok(Some(_)) => {
            ops::call(agent, cap.resolve, Value::Undefined, &[Value::Undefined])?;
        }
        Err(Throw(e)) => {
            ops::call(agent, cap.reject, Value::Undefined, &[e])?;
        }
    }
    Ok(cap.promise)
}

/// Calls the resolve function in slot 0 with `undefined`.
fn resolve_undefined(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let resolve = native_slots(agent, call.callee)[0];
    ops::call(agent, resolve, Value::Undefined, &[Value::Undefined])?;
    Ok(Value::Undefined)
}
