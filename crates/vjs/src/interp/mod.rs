//! The bytecode interpreter.
//!
//! Frames live on a stack owned by [`run`]: a call from bytecode to a
//! bytecode function pushes a frame and the loop continues, so JS→JS calls
//! never recurse on the native stack. Calls through native code (getters,
//! callbacks of built-ins, `Function.prototype.call`, bound functions,
//! proxies) go through `[[Call]]`, which starts a nested `run`.
//!
//! Generators, async functions and async generators keep their frame in
//! their object while suspended; resuming moves the frame back onto a run
//! loop.
//!
//! Safe points: the outermost `run` (the one entered by script evaluation
//! with no native call in progress) offers the collector a safe point
//! between instructions with its frames as extra roots.

pub mod env_ops;
pub mod eval;
mod exec;
pub mod function;
pub mod generator;
pub mod intrinsics;
pub mod iter;
mod operators;
pub mod promise;

use std::rc::Rc;

use crate::agent::{Agent, ErrorKind, Evaluator, ExecutionContext};
use crate::bytecode::Code;
use crate::completion::{JsResult, Throw};
use crate::env::EnvData;
use crate::gc::{Env, Object, Realm, Trace, Tracer};
use crate::object::ObjectKind;
use crate::value::Value;

pub use eval::{evaluate_script, evaluate_script_in};
use promise::Capability;

/// Frames deeper than this (across all run loops) throw a RangeError.
pub const MAX_FRAME_DEPTH: usize = 10_000;

/// The hooks through which `[[Call]]` and `[[Construct]]` reach bytecode
/// functions.
pub const EVALUATOR: Evaluator = Evaluator {
    call: evaluator_call,
    construct: evaluator_construct,
};

/// An installed exception handler.
#[derive(Clone, Copy, Debug)]
pub struct Handler {
    pub pc: u32,
    pub stack_len: usize,
    pub env: Env,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameKind {
    Call,
    Construct,
    Script,
    Eval,
}

/// An activation of a code block.
pub struct Frame {
    pub code: Rc<Code>,
    pub pc: usize,
    pub stack: Vec<Value>,
    pub regs: Vec<Value>,
    pub env: Env,
    /// The active function object.
    pub function: Option<Object>,
    /// `None` while the `this` of a derived constructor is uninitialized.
    pub this: Option<Value>,
    pub new_target: Value,
    pub args: Vec<Value>,
    pub handlers: Vec<Handler>,
    /// The completion value register of script and eval code.
    pub completion: Value,
    pub kind: FrameKind,
    pub realm: Realm,
    pub script_or_module: Option<Object>,
    /// Promise capability of an async function body.
    pub async_cap: Option<Capability>,
    /// The promise of `async_cap` was already handed to the caller.
    pub delivered: bool,
    /// Generator object, or the continuation object of an async function.
    pub generator: Option<Object>,
    /// The environment that holds the hidden `this` binding.
    pub this_env: Option<Env>,
}

impl Trace for Frame {
    fn trace(&self, tracer: &mut Tracer) {
        self.code.trace(tracer);
        self.stack.trace(tracer);
        self.regs.trace(tracer);
        tracer.env(self.env);
        if let Some(f) = self.function {
            tracer.object(f);
        }
        if let Some(t) = self.this {
            tracer.value(t);
        }
        tracer.value(self.new_target);
        self.args.trace(tracer);
        for h in &self.handlers {
            tracer.env(h.env);
        }
        tracer.value(self.completion);
        tracer.realm(self.realm);
        if let Some(o) = self.script_or_module {
            tracer.object(o);
        }
        if let Some(c) = &self.async_cap {
            c.trace(tracer);
        }
        if let Some(g) = self.generator {
            tracer.object(g);
        }
        if let Some(e) = self.this_env {
            tracer.env(e);
        }
    }
}

impl Frame {
    /// A frame for `code` with no function (script or eval code).
    pub fn new(code: Rc<Code>, env: Env, realm: Realm, kind: FrameKind) -> Frame {
        let regs = vec![Value::Undefined; code.num_regs as usize];
        Frame {
            code,
            pc: 0,
            stack: Vec::new(),
            regs,
            env,
            function: None,
            this: None,
            new_target: Value::Undefined,
            args: Vec::new(),
            handlers: Vec::new(),
            completion: Value::Undefined,
            kind,
            realm,
            script_or_module: None,
            async_cap: None,
            delivered: false,
            generator: None,
            this_env: None,
        }
    }

