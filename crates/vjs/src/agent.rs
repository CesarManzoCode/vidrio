//! The agent (ECMA-262 §9.7): the heap, the execution context stack, the
//! job queues, the well-known symbols and the `Symbol.for` registry.
//!
//! Safe points. The collector only runs from [`Agent::safepoint`] and
//! [`Agent::collect_garbage`], and those refuse to run while a native call
//! is in progress (`nested_calls > 0`): any Rust frame below the current
//! one may hold unrooted handles. The interpreter calls `safepoint` between
//! instructions of its outermost activation and the embedder between
//! tasks; JS→JS calls never go through [`crate::ops::call`], so they keep
//! the interpreter at depth 0. A collection requested from inside a call
//! (for example `$262.gc()`) is deferred with [`Agent::request_gc`] to the
//! next safe point.

use std::collections::{HashMap, VecDeque};
use std::rc::Rc;

use crate::completion::{JsResult, Throw};
use crate::gc::{GcReport, Heap, JsString, Object, Realm, Symbol, Trace, Tracer};
use crate::object::{Attributes, ObjectKind, Slot};
use crate::realm::{Intrinsic, error_prototype};
use crate::value::{PropertyKey, Value, WeakKey};

/// Hooks through which bytecode functions are executed (installed by the
/// interpreter).
#[derive(Clone, Copy)]
pub struct Evaluator {
    pub call: fn(&mut Agent, Object, Value, &[Value]) -> JsResult<Value>,
    pub construct: fn(&mut Agent, Object, &[Value], Object) -> JsResult<Object>,
}

/// An execution context as far as the object model needs it.
#[derive(Clone, Copy, Debug)]
pub struct ExecutionContext {
    pub realm: Realm,
    pub function: Option<Object>,
    pub script_or_module: Option<Object>,
}

impl Trace for ExecutionContext {
    fn trace(&self, tracer: &mut Tracer) {
        tracer.realm(self.realm);
        for o in [self.function, self.script_or_module].into_iter().flatten() {
            tracer.object(o);
        }
    }
}

/// The work of a job. Its handles are roots while it is queued.
pub trait JobTask: Trace {
    fn run(self: Box<Self>, agent: &mut Agent) -> JsResult<()>;
}

/// A Job Abstract Closure with its realm.
pub struct Job {
    pub realm: Option<Realm>,
    pub task: Box<dyn JobTask>,
}

impl Trace for Job {
    fn trace(&self, tracer: &mut Tracer) {
        if let Some(r) = self.realm {
            tracer.realm(r);
        }
        self.task.trace(tracer);
    }
}

/// A job that calls `callback` with `this` and `args`.
pub struct CallJob {
    pub callback: Value,
    pub this: Value,
    pub args: Vec<Value>,
}

impl Trace for CallJob {
    fn trace(&self, tracer: &mut Tracer) {
        tracer.value(self.callback);
        tracer.value(self.this);
        self.args.trace(tracer);
    }
}

impl JobTask for CallJob {
    fn run(self: Box<Self>, agent: &mut Agent) -> JsResult<()> {
        crate::ops::call(agent, self.callback, self.this, &self.args)?;
        Ok(())
    }
}

/// CleanupFinalizationRegistry(registry) as a host job.
pub struct CleanupFinalizationRegistryJob {
    pub registry: Object,
}

impl Trace for CleanupFinalizationRegistryJob {
    fn trace(&self, tracer: &mut Tracer) {
        tracer.object(self.registry);
    }
}

impl JobTask for CleanupFinalizationRegistryJob {
    fn run(self: Box<Self>, agent: &mut Agent) -> JsResult<()> {
        crate::weak::cleanup_finalization_registry(agent, self.registry, None)
    }
}

