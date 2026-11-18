//! The bindings' [`DomHost`]: DOM algorithms call event listeners,
//! mutation observers and node filters through it, and it maps event
//! targets, events and records to their wrappers.
//!
//! The listener lists of `Window` and of plain `EventTarget` objects live
//! in the realm state. [`DomHost::other_listeners`] lends one list to the
//! DOM; the host puts it back before any other call (and when dropped),
//! so no script ever runs while a list is lent.

use std::rc::Rc;

use vjs::agent::{Job, JobTask};
use vjs::function::is_callable;
use vjs::gc::{Trace, Tracer};
use vjs::ops;
use vjs::{Agent, JsResult, Object, Value};

use super::convert;
use super::exception;
use super::generated::{ids, interface_by_name};
use super::meta::{Id, Ty};
use super::platform::{self, Native};
use super::{RealmState, current_state, host_value};
use crate::dom::abort::SignalId;
use crate::dom::events::{Event, EventInterface, EventRef, ListenerRef, Target};
use crate::dom::host::DomHost;
use crate::dom::observer::{MutationRecord, ObserverId, notify_mutation_observers};
use crate::dom::{DocRef, DomException, HostRef, NodeId};

/// "Call a user object's operation" (Web IDL §3.11): calls `callback` if
/// it is callable (with `this_arg`), else its `op` method (with the
/// object as `this`).
pub fn call_user_object_operation(
    agent: &mut Agent,
    callback: Value,
    op: &str,
    this_arg: Value,
    args: &[Value],
) -> JsResult<Value> {
    if is_callable(agent, callback) {
        return ops::call(agent, callback, this_arg, args);
    }
    let Value::Object(o) = callback else {
        return agent.type_error("The callback is not an object.");
    };
    let key = agent.heap.key_from_str(op);
    let f = ops::get(agent, o, key)?;
    if !is_callable(agent, f) {
        return agent.type_error(&format!("The callback's '{op}' is not a function."));
    }
    ops::call(agent, f, callback, args)
}

/// The interface of an event's wrapper.
fn event_interface(e: &Event) -> Id {
    let name = match e.interface {
        EventInterface::Event => return ids::Event,
        EventInterface::CustomEvent => "CustomEvent",
        EventInterface::UiEvent => "UIEvent",
        EventInterface::MouseEvent => "MouseEvent",
        EventInterface::Other(name) => name,
    };
    interface_by_name(name).unwrap_or(ids::Event)
}

/// Registers `o` as the wrapper of `event`; a finalizer forgets it.
pub fn register_event_wrapper(
    agent: &mut Agent,
    state: &Rc<RealmState>,
    event: &EventRef,
    o: Object,
) {
    let key = Rc::as_ptr(event) as usize;
    state
        .events
        .borrow_mut()
        .insert(key, (Rc::downgrade(event), o));
    let weak = Rc::downgrade(state);
    agent.heap.add_finalizer(
        o,
        Box::new(move || {
            if let Some(s) = weak.upgrade() {
                let mut events = s.events.borrow_mut();
                if events.get(&key).is_some_and(|(_, w)| *w == o) {
                    events.remove(&key);
                }
            }
        }),
    );
}

/// The wrapper of an event (created for events the DOM made).
pub fn event_wrapper(agent: &mut Agent, state: &Rc<RealmState>, event: &EventRef) -> Object {
    let key = Rc::as_ptr(event) as usize;
    if let Some((weak, o)) = state.events.borrow().get(&key) {
        if weak.upgrade().is_some_and(|e| Rc::ptr_eq(&e, event)) {
            return *o;
        }
    }
    let iface = event_interface(&event.borrow());
    let o = platform::create(agent, state, iface, Native::Event(event.clone()), None);
    register_event_wrapper(agent, state, event, o);
    o
}

/// The slot of an event wrapper that holds the wrapper of the document
/// its node targets belong to (an [`Event`] names nodes by id only).
pub const EVENT_DOCUMENT: &str = "[[Document]]";