    pub(crate) fn pop(&mut self) -> Value {
        self.stack.pop().unwrap_or(Value::Undefined)
    }

    pub(crate) fn top(&self) -> Value {
        self.stack.last().copied().unwrap_or(Value::Undefined)
    }

    pub(crate) fn push(&mut self, v: Value) {
        self.stack.push(v);
    }

    /// The value `depth` entries below the top.
    pub(crate) fn peek(&self, depth: usize) -> Value {
        let len = self.stack.len();
        if depth < len {
            self.stack[len - 1 - depth]
        } else {
            Value::Undefined
        }
    }
}

/// What the instruction asks the run loop to do.
pub(crate) enum Step {
    Next,
    Call(Box<Frame>),
    Return(Value),
    Suspend(Suspend),
}

pub(crate) enum Suspend {
    /// The generator object was created; hand it to the caller.
    Initial(Object),
    /// `yield`; `raw` for the result objects of `yield*`.
    Yield {
        value: Value,
        raw: bool,
    },
    Await,
    AsyncGenYield,
}

struct FrameRoots<'a>(&'a [Frame]);

impl Trace for FrameRoots<'_> {
    fn trace(&self, tracer: &mut Tracer) {
        for f in self.0 {
            f.trace(tracer);
        }
    }
}

fn push_context(agent: &mut Agent, frame: &Frame) {
    agent.push_context(ExecutionContext {
        realm: frame.realm,
        function: frame.function,
        script_or_module: frame.script_or_module,
    });
}

/// Runs `frame` (and the frames it calls) until it returns or suspends.
/// The result is the value its caller receives: the return value, the
/// generator object of a generator's first activation, the promise of an
/// async function.
pub fn run(agent: &mut Agent, frame: Frame) -> JsResult<Value> {
    if agent.frame_depth >= MAX_FRAME_DEPTH {
        return agent.range_error("Maximum call stack size exceeded");
    }
    let base_depth = agent.frame_depth;
    let mut frames: Vec<Frame> = Vec::with_capacity(8);
    push_context(agent, &frame);
    frames.push(frame);
    agent.frame_depth += 1;
    let result = run_loop(agent, &mut frames);
    // Frames left behind by an error path are discarded with their
    // contexts.
    while frames.pop().is_some() {
        agent.pop_context();
    }
    agent.frame_depth = base_depth;
    result
}

fn run_loop(agent: &mut Agent, frames: &mut Vec<Frame>) -> JsResult<Value> {
    loop {
        if agent.nested_calls == 0 {
            agent.safepoint(&[&FrameRoots(frames.as_slice())]);
        }
        let frame = frames.last_mut().expect("running frame");
        let Some(&op) = frame.code.ops.get(frame.pc) else {
            // Falling off the end returns undefined.
            let v = Value::Undefined;
            if let Some(r) = complete_frame(agent, frames, Ok(v)) {
                return r;
            }
            continue;
        };
        frame.pc += 1;
        match exec::step(agent, frame, op) {
            Ok(Step::Next) => {}
            Ok(Step::Call(new_frame)) => {
                if agent.frame_depth >= MAX_FRAME_DEPTH {
                    let e = agent.throw_error(ErrorKind::Range, "Maximum call stack size exceeded");
                    if let Some(r) = unwind(agent, frames, e.0) {
                        return r;
                    }
                    continue;
                }
                push_context(agent, &new_frame);
                frames.push(*new_frame);
                agent.frame_depth += 1;
            }
            Ok(Step::Return(v)) => {
                if let Some(r) = complete_frame(agent, frames, Ok(v)) {
                    return r;
                }
            }
            Ok(Step::Suspend(s)) => {
                let f = frames.pop().expect("suspending frame");
                agent.pop_context();
                agent.frame_depth -= 1;
                let deliver = generator::suspend_frame(agent, f, s);
                match frames.last_mut() {
                    Some(caller) => caller.push(deliver),
                    None => return Ok(deliver),
                }
            }
            Err(Throw(e)) => {
                if let Some(r) = unwind(agent, frames, e) {
                    return r;
                }
            }
        }
    }
}

/// Pops the finished top frame; returns `Some` when the run is over.
fn complete_frame(
    agent: &mut Agent,
    frames: &mut Vec<Frame>,
    result: Result<Value, Value>,
) -> Option<JsResult<Value>> {
    let f = frames.pop().expect("finishing frame");
    agent.pop_context();
    agent.frame_depth -= 1;
    let delivered = finish_frame(agent, f, result);
    if frames.is_empty() {
        return Some(delivered);
    }
    match delivered {
        Ok(v) => {
            frames.last_mut().expect("caller").push(v);
            None
        }
        Err(Throw(e)) => unwind(agent, frames, e),
    }
}