macro_rules! well_known_symbols {
    ($($field:ident = $description:literal),* $(,)?) => {
        /// The well-known symbols, shared by every realm of the agent.
        #[derive(Clone, Copy, Debug)]
        pub struct WellKnownSymbols {
            $(pub $field: Symbol,)*
        }

        impl WellKnownSymbols {
            fn create(heap: &mut Heap) -> WellKnownSymbols {
                WellKnownSymbols {
                    $($field: {
                        let d = heap.alloc_str($description);
                        heap.alloc_symbol(Some(d))
                    },)*
                }
            }
        }

        impl Trace for WellKnownSymbols {
            fn trace(&self, tracer: &mut Tracer) {
                $(tracer.symbol(self.$field);)*
            }
        }
    };
}

well_known_symbols! {
    async_dispose = "Symbol.asyncDispose",
    async_iterator = "Symbol.asyncIterator",
    dispose = "Symbol.dispose",
    has_instance = "Symbol.hasInstance",
    is_concat_spreadable = "Symbol.isConcatSpreadable",
    iterator = "Symbol.iterator",
    match_ = "Symbol.match",
    match_all = "Symbol.matchAll",
    replace = "Symbol.replace",
    search = "Symbol.search",
    species = "Symbol.species",
    split = "Symbol.split",
    to_primitive = "Symbol.toPrimitive",
    to_string_tag = "Symbol.toStringTag",
    unscopables = "Symbol.unscopables",
}

/// The kinds of native errors the engine throws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    Error,
    Eval,
    Range,
    Reference,
    Syntax,
    Type,
    Uri,
}

/// Nested native calls deeper than this throw a RangeError.
pub const MAX_NATIVE_CALL_DEPTH: usize = 1500;

pub struct Agent {
    pub heap: Heap,
    pub symbols: WellKnownSymbols,
    pub evaluator: Option<Evaluator>,
    pub(crate) symbol_registry: HashMap<Rc<[u16]>, Symbol>,
    pub(crate) realms: Vec<Realm>,
    pub(crate) contexts: Vec<ExecutionContext>,
    /// HostEnqueuePromiseJob queue (microtasks).
    pub(crate) jobs: VecDeque<Job>,
    /// Host tasks: FinalizationRegistry cleanup and other host jobs.
    pub(crate) host_tasks: VecDeque<Job>,
    /// `[[KeptAlive]]`: WeakRef targets kept until ClearKeptObjects.
    pub(crate) kept_alive: Vec<WeakKey>,
    /// Native calls in progress; the collector does not run while > 0.
    pub(crate) nested_calls: usize,
    /// Interpreter frames on all run loops (see `interp::MAX_FRAME_DEPTH`).
    pub(crate) frame_depth: usize,
    gc_requested: bool,
}

impl Default for Agent {
    fn default() -> Agent {
        Agent::new()
    }
}

/// The agent's own roots.
struct AgentRoots<'a> {
    symbols: &'a WellKnownSymbols,
    registry: &'a HashMap<Rc<[u16]>, Symbol>,
    realms: &'a [Realm],
    contexts: &'a [ExecutionContext],
    jobs: &'a VecDeque<Job>,
    host_tasks: &'a VecDeque<Job>,
    kept_alive: &'a [WeakKey],
}

impl Trace for AgentRoots<'_> {
    fn trace(&self, tracer: &mut Tracer) {
        self.symbols.trace(tracer);
        for s in self.registry.values() {
            tracer.symbol(*s);
        }
        for r in self.realms {
            tracer.realm(*r);
        }
        self.contexts.trace(tracer);
        self.jobs.trace(tracer);
        self.host_tasks.trace(tracer);
        self.kept_alive.trace(tracer);
    }
}

impl Agent {
    pub fn new() -> Agent {
        let mut heap = Heap::new();
        let symbols = WellKnownSymbols::create(&mut heap);
        Agent {
            heap,
            symbols,
            evaluator: Some(crate::interp::EVALUATOR),
            symbol_registry: HashMap::new(),
            realms: Vec::new(),
            contexts: Vec::new(),
            jobs: VecDeque::new(),
            host_tasks: VecDeque::new(),
            kept_alive: Vec::new(),
            nested_calls: 0,
            frame_depth: 0,
            gc_requested: false,
        }
    }