/// Records `doc` as the document of the targets of the event wrapper
/// `ev`.
pub fn set_event_document(agent: &mut Agent, ev: Object, doc: &DocRef) {
    let d = super::wrappers::wrap_node(agent, doc, NodeId::DOCUMENT);
    if let Some(p) = platform::platform_ref(agent, Value::Object(ev)) {
        p.get().set_slot(EVENT_DOCUMENT, Value::Object(d));
    }
}

/// The document recorded by [`set_event_document`].
pub fn event_document(agent: &Agent, ev: Object) -> Option<DocRef> {
    let p = platform::platform_ref(agent, Value::Object(ev))?;
    let d = p.get().slot(EVENT_DOCUMENT)?;
    let doc = platform::platform_ref(agent, d)?;
    let native = doc.get().native.borrow();
    match &*native {
        Native::Node(doc, _) => Some(doc.clone()),
        _ => None,
    }
}

/// The wrapper of an abort signal.
pub fn signal_wrapper(
    agent: &mut Agent,
    state: &RealmState,
    doc: &DocRef,
    signal: SignalId,
) -> Object {
    let key = (Rc::as_ptr(doc) as usize, signal.0);
    if let Some(o) = state.signals.borrow().get(&key) {
        return *o;
    }
    let o = platform::create(
        agent,
        state,
        ids::AbortSignal,
        Native::AbortSignal(doc.clone(), signal),
        None,
    );
    state.signals.borrow_mut().insert(key, o);
    o
}

/// The wrapper of a mutation record.
pub fn record_wrapper(
    agent: &mut Agent,
    state: &RealmState,
    doc: &DocRef,
    record: MutationRecord,
) -> Object {
    platform::create(
        agent,
        state,
        ids::MutationRecord,
        Native::MutationRecord(doc.clone(), Rc::new(record)),
        None,
    )
}

/// The microtask that notifies mutation observers.
struct ObserverMicrotask {
    doc: DocRef,
}

impl Trace for ObserverMicrotask {
    fn trace(&self, _tracer: &mut Tracer) {}
}

impl JobTask for ObserverMicrotask {
    fn run(self: Box<Self>, agent: &mut Agent) -> JsResult<()> {
        let mut host = JsHost::new(agent, self.doc.clone())?;
        notify_mutation_observers(&self.doc, &mut host);
        Ok(())
    }
}

/// Queues a mutation observer microtask if a DOM algorithm asked for one
/// in `doc`.
pub fn queue_observer_microtask(agent: &mut Agent, doc: &DocRef) {
    let requested = match doc.try_borrow_mut() {
        Ok(mut d) => d.take_mutation_observer_microtask(),
        Err(_) => false,
    };
    if requested {
        let realm = agent.current_realm();
        agent.enqueue_job(Job {
            realm: Some(realm),
            task: Box::new(ObserverMicrotask { doc: doc.clone() }),
        });
    }
}

/// A [`DomHost`] that runs script in the current realm for algorithms on
/// `doc`.
pub struct JsHost<'a> {
    pub agent: &'a mut Agent,
    pub doc: DocRef,
    pub state: Rc<RealmState>,
    lent: Option<(Target, Vec<ListenerRef>)>,
}

impl<'a> JsHost<'a> {
    pub fn new(agent: &'a mut Agent, doc: DocRef) -> JsResult<JsHost<'a>> {
        let state = current_state(agent)?;
        Ok(JsHost {
            agent,
            doc,
            state,
            lent: None,
        })
    }

    /// Puts a lent listener list back.
    fn give_back(&mut self) {
        if let Some((target, list)) = self.lent.take() {
            let mut lists = self.state.listeners.borrow_mut();
            match lists.get_mut(&target) {
                // A nested host created a list meanwhile: keep both.
                Some(existing) => {
                    let mut merged = list;
                    for l in existing.drain(..) {
                        if !merged.iter().any(|m| Rc::ptr_eq(m, &l)) {
                            merged.push(l);
                        }
                    }
                    *existing = merged;
                }
                None => {
                    lists.insert(target, list);
                }
            }
        }
    }

    /// The JavaScript object of an event target.
    pub fn target_value(&mut self, target: Target) -> Value {
        match target {
            Target::Node(n) => Value::Object(super::wrappers::wrap_node(self.agent, &self.doc, n)),
            Target::Signal(s) => {
                let doc = self.doc.clone();
                Value::Object(signal_wrapper(self.agent, &self.state, &doc, s))
            }
            Target::Window => self
                .state
                .global
                .get()
                .map_or(Value::Undefined, Value::Object),
            Target::Other(id) => self
                .state
                .targets
                .borrow()
                .get(&id)
                .copied()
                .map_or(Value::Undefined, Value::Object),
        }
    }
}

impl Drop for JsHost<'_> {
    fn drop(&mut self) {
        self.give_back();
    }
}

