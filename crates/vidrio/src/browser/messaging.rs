//! Cross-document messaging and channel messaging (HTML §9.3, §9.4):
//! `window.postMessage` and `MessageChannel`/`MessagePort`, with
//! structured cloning and transfer ([`super::clone`]).
//!
//! A port is a record of the user agent ([`PortState`]) entangled with
//! another one; its `MessagePort` object can change when the port is
//! transferred (the old object is left detached). Messages wait in the
//! port message queue until the port is started (`start()` or setting
//! `onmessage`), then each one is delivered by a task of the posted
//! message task source of the port's window.

use std::collections::{HashMap, VecDeque};
use std::rc::Rc;

use vjs::gc::{Realm, Trace, Tracer};
use vjs::{Agent, JsResult, Object, Value};

use super::clone::{Serialized, deserialize_with_transfer, serialize_with_transfer};
use super::event_loop::{TaskRun, TaskSource};
use super::navigable::fire_window_event;
use super::{UserAgent, in_realm};
use crate::bindings::RealmState;
use crate::bindings::exception::dom;
use crate::bindings::generated::interface_by_name;
use crate::bindings::platform::{self, Native, PlatformRef};
use crate::dom::DomException;
use crate::dom::events::{Event, EventInit, EventInterface, Target, dispatch};
use crate::url::{Origin, Url};

pub type PortId = u64;

/// A message port.
pub struct PortState {
    pub entangled: Option<PortId>,
    /// The realm and object of its current `MessagePort`, and its target
    /// id in that realm.
    pub realm: Option<Realm>,
    pub object: Option<Object>,
    pub target: u64,
    pub enabled: bool,
    pub closed: bool,
    pub queue: VecDeque<Rc<Serialized>>,
}

/// Every port of the user agent.
#[derive(Default)]
pub struct Ports {
    pub ports: HashMap<PortId, PortState>,
    next: PortId,
    /// `MessagePort` objects whose port was transferred away.
    detached: Vec<Object>,
}

impl Trace for Ports {
    fn trace(&self, tracer: &mut Tracer) {
        for p in self.ports.values() {
            if let Some(o) = p.object {
                tracer.object(o);
            }
            if let Some(r) = p.realm {
                tracer.realm(r);
            }
            for m in &p.queue {
                m.trace(tracer);
            }
        }
        for o in &self.detached {
            tracer.object(*o);
        }
    }
}

/// The port whose current object is `o`.
pub fn port_of_object(_agent: &Agent, o: Object) -> Option<PortId> {
    let ua = super::ua()?;
    let ports = ua.ports.borrow();
    ports
        .ports
        .iter()
        .find(|(_, p)| p.object == Some(o))
        .map(|(id, _)| *id)
}

/// True if `o` is a `MessagePort` whose port was transferred away.
pub fn port_is_detached(_agent: &Agent, o: Object) -> bool {
    super::ua().is_some_and(|ua| ua.ports.borrow().detached.contains(&o))
}

/// Detaches the port of `o` from its object for a transfer.
pub fn ship_port(agent: &mut Agent, o: Object) -> Option<PortId> {
    let id = port_of_object(agent, o)?;
    let ua = super::ua()?;
    let mut ports = ua.ports.borrow_mut();
    let p = ports.ports.get_mut(&id)?;
    p.object = None;
    p.realm = None;
    p.enabled = false;
    ports.detached.push(o);
    Some(id)
}

/// A new `MessagePort` object in the realm of `state` (no port yet).
fn new_port_object(agent: &mut Agent, state: &Rc<RealmState>) -> (Object, u64) {
    let target = state.next_target.get();
    state.next_target.set(target + 1);
    let iface =
        interface_by_name("MessagePort").unwrap_or(crate::bindings::generated::ids::EventTarget);
    let o = platform::create(agent, state, iface, Native::EventTarget(target), None);
    state.targets.borrow_mut().insert(target, o);
    (o, target)
}

