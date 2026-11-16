//! The `vjs` shell: `vjs [--strict] [--module] [--test262] FILE…`.
//!
//! Evaluates the files in order in one realm: as sloppy scripts, as strict
//! scripts (`--strict`) or as modules resolved from the file system
//! (`--module`). Defines `print`; with `--test262` it also installs the
//! `$262` object of test262's INTERPRETING.md, agents included. After each
//! file it drains the job queue. An uncaught error prints one line
//! `Uncaught <Name>: <message>` to stderr and exits with 1.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::rc::Rc;
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use vjs::agent::{Agent, ErrorKind};
use vjs::bigint::BigInt;
use vjs::buffer::{ArrayBufferData, BufferStorage, SharedBlock};
use vjs::exotic::{HostObject, InternalMethods};
use vjs::function::{NativeCall, NativeFn, create_builtin_function};
use vjs::gc::{Object, Realm, Trace, Tracer};
use vjs::interp::promise::{PromiseState, promise_state};
use vjs::module::{self, LoadPayload, ModuleHost, ModuleRequest, Referrer};
use vjs::object::{ObjectKind, PropertyDescriptor};
use vjs::realm::Intrinsic;
use vjs::waiter::AsyncWait;
use vjs::{JsResult, Throw, Value, evaluate_script_record, ops};

const USAGE: &str = "usage: vjs --version | vjs [--strict] [--module] [--test262] FILE...";

/// What the command line asks for.
#[derive(Debug, PartialEq, Eq)]
enum Command {
    Version,
    Run(Options),
    Usage,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct Options {
    strict: bool,
    module: bool,
    test262: bool,
    files: Vec<String>,
}

fn parse_args(args: &[String]) -> Command {
    if let [flag] = args {
        if flag == "--version" {
            return Command::Version;
        }
    }
    let mut options = Options::default();
    for a in args {
        match a.as_str() {
            "--strict" => options.strict = true,
            "--module" => options.module = true,
            "--test262" => options.test262 = true,
            s if s.starts_with("--") => return Command::Usage,
            s => options.files.push(s.to_string()),
        }
    }
    if options.files.is_empty() {
        return Command::Usage;
    }
    Command::Run(options)
}

// ----- per-realm host state -----

/// The shell's `[[HostDefined]]` of a realm: values it must keep alive
/// (the receiver of `$262.agent.receiveBroadcast`, promises the shell
/// still looks at).
#[derive(Default)]
struct RealmHost {
    receiver: RefCell<Option<Value>>,
    roots: RefCell<Vec<Value>>,
}

impl Trace for RealmHost {
    fn trace(&self, tracer: &mut Tracer) {
        if let Some(v) = *self.receiver.borrow() {
            tracer.value(v);
        }
        for v in self.roots.borrow().iter() {
            tracer.value(*v);
        }
    }
}

thread_local! {
    /// The shell realms of this thread's agent.
    static REALM_HOSTS: RefCell<Vec<(Realm, Rc<RealmHost>)>> = const { RefCell::new(Vec::new()) };
    /// The realm host that holds the broadcast receiver.
    static RECEIVER: RefCell<Option<Rc<RealmHost>>> = const { RefCell::new(None) };
    /// This thread runs an agent started by `$262.agent.start`.
    static IS_AGENT: Cell<bool> = const { Cell::new(false) };
    /// The agent called `$262.agent.leaving()`.
    static LEAVING: Cell<bool> = const { Cell::new(false) };
    static TEST262: Cell<bool> = const { Cell::new(false) };
}

fn realm_host(realm: Realm) -> Option<Rc<RealmHost>> {
    REALM_HOSTS.with(|h| {
        h.borrow()
            .iter()
            .find(|(r, _)| *r == realm)
            .map(|(_, host)| host.clone())
    })
}

fn keep_alive(realm: Realm, v: Value) {
    if let Some(host) = realm_host(realm) {
        host.roots.borrow_mut().push(v);
    }
}

// ----- module loading from the file system -----

/// HostLoadImportedModule over the file system: specifiers are paths
/// relative to the file of the referrer (or to the working directory).
/// One record per realm, file and module type.
#[derive(Default)]
struct FsHost {
    map: RefCell<HashMap<(Realm, PathBuf, bool), Object>>,
}

impl FsHost {
    fn remember(&self, realm: Realm, path: PathBuf, json: bool, module: Object) {
        self.map.borrow_mut().insert((realm, path, json), module);
    }