    // ----- execution contexts and realms -----

    pub fn push_context(&mut self, context: ExecutionContext) {
        self.contexts.push(context);
    }

    pub fn pop_context(&mut self) -> Option<ExecutionContext> {
        self.contexts.pop()
    }

    pub fn running_context(&self) -> Option<&ExecutionContext> {
        self.contexts.last()
    }

    /// The current Realm Record: that of the running execution context, or
    /// the first realm when no context is running.
    pub fn current_realm(&self) -> Realm {
        self.contexts
            .last()
            .map(|c| c.realm)
            .or_else(|| self.realms.first().copied())
            .expect("the agent has no realm")
    }

    /// The realms the agent keeps alive.
    pub fn realms(&self) -> &[Realm] {
        &self.realms
    }

    /// Stops rooting a realm (a discarded document, for example).
    pub fn remove_realm(&mut self, realm: Realm) {
        self.realms.retain(|r| *r != realm);
    }

    /// An intrinsic of the current realm.
    pub fn intrinsic(&self, which: Intrinsic) -> Object {
        self.heap.realm(self.current_realm()).intrinsics.get(which)
    }

    pub fn global_object(&self) -> Object {
        self.heap
            .realm(self.current_realm())
            .global_object
            .expect("the realm has no global object")
    }

    // ----- native call depth -----

    pub(crate) fn enter_native_call(&mut self) -> JsResult<()> {
        if self.nested_calls >= MAX_NATIVE_CALL_DEPTH {
            return self.range_error("Maximum call stack size exceeded");
        }
        self.nested_calls += 1;
        Ok(())
    }

    pub(crate) fn exit_native_call(&mut self) {
        self.nested_calls -= 1;
    }

    // ----- errors -----

    /// A new error object of `kind` from the current realm.
    pub fn create_error(&mut self, kind: ErrorKind, message: &str) -> Object {
        let proto = self.intrinsic(error_prototype(kind));
        let o = self.heap.create_object(Some(proto), ObjectKind::Error);
        let msg = Value::String(self.heap.alloc_str(message));
        let key = PropertyKey::String(self.heap.names.message);
        self.heap
            .set_own(o, key, Attributes::data(true, false, true), Slot::Data(msg));
        o
    }

    pub fn throw_error(&mut self, kind: ErrorKind, message: &str) -> Throw {
        Throw(Value::Object(self.create_error(kind, message)))
    }

    pub fn type_error<T>(&mut self, message: &str) -> JsResult<T> {
        Err(self.throw_error(ErrorKind::Type, message))
    }

    pub fn range_error<T>(&mut self, message: &str) -> JsResult<T> {
        Err(self.throw_error(ErrorKind::Range, message))
    }

    pub fn reference_error<T>(&mut self, message: &str) -> JsResult<T> {
        Err(self.throw_error(ErrorKind::Reference, message))
    }

    pub fn syntax_error<T>(&mut self, message: &str) -> JsResult<T> {
        Err(self.throw_error(ErrorKind::Syntax, message))
    }

    // ----- symbols -----

    /// `Symbol.for(key)`: the registered symbol for the string.
    pub fn symbol_for(&mut self, key: JsString) -> Symbol {
        let units: Rc<[u16]> = Rc::from(self.heap.string_units(key));
        if let Some(s) = self.symbol_registry.get(&units) {
            return *s;
        }
        let s = self.heap.alloc_symbol(Some(key));
        self.heap.symbol_mut(s).registered = true;
        self.symbol_registry.insert(units, s);
        s
    }

    /// `Symbol.keyFor(sym)`: the key of a registered symbol.
    pub fn key_for_symbol(&self, s: Symbol) -> Option<JsString> {
        let data = self.heap.symbol(s);
        if data.registered {
            data.description
        } else {
            None
        }
    }

    // ----- jobs -----

