//! Web IDL bindings between the DOM and vjs (Web IDL §3, the ECMAScript
//! binding).
//!
//! The generator of `crates/vidrio/bindgen` turns the IDL files of
//! `crates/vidrio/idl` into static tables ([`generated`], described by
//! [`meta`]). This module reads them:
//!
//! - [`install`] creates interface objects, interface prototype objects,
//!   namespaces, legacy factory functions and window aliases in a realm,
//!   with attributes as accessors on the prototype, operations as methods,
//!   constants, iterators and `maplike`/`setlike` members;
//! - [`convert`] converts between ECMAScript values and IDL values
//!   ([`value::IdlValue`]); [`overload`] runs the overload resolution
//!   algorithm;
//! - [`platform`] defines platform objects (vjs host objects) and the
//!   exotic internal methods of legacy platform objects (indexed and named
//!   properties);
//! - [`wrappers`] keeps one wrapper per node and the node→wrapper table of
//!   each document, a garbage collector root while the document is active
//!   (vault/CONTRACT.md §3.4);
//! - [`host`] implements [`crate::dom::host::DomHost`] so that DOM
//!   algorithms call listeners, observers and filters;
//! - [`imp`] implements the members natively on top of `crate::dom`,
//!   after `crate::fetch` (the data APIs of k3-11) and
//!   `crate::browser::members` (HTML) had their turn. Members without an
//!   implementation (CSSOM, UI Events, …) exist on their objects and throw
//!   a `TypeError` when used.

pub mod convert;
pub mod exception;
pub mod generated;
pub mod host;
pub mod imp;
pub mod install;
pub mod meta;
pub mod overload;
pub mod platform;
pub mod value;
pub mod wrappers;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};

use vjs::gc::{Realm, Trace, Tracer};
use vjs::{Agent, JsResult, Object, Value};

use crate::dom::events::{ListenerRef, Target};
use crate::dom::{DocRef, HostRef};

pub use value::{Dict, IdlValue, Invocation};

/// Options of a window realm.
#[derive(Clone, Copy, Debug, Default)]
pub struct WindowOptions {
    /// The realm is a secure context: `[SecureContext]` members exist.
    pub secure_context: bool,
}

/// Values the DOM holds as [`HostRef`]s (callbacks, abort reasons,
/// `CustomEvent.detail`, thrown exceptions). Objects keep one reference
/// each, so that `removeEventListener(f)` finds `addEventListener(f)`.
#[derive(Default)]
pub struct HostValues {
    values: HashMap<u64, Value>,
    objects: HashMap<Object, u64>,
}

thread_local! {
    /// Every realm state of this thread (weakly: the realm owns it).
    static STATES: RefCell<Vec<Weak<RealmState>>> = const { RefCell::new(Vec::new()) };
    /// The next [`HostRef`]; unique in the thread so that every realm
    /// can resolve every reference.
    static NEXT_HOST_REF: Cell<u64> = const { Cell::new(1) };
}

/// The bindings' state of one realm: interface objects and prototypes,
/// the window's document, host values and the identity maps of objects
/// that are not nodes. It is the realm's `[[HostDefined]]`, so the realm
/// traces it.
pub struct RealmState {
    pub realm: Realm,
    pub options: WindowOptions,
    pub global: Cell<Option<Object>>,
    pub protos: RefCell<Vec<Option<Object>>>,
    pub ctors: RefCell<Vec<Option<Object>>>,
    pub iterator_protos: RefCell<Vec<Option<Object>>>,
    /// The document of the window.
    pub document: RefCell<Option<DocRef>>,
    pub values: RefCell<HostValues>,
    /// Wrappers of events, by the address of their `EventRef`; dropped by
    /// a finalizer when the wrapper dies.
    pub events: RefCell<HashMap<usize, (Weak<RefCell<crate::dom::events::Event>>, Object)>>,
    /// Wrappers of abort signals, by document address and signal.
    pub signals: RefCell<HashMap<(usize, u32), Object>>,
    /// Wrappers of mutation observers, by document address and observer.
    pub observers: RefCell<HashMap<(usize, u32), Object>>,
    /// Wrappers of `EventTarget` objects that are not nodes or signals.
    pub targets: RefCell<HashMap<u64, Object>>,
    pub next_target: Cell<u64>,
    /// The listener lists of `Window` and of other event targets.
    pub listeners: RefCell<HashMap<Target, Vec<ListenerRef>>>,
    /// Exceptions reported by callbacks ("report the exception"), oldest
    /// first; at most [`MAX_REPORTED`].
    pub reported: RefCell<Vec<Value>>,
    /// The `Window`'s current event (`window.event`).
    pub current_event: Cell<Option<Object>>,
    /// The time origin of `Event.timeStamp`.
    pub time_origin: std::time::Instant,
    /// `AbortSignal.timeout` timers waiting for the event loop: document,
    /// signal and milliseconds.
    pub timeouts: RefCell<Vec<(DocRef, crate::dom::abort::SignalId, u64)>>,
    /// What the HTML user agent keeps per `Window` (k3-10).
    pub html: crate::browser::window::WindowData,
}