impl DomHost for JsHost<'_> {
    fn call_listener(
        &mut self,
        callback: HostRef,
        current_target: Target,
        event: &EventRef,
        in_shadow_tree: bool,
    ) -> Result<(), HostRef> {
        self.give_back();
        let cb = host_value(self.agent, callback);
        let this = self.target_value(current_target);
        let state = self.state.clone();
        let ev = event_wrapper(self.agent, &state, event);
        let doc = self.doc.clone();
        set_event_document(self.agent, ev, &doc);
        let previous = state.current_event.get();
        if !in_shadow_tree {
            state.current_event.set(Some(ev));
        }
        let result =
            call_user_object_operation(self.agent, cb, "handleEvent", this, &[Value::Object(ev)]);
        state.current_event.set(previous);
        result.map(|_| ()).map_err(|e| state.host_ref(e.0))
    }

    fn report_exception(&mut self, exception: HostRef) {
        self.give_back();
        let v = host_value(self.agent, exception);
        self.state.report(v);
    }

    fn call_mutation_observer(
        &mut self,
        observer: ObserverId,
        callback: HostRef,
        records: Vec<MutationRecord>,
    ) -> Result<(), HostRef> {
        self.give_back();
        let cb = host_value(self.agent, callback);
        let key = (Rc::as_ptr(&self.doc) as usize, observer.0);
        let this = self
            .state
            .observers
            .borrow()
            .get(&key)
            .copied()
            .map_or(Value::Undefined, Value::Object);
        let state = self.state.clone();
        let doc = self.doc.clone();
        let mut values = Vec::with_capacity(records.len());
        for r in records {
            values.push(Value::Object(record_wrapper(self.agent, &state, &doc, r)));
        }
        let list = Value::Object(ops::create_array_from_list(self.agent, &values));
        ops::call(self.agent, cb, this, &[list, this])
            .map(|_| ())
            .map_err(|e| state.host_ref(e.0))
    }

    fn call_node_filter(&mut self, filter: HostRef, node: NodeId) -> Result<u16, HostRef> {
        self.give_back();
        let f = host_value(self.agent, filter);
        let n = Value::Object(super::wrappers::wrap_node(self.agent, &self.doc, node));
        let state = self.state.clone();
        let result =
            call_user_object_operation(self.agent, f, "acceptNode", Value::Undefined, &[n])
                .and_then(|v| convert::to_idl(self.agent, v, Ty::UnsignedShort));
        match result {
            Ok(v) => Ok(v.as_u64() as u16),
            Err(e) => Err(state.host_ref(e.0)),
        }
    }

    fn create_exception(&mut self, e: DomException) -> HostRef {
        self.give_back();
        let o = exception::throw(self.agent, e);
        self.state.host_ref(o.0)
    }

    fn now(&mut self) -> f64 {
        self.state.time_origin.elapsed().as_secs_f64() * 1000.0
    }

    fn document_parent(&mut self, _event: &Event) -> Option<Target> {
        match self.state.document() {
            Some(d) if Rc::ptr_eq(&d, &self.doc) => Some(Target::Window),
            _ => None,
        }
    }

    fn other_listeners(&mut self, target: Target) -> Option<&mut Vec<ListenerRef>> {
        self.give_back();
        let list = self
            .state
            .listeners
            .borrow_mut()
            .remove(&target)
            .unwrap_or_default();
        self.lent = Some((target, list));
        self.lent.as_mut().map(|(_, l)| l)
    }

    fn start_abort_timeout(&mut self, signal: SignalId, milliseconds: u64) {
        self.give_back();
        self.state
            .timeouts
            .borrow_mut()
            .push((self.doc.clone(), signal, milliseconds));
    }
}
