//! The user agent of WHATWG HTML §7–§8 over a single agent
//! (vault/CONTRACT.md §3.2): the event loop with its task sources, timers,
//! microtask checkpoints and rendering opportunities ([`event_loop`]);
//! browsing contexts, `Window`/`WindowProxy`, navigation and session
//! history ([`navigable`], [`window`]); document loading over vnet with
//! encoding sniffing and incremental parsing ([`loader`], [`document`]);
//! `script` elements, module scripts and import maps ([`script`],
//! [`module`]); error reporting and promise rejection tracking
//! ([`errors`]); event handler attributes ([`handlers`]); structured
//! cloning and messaging ([`clone`], [`messaging`]); the members of the
//! HTML interfaces that the bindings route here ([`members`]).
//!
//! Everything runs on the thread that owns the [`Browser`]. The bindings
//! reach the user agent through [`ua`] (a thread-local), because the code
//! behind a JavaScript call only receives the [`Agent`]. The user agent is
//! a root set of the garbage collector: the values of queued tasks,
//! timers, module loads and serialized messages stay alive while it holds
//! them. It never keeps a `RefCell` borrowed while script runs, so the
//! collector (which only runs at safe points) can always trace it.

pub mod clone;
pub mod document;
pub mod errors;
pub mod event_loop;
pub mod handlers;
pub mod loader;
pub mod members;
pub mod messaging;
pub mod module;
pub mod navigable;
pub mod script;
pub mod window;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use vjs::agent::ExecutionContext;
use vjs::gc::{Realm, RootSetId, Trace, Tracer};
use vjs::{Agent, JsResult, Value};

use crate::dom::DocRef;
use crate::url::Url;

pub use event_loop::{ClockMode, RenderingHook, TaskSource};
pub use navigable::ContextId;

thread_local! {
    /// The user agent of this thread's [`Browser`], if any.
    static UA: RefCell<Option<Rc<UserAgent>>> = const { RefCell::new(None) };
}

/// The user agent of the current thread (set while a [`Browser`] lives).
pub fn ua() -> Option<Rc<UserAgent>> {
    UA.with(|u| u.borrow().clone())
}

/// Options of a [`Browser`].
#[derive(Clone, Debug)]
pub struct BrowserOptions {
    /// Real time, or a virtual clock that jumps to the next timer or
    /// rendering opportunity when nothing else is runnable.
    pub clock: ClockMode,
    /// Whether scripting is enabled.
    pub scripting: bool,
    /// The `User-Agent` string (`navigator.userAgent`).
    pub user_agent: String,
    /// The CA certificate file for `https` (besides the system roots).
    pub ca_cert: Option<std::path::PathBuf>,
}

impl Default for BrowserOptions {
    fn default() -> BrowserOptions {
        BrowserOptions {
            clock: ClockMode::Real,
            scripting: true,
            user_agent: format!(
                "Mozilla/5.0 (X11; Linux x86_64) vidrio/{}",
                env!("CARGO_PKG_VERSION")
            ),
            ca_cert: None,
        }
    }
}

/// The state of the user agent shared by the event loop and the bindings.
pub struct UserAgent {
    pub options: BrowserOptions,
    pub event_loop: event_loop::EventLoop,
    pub contexts: RefCell<Vec<Rc<navigable::BrowsingContext>>>,
    next_context: Cell<ContextId>,
    /// The HTML state of each document that belongs to a browsing context
    /// (or once did), by the address of its `RefCell`.
    pub docs: RefCell<HashMap<usize, Rc<document::DocInfo>>>,
    pub net: loader::Network,
    pub scripts: RefCell<HashMap<script::ScriptId, script::ScriptEntry>>,
    pub next_script: Cell<script::ScriptId>,
    pub modules: RefCell<module::ModuleMaps>,
    pub ports: RefCell<messaging::Ports>,
    /// The state of the data APIs (k3-11): rooted values, cookies, abort
    /// algorithms.
    pub fetch: crate::fetch::FetchState,
}

impl Trace for UserAgent {
    fn trace(&self, tracer: &mut Tracer) {
        self.event_loop.trace(tracer);
        for c in self.contexts.borrow().iter() {
            c.trace(tracer);
        }
        for s in self.scripts.borrow().values() {
            s.trace(tracer);
        }
        self.modules.borrow().trace(tracer);
        self.ports.borrow().trace(tracer);
        self.fetch.trace(tracer);
    }
}