    fn load(
        &self,
        agent: &mut Agent,
        referrer: Referrer,
        request: &ModuleRequest,
    ) -> Result<Object, Throw> {
        let (base, realm) = match referrer {
            Referrer::Record(o) => {
                let base = module::host_defined(agent, o)
                    .and_then(|p| Path::new(&*p).parent().map(Path::to_path_buf));
                (base, module::record_realm(agent, o))
            }
            Referrer::Realm(r) => (None, r),
        };
        let base = base
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_default();
        let json = match request.module_type() {
            None => false,
            Some("json") => true,
            Some(t) => {
                return Err(
                    agent.throw_error(ErrorKind::Type, &format!("unsupported module type '{t}'"))
                );
            }
        };
        let path = match base.join(&request.specifier).canonicalize() {
            Ok(p) => p,
            Err(e) => {
                return Err(agent.throw_error(
                    ErrorKind::Type,
                    &format!("cannot load module '{}': {e}", request.specifier),
                ));
            }
        };
        if let Some(m) = self.map.borrow().get(&(realm, path.clone(), json)) {
            return Ok(*m);
        }
        let source = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => {
                return Err(agent.throw_error(
                    ErrorKind::Type,
                    &format!("cannot read module '{}': {e}", request.specifier),
                ));
            }
        };
        let name: Rc<str> = Rc::from(path.to_string_lossy().as_ref());
        let m = if json {
            module::create_json_module(agent, realm, &source, Some(name))?
        } else {
            match module::parse_module_record(agent, realm, &source, Some(name)) {
                Ok(m) => m,
                Err(e) => return Err(agent.throw_error(ErrorKind::Syntax, &e.message)),
            }
        };
        self.remember(realm, path, json, m);
        Ok(m)
    }
}

impl ModuleHost for FsHost {
    fn load_imported_module(
        &self,
        agent: &mut Agent,
        referrer: Referrer,
        request: &ModuleRequest,
        payload: LoadPayload,
    ) {
        let result = self.load(agent, referrer, request);
        module::finish_loading_imported_module(agent, referrer, request, payload, result);
    }
}

// ----- uncaught errors -----

fn get_string(agent: &mut Agent, o: Object, name: &str) -> Option<String> {
    let key = agent.heap.key_from_str(name);
    let v = ops::get(agent, o, key).ok()?;
    if v.is_undefined() {
        return None;
    }
    let s = ops::to_string(agent, v).ok()?;
    Some(agent.heap.string_to_rust(s))
}

/// The line printed for an uncaught value: `Uncaught <name>: <message>`
/// for errors, `Uncaught <ToString(value)>` otherwise.
fn describe_uncaught(agent: &mut Agent, v: Value) -> String {
    if let Value::Object(o) = v {
        if matches!(agent.heap.object(o).kind, ObjectKind::Error) {
            let name = get_string(agent, o, "name").unwrap_or_else(|| "Error".to_string());
            let message = get_string(agent, o, "message").unwrap_or_default();
            return format!("Uncaught {name}: {message}");
        }
    }
    match ops::to_string(agent, v) {
        Ok(s) => format!("Uncaught {}", agent.heap.string_to_rust(s)),
        Err(_) => match v {
            Value::Object(o) => {
                let name = get_string(agent, o, "name").unwrap_or_else(|| "Error".to_string());
                let message = get_string(agent, o, "message").unwrap_or_default();
                format!("Uncaught {name}: {message}")
            }
            _ => "Uncaught exception".to_string(),
        },
    }
}

/// Runs queued jobs and host tasks until both queues are empty, and waits
/// for the pending promises of `Atomics.waitAsync`; the first error that
/// escapes is returned. A waiter without a timeout is only waited for
/// while another agent could notify it.
fn drain(agent: &mut Agent) -> Result<(), Value> {
    loop {
        if let Some(Throw(e)) = agent.run_jobs().into_iter().next() {
            return Err(e);
        }
        match agent.run_host_task() {
            Some(Err(Throw(e))) => return Err(e),
            Some(Ok(())) => {}
            None => {
                if agent.has_pending_jobs() {
                    continue;
                }
                if agent.has_pending_async_waits() {
                    let wait = if other_agents_alive() {
                        AsyncWait::Forever
                    } else {
                        AsyncWait::Timeouts
                    };
                    if agent.settle_async_waits(wait) {
                        continue;
                    }
                }
                return Ok(());
            }
        }
    }
}