/// Delivers a throw to the nearest handler; returns `Some` if it escapes
/// the run.
fn unwind(agent: &mut Agent, frames: &mut Vec<Frame>, mut e: Value) -> Option<JsResult<Value>> {
    loop {
        let frame = frames.last_mut()?;
        if let Some(h) = frame.handlers.pop() {
            frame.stack.truncate(h.stack_len);
            frame.env = h.env;
            frame.stack.push(e);
            frame.pc = h.pc as usize;
            return None;
        }
        let f = frames.pop().expect("frame");
        agent.pop_context();
        agent.frame_depth -= 1;
        let delivered = finish_frame(agent, f, Err(e));
        if frames.is_empty() {
            return Some(delivered);
        }
        match delivered {
            Ok(v) => {
                frames.last_mut().expect("caller").push(v);
                return None;
            }
            Err(Throw(e2)) => e = e2,
        }
    }
}

/// The completion of a frame turned into what its caller receives.
fn finish_frame(agent: &mut Agent, frame: Frame, result: Result<Value, Value>) -> JsResult<Value> {
    if let Some(g) = frame.generator {
        let kind = match &agent.heap.object(g).kind {
            ObjectKind::Generator(data) => Some(data.kind),
            _ => None,
        };
        match kind {
            Some(generator::GenKind::Sync) => {
                generator::set_completed(agent, g);
                return result.map_err(Throw);
            }
            Some(generator::GenKind::Async) => {
                generator::set_completed(agent, g);
                generator::async_generator_complete_step(agent, g, result, true);
                generator::async_generator_drain_queue(agent, g);
                return Ok(Value::Undefined);
            }
            _ => {}
        }
    }
    if let Some(cap) = frame.async_cap {
        let (f, arg) = match result {
            Ok(v) => (cap.resolve, v),
            Err(e) => (cap.reject, e),
        };
        let _ = crate::ops::call(agent, f, Value::Undefined, &[arg]);
        return Ok(if frame.delivered {
            Value::Undefined
        } else {
            cap.promise
        });
    }
    let v = result.map_err(Throw)?;
    if frame.kind != FrameKind::Construct {
        return Ok(v);
    }
    if v.is_object() {
        return Ok(v);
    }
    let derived = frame.code.flags.is_derived;
    if !derived {
        return Ok(frame.this.unwrap_or(Value::Undefined));
    }
    if !v.is_undefined() {
        return agent.type_error("derived constructors may only return an object or undefined");
    }
    match env_ops::hidden_this_of(agent, frame.this_env) {
        Some(this) => Ok(this),
        None => agent.reference_error(
            "must call super constructor in derived class before accessing 'this' or returning from derived constructor",
        ),
    }
}

/// The `Code` of a bytecode function.
pub(crate) fn function_code(agent: &Agent, f: Object) -> Option<Rc<Code>> {
    match &agent.heap.object(f).kind {
        ObjectKind::BytecodeFunction(data) => data.code.clone().as_any_rc().downcast::<Code>().ok(),
        _ => None,
    }
}

fn evaluator_call(agent: &mut Agent, f: Object, this: Value, args: &[Value]) -> JsResult<Value> {
    let frame = function::prepare_call(agent, f, this, args.to_vec(), None)?;
    run(agent, frame)
}

fn evaluator_construct(
    agent: &mut Agent,
    f: Object,
    args: &[Value],
    new_target: Object,
) -> JsResult<Object> {
    let frame =
        function::prepare_call(agent, f, Value::Undefined, args.to_vec(), Some(new_target))?;
    match run(agent, frame)? {
        Value::Object(o) => Ok(o),
        _ => agent.type_error("constructor did not return an object"),
    }
}

/// The global Environment Record of a realm, created on first use.
pub fn global_env(agent: &mut Agent, realm: Realm) -> Env {
    if let Some(e) = agent.heap.realm(realm).global_env {
        return e;
    }
    let data = agent.heap.realm(realm);
    let global = data.global_object.expect("the realm has no global object");
    let this = data.global_this.unwrap_or(global);
    let env = agent
        .heap
        .alloc_env_data(EnvData::global(global, Value::Object(this)));
    agent.heap.realm_mut(realm).global_env = Some(env);
    env
}