impl UserAgent {
    fn new(options: BrowserOptions) -> UserAgent {
        let clock = options.clock;
        UserAgent {
            event_loop: event_loop::EventLoop::new(clock),
            contexts: RefCell::new(Vec::new()),
            next_context: Cell::new(1),
            docs: RefCell::new(HashMap::new()),
            net: loader::Network::new(options.ca_cert.as_deref()),
            scripts: RefCell::new(HashMap::new()),
            next_script: Cell::new(1),
            modules: RefCell::new(module::ModuleMaps::default()),
            ports: RefCell::new(messaging::Ports::default()),
            fetch: crate::fetch::FetchState::default(),
            options,
        }
    }

    /// A new browsing context id.
    pub fn new_context_id(&self) -> ContextId {
        let id = self.next_context.get();
        self.next_context.set(id + 1);
        id
    }

    /// The browsing context `id`, if it was not discarded.
    pub fn context(&self, id: ContextId) -> Option<Rc<navigable::BrowsingContext>> {
        self.contexts
            .borrow()
            .iter()
            .find(|c| c.id == id && !c.discarded.get())
            .cloned()
    }

    /// The browsing context `id`, even if discarded (its `WindowProxy`
    /// outlives it).
    pub fn context_any(&self, id: ContextId) -> Option<Rc<navigable::BrowsingContext>> {
        self.contexts.borrow().iter().find(|c| c.id == id).cloned()
    }

    /// The HTML state of `doc`, if the user agent knows it.
    pub fn doc_info(&self, doc: &DocRef) -> Option<Rc<document::DocInfo>> {
        // The key is an address: check the entry is still this document's.
        self.docs
            .borrow()
            .get(&doc_key(doc))
            .filter(|i| i.document().is_some_and(|d| Rc::ptr_eq(&d, doc)))
            .cloned()
    }

    /// True if `doc` is the active document of a browsing context whose
    /// ancestors' documents are all active ("fully active").
    pub fn is_fully_active(&self, doc: &DocRef) -> bool {
        let Some(info) = self.doc_info(doc) else {
            return false;
        };
        let Some(ctx) = info.context.get().and_then(|c| self.context(c)) else {
            return false;
        };
        if !ctx.active_document().is_some_and(|d| Rc::ptr_eq(&d, doc)) {
            return false;
        }
        match ctx.parent {
            None => true,
            Some(p) => match self.context(p).and_then(|p| p.active_document()) {
                Some(parent_doc) => {
                    // The container must be in the parent's active document.
                    let container_ok = ctx
                        .container_document()
                        .is_some_and(|d| Rc::ptr_eq(&d, &parent_doc));
                    container_ok && self.is_fully_active(&parent_doc)
                }
                None => false,
            },
        }
    }

    /// Whether scripting is enabled for `doc`: it has a browsing context
    /// and the user agent runs scripts.
    pub fn scripting_enabled(&self, doc: &DocRef) -> bool {
        self.options.scripting
            && self
                .doc_info(doc)
                .is_some_and(|i| i.context.get().is_some() && i.realm.get().is_some())
    }
}

/// The key of a document in [`UserAgent::docs`].
pub fn doc_key(doc: &DocRef) -> usize {
    Rc::as_ptr(doc) as *const () as usize
}

/// Runs `f` with an execution context of `realm` on the stack, so that
/// objects and errors it creates belong to that realm.
pub fn in_realm<T>(agent: &mut Agent, realm: Realm, f: impl FnOnce(&mut Agent) -> T) -> T {
    agent.push_context(ExecutionContext {
        realm,
        function: None,
        script_or_module: None,
    });
    let r = f(agent);
    agent.pop_context();
    r
}

/// A browser: the agent (one heap for every window) and its user agent.
/// Only one may exist per thread at a time.
pub struct Browser {
    pub agent: Agent,
    pub ua: Rc<UserAgent>,
    root_set: Option<RootSetId>,
}

impl Browser {
    pub fn new(options: BrowserOptions) -> Browser {
        let mut agent = Agent::new();
        // A window agent cannot block (HTML §8.1.2).
        agent.can_block = false;
        let ua = Rc::new(UserAgent::new(options));
        UA.with(|u| *u.borrow_mut() = Some(ua.clone()));
        let root_set = agent.heap.add_root_set(ua.clone() as Rc<dyn Trace>);
        agent.module_host =
            Some(Rc::new(module::HtmlModuleHost) as Rc<dyn vjs::module::ModuleHost>);
        agent.promise_rejection_tracker =
            Some(errors::promise_rejection_tracker as vjs::agent::RejectionTracker);
        Browser {
            agent,
            ua,
            root_set: Some(root_set),
        }
    }