/// The number of reported exceptions a realm keeps.
pub const MAX_REPORTED: usize = 64;

impl Trace for RealmState {
    fn trace(&self, tracer: &mut Tracer) {
        if let Some(g) = self.global.get() {
            tracer.object(g);
        }
        for list in [&self.protos, &self.ctors, &self.iterator_protos] {
            for o in list.borrow().iter().flatten() {
                tracer.object(*o);
            }
        }
        for v in self.values.borrow().values.values() {
            tracer.value(*v);
        }
        for map in [&self.signals, &self.observers] {
            for o in map.borrow().values() {
                tracer.object(*o);
            }
        }
        for o in self.targets.borrow().values() {
            tracer.object(*o);
        }
        for v in self.reported.borrow().iter() {
            tracer.value(*v);
        }
        if let Some(e) = self.current_event.get() {
            tracer.object(e);
        }
        self.html.trace(tracer);
    }
}

impl RealmState {
    fn new(realm: Realm, options: WindowOptions) -> RealmState {
        let n = generated::INTERFACES.len();
        RealmState {
            realm,
            options,
            global: Cell::new(None),
            protos: RefCell::new(vec![None; n]),
            ctors: RefCell::new(vec![None; n]),
            iterator_protos: RefCell::new(vec![None; n]),
            document: RefCell::new(None),
            values: RefCell::new(HostValues::default()),
            events: RefCell::new(HashMap::new()),
            signals: RefCell::new(HashMap::new()),
            observers: RefCell::new(HashMap::new()),
            targets: RefCell::new(HashMap::new()),
            next_target: Cell::new(1),
            listeners: RefCell::new(HashMap::new()),
            reported: RefCell::new(Vec::new()),
            current_event: Cell::new(None),
            time_origin: std::time::Instant::now(),
            timeouts: RefCell::new(Vec::new()),
            html: crate::browser::window::WindowData::default(),
        }
    }

    /// The interface prototype object of `iface` in this realm.
    pub fn proto(&self, iface: meta::Id) -> Option<Object> {
        self.protos.borrow().get(iface as usize).copied().flatten()
    }

    /// The interface object of `iface` in this realm.
    pub fn ctor(&self, iface: meta::Id) -> Option<Object> {
        self.ctors.borrow().get(iface as usize).copied().flatten()
    }

    /// The window's document.
    pub fn document(&self) -> Option<DocRef> {
        self.document.borrow().clone()
    }

    /// A [`HostRef`] for `value` (the same one for the same object).
    pub fn host_ref(&self, value: Value) -> HostRef {
        let mut values = self.values.borrow_mut();
        if let Value::Object(o) = value {
            if let Some(id) = values.objects.get(&o) {
                return HostRef(*id);
            }
        }
        let id = NEXT_HOST_REF.with(|n| {
            let id = n.get();
            n.set(id + 1);
            id
        });
        values.values.insert(id, value);
        if let Value::Object(o) = value {
            values.objects.insert(o, id);
        }
        HostRef(id)
    }

    fn lookup_host_ref(&self, r: HostRef) -> Option<Value> {
        self.values.borrow().values.get(&r.0).copied()
    }