    /// HostEnqueuePromiseJob.
    pub fn enqueue_job(&mut self, job: Job) {
        self.jobs.push_back(job);
    }

    /// Queues a host task (FinalizationRegistry cleanup, timers…).
    pub fn enqueue_host_task(&mut self, job: Job) {
        self.host_tasks.push_back(job);
    }

    pub fn has_pending_jobs(&self) -> bool {
        !self.jobs.is_empty()
    }

    pub fn has_pending_host_tasks(&self) -> bool {
        !self.host_tasks.is_empty()
    }

    fn run_job(&mut self, job: Job) -> JsResult<()> {
        let pushed = match job.realm {
            Some(realm) => {
                self.push_context(ExecutionContext {
                    realm,
                    function: None,
                    script_or_module: None,
                });
                true
            }
            None => false,
        };
        let result = job.task.run(self);
        if pushed {
            self.pop_context();
        }
        result
    }

    /// Runs queued jobs until the queue is empty and returns the thrown
    /// values (each would be reported by the host). It is a safe point
    /// between jobs: call it only when no unrooted handle is held.
    pub fn run_jobs(&mut self) -> Vec<Throw> {
        let mut errors = Vec::new();
        while let Some(job) = self.jobs.pop_front() {
            if let Err(e) = self.run_job(job) {
                errors.push(e);
            }
            self.safepoint(&[]);
        }
        self.clear_kept_objects();
        errors
    }

    /// Runs the oldest host task, if any. Same safe-point rule as
    /// [`Agent::run_jobs`].
    pub fn run_host_task(&mut self) -> Option<JsResult<()>> {
        let job = self.host_tasks.pop_front()?;
        let result = self.run_job(job);
        self.clear_kept_objects();
        Some(result)
    }

    // ----- weak references and collection -----

    /// AddToKeptObjects(value).
    pub fn add_to_kept_objects(&mut self, key: WeakKey) {
        self.kept_alive.push(key);
    }

    /// ClearKeptObjects().
    pub fn clear_kept_objects(&mut self) {
        self.kept_alive.clear();
    }

    /// Asks for a collection at the next safe point.
    pub fn request_gc(&mut self) {
        self.gc_requested = true;
    }

    /// Collects if the allocation threshold was reached or a collection was
    /// requested, and no native call is in progress. `extra_roots` are the
    /// caller's roots (the interpreter's stack, for example).
    pub fn safepoint(&mut self, extra_roots: &[&dyn Trace]) -> Option<GcReport> {
        if self.nested_calls > 0 {
            return None;
        }
        if self.gc_requested || self.heap.should_collect() {
            return self.collect_garbage(extra_roots);
        }
        None
    }

    /// Collects now, with the agent's roots plus `extra_roots`, and queues
    /// a cleanup host task for every FinalizationRegistry with collected
    /// targets. Returns `None` (and requests a later collection) if a
    /// native call is in progress.
    pub fn collect_garbage(&mut self, extra_roots: &[&dyn Trace]) -> Option<GcReport> {
        if self.nested_calls > 0 {
            self.gc_requested = true;
            return None;
        }
        self.gc_requested = false;
        let roots = AgentRoots {
            symbols: &self.symbols,
            registry: &self.symbol_registry,
            realms: &self.realms,
            contexts: &self.contexts,
            jobs: &self.jobs,
            host_tasks: &self.host_tasks,
            kept_alive: &self.kept_alive,
        };
        let mut all: Vec<&dyn Trace> = Vec::with_capacity(extra_roots.len() + 1);
        all.push(&roots);
        all.extend_from_slice(extra_roots);
        let report = self.heap.collect(&all);
        for &registry in &report.cleanup_registries {
            let realm = match &self.heap.object(registry).kind {
                ObjectKind::FinalizationRegistry(data) => Some(data.realm),
                _ => None,
            };
            self.host_tasks.push_back(Job {
                realm,
                task: Box::new(CleanupFinalizationRegistryJob { registry }),
            });
        }
        Some(report)
    }
}