/// The `MessagePort` object of the transferred port `id` in the realm of
/// `state`.
pub fn port_object(agent: &mut Agent, state: &Rc<RealmState>, id: PortId) -> Object {
    let (o, target) = new_port_object(agent, state);
    if let Some(ua) = super::ua() {
        if let Some(p) = ua.ports.borrow_mut().ports.get_mut(&id) {
            p.object = Some(o);
            p.realm = Some(state.realm);
            p.target = target;
            p.enabled = false;
        }
    }
    o
}

/// `new MessageChannel()`: two entangled ports.
pub fn new_channel(agent: &mut Agent, state: &Rc<RealmState>) -> (Object, Object) {
    let (a, ta) = new_port_object(agent, state);
    let (b, tb) = new_port_object(agent, state);
    if let Some(ua) = super::ua() {
        let mut ports = ua.ports.borrow_mut();
        let ia = ports.next + 1;
        let ib = ports.next + 2;
        ports.next += 2;
        for (id, other, o, t) in [(ia, ib, a, ta), (ib, ia, b, tb)] {
            ports.ports.insert(
                id,
                PortState {
                    entangled: Some(other),
                    realm: Some(state.realm),
                    object: Some(o),
                    target: t,
                    enabled: false,
                    closed: false,
                    queue: VecDeque::new(),
                },
            );
        }
    }
    (a, b)
}

/// The transfer list of `postMessage(message, transfer)` or of
/// `postMessage(message, options)`.
pub fn transfer_list(agent: &mut Agent, v: Value) -> JsResult<Vec<Value>> {
    let Value::Object(o) = v else {
        return Ok(Vec::new());
    };
    if vjs::ops::is_array(agent, v)? {
        return vjs::ops::create_list_from_array_like(agent, v, vjs::ops::ListElementTypes::All);
    }
    let key = agent.heap.key_from_str("transfer");
    let t = vjs::ops::get(agent, o, key)?;
    if t.is_undefined() {
        return Ok(Vec::new());
    }
    vjs::ops::create_list_from_array_like(agent, t, vjs::ops::ListElementTypes::All)
}

/// The message port post message steps of `port.postMessage`.
pub fn port_post_message(
    agent: &mut Agent,
    port: Object,
    message: Value,
    transfer: &[Value],
) -> JsResult<()> {
    if transfer.contains(&Value::Object(port)) {
        return dom(agent, DomException::DataCloneError);
    }
    let serialized = serialize_with_transfer(agent, message, transfer)?;
    let Some(ua) = super::ua() else {
        return Ok(());
    };
    let Some(id) = port_of_object(agent, port) else {
        return Ok(());
    };
    let target = ua.ports.borrow().ports.get(&id).and_then(|p| p.entangled);
    let Some(target) = target else {
        return Ok(());
    };
    let enabled = {
        let mut ports = ua.ports.borrow_mut();
        match ports.ports.get_mut(&target) {
            Some(p) if !p.closed => {
                p.queue.push_back(Rc::new(serialized));
                p.enabled
            }
            _ => false,
        }
    };
    if enabled {
        flush(&ua, target);
    }
    Ok(())
}

/// Starts the port of `o` (`start()`).
pub fn start(agent: &mut Agent, o: Object) {
    let Some(ua) = super::ua() else {
        return;
    };
    let Some(id) = port_of_object(agent, o) else {
        return;
    };
    if let Some(p) = ua.ports.borrow_mut().ports.get_mut(&id) {
        p.enabled = true;
    }
    flush(&ua, id);
}

/// Setting `onmessage` of a `MessagePort` starts it.
pub fn implicitly_started(agent: &mut Agent, owner: &PlatformRef) {
    let is_port = interface_by_name("MessagePort").is_some_and(|i| owner.get().implements(i));
    if is_port {
        start(agent, owner.object);
    }
}

/// `close()`: disentangles the port.
pub fn close(agent: &mut Agent, o: Object) {
    let Some(ua) = super::ua() else {
        return;
    };
    let Some(id) = port_of_object(agent, o) else {
        return;
    };
    let mut ports = ua.ports.borrow_mut();
    let other = match ports.ports.get_mut(&id) {
        Some(p) => {
            p.closed = true;
            p.entangled.take()
        }
        None => None,
    };
    if let Some(other) = other {
        if let Some(p) = ports.ports.get_mut(&other) {
            p.entangled = None;
        }
    }
}

