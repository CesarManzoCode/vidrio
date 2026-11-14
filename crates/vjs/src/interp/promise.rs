//! The core of Promise (ECMA-262 §27.2) that `await` needs: promise
//! objects, resolving functions, capabilities, reactions and their jobs
//! (HostEnqueuePromiseJob is `Agent::enqueue_job`), the constructor and
//! `Promise.prototype.then`. The rest of the built-in arrives with the
//! Promise built-ins.

use crate::agent::{Agent, ErrorKind, Job, JobTask};
use crate::completion::{JsResult, Throw};
use crate::function::{NativeCall, create_native_function, native_slots, native_slots_mut};
use crate::gc::{Object, Realm, Trace, Tracer};
use crate::object::ObjectKind;
use crate::ops;
use crate::realm::Intrinsic;
use crate::value::{PropertyKey, Value};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromiseState {
    Pending,
    Fulfilled,
    Rejected,
}

/// PromiseCapability Record.
#[derive(Clone, Copy, Debug)]
pub struct Capability {
    pub promise: Value,
    pub resolve: Value,
    pub reject: Value,
}

impl Trace for Capability {
    fn trace(&self, tracer: &mut Tracer) {
        tracer.value(self.promise);
        tracer.value(self.resolve);
        tracer.value(self.reject);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReactionKind {
    Fulfill,
    Reject,
}

/// PromiseReaction Record.
#[derive(Clone, Copy, Debug)]
pub struct Reaction {
    pub capability: Option<Capability>,
    pub kind: ReactionKind,
    pub handler: Option<Value>,
}

impl Trace for Reaction {
    fn trace(&self, tracer: &mut Tracer) {
        if let Some(c) = &self.capability {
            c.trace(tracer);
        }
        if let Some(h) = self.handler {
            tracer.value(h);
        }
    }
}

/// The internal slots of a promise.
pub struct PromiseData {
    pub state: PromiseState,
    pub result: Value,
    pub fulfill_reactions: Vec<Reaction>,
    pub reject_reactions: Vec<Reaction>,
    pub is_handled: bool,
}

impl Trace for PromiseData {
    fn trace(&self, tracer: &mut Tracer) {
        tracer.value(self.result);
        self.fulfill_reactions.trace(tracer);
        self.reject_reactions.trace(tracer);
    }
}

impl PromiseData {
    pub fn pending() -> PromiseData {
        PromiseData {
            state: PromiseState::Pending,
            result: Value::Undefined,
            fulfill_reactions: Vec::new(),
            reject_reactions: Vec::new(),
            is_handled: false,
        }
    }
}

/// IsPromise(x).
pub fn is_promise(agent: &Agent, v: Value) -> bool {
    matches!(v, Value::Object(o) if matches!(agent.heap.object(o).kind, ObjectKind::Promise(_)))
}

fn promise_data(agent: &mut Agent, p: Object) -> Option<&mut PromiseData> {
    match &mut agent.heap.object_mut(p).kind {
        ObjectKind::Promise(d) => Some(d),
        _ => None,
    }
}

/// The state and result of a promise.
pub fn promise_state(agent: &Agent, p: Object) -> Option<(PromiseState, Value)> {
    match &agent.heap.object(p).kind {
        ObjectKind::Promise(d) => Some((d.state, d.result)),
        _ => None,
    }
}

/// CreateResolvingFunctions(promise).
pub fn create_resolving_functions(agent: &mut Agent, promise: Object) -> (Object, Object) {
    let already = agent.heap.create_object(None, ObjectKind::Boolean(false));
    let slots = vec![Value::Object(promise), Value::Object(already)];
    let resolve = create_native_function(agent, resolve_function, 1, "", slots.clone(), None);
    let reject = create_native_function(agent, reject_function, 1, "", slots, None);
    (resolve, reject)
}

/// Checks and sets `[[AlreadyResolved]]`; returns the promise if it was
/// not resolved yet.
fn take_resolution(agent: &mut Agent, f: Object) -> Option<Object> {
    let slots = native_slots(agent, f);
    let (Value::Object(promise), Value::Object(flag)) = (slots[0], slots[1]) else {
        return None;
    };
    match &mut agent.heap.object_mut(flag).kind {
        ObjectKind::Boolean(b) if !*b => {
            *b = true;
            Some(promise)
        }
        _ => None,
    }
}

fn resolve_function(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let Some(promise) = take_resolution(agent, call.callee) else {
        return Ok(Value::Undefined);
    };
    resolve_promise(agent, promise, call.arg(0));
    Ok(Value::Undefined)
}

/// The steps of a promise resolve function after `[[AlreadyResolved]]`.
fn resolve_promise(agent: &mut Agent, promise: Object, resolution: Value) {
    if resolution == Value::Object(promise) {
        let e = agent.create_error(ErrorKind::Type, "a promise cannot be resolved with itself");
        reject_promise(agent, promise, Value::Object(e));
        return;
    }
    let Value::Object(thenable) = resolution else {
        fulfill_promise(agent, promise, resolution);
        return;
    };
    let then_key = PropertyKey::String(agent.heap.names.then);
    let then = match ops::get(agent, thenable, then_key) {
        Ok(t) => t,
        Err(Throw(e)) => {
            reject_promise(agent, promise, e);
            return;
        }
    };
    if !ops::is_callable(agent, then) {
        fulfill_promise(agent, promise, resolution);
        return;
    }
    let realm = match then {
        Value::Object(t) => ops::get_function_realm(agent, t).ok(),
        _ => None,
    };
    agent.enqueue_job(Job {
        realm: realm.or_else(|| Some(agent.current_realm())),
        task: Box::new(ResolveThenableJob {
            promise,
            thenable: resolution,
            then,
        }),
    });
}

fn reject_function(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let Some(promise) = take_resolution(agent, call.callee) else {
        return Ok(Value::Undefined);
    };
    reject_promise(agent, promise, call.arg(0));
    Ok(Value::Undefined)
}

/// FulfillPromise(promise, value).
pub fn fulfill_promise(agent: &mut Agent, promise: Object, value: Value) {
    let Some(d) = promise_data(agent, promise) else {
        return;
    };
    let reactions = std::mem::take(&mut d.fulfill_reactions);
    d.reject_reactions.clear();
    d.result = value;
    d.state = PromiseState::Fulfilled;
    trigger_reactions(agent, reactions, value);
}

/// RejectPromise(promise, reason).
pub fn reject_promise(agent: &mut Agent, promise: Object, reason: Value) {
    let Some(d) = promise_data(agent, promise) else {
        return;
    };
    let reactions = std::mem::take(&mut d.reject_reactions);
    d.fulfill_reactions.clear();
    d.result = reason;
    d.state = PromiseState::Rejected;
    trigger_reactions(agent, reactions, reason);
}

fn trigger_reactions(agent: &mut Agent, reactions: Vec<Reaction>, argument: Value) {
    for r in reactions {
        enqueue_reaction_job(agent, r, argument);
    }
}

fn enqueue_reaction_job(agent: &mut Agent, reaction: Reaction, argument: Value) {
    let realm = match reaction.handler {
        Some(Value::Object(h)) => ops::get_function_realm(agent, h)
            .ok()
            .or_else(|| Some(agent.current_realm())),
        _ => None,
    };
    agent.enqueue_job(Job {
        realm,
        task: Box::new(ReactionJob { reaction, argument }),
    });
}

/// NewPromiseReactionJob.
struct ReactionJob {
    reaction: Reaction,
    argument: Value,
}

impl Trace for ReactionJob {
    fn trace(&self, tracer: &mut Tracer) {
        self.reaction.trace(tracer);
        tracer.value(self.argument);
    }
}

impl JobTask for ReactionJob {
    fn run(self: Box<Self>, agent: &mut Agent) -> JsResult<()> {
        let r = self.reaction;
        let result = match r.handler {
            None => match r.kind {
                ReactionKind::Fulfill => Ok(self.argument),
                ReactionKind::Reject => Err(Throw(self.argument)),
            },
            Some(h) => ops::call(agent, h, Value::Undefined, &[self.argument]),
        };
        let Some(cap) = r.capability else {
            return result.map(|_| ());
        };
        match result {
            Ok(v) => ops::call(agent, cap.resolve, Value::Undefined, &[v])?,
            Err(Throw(e)) => ops::call(agent, cap.reject, Value::Undefined, &[e])?,
        };
        Ok(())
    }
}

/// NewPromiseResolveThenableJob.
struct ResolveThenableJob {
    promise: Object,
    thenable: Value,
    then: Value,
}

impl Trace for ResolveThenableJob {
    fn trace(&self, tracer: &mut Tracer) {
        tracer.object(self.promise);
        tracer.value(self.thenable);
        tracer.value(self.then);
    }
}

impl JobTask for ResolveThenableJob {
    fn run(self: Box<Self>, agent: &mut Agent) -> JsResult<()> {
        let (resolve, reject) = create_resolving_functions(agent, self.promise);
        let result = ops::call(
            agent,
            self.then,
            self.thenable,
            &[Value::Object(resolve), Value::Object(reject)],
        );
        if let Err(Throw(e)) = result {
            ops::call(agent, Value::Object(reject), Value::Undefined, &[e])?;
        }
        Ok(())
    }
}

/// A pending promise of `%Promise%` of `realm` with its resolving
/// functions (the capability async functions use; no user code runs).
pub fn new_intrinsic_capability(agent: &mut Agent, realm: Realm) -> Capability {
    let proto = agent
        .heap
        .realm(realm)
        .intrinsics
        .get(Intrinsic::PromisePrototype);
    let p = agent.heap.create_object(
        Some(proto),
        ObjectKind::Promise(Box::new(PromiseData::pending())),
    );
    let (resolve, reject) = create_resolving_functions(agent, p);
    Capability {
        promise: Value::Object(p),
        resolve: Value::Object(resolve),
        reject: Value::Object(reject),
    }
}

/// NewPromiseCapability(C).
pub fn new_promise_capability(agent: &mut Agent, c: Value) -> JsResult<Capability> {
    let Value::Object(ctor) = c else {
        return agent.type_error("promise capability constructor is not a constructor");
    };
    if !agent.heap.object(ctor).is_constructor() {
        return agent.type_error("promise capability constructor is not a constructor");
    }
    let executor = create_native_function(
        agent,
        capability_executor,
        2,
        "",
        vec![Value::Undefined, Value::Undefined],
        None,
    );
    let promise = ops::construct(agent, ctor, &[Value::Object(executor)], None)?;
    let slots = native_slots(agent, executor);
    let (resolve, reject) = (slots[0], slots[1]);
    if !ops::is_callable(agent, resolve) || !ops::is_callable(agent, reject) {
        return agent.type_error("promise capability functions are not callable");
    }
    Ok(Capability {
        promise: Value::Object(promise),
        resolve,
        reject,
    })
}

/// GetCapabilitiesExecutor functions.
fn capability_executor(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let slots = native_slots(agent, call.callee);
    if !slots[0].is_undefined() || !slots[1].is_undefined() {
        return agent.type_error("promise capability executor already called");
    }
    let (resolve, reject) = (call.arg(0), call.arg(1));
    if let Some(s) = native_slots_mut(agent, call.callee) {
        s[0] = resolve;
        s[1] = reject;
    }
    Ok(Value::Undefined)
}

/// PromiseResolve(C, x).
pub fn promise_resolve(agent: &mut Agent, c: Object, x: Value) -> JsResult<Value> {
    if is_promise(agent, x) {
        let key = PropertyKey::String(agent.heap.names.constructor);
        let ctor = ops::get_v(agent, x, key)?;
        if ctor == Value::Object(c) {
            return Ok(x);
        }
    }
    let cap = new_promise_capability(agent, Value::Object(c))?;
    ops::call(agent, cap.resolve, Value::Undefined, &[x])?;
    Ok(cap.promise)
}

/// PerformPromiseThen(promise, onFulfilled, onRejected, resultCapability).
pub fn perform_promise_then(
    agent: &mut Agent,
    promise: Object,
    on_fulfilled: Value,
    on_rejected: Value,
    capability: Option<Capability>,
) -> Value {
    let on_fulfilled = ops::is_callable(agent, on_fulfilled).then_some(on_fulfilled);
    let on_rejected = ops::is_callable(agent, on_rejected).then_some(on_rejected);
    let fulfill = Reaction {
        capability,
        kind: ReactionKind::Fulfill,
        handler: on_fulfilled,
    };
    let reject = Reaction {
        capability,
        kind: ReactionKind::Reject,
        handler: on_rejected,
    };
    let Some(d) = promise_data(agent, promise) else {
        return Value::Undefined;
    };
    let state = d.state;
    let result = d.result;
    d.is_handled = true;
    match state {
        PromiseState::Pending => {
            d.fulfill_reactions.push(fulfill);
            d.reject_reactions.push(reject);
        }
        PromiseState::Fulfilled => enqueue_reaction_job(agent, fulfill, result),
        PromiseState::Rejected => enqueue_reaction_job(agent, reject, result),
    }
    capability.map_or(Value::Undefined, |c| c.promise)
}

/// `Promise(executor)`.
pub(crate) fn promise_constructor(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let Some(new_target) = call.new_target else {
        return agent.type_error("Promise constructor cannot be invoked without 'new'");
    };
    let executor = call.arg(0);
    if !ops::is_callable(agent, executor) {
        return agent.type_error("Promise resolver is not a function");
    }
    let p = ops::ordinary_create_from_constructor(
        agent,
        new_target,
        Intrinsic::PromisePrototype,
        ObjectKind::Promise(Box::new(PromiseData::pending())),
    )?;
    let (resolve, reject) = create_resolving_functions(agent, p);
    let result = ops::call(
        agent,
        executor,
        Value::Undefined,
        &[Value::Object(resolve), Value::Object(reject)],
    );
    if let Err(Throw(e)) = result {
        ops::call(agent, Value::Object(reject), Value::Undefined, &[e])?;
    }
    Ok(Value::Object(p))
}

/// `Promise.prototype.then(onFulfilled, onRejected)`.
pub(crate) fn promise_then(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let Value::Object(p) = call.this else {
        return agent.type_error("Promise.prototype.then called on a non-promise");
    };
    if !is_promise(agent, call.this) {
        return agent.type_error("Promise.prototype.then called on a non-promise");
    }
    let default = agent.intrinsic(Intrinsic::Promise);
    let c = ops::species_constructor(agent, p, default)?;
    let cap = new_promise_capability(agent, Value::Object(c))?;
    Ok(perform_promise_then(
        agent,
        p,
        call.arg(0),
        call.arg(1),
        Some(cap),
    ))
}