    /// Creates a top-level browsing context and navigates it to `url`;
    /// the document loads as the event loop runs.
    pub fn open(&mut self, url: &str) -> Result<ContextId, String> {
        let parsed = Url::parse(url, None).map_err(|_| format!("invalid URL: {url}"))?;
        let id = navigable::create_top_level(&mut self.agent, None, "");
        if parsed.href() != "about:blank" {
            navigable::navigate(&mut self.agent, id, navigable::NavigateParams::new(parsed));
        }
        Ok(id)
    }

    /// Runs the event loop until nothing is left to do, or until `limit`
    /// of clock time (virtual or real) has passed. Returns false if the
    /// limit was reached first.
    pub fn run_until_idle(&mut self, limit: Duration) -> bool {
        event_loop::run_until(&mut self.agent, limit, &mut |_| false)
    }

    /// Runs the event loop until `done` returns true (checked between
    /// tasks), it becomes idle, or `limit` passes. Returns whether `done`
    /// returned true.
    pub fn run_until(&mut self, limit: Duration, mut done: impl FnMut(&mut Agent) -> bool) -> bool {
        let mut reached = false;
        event_loop::run_until(&mut self.agent, limit, &mut |agent| {
            reached = done(agent);
            reached
        });
        reached
    }

    /// Runs one step of the event loop (a task, a vjs host task or a
    /// rendering update); false if nothing was runnable.
    pub fn spin_once(&mut self) -> bool {
        event_loop::run_once(&mut self.agent)
    }

    /// The active document of browsing context `id`.
    pub fn document(&self, id: ContextId) -> Option<DocRef> {
        self.ua.context(id).and_then(|c| c.active_document())
    }

    /// The realm of the active window of `id`.
    pub fn realm(&self, id: ContextId) -> Option<Realm> {
        self.ua
            .context(id)
            .and_then(|c| c.active_window())
            .map(|w| w.realm)
    }

    /// Evaluates `source` as a classic script in the active window of
    /// `id` and performs a microtask checkpoint afterwards.
    pub fn evaluate(&mut self, id: ContextId, source: &str) -> JsResult<Value> {
        let Some(realm) = self.realm(id) else {
            return self.agent.type_error("no active window");
        };
        let r = vjs::evaluate_script_in(&mut self.agent, realm, source);
        event_loop::clean_up_after_running_script(&mut self.agent);
        r
    }

    /// [`Browser::evaluate`], with the completion value (or the thrown
    /// value, prefixed with `threw: `) converted to a string.
    pub fn evaluate_to_string(&mut self, id: ContextId, source: &str) -> String {
        match self.evaluate(id, source) {
            Ok(v) => value_to_string(&mut self.agent, v),
            Err(e) => format!("threw: {}", value_to_string(&mut self.agent, e.0)),
        }
    }

    /// Installs the rendering hook of K4 (style, layout and painting).
    pub fn set_rendering_hook(&mut self, hook: Rc<dyn RenderingHook>) {
        *self.ua.event_loop.rendering_hook.borrow_mut() = Some(hook);
    }

    /// The top-level browsing contexts that are open.
    pub fn top_level_contexts(&self) -> Vec<ContextId> {
        self.ua
            .contexts
            .borrow()
            .iter()
            .filter(|c| c.parent.is_none() && !c.discarded.get())
            .map(|c| c.id)
            .collect()
    }
}

impl Drop for Browser {
    fn drop(&mut self) {
        if let Some(id) = self.root_set.take() {
            self.agent.heap.remove_root_set(id);
        }
        UA.with(|u| {
            let mut u = u.borrow_mut();
            if u.as_ref().is_some_and(|x| Rc::ptr_eq(x, &self.ua)) {
                *u = None;
            }
        });
    }
}

/// `ToString(v)` as a Rust string; a value whose conversion throws becomes
/// `<unprintable>`.
pub fn value_to_string(agent: &mut Agent, v: Value) -> String {
    match vjs::ops::to_string(agent, v) {
        Ok(s) => agent.heap.string_to_rust(s),
        Err(_) => "<unprintable>".to_string(),
    }
}