/// Queues one task per waiting message of port `id`.
fn flush(ua: &UserAgent, id: PortId) {
    let (messages, realm) = {
        let mut ports = ua.ports.borrow_mut();
        let Some(p) = ports.ports.get_mut(&id) else {
            return;
        };
        if !p.enabled || p.realm.is_none() {
            return;
        }
        (std::mem::take(&mut p.queue), p.realm)
    };
    let Some(realm) = realm else {
        return;
    };
    let doc = super::window::document_of_realm(ua, realm);
    for m in messages {
        ua.event_loop.queue(
            TaskSource::PostedMessage,
            doc.clone(),
            Box::new(PortMessageTask {
                port: id,
                message: m,
            }),
        );
    }
}

/// The task that delivers one message to a port.
struct PortMessageTask {
    port: PortId,
    message: Rc<Serialized>,
}

impl Trace for PortMessageTask {
    fn trace(&self, tracer: &mut Tracer) {
        self.message.trace(tracer);
    }
}

impl TaskRun for PortMessageTask {
    fn run(self: Box<Self>, agent: &mut Agent) {
        let Some(ua) = super::ua() else {
            return;
        };
        let (realm, object, target) = match ua.ports.borrow().ports.get(&self.port) {
            Some(p) => (p.realm, p.object, p.target),
            None => return,
        };
        let (Some(realm), Some(object)) = (realm, object) else {
            return;
        };
        let Some(state) = crate::bindings::state_of(agent, realm) else {
            return;
        };
        let keep = Rc::new(super::window::Roots(vec![Value::Object(object)]));
        let root = agent.heap.add_root_set(keep.clone() as Rc<dyn Trace>);
        match deserialize_with_transfer(agent, realm, &self.message) {
            Ok((data, ports)) => {
                let ports = frozen_array(agent, realm, &ports);
                let origin = Value::String(agent.heap.alloc_str(""));
                fire_at_port(
                    agent,
                    &state,
                    target,
                    "message",
                    &[
                        ("data", data),
                        ("origin", origin),
                        ("lastEventId", origin),
                        ("source", Value::Null),
                        ("ports", ports),
                    ],
                );
            }
            Err(_) => {
                fire_at_port(agent, &state, target, "messageerror", &[]);
            }
        }
        agent.heap.remove_root_set(root);
    }
}

/// A frozen array of `items` in `realm`.
fn frozen_array(agent: &mut Agent, realm: Realm, items: &[Object]) -> Value {
    let values: Vec<Value> = items.iter().map(|o| Value::Object(*o)).collect();
    in_realm(agent, realm, |agent| {
        let a = vjs::ops::create_array_from_list(agent, &values);
        let _ = vjs::ops::set_integrity_level(agent, a, vjs::ops::IntegrityLevel::Frozen);
        Value::Object(a)
    })
}