/// Whether another agent of the process is running: the main thread for
/// an agent, a started agent for the main thread.
fn other_agents_alive() -> bool {
    IS_AGENT.with(Cell::get)
        || hub()
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .agents
            .iter()
            .any(|s| s.alive)
}

// ----- globals -----

fn callee_realm(agent: &Agent, f: Object) -> Realm {
    match &agent.heap.object(f).kind {
        ObjectKind::NativeFunction(d) => d.realm,
        _ => agent.current_realm(),
    }
}

fn define(agent: &mut Agent, o: Object, name: &str, value: Value) {
    let key = agent.heap.key_from_str(name);
    let _ = ops::define_property_or_throw(
        agent,
        o,
        key,
        PropertyDescriptor::data(value, true, false, true),
    );
}

fn define_fn(agent: &mut Agent, realm: Realm, o: Object, name: &str, f: NativeFn, length: u32) {
    let key = agent.heap.key_from_str(name);
    let func = create_builtin_function(agent, f, length, key, Some(realm), None, None);
    define(agent, o, name, Value::Object(func));
}

fn plain_object(agent: &mut Agent, realm: Realm) -> Object {
    let proto = agent
        .heap
        .realm(realm)
        .intrinsics
        .get(Intrinsic::ObjectPrototype);
    agent.heap.create_object(Some(proto), ObjectKind::Ordinary)
}

fn to_rust_string(agent: &mut Agent, v: Value) -> JsResult<String> {
    let s = ops::to_string(agent, v)?;
    Ok(agent.heap.string_to_rust(s))
}

fn print(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let mut parts = Vec::with_capacity(call.args.len());
    for &a in call.args {
        parts.push(to_rust_string(agent, a)?);
    }
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{}", parts.join(" "));
    let _ = out.flush();
    Ok(Value::Undefined)
}

/// A realm with the shell's globals (`print`, and `$262` under
/// `--test262`).
fn new_shell_realm(agent: &mut Agent) -> Realm {
    let realm = agent.initialize_host_defined_realm();
    let host = Rc::new(RealmHost::default());
    agent.heap.realm_mut(realm).host_defined = Some(host.clone() as Rc<dyn Trace>);
    REALM_HOSTS.with(|h| h.borrow_mut().push((realm, host)));
    let global = agent
        .heap
        .realm(realm)
        .global_object
        .expect("global object");
    define_fn(agent, realm, global, "print", print, 1);
    if TEST262.with(Cell::get) {
        let o = create_262(agent, realm);
        define(agent, global, "$262", Value::Object(o));
    }
    realm
}

// ----- $262 -----

/// `[[IsHTMLDDA]]`: an object that `typeof` reports as "undefined" and
/// that returns `null` when called.
struct HtmlDda;

impl InternalMethods for HtmlDda {}

impl Trace for HtmlDda {
    fn trace(&self, _tracer: &mut Tracer) {}
}

impl HostObject for HtmlDda {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn is_htmldda(&self) -> bool {
        true
    }

    fn is_callable(&self) -> bool {
        true
    }

    fn call(
        &self,
        _agent: &mut Agent,
        _o: Object,
        _this: Value,
        _args: &[Value],
    ) -> JsResult<Value> {
        Ok(Value::Null)
    }
}