    /// Records an exception reported by a callback.
    pub fn report(&self, value: Value) {
        let mut reported = self.reported.borrow_mut();
        if reported.len() >= MAX_REPORTED {
            reported.remove(0);
        }
        reported.push(value);
    }
}

/// The state of `realm`, if the bindings created it.
pub fn state_of(agent: &Agent, realm: Realm) -> Option<Rc<RealmState>> {
    let host = agent.heap.realm(realm).host_defined.clone()?;
    let target = Rc::as_ptr(&host) as *const ();
    STATES.with(|s| {
        s.borrow()
            .iter()
            .find(|w| w.as_ptr() as *const () == target)
            .and_then(Weak::upgrade)
    })
}

/// The state of the current realm, or of the first window realm of the
/// agent when the current realm has none.
pub fn current(agent: &Agent) -> Option<Rc<RealmState>> {
    if let Some(s) = agent
        .running_context()
        .and_then(|c| state_of(agent, c.realm))
    {
        return Some(s);
    }
    agent.realms().iter().find_map(|r| state_of(agent, *r))
}

/// The state of the current realm; a `TypeError` outside window realms.
pub fn current_state(agent: &mut Agent) -> JsResult<Rc<RealmState>> {
    match current(agent) {
        Some(s) => Ok(s),
        None => agent.type_error("no window realm"),
    }
}

/// The value of a [`HostRef`] of any realm of the agent.
pub fn host_value(agent: &Agent, r: HostRef) -> Value {
    for realm in agent.realms() {
        if let Some(s) = state_of(agent, *realm) {
            if let Some(v) = s.lookup_host_ref(r) {
                return v;
            }
        }
    }
    Value::Undefined
}

/// Creates a window realm for `document`: a `Window` global object with
/// every interface exposed in `Window` installed. The document becomes
/// active: its node→wrapper table is a root until
/// [`discard_document`].
pub fn create_window_realm(agent: &mut Agent, document: DocRef, options: WindowOptions) -> Realm {
    create_window_realm_with(agent, document, options, None, None)
}

/// [`create_window_realm`] for the `Window` of a browsing context: the
/// realm's `[[GlobalThisValue]]` is the context's `WindowProxy` `proxy`,
/// and the document records the tree changes the HTML user agent reacts
/// to ([`crate::dom::HtmlChange`]).
pub fn create_window_realm_with(
    agent: &mut Agent,
    document: DocRef,
    options: WindowOptions,
    proxy: Option<Object>,
    context: Option<crate::browser::ContextId>,
) -> Realm {
    let realm = agent.create_realm();
    let state = Rc::new(RealmState::new(realm, options));
    state.html.proxy.set(proxy);
    state.html.context.set(context);
    if context.is_some() {
        document.borrow_mut().html_tracking = true;
    }
    STATES.with(|s| {
        let mut s = s.borrow_mut();
        s.retain(|w| w.strong_count() > 0);
        s.push(Rc::downgrade(&state));
    });
    agent.heap.realm_mut(realm).host_defined = Some(state.clone() as Rc<dyn Trace>);
    *state.document.borrow_mut() = Some(document.clone());
    install::install_window(agent, &state);
    wrappers::activate_document(agent, &document, realm);
    realm
}

/// Discards the window's document: its wrappers stop being roots (the
/// ones still reachable keep the arena alive) and the realm is no longer
/// rooted by the agent.
pub fn discard_document(agent: &mut Agent, realm: Realm) {
    if let Some(state) = state_of(agent, realm) {
        if let Some(doc) = state.document() {
            wrappers::deactivate_document(agent, &doc);
        }
    }
    agent.remove_realm(realm);
}

/// The wrapper of a node (created on first use; one per node).
pub fn wrap_node(agent: &mut Agent, doc: &DocRef, node: crate::dom::NodeId) -> Object {
    wrappers::wrap_node(agent, doc, node)
}

/// The exceptions reported by callbacks in `realm` since the last call.
pub fn take_reported(agent: &Agent, realm: Realm) -> Vec<Value> {
    match state_of(agent, realm) {
        Some(s) => std::mem::take(&mut *s.reported.borrow_mut()),
        None => Vec::new(),
    }
}