/// Fires a `MessageEvent` named `type_` at the port with target id
/// `target` in the window of `state`.
fn fire_at_port(
    agent: &mut Agent,
    state: &Rc<RealmState>,
    target: u64,
    type_: &str,
    slots: &[(&'static str, Value)],
) {
    let Some(doc) = state.document() else {
        return;
    };
    let now = super::window::window_now(state);
    let mut e = Event::new_trusted(type_, EventInit::default(), now);
    e.interface = EventInterface::Other("MessageEvent");
    let ev = e.into_ref();
    let o = crate::bindings::host::event_wrapper(agent, state, &ev);
    if let Some(p) = platform::platform_ref(agent, Value::Object(o)) {
        for (k, v) in slots {
            p.get().set_slot(*k, *v);
        }
    }
    let mut host = crate::bindings::host::JsHost::with_state(agent, doc.clone(), state.clone());
    dispatch(&doc, &mut host, &ev, Target::Other(target), false);
    drop(host);
    super::event_loop::clean_up_after_running_script(agent);
}

/// Forgets the ports of a discarded window.
pub fn window_discarded(ua: &UserAgent, realm: Realm) {
    let mut ports = ua.ports.borrow_mut();
    let gone: Vec<PortId> = ports
        .ports
        .iter()
        .filter(|(_, p)| p.realm == Some(realm))
        .map(|(id, _)| *id)
        .collect();
    for id in gone {
        if let Some(p) = ports.ports.remove(&id) {
            if let Some(other) = p.entangled {
                if let Some(o) = ports.ports.get_mut(&other) {
                    o.entangled = None;
                }
            }
        }
    }
}

// ----- window.postMessage --------------------------------------------------------------------

/// The incumbent window: the realm of the topmost script on the stack.
pub fn incumbent_state(agent: &Agent) -> Option<Rc<RealmState>> {
    let realm = agent
        .execution_contexts()
        .iter()
        .rev()
        .find(|c| c.script_or_module.is_some())
        .map(|c| c.realm)
        .or_else(|| agent.running_context().map(|c| c.realm))?;
    crate::bindings::state_of(agent, realm).or_else(|| crate::bindings::current(agent))
}

/// A message posted to a window.
struct WindowMessageTask {
    target_realm: Realm,
    message: Serialized,
    source: Value,
    origin: String,
    target_origin: Option<Origin>,
}

impl Trace for WindowMessageTask {
    fn trace(&self, tracer: &mut Tracer) {
        tracer.realm(self.target_realm);
        self.message.trace(tracer);
        tracer.value(self.source);
    }
}

impl TaskRun for WindowMessageTask {
    fn run(self: Box<Self>, agent: &mut Agent) {
        let Some(state) = crate::bindings::state_of(agent, self.target_realm) else {
            return;
        };
        let Some(doc) = state.document() else {
            return;
        };
        if let Some(o) = &self.target_origin {
            if !o.same_origin(&doc.borrow().origin) {
                return;
            }
        }
        match deserialize_with_transfer(agent, self.target_realm, &self.message) {
            Ok((data, ports)) => {
                let ports = frozen_array(agent, self.target_realm, &ports);
                let origin = Value::String(agent.heap.alloc_str(&self.origin));
                let empty = Value::String(agent.heap.alloc_str(""));
                fire_window_event(
                    agent,
                    &state,
                    "message",
                    EventInterface::Other("MessageEvent"),
                    EventInit::default(),
                    &[
                        ("data", data),
                        ("origin", origin),
                        ("lastEventId", empty),
                        ("source", self.source),
                        ("ports", ports),
                    ],
                    false,
                );
            }
            Err(_) => {
                let origin = Value::String(agent.heap.alloc_str(&self.origin));
                fire_window_event(
                    agent,
                    &state,
                    "messageerror",
                    EventInterface::Other("MessageEvent"),
                    EventInit::default(),
                    &[("origin", origin), ("source", self.source)],
                    false,
                );
            }
        }
        super::event_loop::clean_up_after_running_script(agent);
    }
}

/// The window post message steps (HTML §9.3.3) for the window of
/// `target`.
pub fn window_post_message(
    agent: &mut Agent,
    target: &Rc<RealmState>,
    message: Value,
    target_origin: &str,
    transfer: &[Value],
) -> JsResult<()> {
    let incumbent = incumbent_state(agent).unwrap_or_else(|| target.clone());
    let incumbent_origin = incumbent
        .document()
        .map(|d| d.borrow().origin.clone())
        .unwrap_or_else(Origin::new_opaque);
    let target_origin = match target_origin {
        "*" => None,
        "/" => Some(incumbent_origin.clone()),
        s => match Url::parse(s, None) {
            Ok(u) => Some(u.origin()),
            Err(_) => return dom(agent, DomException::SyntaxError),
        },
    };
    let serialized = serialize_with_transfer(agent, message, transfer)?;
    let Some(ua) = super::ua() else {
        return Ok(());
    };
    let source = super::window::proxy_of(&incumbent);
    ua.event_loop.queue(
        TaskSource::PostedMessage,
        target.document(),
        Box::new(WindowMessageTask {
            target_realm: target.realm,
            message: serialized,
            source,
            origin: incumbent_origin.serialize(),
            target_origin,
        }),
    );
    Ok(())
}