fn create_262(agent: &mut Agent, realm: Realm) -> Object {
    let o = plain_object(agent, realm);
    define_fn(agent, realm, o, "createRealm", create_realm_262, 0);
    define_fn(agent, realm, o, "evalScript", eval_script_262, 1);
    define_fn(
        agent,
        realm,
        o,
        "detachArrayBuffer",
        detach_array_buffer_262,
        1,
    );
    define_fn(agent, realm, o, "gc", gc_262, 0);
    let global = agent
        .heap
        .realm(realm)
        .global_object
        .expect("global object");
    define(agent, o, "global", Value::Object(global));
    let proto = agent
        .heap
        .realm(realm)
        .intrinsics
        .get(Intrinsic::ObjectPrototype);
    let dda = agent
        .heap
        .create_object(Some(proto), ObjectKind::Host(Rc::new(HtmlDda)));
    define(agent, o, "IsHTMLDDA", Value::Object(dda));
    let a = plain_object(agent, realm);
    if IS_AGENT.with(Cell::get) {
        define_fn(
            agent,
            realm,
            a,
            "receiveBroadcast",
            receive_broadcast_262,
            1,
        );
        define_fn(agent, realm, a, "report", report_262, 1);
        define_fn(agent, realm, a, "leaving", leaving_262, 0);
    } else {
        define_fn(agent, realm, a, "start", start_262, 1);
        define_fn(agent, realm, a, "broadcast", broadcast_262, 2);
        define_fn(agent, realm, a, "getReport", get_report_262, 0);
    }
    define_fn(agent, realm, a, "sleep", sleep_262, 1);
    define_fn(agent, realm, a, "monotonicNow", monotonic_now_262, 0);
    define(agent, o, "agent", Value::Object(a));
    o
}

fn create_realm_262(agent: &mut Agent, _call: NativeCall<'_>) -> JsResult<Value> {
    let realm = new_shell_realm(agent);
    let global = agent
        .heap
        .realm(realm)
        .global_object
        .expect("global object");
    let key = agent.heap.key_from_str("$262");
    ops::get(agent, global, key)
}

fn eval_script_262(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let realm = callee_realm(agent, call.callee);
    let source = to_rust_string(agent, call.arg(0))?;
    evaluate_script_record(agent, realm, &source, None)
}

fn detach_array_buffer_262(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    match call.arg(0) {
        Value::Object(o) if matches!(agent.heap.object(o).kind, ObjectKind::ArrayBuffer(_)) => {
            vjs::buffer::detach_array_buffer(agent, o, Value::Undefined)?;
            Ok(Value::Null)
        }
        _ => agent.type_error("detachArrayBuffer: not an ArrayBuffer"),
    }
}

fn gc_262(agent: &mut Agent, _call: NativeCall<'_>) -> JsResult<Value> {
    agent.request_gc();
    Ok(Value::Undefined)
}

// ----- agents -----

/// The value broadcast with a SharedArrayBuffer.
#[derive(Clone)]
enum MessageId {
    Number(f64),
    BigInt(BigInt),
}

#[derive(Clone)]
struct Message {
    block: Arc<SharedBlock>,
    max_byte_length: Option<usize>,
    id: MessageId,
}

struct AgentSlot {
    alive: bool,
    mailbox: Option<Message>,
}

#[derive(Default)]
struct HubState {
    agents: Vec<AgentSlot>,
    reports: VecDeque<String>,
}

/// What the agents of the process share.
#[derive(Default)]
struct Hub {
    state: Mutex<HubState>,
    cond: Condvar,
}

fn hub() -> &'static Hub {
    static HUB: OnceLock<Hub> = OnceLock::new();
    HUB.get_or_init(Hub::default)
}

fn start_instant() -> Instant {
    static START: OnceLock<Instant> = OnceLock::new();
    *START.get_or_init(Instant::now)
}

const AGENT_STACK: usize = 256 << 20;

fn start_262(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let source = to_rust_string(agent, call.arg(0))?;
    let index = {
        let mut state = hub().state.lock().unwrap_or_else(|e| e.into_inner());
        state.agents.push(AgentSlot {
            alive: true,
            mailbox: None,
        });
        state.agents.len() - 1
    };
    let spawned = std::thread::Builder::new()
        .stack_size(AGENT_STACK)
        .spawn(move || agent_main(index, source));
    if spawned.is_err() {
        mark_dead(index);
        return agent.type_error("cannot start an agent thread");
    }
    Ok(Value::Undefined)
}

fn mark_dead(index: usize) {
    let mut state = hub().state.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(slot) = state.agents.get_mut(index) {
        slot.alive = false;
        slot.mailbox = None;
    }
    hub().cond.notify_all();
}

