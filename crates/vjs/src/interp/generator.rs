//! Generators (ECMA-262 §27.5), async functions (§27.7) and async
//! generators (§27.6): suspended frames kept in their objects, resumption,
//! and the request queue of async generators.

use std::collections::VecDeque;

use crate::agent::Agent;
use crate::completion::{JsResult, Throw};
use crate::function::{NativeCall, create_native_function, native_slots};
use crate::gc::{Object, Trace, Tracer};
use crate::interp::promise::{self, Capability};
use crate::interp::{Frame, Suspend, run};
use crate::object::ObjectKind;
use crate::ops;
use crate::realm::Intrinsic;
use crate::value::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GenState {
    SuspendedStart,
    SuspendedYield,
    Executing,
    AwaitingReturn,
    Completed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GenKind {
    Sync,
    Async,
    /// The suspended body of an async function.
    AsyncFunction,
}

/// Resumption modes: the completion a suspended frame continues with.
pub const MODE_NEXT: i32 = 0;
pub const MODE_THROW: i32 = 1;
pub const MODE_RETURN: i32 = 2;

/// AsyncGeneratorRequest Record.
pub struct AsyncGenRequest {
    pub value: Value,
    pub mode: i32,
    pub capability: Capability,
}

pub struct GeneratorData {
    pub kind: GenKind,
    pub state: GenState,
    pub frame: Option<Box<Frame>>,
    pub queue: VecDeque<AsyncGenRequest>,
    /// The last yield came from `yield*` and returns the inner result
    /// object as is.
    pub raw_yield: bool,
}

impl GeneratorData {
    pub fn new(kind: GenKind) -> GeneratorData {
        GeneratorData {
            kind,
            state: GenState::SuspendedStart,
            frame: None,
            queue: VecDeque::new(),
            raw_yield: false,
        }
    }
}

impl Trace for GeneratorData {
    fn trace(&self, tracer: &mut Tracer) {
        if let Some(f) = &self.frame {
            f.trace(tracer);
        }
        for r in &self.queue {
            tracer.value(r.value);
            r.capability.trace(tracer);
        }
    }
}

fn data_mut(agent: &mut Agent, g: Object) -> Option<&mut GeneratorData> {
    match &mut agent.heap.object_mut(g).kind {
        ObjectKind::Generator(d) => Some(d),
        _ => None,
    }
}

fn data(agent: &Agent, g: Object) -> Option<&GeneratorData> {
    match &agent.heap.object(g).kind {
        ObjectKind::Generator(d) => Some(d),
        _ => None,
    }
}

pub(crate) fn set_completed(agent: &mut Agent, g: Object) {
    if let Some(d) = data_mut(agent, g) {
        d.state = GenState::Completed;
        d.frame = None;
    }
}

/// Creates the generator object of a generator function's first
/// activation (OrdinaryCreateFromConstructor with the function's
/// `prototype`).
pub(crate) fn create_generator(agent: &mut Agent, frame: &Frame) -> JsResult<Object> {
    let is_async = frame.code.flags.is_async;
    let default = if is_async {
        Intrinsic::AsyncGeneratorPrototype
    } else {
        Intrinsic::GeneratorPrototype
    };
    let f = frame.function.expect("generator frame has a function");
    let proto = ops::get_prototype_from_constructor(agent, f, default)?;
    let kind = if is_async {
        GenKind::Async
    } else {
        GenKind::Sync
    };
    Ok(agent.heap.create_object(
        Some(proto),
        ObjectKind::Generator(Box::new(GeneratorData::new(kind))),
    ))
}

/// Stores a suspending frame in its object and returns what its caller
/// receives.
pub(crate) fn suspend_frame(agent: &mut Agent, mut frame: Frame, s: Suspend) -> Value {
    match s {
        Suspend::Initial(g) => {
            frame.generator = Some(g);
            if let Some(d) = data_mut(agent, g) {
                d.state = GenState::SuspendedStart;
                d.frame = Some(Box::new(frame));
            }
            Value::Object(g)
        }
        Suspend::Yield { value, raw } => {
            let g = frame.generator.expect("yield outside a generator");
            if let Some(d) = data_mut(agent, g) {
                d.state = GenState::SuspendedYield;
                d.raw_yield = raw;
                d.frame = Some(Box::new(frame));
            }
            value
        }
        Suspend::AsyncGenYield => {
            let g = frame.generator.expect("yield outside a generator");
            if let Some(d) = data_mut(agent, g) {
                d.state = GenState::SuspendedYield;
                d.frame = Some(Box::new(frame));
            }
            Value::Undefined
        }
        Suspend::Await => {
            let deliver = match &frame.async_cap {
                Some(cap) if !frame.delivered => cap.promise,
                _ => Value::Undefined,
            };
            frame.delivered = true;
            let g = frame.generator.expect("await without a continuation");
            if let Some(d) = data_mut(agent, g) {
                d.frame = Some(Box::new(frame));
            }
            deliver
        }
    }
}

/// The continuation object an async function body suspends into.
pub(crate) fn ensure_continuation(agent: &mut Agent, frame: &mut Frame) -> Object {
    if let Some(g) = frame.generator {
        return g;
    }
    let mut d = GeneratorData::new(GenKind::AsyncFunction);
    d.state = GenState::Executing;
    let g = agent
        .heap
        .create_object(None, ObjectKind::Generator(Box::new(d)));
    frame.generator = Some(g);
    g
}

/// Await(value) for the frame: PromiseResolve and PerformPromiseThen with
/// the continuation as reactions. The caller then suspends the frame.
pub(crate) fn await_value(agent: &mut Agent, frame: &mut Frame, value: Value) -> JsResult<()> {
    let cont = ensure_continuation(agent, frame);
    let ctor = agent.intrinsic(Intrinsic::Promise);
    let p = promise::promise_resolve(agent, ctor, value)?;
    let Value::Object(p) = p else {
        return agent.type_error("await: not a promise");
    };
    let on_fulfilled = create_native_function(
        agent,
        await_fulfilled,
        1,
        "",
        vec![Value::Object(cont)],
        None,
    );
    let on_rejected = create_native_function(
        agent,
        await_rejected,
        1,
        "",
        vec![Value::Object(cont)],
        None,
    );
    promise::perform_promise_then(
        agent,
        p,
        Value::Object(on_fulfilled),
        Value::Object(on_rejected),
        None,
    );
    Ok(())
}

fn await_fulfilled(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let cont = native_slots(agent, call.callee)[0];
    if let Value::Object(g) = cont {
        resume_frame(agent, g, call.arg(0), MODE_NEXT);
    }
    Ok(Value::Undefined)
}

fn await_rejected(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let cont = native_slots(agent, call.callee)[0];
    if let Value::Object(g) = cont {
        resume_frame(agent, g, call.arg(0), MODE_THROW);
    }
    Ok(Value::Undefined)
}

/// Continues the frame suspended in `g` with `[value mode]` on its stack.
/// Errors cannot escape: async bodies settle their promise instead.
fn resume_frame(agent: &mut Agent, g: Object, value: Value, mode: i32) {
    let Some(frame) = data_mut(agent, g).and_then(|d| d.frame.take()) else {
        return;
    };
    let mut frame = *frame;
    frame.push(value);
    frame.push(Value::Number(f64::from(mode)));
    let _ = run(agent, frame);
}

// ----- sync generators -----

/// GeneratorResume / GeneratorResumeAbrupt.
pub fn generator_resume(
    agent: &mut Agent,
    generator: Value,
    value: Value,
    mode: i32,
) -> JsResult<Value> {
    let Value::Object(g) = generator else {
        return agent.type_error("not a generator object");
    };
    let state = match data(agent, g) {
        Some(d) if d.kind == GenKind::Sync => d.state,
        _ => return agent.type_error("not a generator object"),
    };
    match state {
        GenState::Executing => return agent.type_error("generator is already running"),
        GenState::Completed => {
            return match mode {
                MODE_THROW => Err(Throw(value)),
                MODE_RETURN => Ok(Value::Object(ops::create_iter_result_object(
                    agent, value, true,
                ))),
                _ => Ok(Value::Object(ops::create_iter_result_object(
                    agent,
                    Value::Undefined,
                    true,
                ))),
            };
        }
        GenState::SuspendedStart if mode != MODE_NEXT => {
            set_completed(agent, g);
            return if mode == MODE_THROW {
                Err(Throw(value))
            } else {
                Ok(Value::Object(ops::create_iter_result_object(
                    agent, value, true,
                )))
            };
        }
        _ => {}
    }
    let Some(frame) = data_mut(agent, g).and_then(|d| {
        d.state = GenState::Executing;
        d.frame.take()
    }) else {
        return agent.type_error("generator has no frame");
    };
    let mut frame = *frame;
    if state == GenState::SuspendedYield {
        frame.push(value);
        frame.push(Value::Number(f64::from(mode)));
    }
    let result = run(agent, frame);
    let (yielded, raw) = match data(agent, g) {
        Some(d) => (d.frame.is_some(), d.raw_yield),
        None => (false, false),
    };
    let v = result?;
    if yielded {
        if raw {
            return Ok(v);
        }
        return Ok(Value::Object(ops::create_iter_result_object(
            agent, v, false,
        )));
    }
    Ok(Value::Object(ops::create_iter_result_object(
        agent, v, true,
    )))
}

pub(crate) fn generator_next(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    generator_resume(agent, call.this, call.arg(0), MODE_NEXT)
}

pub(crate) fn generator_return(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    generator_resume(agent, call.this, call.arg(0), MODE_RETURN)
}

pub(crate) fn generator_throw(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    generator_resume(agent, call.this, call.arg(0), MODE_THROW)
}

// ----- async generators -----

/// AsyncGeneratorCompleteStep.
pub(crate) fn async_generator_complete_step(
    agent: &mut Agent,
    g: Object,
    completion: Result<Value, Value>,
    done: bool,
) {
    let Some(next) = data_mut(agent, g).and_then(|d| d.queue.pop_front()) else {
        return;
    };
    let cap = next.capability;
    match completion {
        Err(e) => {
            let _ = ops::call(agent, cap.reject, Value::Undefined, &[e]);
        }
        Ok(v) => {
            let r = ops::create_iter_result_object(agent, v, done);
            let _ = ops::call(agent, cap.resolve, Value::Undefined, &[Value::Object(r)]);
        }
    }
}

/// AsyncGeneratorResume.
fn async_generator_resume(agent: &mut Agent, g: Object, value: Value, mode: i32) {
    let Some((frame, state)) = data_mut(agent, g).and_then(|d| {
        let state = d.state;
        d.state = GenState::Executing;
        d.frame.take().map(|f| (f, state))
    }) else {
        return;
    };
    let mut frame = *frame;
    if state == GenState::SuspendedYield {
        frame.push(value);
        frame.push(Value::Number(f64::from(mode)));
    }
    let _ = run(agent, frame);
}

/// AsyncGeneratorAwaitReturn.
fn async_generator_await_return(agent: &mut Agent, g: Object) {
    let Some(value) = data(agent, g).and_then(|d| d.queue.front().map(|r| r.value)) else {
        return;
    };
    let ctor = agent.intrinsic(Intrinsic::Promise);
    let p = match promise::promise_resolve(agent, ctor, value) {
        Ok(Value::Object(p)) => p,
        Ok(_) => return,
        Err(Throw(e)) => {
            set_completed(agent, g);
            async_generator_complete_step(agent, g, Err(e), true);
            async_generator_drain_queue(agent, g);
            return;
        }
    };
    let on_fulfilled = create_native_function(
        agent,
        await_return_fulfilled,
        1,
        "",
        vec![Value::Object(g)],
        None,
    );
    let on_rejected = create_native_function(
        agent,
        await_return_rejected,
        1,
        "",
        vec![Value::Object(g)],
        None,
    );
    promise::perform_promise_then(
        agent,
        p,
        Value::Object(on_fulfilled),
        Value::Object(on_rejected),
        None,
    );
}

fn await_return_fulfilled(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    if let Value::Object(g) = native_slots(agent, call.callee)[0] {
        set_completed(agent, g);
        async_generator_complete_step(agent, g, Ok(call.arg(0)), true);
        async_generator_drain_queue(agent, g);
    }
    Ok(Value::Undefined)
}

fn await_return_rejected(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    if let Value::Object(g) = native_slots(agent, call.callee)[0] {
        set_completed(agent, g);
        async_generator_complete_step(agent, g, Err(call.arg(0)), true);
        async_generator_drain_queue(agent, g);
    }
    Ok(Value::Undefined)
}

/// AsyncGeneratorDrainQueue.
pub(crate) fn async_generator_drain_queue(agent: &mut Agent, g: Object) {
    loop {
        let Some((mode, value)) =
            data(agent, g).and_then(|d| d.queue.front().map(|r| (r.mode, r.value)))
        else {
            return;
        };
        match mode {
            MODE_RETURN => {
                if let Some(d) = data_mut(agent, g) {
                    d.state = GenState::AwaitingReturn;
                }
                async_generator_await_return(agent, g);
                return;
            }
            MODE_THROW => async_generator_complete_step(agent, g, Err(value), true),
            _ => async_generator_complete_step(agent, g, Ok(Value::Undefined), true),
        }
    }
}

/// AsyncGeneratorYield after the completion step: continues at once with
/// the next queued request, if any. Returns the resumption `[value mode]`.
pub(crate) fn async_generator_yield(
    agent: &mut Agent,
    g: Object,
    value: Value,
) -> Option<(Value, i32)> {
    async_generator_complete_step(agent, g, Ok(value), false);
    data(agent, g).and_then(|d| d.queue.front().map(|r| (r.value, r.mode)))
}

/// The common part of `next`, `return` and `throw` of
/// `%AsyncGeneratorPrototype%`.
fn async_generator_enqueue(
    agent: &mut Agent,
    this: Value,
    value: Value,
    mode: i32,
) -> JsResult<Value> {
    let realm = agent.current_realm();
    let cap = promise::new_intrinsic_capability(agent, realm);
    let g = match this {
        Value::Object(o) if matches!(data(agent, o), Some(d) if d.kind == GenKind::Async) => o,
        _ => {
            let e = agent.throw_error(crate::agent::ErrorKind::Type, "not an async generator");
            let _ = ops::call(agent, cap.reject, Value::Undefined, &[e.0]);
            return Ok(cap.promise);
        }
    };
    let mut state = data(agent, g)
        .map(|d| d.state)
        .unwrap_or(GenState::Completed);
    match mode {
        MODE_NEXT if state == GenState::Completed => {
            let r = ops::create_iter_result_object(agent, Value::Undefined, true);
            ops::call(agent, cap.resolve, Value::Undefined, &[Value::Object(r)])?;
            return Ok(cap.promise);
        }
        MODE_THROW => {
            if state == GenState::SuspendedStart {
                set_completed(agent, g);
                state = GenState::Completed;
            }
            if state == GenState::Completed {
                ops::call(agent, cap.reject, Value::Undefined, &[value])?;
                return Ok(cap.promise);
            }
        }
        _ => {}
    }
    if let Some(d) = data_mut(agent, g) {
        d.queue.push_back(AsyncGenRequest {
            value,
            mode,
            capability: cap,
        });
    }
    match (mode, state) {
        (MODE_RETURN, GenState::SuspendedStart | GenState::Completed) => {
            if let Some(d) = data_mut(agent, g) {
                d.state = GenState::AwaitingReturn;
                d.frame = None;
            }
            async_generator_await_return(agent, g);
        }
        (_, GenState::SuspendedStart | GenState::SuspendedYield) => {
            async_generator_resume(agent, g, value, mode);
        }
        _ => {}
    }
    Ok(cap.promise)
}

pub(crate) fn async_generator_next(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    async_generator_enqueue(agent, call.this, call.arg(0), MODE_NEXT)
}

pub(crate) fn async_generator_return(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    async_generator_enqueue(agent, call.this, call.arg(0), MODE_RETURN)
}

pub(crate) fn async_generator_throw(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    async_generator_enqueue(agent, call.this, call.arg(0), MODE_THROW)
}