/// The body of an agent thread: its own heap and realm, the script, and
/// then the broadcasts it receives until it leaves.
fn agent_main(index: usize, source: String) {
    IS_AGENT.with(|a| a.set(true));
    TEST262.with(|t| t.set(true));
    let mut agent = Agent::new();
    agent.module_host = Some(Rc::new(FsHost::default()) as Rc<dyn ModuleHost>);
    let realm = new_shell_realm(&mut agent);
    if let Err(Throw(e)) = evaluate_script_record(&mut agent, realm, &source, None) {
        eprintln!("{}", describe_uncaught(&mut agent, e));
    }
    if let Err(e) = drain(&mut agent) {
        eprintln!("{}", describe_uncaught(&mut agent, e));
    }
    loop {
        if LEAVING.with(Cell::get) {
            break;
        }
        let receiver = RECEIVER.with(|r| r.borrow().as_ref().and_then(|h| *h.receiver.borrow()));
        let Some(receiver) = receiver else {
            break;
        };
        let message = {
            let mut state = hub().state.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                if let Some(m) = state.agents[index].mailbox.take() {
                    hub().cond.notify_all();
                    break m;
                }
                state = hub().cond.wait(state).unwrap_or_else(|e| e.into_inner());
            }
        };
        let proto = agent
            .heap
            .realm(realm)
            .intrinsics
            .try_get(Intrinsic::SharedArrayBufferPrototype)
            .or_else(|| {
                agent
                    .heap
                    .realm(realm)
                    .intrinsics
                    .try_get(Intrinsic::ObjectPrototype)
            });
        let sab = agent.heap.create_object(
            proto,
            ObjectKind::ArrayBuffer(Box::new(ArrayBufferData {
                storage: BufferStorage::Shared(message.block),
                max_byte_length: message.max_byte_length,
                detach_key: Value::Undefined,
            })),
        );
        let id = match message.id {
            MessageId::Number(n) => Value::Number(n),
            MessageId::BigInt(b) => Value::BigInt(agent.heap.alloc_bigint(b)),
        };
        if let Err(Throw(e)) = ops::call(
            &mut agent,
            receiver,
            Value::Undefined,
            &[Value::Object(sab), id],
        ) {
            eprintln!("{}", describe_uncaught(&mut agent, e));
        }
        if let Err(e) = drain(&mut agent) {
            eprintln!("{}", describe_uncaught(&mut agent, e));
        }
    }
    mark_dead(index);
}

fn broadcast_262(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let (block, max_byte_length) = match call.arg(0) {
        Value::Object(o) => match &agent.heap.object(o).kind {
            ObjectKind::ArrayBuffer(d) => match &d.storage {
                BufferStorage::Shared(b) => (Some(b.clone()), d.max_byte_length),
                _ => (None, None),
            },
            _ => (None, None),
        },
        _ => (None, None),
    };
    let Some(block) = block else {
        return agent.type_error("broadcast: not a SharedArrayBuffer");
    };
    let id = match call.arg(1) {
        Value::BigInt(b) => MessageId::BigInt(agent.heap.bigint(b).clone()),
        v => MessageId::Number(ops::to_number(agent, v)?),
    };
    let message = Message {
        block,
        max_byte_length,
        id,
    };
    let mut state = hub().state.lock().unwrap_or_else(|e| e.into_inner());
    for slot in state.agents.iter_mut().filter(|s| s.alive) {
        slot.mailbox = Some(message.clone());
    }
    hub().cond.notify_all();
    // Block until every running agent has taken the message.
    while state.agents.iter().any(|s| s.alive && s.mailbox.is_some()) {
        state = hub().cond.wait(state).unwrap_or_else(|e| e.into_inner());
    }
    Ok(Value::Undefined)
}

fn get_report_262(agent: &mut Agent, _call: NativeCall<'_>) -> JsResult<Value> {
    let report = hub()
        .state
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .reports
        .pop_front();
    Ok(match report {
        Some(s) => Value::String(agent.heap.alloc_str(&s)),
        None => Value::Null,
    })
}

fn receive_broadcast_262(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let callback = call.arg(0);
    if !ops::is_callable(agent, callback) {
        return agent.type_error("receiveBroadcast: the callback is not callable");
    }
    let realm = callee_realm(agent, call.callee);
    if let Some(host) = realm_host(realm) {
        *host.receiver.borrow_mut() = Some(callback);
        RECEIVER.with(|r| *r.borrow_mut() = Some(host));
    }
    Ok(Value::Undefined)
}

fn report_262(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let s = to_rust_string(agent, call.arg(0))?;
    hub()
        .state
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .reports
        .push_back(s);
    Ok(Value::Undefined)
}

fn leaving_262(_agent: &mut Agent, _call: NativeCall<'_>) -> JsResult<Value> {
    LEAVING.with(|l| l.set(true));
    Ok(Value::Undefined)
}

fn sleep_262(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let ms = ops::to_number(agent, call.arg(0))?;
    if ms.is_finite() && ms > 0.0 {
        std::thread::sleep(Duration::from_secs_f64(ms / 1000.0));
    }
    Ok(Value::Undefined)
}

fn monotonic_now_262(_agent: &mut Agent, _call: NativeCall<'_>) -> JsResult<Value> {
    Ok(Value::Number(
        (start_instant().elapsed().as_secs_f64() * 1000.0).floor(),
    ))
}

// ----- running files -----

/// Evaluates one file; `Err` holds the uncaught line.
fn run_file(
    agent: &mut Agent,
    host: &FsHost,
    realm: Realm,
    path: &str,
    options: &Options,
) -> Result<(), String> {
    let source =
        std::fs::read_to_string(path).map_err(|e| format!("Uncaught Error: {path}: {e}"))?;
    let full = std::fs::canonicalize(path).unwrap_or_else(|_| PathBuf::from(path));
    let name: Rc<str> = Rc::from(full.to_string_lossy().as_ref());
    if options.module {
        let m = match module::parse_module_record(agent, realm, &source, Some(name)) {
            Ok(m) => m,
            Err(e) => return Err(format!("Uncaught SyntaxError: {}", e.message)),
        };
        // A module that imports itself gets this same record.
        host.remember(realm, full, false, m);
        let p = module::load_link_evaluate(agent, m);
        keep_alive(realm, p);
        if let Err(e) = drain(agent) {
            return Err(describe_uncaught(agent, e));
        }
        if let Value::Object(p) = p {
            if let Some((PromiseState::Rejected, e)) = promise_state(agent, p) {
                return Err(describe_uncaught(agent, e));
            }
        }
        return Ok(());
    }
    let source = if options.strict {
        format!("\"use strict\";\n{source}")
    } else {
        source
    };
    let script = module::create_script_record(agent, realm, Some(name));
    if let Err(Throw(e)) = evaluate_script_record(agent, realm, &source, Some(script)) {
        return Err(describe_uncaught(agent, e));
    }
    if let Err(e) = drain(agent) {
        return Err(describe_uncaught(agent, e));
    }
    Ok(())
}

fn run(options: &Options) -> u8 {
    TEST262.with(|t| t.set(options.test262));
    start_instant();
    let mut agent = Agent::new();
    let host = Rc::new(FsHost::default());
    agent.module_host = Some(host.clone() as Rc<dyn ModuleHost>);
    let realm = new_shell_realm(&mut agent);
    for path in &options.files {
        if let Err(line) = run_file(&mut agent, &host, realm, path, options) {
            eprintln!("{line}");
            return 1;
        }
    }
    0
}

const MAIN_STACK: usize = 512 << 20;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let options = match parse_args(&args) {
        Command::Version => {
            println!("vjs {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Command::Usage => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
        Command::Run(options) => options,
    };
    // The interpreter does not recurse natively for JS calls, but native
    // calls (getters, callbacks of built-ins) do: give them room.
    let code = std::thread::Builder::new()
        .stack_size(MAIN_STACK)
        .spawn(move || run(&options))
        .ok()
        .and_then(|h| h.join().ok())
        .unwrap_or(1);
    use std::io::Write;
    let _ = std::io::stdout().flush();
    // Agent threads may still be waiting for broadcasts: exit now.
    std::process::exit(i32::from(code));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn version_and_usage() {
        assert_eq!(parse_args(&args(&["--version"])), Command::Version);
        for case in [&[][..], &["--help"], &["--version", "x"], &["--strict"]] {
            assert_eq!(parse_args(&args(case)), Command::Usage);
        }
    }

    #[test]
    fn flags_and_files() {
        assert_eq!(
            parse_args(&args(&["--test262", "--strict", "a.js", "b.js"])),
            Command::Run(Options {
                strict: true,
                module: false,
                test262: true,
                files: vec!["a.js".to_string(), "b.js".to_string()],
            })
        );
        assert_eq!(
            parse_args(&args(&["--module", "m.mjs"])),
            Command::Run(Options {
                module: true,
                files: vec!["m.mjs".to_string()],
                ..Options::default()
            })
        );
    }
}
