//! The native implementation behind the generated glue: one arm per
//! member of the DOM interfaces of k3-07 and k3-08 (`dom.idl`, plus the
//! `DOMException` of `webidl.idl` and the few `Window`, `Document`,
//! `Element` and `HTMLSlotElement` members of `html.idl` that the DOM
//! tests need to reach their subjects: `window`, `document`, `body`,
//! `head`, `title`, `innerHTML`, `outerHTML` and slots).
//!
//! The `style` of elements (`CSSStyleProperties` over the `style`
//! attribute, with its property attributes) is implemented here; the
//! rest of CSSOM is routed to `super::cssom` first. Members without an
//! implementation exist on their objects and throw a `TypeError` until
//! their sprints implement them (HTML and the data APIs are routed to
//! `crate::browser::members` and `crate::fetch` first),
//! except for
//! three generic cases: event handler attributes (`on…`) keep the value
//! they are given, and the index count and supported names of legacy
//! platform objects without an implementation are empty.
//!
//! Nodes of another document are adopted into the document an algorithm
//! runs in before the algorithm sees them (each document has its own
//! arena, LIMITATIONS.md).

use std::cell::RefCell;
use std::rc::Rc;

use vjs::{Agent, JsResult, Object, Throw, Value};

use super::exception::{self, dom, dom_error};
use super::generated::{INTERFACES, Op, ids};
use super::host::{
    JsHost, event_document, queue_observer_microtask, record_wrapper, register_event_wrapper,
    set_event_document, signal_wrapper,
};
use super::install::proto_from_new_target;
use super::meta::Id;
use super::platform::{self, Native, ObserverRef, implements};
use super::value::{Dict, IdlValue, Invocation};
use super::wrappers::{move_wrappers, wrap_node, wrap_node_with};
use super::{RealmState, current_state, host_value};
use crate::css::declaration::DeclarationBlock;
use crate::css::properties::property_of_attribute;
use crate::dom::abort::{SignalId, abort_signal_abort, abort_signal_timeout, signal_abort};
use crate::dom::collections::{DomTokenList, HtmlCollection, NamedNodeMap, NodeList};
use crate::dom::events::{
    Event, EventInit, EventInterface, EventRef, ListenerOptions, Target, add_event_listener,
    dispatch_event, remove_event_listener,
};
use crate::dom::implementation::{
    NameContext, create_document, create_document_type, create_html_document,
    is_valid_attribute_local_name, is_valid_element_local_name, validate_and_extract,
};
use crate::dom::mutation::NodeOrString;
use crate::dom::observer::{MutationRecord, ObserverInit};
use crate::dom::range::{BoundaryPoint, RangeId, StaticRange};
use crate::dom::shadow::{ShadowRootInit, ShadowRootMode, SlotAssignmentMode};
use crate::dom::traversal::{
    IteratorId, TreeWalker, WalkerRef, iterator_next_node, iterator_previous_node,
    walker_first_child, walker_last_child, walker_next_node, walker_next_sibling,
    walker_parent_node, walker_previous_node, walker_previous_sibling,
};
use crate::dom::tree::{nodes_equal, utf16_len};
use crate::dom::{
    Atom, DocRef, Document, DocumentKind, DomError, DomException, HostRef, Namespace, NodeData,
    NodeId, QualName, QuirksMode,
};

type R = JsResult<IdlValue>;

/// Runs the member `inv.op`. Mutation observer microtasks the member
/// asked for are queued afterwards.
pub fn call(agent: &mut Agent, inv: &mut Invocation) -> R {
    let state = current_state(agent)?;
    let this_doc = with_this(inv, |n| n.document().cloned());
    // The members of HTML (k3-10) live with the user agent, those of the
    // data APIs (k3-11) in `crate::fetch`.
    let result = match super::fonts::call(agent, &state, inv) {
        Some(r) => r,
        None => match super::cssom::call(agent, &state, inv) {
            Some(r) => r,
            None => match crate::fetch::call(agent, &state, inv) {
                Some(r) => r,
                None => match crate::browser::members::call(agent, &state, inv) {
                    Some(r) => r,
                    None => run(agent, &state, inv),
                },
            },
        },
    };
    if let Some(doc) = &this_doc {
        queue_observer_microtask(agent, doc);
        crate::browser::document::process_html_changes(agent, doc);
    }
    if let Some(doc) = state.document() {
        if !this_doc.as_ref().is_some_and(|d| Rc::ptr_eq(d, &doc)) {
            queue_observer_microtask(agent, &doc);
            crate::browser::document::process_html_changes(agent, &doc);
        }
    }
    result
}

// ----- helpers ------------------------------------------------------------------

fn illegal<T>(agent: &mut Agent) -> JsResult<T> {
    agent.type_error("Illegal invocation")
}

fn ok<T>(agent: &mut Agent, r: Result<T, DomException>) -> JsResult<T> {
    match r {
        Ok(v) => Ok(v),
        Err(e) => dom(agent, e),
    }
}

fn ok_dom<T>(agent: &mut Agent, r: Result<T, DomError>) -> JsResult<T> {
    match r {
        Ok(v) => Ok(v),
        Err(e) => dom_error(agent, e),
    }
}

/// Applies `f` to the native object of `this`.
fn with_this<T>(inv: &Invocation, f: impl FnOnce(&Native) -> Option<T>) -> Option<T> {
    let p = inv.this.as_ref()?;
    let native = p.get().native.borrow();
    f(&native)
}

/// Applies `f` to the native object of the platform object `o`.
fn with_object<T>(agent: &Agent, o: Object, f: impl FnOnce(&Native) -> Option<T>) -> Option<T> {
    let p = platform::platform_ref(agent, Value::Object(o))?;
    let native = p.get().native.borrow();
    f(&native)
}

fn need<T>(agent: &mut Agent, v: Option<T>) -> JsResult<T> {
    match v {
        Some(v) => Ok(v),
        None => illegal(agent),
    }
}

fn this_node(agent: &mut Agent, inv: &Invocation) -> JsResult<(DocRef, NodeId)> {
    let v = with_this(inv, |n| match n {
        Native::Node(d, id) => Some((d.clone(), *id)),
        _ => None,
    });
    need(agent, v)
}

fn node_of(agent: &Agent, o: Object) -> Option<(DocRef, NodeId)> {
    with_object(agent, o, |n| match n {
        Native::Node(d, id) => Some((d.clone(), *id)),
        _ => None,
    })
}

/// The node argument `i` (the conversion already checked its type).
fn arg_node(agent: &mut Agent, inv: &Invocation, i: usize) -> JsResult<(DocRef, NodeId)> {
    let v = inv.object(i).and_then(|o| node_of(agent, o));
    match v {
        Some(v) => Ok(v),
        None => agent.type_error(&format!("Argument {} is not a Node.", i + 1)),
    }
}

/// The nullable node argument `i`.
fn arg_opt_node(agent: &Agent, inv: &Invocation, i: usize) -> Option<(DocRef, NodeId)> {
    inv.object(i).and_then(|o| node_of(agent, o))
}

/// A node of `doc`, or `err` for a node of another document.
fn local_node(
    agent: &mut Agent,
    doc: &DocRef,
    node: (DocRef, NodeId),
    err: DomException,
) -> JsResult<NodeId> {
    if Rc::ptr_eq(doc, &node.0) {
        Ok(node.1)
    } else {
        dom(agent, err)
    }
}

/// `node` as a node of `doc`: a node of another document is adopted
/// into `doc` first, with its wrappers.
fn adopt_into(agent: &mut Agent, doc: &DocRef, (src, node): (DocRef, NodeId)) -> JsResult<NodeId> {
    if Rc::ptr_eq(doc, &src) {
        return Ok(node);
    }
    if src.borrow().node_type(node) == 9 {
        return dom(agent, DomException::HierarchyRequestError);
    }
    let mapping = {
        let mut d = doc.borrow_mut();
        let mut s = src.borrow_mut();
        d.adopt_from(&mut s, node)
    };
    let mapping = ok(agent, mapping)?;
    match mapping.first() {
        Some(&(_, new)) => {
            move_wrappers(agent, &src, doc, &mapping);
            Ok(new)
        }
        None => dom(agent, DomException::HierarchyRequestError),
    }
}

fn wrap(agent: &mut Agent, doc: &DocRef, node: NodeId) -> IdlValue {
    IdlValue::Object(wrap_node(agent, doc, node))
}

fn wrap_opt(agent: &mut Agent, doc: &DocRef, node: Option<NodeId>) -> IdlValue {
    match node {
        Some(n) => wrap(agent, doc, n),
        None => IdlValue::Null,
    }
}

fn wrap_list(agent: &mut Agent, doc: &DocRef, nodes: Vec<NodeId>) -> IdlValue {
    IdlValue::Sequence(nodes.into_iter().map(|n| wrap(agent, doc, n)).collect())
}

fn string(s: impl Into<String>) -> IdlValue {
    IdlValue::String(s.into())
}

fn opt_string(s: Option<impl Into<String>>) -> IdlValue {
    IdlValue::opt_string(s)
}

/// A [`HostRef`] for an `any` value; `null` and `undefined` (and a
/// missing argument) give `None`.
fn host_ref_of(state: &RealmState, v: &IdlValue) -> Option<HostRef> {
    match v {
        IdlValue::Missing
        | IdlValue::Undefined
        | IdlValue::Null
        | IdlValue::Any(Value::Undefined | Value::Null) => None,
        v => Some(state.host_ref(v.as_value())),
    }
}

fn host_ref_value(agent: &Agent, r: Option<HostRef>) -> IdlValue {
    match r {
        Some(r) => IdlValue::Any(host_value(agent, r)),
        None => IdlValue::Null,
    }
}

fn now(state: &RealmState) -> f64 {
    state.time_origin.elapsed().as_secs_f64() * 1000.0
}

/// The window's document; a `TypeError` without one.
fn window_document(agent: &mut Agent, state: &RealmState) -> JsResult<DocRef> {
    match state.document() {
        Some(d) => Ok(d),
        None => agent.type_error("The window has no document."),
    }
}

/// Whether scripting is enabled for `doc` (it is the window's).
fn scripting(state: &RealmState, doc: &DocRef) -> bool {
    state.document().is_some_and(|d| Rc::ptr_eq(&d, doc))
}

fn new_object(agent: &mut Agent, state: &RealmState, iface: Id, native: Native) -> IdlValue {
    IdlValue::Object(platform::create(agent, state, iface, native, None))
}

/// A platform object made by a constructor: its prototype comes from
/// `NewTarget`.
fn constructed(agent: &mut Agent, state: &RealmState, inv: &Invocation, native: Native) -> R {
    let proto = proto_from_new_target(agent, inv.new_target, inv.iface)?;
    Ok(IdlValue::Object(platform::create(
        agent, state, inv.iface, native, proto,
    )))
}

/// A node made by a constructor in the window's document.
fn constructed_node(agent: &mut Agent, inv: &Invocation, doc: &DocRef, node: NodeId) -> R {
    let proto = proto_from_new_target(agent, inv.new_target, inv.iface)?;
    Ok(IdlValue::Object(wrap_node_with(
        agent,
        doc,
        node,
        Some(inv.iface),
        proto,
    )))
}

/// A string of `args` (variadic `DOMString` arguments).
fn strings(args: &[IdlValue]) -> Vec<String> {
    args.iter().map(|a| a.as_str().to_string()).collect()
}

/// `(Node or DOMString)...` arguments as nodes of `doc`.
fn nodes_or_strings(
    agent: &mut Agent,
    doc: &DocRef,
    args: &[IdlValue],
) -> JsResult<Vec<NodeOrString>> {
    let mut out = Vec::with_capacity(args.len());
    for a in args {
        match a {
            IdlValue::Object(o) => {
                let Some(n) = node_of(agent, *o) else {
                    return agent.type_error("The value is not a Node.");
                };
                out.push(NodeOrString::Node(adopt_into(agent, doc, n)?));
            }
            other => out.push(NodeOrString::String(other.as_str().to_string())),
        }
    }
    Ok(out)
}

/// The XML `Name` production (XML 1.0 §2.3).
fn is_xml_name(s: &str) -> bool {
    fn start(c: char) -> bool {
        matches!(c, ':' | 'A'..='Z' | '_' | 'a'..='z'
            | '\u{C0}'..='\u{D6}' | '\u{D8}'..='\u{F6}' | '\u{F8}'..='\u{2FF}'
            | '\u{370}'..='\u{37D}' | '\u{37F}'..='\u{1FFF}' | '\u{200C}'..='\u{200D}'
            | '\u{2070}'..='\u{218F}' | '\u{2C00}'..='\u{2FEF}' | '\u{3001}'..='\u{D7FF}'
            | '\u{F900}'..='\u{FDCF}' | '\u{FDF0}'..='\u{FFFD}' | '\u{10000}'..='\u{EFFFF}')
    }
    fn rest(c: char) -> bool {
        start(c)
            || matches!(c, '-' | '.' | '0'..='9' | '\u{B7}' | '\u{300}'..='\u{36F}' | '\u{203F}'..='\u{2040}')
    }
    let mut chars = s.chars();
    chars.next().is_some_and(start) && chars.all(rest)
}

// ----- events -------------------------------------------------------------------

fn this_event(agent: &mut Agent, inv: &Invocation) -> JsResult<EventRef> {
    let v = with_this(inv, |n| match n {
        Native::Event(e) => Some(e.clone()),
        _ => None,
    });
    need(agent, v)
}

fn event_of(agent: &mut Agent, o: Option<Object>) -> JsResult<EventRef> {
    let v = o.and_then(|o| {
        with_object(agent, o, |n| match n {
            Native::Event(e) => Some(e.clone()),
            _ => None,
        })
    });
    match v {
        Some(e) => Ok(e),
        None => agent.type_error("The value is not an Event."),
    }
}

/// The `EventInit` members of a dictionary.
fn event_init(d: Option<&Dict>) -> EventInit {
    EventInit {
        bubbles: d.and_then(|d| d.bool("bubbles")).unwrap_or(false),
        cancelable: d.and_then(|d| d.bool("cancelable")).unwrap_or(false),
        composed: d.and_then(|d| d.bool("composed")).unwrap_or(false),
    }
}

/// The DOM's interface tag for an event interface.
fn event_interface_for(iface: Id) -> EventInterface {
    match INTERFACES[iface as usize].name {
        "Event" => EventInterface::Event,
        "CustomEvent" => EventInterface::CustomEvent,
        "UIEvent" => EventInterface::UiEvent,
        "MouseEvent" => EventInterface::MouseEvent,
        name => EventInterface::Other(name),
    }
}

/// The constructor of `Event` and of every interface inheriting from it
/// (members beyond `EventInit` and `CustomEventInit.detail` are not kept
/// yet).
fn construct_event(agent: &mut Agent, state: &Rc<RealmState>, inv: &Invocation) -> R {
    let type_ = inv.str(0);
    let dict = inv.arg(1).as_dict();
    let mut e = Event::new(&type_, event_init(dict), now(state));
    e.interface = event_interface_for(inv.iface);
    if e.interface == EventInterface::CustomEvent {
        e.detail = dict
            .and_then(|d| d.get("detail"))
            .and_then(|v| host_ref_of(state, v));
    }
    let proto = proto_from_new_target(agent, inv.new_target, inv.iface)?;
    let ev = e.into_ref();
    let o = platform::create(agent, state, inv.iface, Native::Event(ev.clone()), proto);
    register_event_wrapper(agent, state, &ev, o);
    // The other members of the init dictionary (`MessageEventInit.data`,
    // `ErrorEventInit.message`, …) are kept on the wrapper, where the
    // getters of `crate::browser::members` read them.
    if let Some(d) = dict {
        for (name, v) in &d.members {
            if matches!(*name, "bubbles" | "cancelable" | "composed" | "detail") {
                continue;
            }
            let value = super::convert::to_js_any(agent, v)?;
            if *name == "ports" {
                if let Value::Object(a) = value {
                    vjs::ops::set_integrity_level(agent, a, vjs::ops::IntegrityLevel::Frozen)?;
                }
            }
            if let Some(p) = platform::platform_ref(agent, Value::Object(o)) {
                p.get().set_slot(name, value);
            }
        }
    }
    Ok(IdlValue::Object(o))
}

/// `document.createEvent(interface)` (DOM §4.5).
fn create_event(agent: &mut Agent, state: &Rc<RealmState>, name: &str) -> R {
    let iface = match name.to_ascii_lowercase().as_str() {
        "event" | "events" | "htmlevents" | "svgevents" => "Event",
        "customevent" => "CustomEvent",
        "uievent" | "uievents" => "UIEvent",
        "mouseevent" | "mouseevents" => "MouseEvent",
        "beforeunloadevent" => "BeforeUnloadEvent",
        "compositionevent" => "CompositionEvent",
        "devicemotionevent" => "DeviceMotionEvent",
        "deviceorientationevent" => "DeviceOrientationEvent",
        "dragevent" => "DragEvent",
        "focusevent" => "FocusEvent",
        "hashchangeevent" => "HashChangeEvent",
        "keyboardevent" => "KeyboardEvent",
        "messageevent" => "MessageEvent",
        "storageevent" => "StorageEvent",
        "textevent" => "TextEvent",
        "touchevent" => "TouchEvent",
        _ => return dom(agent, DomException::NotSupportedError),
    };
    let id = super::generated::interface_by_name(iface).unwrap_or(ids::Event);
    let e = Event::new_uninitialized(event_interface_for(id), now(state)).into_ref();
    let o = platform::create(agent, state, id, Native::Event(e.clone()), None);
    register_event_wrapper(agent, state, &e, o);
    Ok(IdlValue::Object(o))
}

/// The value of an event target named in an event.
fn target_value(
    agent: &mut Agent,
    state: &RealmState,
    event: Option<Object>,
    target: Option<Target>,
) -> R {
    let Some(target) = target else {
        return Ok(IdlValue::Null);
    };
    let doc = event
        .and_then(|e| event_document(agent, e))
        .or_else(|| state.document());
    let Some(doc) = doc else {
        return Ok(IdlValue::Null);
    };
    let mut host = JsHost::new(agent, doc)?;
    Ok(IdlValue::Any(host.target_value(target)))
}

/// The document and target of an `EventTarget` platform object.
fn this_target(
    agent: &mut Agent,
    state: &RealmState,
    inv: &Invocation,
) -> JsResult<(DocRef, Target)> {
    let v = with_this(inv, |n| match n {
        Native::Node(d, id) => Some((Some(d.clone()), Target::Node(*id))),
        Native::AbortSignal(d, s) => Some((Some(d.clone()), Target::Signal(*s))),
        Native::Window => Some((None, Target::Window)),
        Native::EventTarget(id) | Native::TargetData(id, _) => Some((None, Target::Other(*id))),
        _ => None,
    });
    match v {
        Some((Some(d), t)) => Ok((d, t)),
        Some((None, t)) => Ok((window_document(agent, state)?, t)),
        None => illegal(agent),
    }
}

fn signal_of(agent: &Agent, o: Object) -> Option<(DocRef, SignalId)> {
    with_object(agent, o, |n| match n {
        Native::AbortSignal(d, s) | Native::AbortController(d, s) => Some((d.clone(), *s)),
        _ => None,
    })
}

fn this_signal(agent: &mut Agent, inv: &Invocation) -> JsResult<(DocRef, SignalId)> {
    let v = with_this(inv, |n| match n {
        Native::AbortSignal(d, s) | Native::AbortController(d, s) => Some((d.clone(), *s)),
        _ => None,
    });
    need(agent, v)
}

fn event_target(agent: &mut Agent, state: &Rc<RealmState>, inv: &Invocation) -> R {
    match inv.op {
        Op::EventTarget_constructor => {
            let id = state.next_target.get();
            state.next_target.set(id + 1);
            let v = constructed(agent, state, inv, Native::EventTarget(id))?;
            if let Some(o) = v.as_object() {
                state.targets.borrow_mut().insert(id, o);
            }
            Ok(v)
        }
        Op::EventTarget_addEventListener => {
            let (doc, target) = this_target(agent, state, inv)?;
            let type_ = inv.str(0);
            let callback = inv.object(1).map(|o| state.host_ref(Value::Object(o)));
            let mut options = ListenerOptions::default();
            match inv.arg(2) {
                IdlValue::Bool(b) => options.capture = *b,
                IdlValue::Dictionary(d) => {
                    options.capture = d.bool("capture").unwrap_or(false);
                    options.passive = d.bool("passive");
                    options.once = d.bool("once").unwrap_or(false);
                    if let Some((sdoc, s)) = d.object("signal").and_then(|o| signal_of(agent, o)) {
                        // Signals live in their document's store; one of
                        // another document cannot remove the listener.
                        if Rc::ptr_eq(&sdoc, &doc) {
                            options.signal = Some(s);
                        }
                    }
                }
                _ => {}
            }
            let mut host = JsHost::new(agent, doc.clone())?;
            add_event_listener(
                &mut doc.borrow_mut(),
                &mut host,
                target,
                &type_,
                callback,
                options,
            );
            Ok(IdlValue::Undefined)
        }
        Op::EventTarget_removeEventListener => {
            let (doc, target) = this_target(agent, state, inv)?;
            let type_ = inv.str(0);
            let callback = inv.object(1).map(|o| state.host_ref(Value::Object(o)));
            let capture = match inv.arg(2) {
                IdlValue::Bool(b) => *b,
                IdlValue::Dictionary(d) => d.bool("capture").unwrap_or(false),
                _ => false,
            };
            let mut host = JsHost::new(agent, doc.clone())?;
            remove_event_listener(
                &mut doc.borrow_mut(),
                &mut host,
                target,
                &type_,
                callback,
                capture,
            );
            Ok(IdlValue::Undefined)
        }
        Op::EventTarget_dispatchEvent => {
            let (doc, target) = this_target(agent, state, inv)?;
            let ev_obj = inv.object(0);
            let event = event_of(agent, ev_obj)?;
            if let Some(o) = ev_obj {
                set_event_document(agent, o, &doc);
            }
            let r = {
                let mut host = JsHost::new(agent, doc.clone())?;
                dispatch_event(&doc, &mut host, target, &event)
            };
            Ok(IdlValue::Bool(ok(agent, r)?))
        }
        _ => not_implemented(agent, inv.op),
    }
}

fn event(agent: &mut Agent, state: &Rc<RealmState>, inv: &Invocation) -> R {
    if matches!(inv.op, Op::Event_constructor | Op::CustomEvent_constructor) {
        return construct_event(agent, state, inv);
    }
    let e = this_event(agent, inv)?;
    let this = inv.this_object();
    Ok(match inv.op {
        Op::Event_get_type => string(e.borrow().type_.clone()),
        Op::Event_get_target | Op::Event_get_srcElement => {
            let t = e.borrow().target;
            return target_value(agent, state, this, t);
        }
        Op::Event_get_currentTarget => {
            let t = e.borrow().current_target;
            return target_value(agent, state, this, t);
        }
        Op::Event_composedPath => {
            let path = e.borrow().composed_path();
            let mut out = Vec::with_capacity(path.len());
            for t in path {
                out.push(target_value(agent, state, this, Some(t))?);
            }
            IdlValue::Sequence(out)
        }
        Op::Event_get_eventPhase => IdlValue::Int(e.borrow().phase as i64),
        Op::Event_stopPropagation => {
            e.borrow_mut().stop_propagation();
            IdlValue::Undefined
        }
        Op::Event_get_cancelBubble => IdlValue::Bool(e.borrow().cancel_bubble()),
        Op::Event_set_cancelBubble => {
            e.borrow_mut().set_cancel_bubble(inv.bool(0));
            IdlValue::Undefined
        }
        Op::Event_stopImmediatePropagation => {
            e.borrow_mut().stop_immediate_propagation();
            IdlValue::Undefined
        }
        Op::Event_get_bubbles => IdlValue::Bool(e.borrow().bubbles),
        Op::Event_get_cancelable => IdlValue::Bool(e.borrow().cancelable),
        Op::Event_get_returnValue => IdlValue::Bool(e.borrow().return_value()),
        Op::Event_set_returnValue => {
            e.borrow_mut().set_return_value(inv.bool(0));
            IdlValue::Undefined
        }
        Op::Event_preventDefault => {
            e.borrow_mut().prevent_default();
            IdlValue::Undefined
        }
        Op::Event_get_defaultPrevented => IdlValue::Bool(e.borrow().default_prevented()),
        Op::Event_get_composed => IdlValue::Bool(e.borrow().composed),
        Op::Event_get_isTrusted => IdlValue::Bool(e.borrow().is_trusted),
        Op::Event_get_timeStamp => IdlValue::Double(e.borrow().time_stamp),
        Op::Event_initEvent => {
            e.borrow_mut()
                .init_event(&inv.str(0), inv.bool(1), inv.bool(2));
            IdlValue::Undefined
        }
        Op::CustomEvent_get_detail => {
            let d = e.borrow().detail;
            host_ref_value(agent, d)
        }
        Op::CustomEvent_initCustomEvent => {
            let detail = host_ref_of(state, inv.arg(3));
            e.borrow_mut()
                .init_custom_event(&inv.str(0), inv.bool(1), inv.bool(2), detail);
            IdlValue::Undefined
        }
        op => return not_implemented(agent, op),
    })
}

fn abort(agent: &mut Agent, state: &Rc<RealmState>, inv: &Invocation) -> R {
    match inv.op {
        Op::AbortController_constructor => {
            let doc = window_document(agent, state)?;
            let s = doc.borrow_mut().create_abort_signal();
            constructed(agent, state, inv, Native::AbortController(doc, s))
        }
        Op::AbortController_get_signal => {
            let (doc, s) = this_signal(agent, inv)?;
            Ok(IdlValue::Object(signal_wrapper(agent, state, &doc, s)))
        }
        Op::AbortController_abort => {
            let (doc, s) = this_signal(agent, inv)?;
            let reason = host_ref_of(state, inv.arg(0));
            let mut host = JsHost::new(agent, doc.clone())?;
            signal_abort(&doc, &mut host, s, reason);
            Ok(IdlValue::Undefined)
        }
        Op::AbortSignal_static_abort => {
            let doc = window_document(agent, state)?;
            let reason = host_ref_of(state, inv.arg(0));
            let s = {
                let mut host = JsHost::new(agent, doc.clone())?;
                abort_signal_abort(&doc, &mut host, reason)
            };
            Ok(IdlValue::Object(signal_wrapper(agent, state, &doc, s)))
        }
        Op::AbortSignal_static_timeout => {
            let doc = window_document(agent, state)?;
            let ms = inv.arg(0).as_u64();
            let s = {
                let mut host = JsHost::new(agent, doc.clone())?;
                abort_signal_timeout(&doc, &mut host, ms)
            };
            Ok(IdlValue::Object(signal_wrapper(agent, state, &doc, s)))
        }
        Op::AbortSignal_static_any => {
            let doc = window_document(agent, state)?;
            let mut signals = Vec::new();
            for v in inv.arg(0).as_seq() {
                if let Some((d, s)) = v.as_object().and_then(|o| signal_of(agent, o)) {
                    if Rc::ptr_eq(&d, &doc) {
                        signals.push(s);
                    }
                }
            }
            let s = doc.borrow_mut().abort_signal_any(&signals);
            Ok(IdlValue::Object(signal_wrapper(agent, state, &doc, s)))
        }
        Op::AbortSignal_get_aborted => {
            let (doc, s) = this_signal(agent, inv)?;
            let aborted = doc.borrow().is_aborted(s);
            Ok(IdlValue::Bool(aborted))
        }
        Op::AbortSignal_get_reason => {
            let (doc, s) = this_signal(agent, inv)?;
            let r = doc.borrow().abort_reason(s);
            Ok(match r {
                Some(r) => IdlValue::Any(host_value(agent, r)),
                None => IdlValue::Undefined,
            })
        }
        Op::AbortSignal_throwIfAborted => {
            let (doc, s) = this_signal(agent, inv)?;
            let r = doc.borrow().throw_if_aborted(s);
            match r {
                Ok(()) => Ok(IdlValue::Undefined),
                Err(r) => Err(Throw(host_value(agent, r))),
            }
        }
        op => not_implemented(agent, op),
    }
}

// ----- mutation observers ---------------------------------------------------------

fn this_observer(agent: &mut Agent, inv: &Invocation) -> JsResult<Rc<ObserverRef>> {
    let v = with_this(inv, |n| match n {
        Native::MutationObserver(o) => Some(o.clone()),
        _ => None,
    });
    need(agent, v)
}

fn observer_init(d: Option<&Dict>) -> ObserverInit {
    let Some(d) = d else {
        return ObserverInit::default();
    };
    ObserverInit {
        child_list: d.bool("childList").unwrap_or(false),
        attributes: d.bool("attributes"),
        character_data: d.bool("characterData"),
        subtree: d.bool("subtree").unwrap_or(false),
        attribute_old_value: d.bool("attributeOldValue"),
        character_data_old_value: d.bool("characterDataOldValue"),
        attribute_filter: d.get("attributeFilter").map(|v| strings(v.as_seq())),
    }
}

fn mutation_observer(agent: &mut Agent, state: &Rc<RealmState>, inv: &Invocation) -> R {
    match inv.op {
        Op::MutationObserver_constructor => {
            let Some(cb) = inv.object(0) else {
                return agent.type_error("The callback is not a function.");
            };
            let callback = state.host_ref(Value::Object(cb));
            constructed(
                agent,
                state,
                inv,
                Native::MutationObserver(Rc::new(ObserverRef {
                    callback,
                    per_document: RefCell::new(Vec::new()),
                })),
            )
        }
        Op::MutationObserver_observe => {
            let ob = this_observer(agent, inv)?;
            let (doc, node) = arg_node(agent, inv, 0)?;
            let existing = ob
                .per_document
                .borrow()
                .iter()
                .find(|(d, _)| Rc::ptr_eq(d, &doc))
                .map(|(_, id)| *id);
            let id = match existing {
                Some(id) => id,
                None => {
                    let id = doc.borrow_mut().create_mutation_observer(ob.callback);
                    ob.per_document.borrow_mut().push((doc.clone(), id));
                    if let Some(o) = inv.this_object() {
                        state
                            .observers
                            .borrow_mut()
                            .insert((Rc::as_ptr(&doc) as usize, id.0), o);
                    }
                    id
                }
            };
            let init = observer_init(inv.arg(1).as_dict());
            let r = doc.borrow_mut().observe(id, node, init);
            ok_dom(agent, r)?;
            Ok(IdlValue::Undefined)
        }
        Op::MutationObserver_disconnect => {
            let ob = this_observer(agent, inv)?;
            for (doc, id) in ob.per_document.borrow().iter() {
                doc.borrow_mut().disconnect_mutation_observer(*id);
            }
            Ok(IdlValue::Undefined)
        }
        Op::MutationObserver_takeRecords => {
            let ob = this_observer(agent, inv)?;
            let per_document: Vec<_> = ob.per_document.borrow().clone();
            let mut out = Vec::new();
            for (doc, id) in per_document {
                let records = doc.borrow_mut().take_mutation_records(id);
                for r in records {
                    out.push(IdlValue::Object(record_wrapper(agent, state, &doc, r)));
                }
            }
            Ok(IdlValue::Sequence(out))
        }
        op => not_implemented(agent, op),
    }
}

fn mutation_record(agent: &mut Agent, state: &Rc<RealmState>, inv: &Invocation) -> R {
    let v = with_this(inv, |n| match n {
        Native::MutationRecord(d, r) => Some((d.clone(), r.clone())),
        _ => None,
    });
    let (doc, r): (DocRef, Rc<MutationRecord>) = need(agent, v)?;
    Ok(match inv.op {
        Op::MutationRecord_get_type => string(r.kind.as_str()),
        Op::MutationRecord_get_target => wrap(agent, &doc, r.target),
        Op::MutationRecord_get_addedNodes => new_object(
            agent,
            state,
            ids::NodeList,
            Native::NodeList(doc.clone(), NodeList::Static(r.added_nodes.clone())),
        ),
        Op::MutationRecord_get_removedNodes => new_object(
            agent,
            state,
            ids::NodeList,
            Native::NodeList(doc.clone(), NodeList::Static(r.removed_nodes.clone())),
        ),
        Op::MutationRecord_get_previousSibling => wrap_opt(agent, &doc, r.previous_sibling),
        Op::MutationRecord_get_nextSibling => wrap_opt(agent, &doc, r.next_sibling),
        Op::MutationRecord_get_attributeName => opt_string(r.attribute_name.clone()),
        Op::MutationRecord_get_attributeNamespace => opt_string(r.attribute_namespace.clone()),
        Op::MutationRecord_get_oldValue => opt_string(r.old_value.clone()),
        op => return not_implemented(agent, op),
    })
}

// ----- nodes -------------------------------------------------------------------

fn node(agent: &mut Agent, state: &Rc<RealmState>, inv: &Invocation) -> R {
    let (doc, n) = this_node(agent, inv)?;
    Ok(match inv.op {
        Op::Node_get_nodeType => IdlValue::Int(i64::from(doc.borrow().node_type(n))),
        Op::Node_get_nodeName => string(doc.borrow().node_name(n)),
        Op::Node_get_baseURI => string(doc.borrow().url.href()),
        Op::Node_get_isConnected => IdlValue::Bool(doc.borrow().is_connected(n)),
        Op::Node_get_ownerDocument => {
            if doc.borrow().node_type(n) == 9 {
                IdlValue::Null
            } else {
                wrap(agent, &doc, NodeId::DOCUMENT)
            }
        }
        Op::Node_getRootNode => {
            let composed = inv
                .arg(0)
                .as_dict()
                .and_then(|d| d.bool("composed"))
                .unwrap_or(false);
            let root = doc.borrow().get_root_node(n, composed);
            wrap(agent, &doc, root)
        }
        Op::Node_get_parentNode => {
            let p = doc.borrow().parent(n);
            wrap_opt(agent, &doc, p)
        }
        Op::Node_get_parentElement => {
            let p = doc.borrow().parent_element(n);
            wrap_opt(agent, &doc, p)
        }
        Op::Node_hasChildNodes => IdlValue::Bool(doc.borrow().has_child_nodes(n)),
        Op::Node_get_childNodes => {
            let list = doc.borrow().child_nodes_list(n);
            new_object(
                agent,
                state,
                ids::NodeList,
                Native::NodeList(doc.clone(), list),
            )
        }
        Op::Node_get_firstChild => {
            let c = doc.borrow().first_child(n);
            wrap_opt(agent, &doc, c)
        }
        Op::Node_get_lastChild => {
            let c = doc.borrow().last_child(n);
            wrap_opt(agent, &doc, c)
        }
        Op::Node_get_previousSibling => {
            let c = doc.borrow().prev_sibling(n);
            wrap_opt(agent, &doc, c)
        }
        Op::Node_get_nextSibling => {
            let c = doc.borrow().next_sibling(n);
            wrap_opt(agent, &doc, c)
        }
        Op::Node_get_nodeValue => opt_string(doc.borrow().node_value(n)),
        Op::Node_set_nodeValue => {
            let v = inv.opt_str(0);
            doc.borrow_mut().set_node_value(n, v.as_deref());
            IdlValue::Undefined
        }
        Op::Node_get_textContent => opt_string(doc.borrow().text_content(n)),
        Op::Node_set_textContent => {
            let v = inv.opt_str(0);
            doc.borrow_mut().set_text_content(n, v.as_deref());
            IdlValue::Undefined
        }
        Op::Node_normalize => {
            doc.borrow_mut().normalize(n);
            IdlValue::Undefined
        }
        Op::Node_cloneNode => {
            let deep = inv.bool(0);
            if doc.borrow().node_type(n) == 9 {
                let copy: DocRef = Rc::new(RefCell::new(doc.borrow().clone_document(deep)));
                wrap(agent, &copy, NodeId::DOCUMENT)
            } else {
                let r = doc.borrow_mut().clone_node(n, deep);
                let c = ok(agent, r)?;
                wrap(agent, &doc, c)
            }
        }
        Op::Node_isEqualNode => match arg_opt_node(agent, inv, 0) {
            None => IdlValue::Bool(false),
            Some((od, o)) => {
                if Rc::ptr_eq(&od, &doc) {
                    IdlValue::Bool(doc.borrow().is_equal_node(n, o))
                } else {
                    IdlValue::Bool(nodes_equal(&doc.borrow(), n, &od.borrow(), o))
                }
            }
        },
        Op::Node_isSameNode => {
            IdlValue::Bool(inv.object(0).is_some() && inv.object(0) == inv.this_object())
        }
        Op::Node_compareDocumentPosition => {
            let (od, o) = arg_node(agent, inv, 0)?;
            if Rc::ptr_eq(&od, &doc) {
                IdlValue::Int(i64::from(doc.borrow().compare_document_position(n, o)))
            } else {
                // Disconnected, implementation specific and a consistent
                // order between the two documents.
                let order = if Rc::as_ptr(&od) < Rc::as_ptr(&doc) {
                    0x02
                } else {
                    0x04
                };
                IdlValue::Int(0x01 | 0x20 | order)
            }
        }
        Op::Node_contains => match arg_opt_node(agent, inv, 0) {
            Some((od, o)) if Rc::ptr_eq(&od, &doc) => {
                IdlValue::Bool(doc.borrow().contains(n, Some(o)))
            }
            _ => IdlValue::Bool(false),
        },
        Op::Node_lookupPrefix => {
            let ns = inv.opt_str(0);
            opt_string(doc.borrow().lookup_prefix(n, ns.as_deref()))
        }
        Op::Node_lookupNamespaceURI => {
            let prefix = inv.opt_str(0);
            opt_string(doc.borrow().lookup_namespace_uri(n, prefix.as_deref()))
        }
        Op::Node_isDefaultNamespace => {
            let ns = inv.opt_str(0);
            IdlValue::Bool(doc.borrow().is_default_namespace(n, ns.as_deref()))
        }
        Op::Node_insertBefore => {
            let node = arg_node(agent, inv, 0)?;
            let child = match arg_opt_node(agent, inv, 1) {
                Some(c) => Some(local_node(agent, &doc, c, DomException::NotFoundError)?),
                None => None,
            };
            let node = adopt_into(agent, &doc, node)?;
            let r = doc.borrow_mut().pre_insert(node, n, child);
            let r = ok(agent, r)?;
            wrap(agent, &doc, r)
        }
        Op::Node_appendChild => {
            let node = arg_node(agent, inv, 0)?;
            let node = adopt_into(agent, &doc, node)?;
            let r = doc.borrow_mut().pre_insert(node, n, None);
            let r = ok(agent, r)?;
            wrap(agent, &doc, r)
        }
        Op::Node_replaceChild => {
            let node = arg_node(agent, inv, 0)?;
            let child = arg_node(agent, inv, 1)?;
            let child = local_node(agent, &doc, child, DomException::NotFoundError)?;
            let node = adopt_into(agent, &doc, node)?;
            let r = doc.borrow_mut().replace_child(n, node, child);
            let r = ok(agent, r)?;
            wrap(agent, &doc, r)
        }
        Op::Node_removeChild => {
            let child = arg_node(agent, inv, 0)?;
            let child = local_node(agent, &doc, child, DomException::NotFoundError)?;
            let r = doc.borrow_mut().pre_remove(child, n);
            let r = ok(agent, r)?;
            wrap(agent, &doc, r)
        }

        // ParentNode
        Op::ParentNode_get_children => {
            let c = doc.borrow().children_collection(n);
            new_object(
                agent,
                state,
                ids::HTMLCollection,
                Native::HtmlCollection(doc.clone(), c),
            )
        }
        Op::ParentNode_get_firstElementChild => {
            let c = doc.borrow().first_element_child(n);
            wrap_opt(agent, &doc, c)
        }
        Op::ParentNode_get_lastElementChild => {
            let c = doc.borrow().last_element_child(n);
            wrap_opt(agent, &doc, c)
        }
        Op::ParentNode_get_childElementCount => {
            IdlValue::Int(doc.borrow().child_element_count(n) as i64)
        }
        Op::ParentNode_prepend | Op::ParentNode_append | Op::ParentNode_replaceChildren => {
            let nodes = nodes_or_strings(agent, &doc, &inv.args)?;
            let r = {
                let mut d = doc.borrow_mut();
                match inv.op {
                    Op::ParentNode_prepend => d.prepend(n, nodes),
                    Op::ParentNode_append => d.append(n, nodes),
                    _ => d.replace_children(n, nodes),
                }
            };
            ok(agent, r)?;
            IdlValue::Undefined
        }
        Op::ParentNode_moveBefore => {
            let node = arg_node(agent, inv, 0)?;
            let node = local_node(agent, &doc, node, DomException::HierarchyRequestError)?;
            let child = match arg_opt_node(agent, inv, 1) {
                Some(c) => Some(local_node(agent, &doc, c, DomException::NotFoundError)?),
                None => None,
            };
            let r = doc.borrow_mut().move_before(n, node, child);
            ok(agent, r)?;
            IdlValue::Undefined
        }
        Op::ParentNode_querySelector => {
            let r = doc.borrow().query_selector(n, &inv.str(0));
            let r = ok(agent, r)?;
            wrap_opt(agent, &doc, r)
        }
        Op::ParentNode_querySelectorAll => {
            let r = doc.borrow().query_selector_all(n, &inv.str(0));
            let r = ok(agent, r)?;
            new_object(
                agent,
                state,
                ids::NodeList,
                Native::NodeList(doc.clone(), NodeList::Static(r)),
            )
        }
        Op::NonElementParentNode_getElementById => {
            let r = doc.borrow().get_element_by_id(n, &inv.str(0));
            wrap_opt(agent, &doc, r)
        }
        Op::DocumentOrShadowRoot_get_customElementRegistry => IdlValue::Null,

        // NonDocumentTypeChildNode, ChildNode, Slottable
        Op::NonDocumentTypeChildNode_get_previousElementSibling => {
            let s = doc.borrow().previous_element_sibling(n);
            wrap_opt(agent, &doc, s)
        }
        Op::NonDocumentTypeChildNode_get_nextElementSibling => {
            let s = doc.borrow().next_element_sibling(n);
            wrap_opt(agent, &doc, s)
        }
        Op::ChildNode_before | Op::ChildNode_after | Op::ChildNode_replaceWith => {
            let nodes = nodes_or_strings(agent, &doc, &inv.args)?;
            let r = {
                let mut d = doc.borrow_mut();
                match inv.op {
                    Op::ChildNode_before => d.before(n, nodes),
                    Op::ChildNode_after => d.after(n, nodes),
                    _ => d.replace_with(n, nodes),
                }
            };
            ok(agent, r)?;
            IdlValue::Undefined
        }
        Op::ChildNode_remove => {
            doc.borrow_mut().remove_self(n);
            IdlValue::Undefined
        }
        Op::Slottable_get_assignedSlot => {
            let s = doc.borrow().assigned_slot(n);
            wrap_opt(agent, &doc, s)
        }
        op => return not_implemented(agent, op),
    })
}

// ----- documents ------------------------------------------------------------------

fn new_range_object(
    agent: &mut Agent,
    state: &RealmState,
    doc: &DocRef,
    id: RangeId,
    proto: Option<Object>,
) -> Object {
    let o = platform::create(
        agent,
        state,
        ids::Range,
        Native::Range(doc.clone(), id),
        proto,
    );
    let weak = Rc::downgrade(doc);
    agent.heap.add_finalizer(
        o,
        Box::new(move || {
            if let Some(d) = weak.upgrade() {
                if let Ok(mut d) = d.try_borrow_mut() {
                    d.release_range(id);
                }
            }
        }),
    );
    o
}

fn new_iterator_object(
    agent: &mut Agent,
    state: &RealmState,
    doc: &DocRef,
    id: IteratorId,
) -> Object {
    let o = platform::create(
        agent,
        state,
        ids::NodeIterator,
        Native::NodeIterator(doc.clone(), id),
        None,
    );
    let weak = Rc::downgrade(doc);
    agent.heap.add_finalizer(
        o,
        Box::new(move || {
            if let Some(d) = weak.upgrade() {
                if let Ok(mut d) = d.try_borrow_mut() {
                    d.release_node_iterator(id);
                }
            }
        }),
    );
    o
}

/// The `title` element of a document (HTML §3.1.3): the first one in
/// tree order.
fn title_element(d: &Document) -> Option<NodeId> {
    d.descendant_elements(NodeId::DOCUMENT)
        .into_iter()
        .find(|e| d.is_html_element(*e, "title"))
}

/// Strip and collapse ASCII whitespace.
fn collapse_whitespace(s: &str) -> String {
    s.split(['\t', '\n', '\u{C}', '\r', ' '])
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn document(agent: &mut Agent, state: &Rc<RealmState>, inv: &Invocation) -> R {
    if inv.op == Op::Document_constructor {
        let window = window_document(agent, state)?;
        let mut d = Document::new(DocumentKind::Xml, "application/xml");
        d.origin = window.borrow().origin.clone();
        let doc: DocRef = Rc::new(RefCell::new(d));
        return constructed_node(agent, inv, &doc, NodeId::DOCUMENT);
    }
    let (doc, n) = this_node(agent, inv)?;
    Ok(match inv.op {
        Op::Document_get_implementation => new_object(
            agent,
            state,
            ids::DOMImplementation,
            Native::Implementation(doc.clone()),
        ),
        Op::Document_get_URL | Op::Document_get_documentURI => string(doc.borrow().url.href()),
        Op::Document_get_compatMode => string(if doc.borrow().mode == QuirksMode::Quirks {
            "BackCompat"
        } else {
            "CSS1Compat"
        }),
        Op::Document_get_characterSet
        | Op::Document_get_charset
        | Op::Document_get_inputEncoding => string(doc.borrow().encoding.name()),
        Op::Document_get_contentType => string(doc.borrow().content_type.clone()),
        Op::Document_get_doctype => {
            let d = doc.borrow().doctype();
            wrap_opt(agent, &doc, d)
        }
        Op::Document_get_documentElement => {
            let d = doc.borrow().document_element();
            wrap_opt(agent, &doc, d)
        }
        Op::Document_getElementsByTagName
        | Op::Document_getElementsByTagNameNS
        | Op::Document_getElementsByClassName => {
            let c = collection_query(inv, &doc.borrow(), n);
            new_object(
                agent,
                state,
                ids::HTMLCollection,
                Native::HtmlCollection(doc.clone(), c),
            )
        }
        Op::Document_createElement => {
            let mut local = inv.str(0);
            if !is_valid_element_local_name(&local) {
                return dom(agent, DomException::InvalidCharacterError);
            }
            let is = match inv.arg(1) {
                IdlValue::Dictionary(d) => d.str("is").map(str::to_string),
                _ => None,
            };
            let mut d = doc.borrow_mut();
            if d.is_html() {
                local = local.to_ascii_lowercase();
            }
            let ns = if d.is_html() || d.content_type == "application/xhtml+xml" {
                Some(Namespace::Html)
            } else {
                None
            };
            let e = d.create_element(QualName::new(ns, None, Atom::new(&local)));
            if let (Some(is), Some(data)) = (is, d.element_mut(e)) {
                data.is_value = Some(Atom::new(&is));
            }
            drop(d);
            wrap(agent, &doc, e)
        }
        Op::Document_createElementNS => {
            let ns = inv.opt_str(0);
            let r = validate_and_extract(ns.as_deref(), &inv.str(1), NameContext::Element);
            let (ns, prefix, local) = ok(agent, r)?;
            let e = doc
                .borrow_mut()
                .create_element(QualName::new(ns, prefix, local));
            wrap(agent, &doc, e)
        }
        Op::Document_createDocumentFragment => {
            let f = doc.borrow_mut().create_document_fragment();
            wrap(agent, &doc, f)
        }
        Op::Document_createTextNode => {
            let t = doc.borrow_mut().create_text(&inv.str(0));
            wrap(agent, &doc, t)
        }
        Op::Document_createCDATASection => {
            let data = inv.str(0);
            if doc.borrow().is_html() {
                return dom(agent, DomException::NotSupportedError);
            }
            if data.contains("]]>") {
                return dom(agent, DomException::InvalidCharacterError);
            }
            let t = doc.borrow_mut().create_cdata_section(&data);
            wrap(agent, &doc, t)
        }
        Op::Document_createComment => {
            let c = doc.borrow_mut().create_comment(&inv.str(0));
            wrap(agent, &doc, c)
        }
        Op::Document_createProcessingInstruction => {
            let (target, data) = (inv.str(0), inv.str(1));
            if !is_xml_name(&target) || data.contains("?>") {
                return dom(agent, DomException::InvalidCharacterError);
            }
            let p = doc
                .borrow_mut()
                .create_processing_instruction(&target, &data);
            wrap(agent, &doc, p)
        }
        Op::Document_importNode => {
            let (src, node) = arg_node(agent, inv, 0)?;
            let deep = match inv.arg(1) {
                IdlValue::Bool(b) => *b,
                IdlValue::Dictionary(d) => !d.bool("selfOnly").unwrap_or(false),
                _ => false,
            };
            let r = if Rc::ptr_eq(&src, &doc) {
                doc.borrow_mut().clone_node(node, deep)
            } else {
                let s = src.borrow();
                doc.borrow_mut().import_node(&s, node, deep)
            };
            let c = ok(agent, r)?;
            wrap(agent, &doc, c)
        }
        Op::Document_adoptNode => {
            let (src, node) = arg_node(agent, inv, 0)?;
            if Rc::ptr_eq(&src, &doc) {
                let r = doc.borrow_mut().adopt_node(node);
                let a = ok(agent, r)?;
                wrap(agent, &doc, a)
            } else {
                let mapping = {
                    let mut d = doc.borrow_mut();
                    let mut s = src.borrow_mut();
                    d.adopt_from(&mut s, node)
                };
                let mapping = ok(agent, mapping)?;
                match mapping.first() {
                    Some(&(_, new)) => {
                        move_wrappers(agent, &src, &doc, &mapping);
                        wrap(agent, &doc, new)
                    }
                    // Template contents stay where they are.
                    None => wrap(agent, &src, node),
                }
            }
        }
        Op::Document_createAttribute => {
            let mut local = inv.str(0);
            if !is_valid_attribute_local_name(&local) {
                return dom(agent, DomException::InvalidCharacterError);
            }
            if doc.borrow().is_html() {
                local = local.to_ascii_lowercase();
            }
            let a = doc.borrow_mut().create_attribute(None, None, &local, "");
            wrap(agent, &doc, a)
        }
        Op::Document_createAttributeNS => {
            let ns = inv.opt_str(0);
            let r = validate_and_extract(ns.as_deref(), &inv.str(1), NameContext::Attribute);
            let (ns, prefix, local) = ok(agent, r)?;
            let a = doc
                .borrow_mut()
                .create_attribute(ns, prefix, local.as_str(), "");
            wrap(agent, &doc, a)
        }
        Op::Document_createEvent => return create_event(agent, state, &inv.str(0)),
        Op::Document_createRange => {
            let id = doc.borrow_mut().create_range();
            IdlValue::Object(new_range_object(agent, state, &doc, id, None))
        }
        Op::Document_createNodeIterator => {
            let (rdoc, root) = arg_node(agent, inv, 0)?;
            let what = inv.u32(1);
            let filter = inv.object(2).map(|o| state.host_ref(Value::Object(o)));
            let id = rdoc.borrow_mut().create_node_iterator(root, what, filter);
            IdlValue::Object(new_iterator_object(agent, state, &rdoc, id))
        }
        Op::Document_createTreeWalker => {
            let (rdoc, root) = arg_node(agent, inv, 0)?;
            let what = inv.u32(1);
            let filter = inv.object(2).map(|o| state.host_ref(Value::Object(o)));
            let walker = TreeWalker::new(root, what, filter);
            new_object(
                agent,
                state,
                ids::TreeWalker,
                Native::TreeWalker(rdoc, walker),
            )
        }

        // html.idl: the DOM tree accessors and the browsing context.
        Op::Document_get_body => {
            let b = doc.borrow().body();
            wrap_opt(agent, &doc, b)
        }
        Op::Document_get_head => {
            let h = doc.borrow().head();
            wrap_opt(agent, &doc, h)
        }
        Op::Document_get_title => {
            let d = doc.borrow();
            let text = title_element(&d)
                .map(|t| d.child_text_content(t))
                .unwrap_or_default();
            string(collapse_whitespace(&text))
        }
        Op::Document_set_title => {
            let value = inv.str(0);
            let mut d = doc.borrow_mut();
            let html_root = d
                .document_element()
                .is_some_and(|e| d.namespace_of(e) == Some(Namespace::Html));
            if html_root {
                let title = match title_element(&d) {
                    Some(t) => Some(t),
                    None => match d.head() {
                        Some(head) => {
                            let t = d.create_html_element("title");
                            d.append_child(head, t);
                            Some(t)
                        }
                        None => None,
                    },
                };
                if let Some(t) = title {
                    d.set_text_content(t, Some(value.as_str()));
                }
            }
            IdlValue::Undefined
        }
        Op::Document_get_defaultView => {
            if scripting(state, &doc) {
                state.global.get().map_or(IdlValue::Null, IdlValue::Object)
            } else {
                IdlValue::Null
            }
        }
        _ => return node(agent, state, inv),
    })
}

/// The `HTMLCollection` of `getElementsByTagName`, `…NS` or
/// `…ClassName` on `root`.
fn collection_query(inv: &Invocation, d: &Document, root: NodeId) -> HtmlCollection {
    let name = inv.op.name();
    if name.ends_with("getElementsByTagNameNS") {
        let ns = inv.opt_str(0);
        d.get_elements_by_tag_name_ns(root, ns.as_deref(), &inv.str(1))
    } else if name.ends_with("getElementsByClassName") {
        d.get_elements_by_class_name(root, &inv.str(0))
    } else {
        d.get_elements_by_tag_name(root, &inv.str(0))
    }
}

fn implementation(agent: &mut Agent, _state: &Rc<RealmState>, inv: &Invocation) -> R {
    let v = with_this(inv, |n| match n {
        Native::Implementation(d) => Some(d.clone()),
        _ => None,
    });
    let doc = need(agent, v)?;
    Ok(match inv.op {
        Op::DOMImplementation_createDocumentType => {
            let r =
                create_document_type(&mut doc.borrow_mut(), &inv.str(0), &inv.str(1), &inv.str(2));
            let t = ok(agent, r)?;
            wrap(agent, &doc, t)
        }
        Op::DOMImplementation_createDocument => {
            let ns = inv.opt_str(0);
            let doctype = arg_opt_node(agent, inv, 2);
            let data = doctype
                .as_ref()
                .and_then(|(d, t)| match &d.borrow().node(*t).data {
                    NodeData::DocumentType(dt) => Some(dt.clone()),
                    _ => None,
                });
            let origin = doc.borrow().origin.clone();
            let r = create_document(&origin, ns.as_deref(), &inv.str(1), data);
            let new_doc: DocRef = Rc::new(RefCell::new(ok(agent, r)?));
            if let Some((src, old)) = doctype {
                // The doctype moves into the new document.
                src.borrow_mut().remove(old);
                let new = new_doc.borrow().doctype();
                if let Some(new) = new {
                    move_wrappers(agent, &src, &new_doc, &[(old, new)]);
                }
            }
            wrap(agent, &new_doc, NodeId::DOCUMENT)
        }
        Op::DOMImplementation_createHTMLDocument => {
            let title = inv.opt_str(0);
            let origin = doc.borrow().origin.clone();
            let new_doc: DocRef = Rc::new(RefCell::new(create_html_document(
                &origin,
                title.as_deref(),
            )));
            wrap(agent, &new_doc, NodeId::DOCUMENT)
        }
        Op::DOMImplementation_hasFeature => IdlValue::Bool(true),
        op => return not_implemented(agent, op),
    })
}

fn document_type(agent: &mut Agent, inv: &Invocation) -> R {
    let (doc, n) = this_node(agent, inv)?;
    let d = doc.borrow();
    let NodeData::DocumentType(t) = &d.node(n).data else {
        drop(d);
        return illegal(agent);
    };
    Ok(match inv.op {
        Op::DocumentType_get_name => string(t.name.clone()),
        Op::DocumentType_get_publicId => string(t.public_id.clone()),
        Op::DocumentType_get_systemId => string(t.system_id.clone()),
        op => {
            drop(d);
            return not_implemented(agent, op);
        }
    })
}

fn fragment(agent: &mut Agent, state: &Rc<RealmState>, inv: &Invocation) -> R {
    match inv.op {
        Op::DocumentFragment_constructor => {
            let doc = window_document(agent, state)?;
            let f = doc.borrow_mut().create_document_fragment();
            constructed_node(agent, inv, &doc, f)
        }
        Op::ShadowRoot_get_host => {
            let (doc, n) = this_node(agent, inv)?;
            let h = doc.borrow().shadow_root_host(n);
            Ok(wrap_opt(agent, &doc, h))
        }
        Op::ShadowRoot_get_innerHTML | Op::ShadowRoot_set_innerHTML => {
            inner_html(agent, state, inv)
        }
        _ => {
            let (doc, n) = this_node(agent, inv)?;
            let data = doc.borrow().shadow_root_data(n).copied();
            let Some(s) = data else {
                return illegal(agent);
            };
            Ok(match inv.op {
                Op::ShadowRoot_get_mode => string(s.mode.as_str()),
                Op::ShadowRoot_get_delegatesFocus => IdlValue::Bool(s.delegates_focus),
                Op::ShadowRoot_get_serializable => IdlValue::Bool(s.serializable),
                Op::ShadowRoot_get_slotAssignment => string(match s.slot_assignment {
                    SlotAssignmentMode::Manual => "manual",
                    SlotAssignmentMode::Named => "named",
                }),
                Op::ShadowRoot_get_clonable => IdlValue::Bool(s.clonable),
                op => return not_implemented(agent, op),
            })
        }
    }
}

// ----- elements and attributes ------------------------------------------------------

/// `innerHTML` of elements and shadow roots (HTML parsing for every
/// document: there is no XML parser yet).
fn inner_html(agent: &mut Agent, state: &Rc<RealmState>, inv: &Invocation) -> R {
    let (doc, n) = this_node(agent, inv)?;
    let script = scripting(state, &doc);
    let getter = inv.op.name().contains("_get_");
    if getter {
        let d = doc.borrow();
        return Ok(string(crate::html::serialize::serialize_fragment(
            &d, n, script,
        )));
    }
    let value = inv.str(0);
    let mut d = doc.borrow_mut();
    let context = d.shadow_root_host(n).unwrap_or(n);
    let target = d.template_contents(n).unwrap_or(n);
    let fragment = crate::html::parse_fragment(&mut d, context, &value, script);
    d.replace_all(Some(fragment), target);
    Ok(IdlValue::Undefined)
}

fn this_attr(agent: &mut Agent, inv: &Invocation) -> JsResult<(DocRef, NodeId)> {
    let (doc, n) = this_node(agent, inv)?;
    if doc.borrow().is_attr(n) {
        Ok((doc, n))
    } else {
        illegal(agent)
    }
}

fn attr(agent: &mut Agent, inv: &Invocation) -> R {
    let (doc, n) = this_attr(agent, inv)?;
    if inv.op == Op::Attr_set_value {
        doc.borrow_mut()
            .set_existing_attribute_value(n, &inv.str(0));
        return Ok(IdlValue::Undefined);
    }
    if inv.op == Op::Attr_get_ownerElement {
        let o = doc.borrow().owner_element(n);
        return Ok(wrap_opt(agent, &doc, o));
    }
    let d = doc.borrow();
    let NodeData::Attr(a) = &d.node(n).data else {
        drop(d);
        return illegal(agent);
    };
    Ok(match inv.op {
        Op::Attr_get_namespaceURI => opt_string(a.ns.map(|ns| ns.url())),
        Op::Attr_get_prefix => opt_string(a.prefix.map(|p| p.to_string())),
        Op::Attr_get_localName => string(a.local.to_string()),
        Op::Attr_get_name => string(match a.prefix {
            Some(p) => format!("{p}:{}", a.local),
            None => a.local.to_string(),
        }),
        Op::Attr_get_value => string(d.attr_node_value(n).unwrap_or_default()),
        Op::Attr_get_specified => IdlValue::Bool(true),
        op => {
            drop(d);
            return not_implemented(agent, op);
        }
    })
}

fn element(agent: &mut Agent, state: &Rc<RealmState>, inv: &Invocation) -> R {
    let (doc, n) = this_node(agent, inv)?;
    if !doc.borrow().is_element(n) {
        return illegal(agent);
    }
    Ok(match inv.op {
        Op::Element_get_namespaceURI => opt_string(doc.borrow().namespace_of(n).map(|ns| ns.url())),
        Op::Element_get_prefix => {
            let p = doc.borrow().element(n).and_then(|e| e.name.prefix);
            opt_string(p.map(|p| p.to_string()))
        }
        Op::Element_get_localName => {
            let l = doc.borrow().element(n).map(|e| e.name.local.to_string());
            string(l.unwrap_or_default())
        }
        Op::Element_get_tagName => string(doc.borrow().tag_name(n)),
        Op::Element_get_id | Op::Element_get_className | Op::Element_get_slot => {
            let name = reflected(inv.op);
            string(
                doc.borrow()
                    .get_attribute(n, name)
                    .unwrap_or_default()
                    .to_string(),
            )
        }
        Op::Element_set_id | Op::Element_set_className | Op::Element_set_slot => {
            let name = reflected(inv.op);
            doc.borrow_mut()
                .set_attribute_value(n, name, &inv.str(0), None, None);
            IdlValue::Undefined
        }
        Op::Element_get_classList => new_object(
            agent,
            state,
            ids::DOMTokenList,
            Native::TokenList(doc.clone(), DomTokenList::class_list(n)),
        ),
        Op::Element_hasAttributes => IdlValue::Bool(doc.borrow().has_attributes(n)),
        Op::Element_get_attributes => new_object(
            agent,
            state,
            ids::NamedNodeMap,
            Native::NamedNodeMap(doc.clone(), NamedNodeMap { element: n }),
        ),
        Op::Element_getAttributeNames => IdlValue::Sequence(
            doc.borrow()
                .get_attribute_names(n)
                .into_iter()
                .map(IdlValue::String)
                .collect(),
        ),
        Op::Element_getAttribute => opt_string(doc.borrow().get_attribute_by_name(n, &inv.str(0))),
        Op::Element_getAttributeNS => {
            let ns = inv.opt_str(0);
            opt_string(doc.borrow().get_attribute_ns(n, ns.as_deref(), &inv.str(1)))
        }
        Op::Element_setAttribute => {
            let r = doc
                .borrow_mut()
                .set_attribute_checked(n, &inv.str(0), &inv.str(1));
            ok(agent, r)?;
            IdlValue::Undefined
        }
        Op::Element_setAttributeNS => {
            let ns = inv.opt_str(0);
            let r = doc
                .borrow_mut()
                .set_attribute_ns(n, ns.as_deref(), &inv.str(1), &inv.str(2));
            ok(agent, r)?;
            IdlValue::Undefined
        }
        Op::Element_removeAttribute => {
            doc.borrow_mut().remove_attribute_by_name(n, &inv.str(0));
            IdlValue::Undefined
        }
        Op::Element_removeAttributeNS => {
            let ns = inv.opt_str(0);
            doc.borrow_mut()
                .remove_attribute_ns(n, ns.as_deref(), &inv.str(1));
            IdlValue::Undefined
        }
        Op::Element_toggleAttribute => {
            let force = inv.arg(1).as_opt_bool();
            let r = doc.borrow_mut().toggle_attribute(n, &inv.str(0), force);
            IdlValue::Bool(ok(agent, r)?)
        }
        Op::Element_hasAttribute => IdlValue::Bool(doc.borrow().has_attribute(n, &inv.str(0))),
        Op::Element_hasAttributeNS => {
            let ns = inv.opt_str(0);
            IdlValue::Bool(doc.borrow().has_attribute_ns(n, ns.as_deref(), &inv.str(1)))
        }
        Op::Element_getAttributeNode => {
            let a = doc.borrow_mut().get_attribute_node(n, &inv.str(0));
            wrap_opt(agent, &doc, a)
        }
        Op::Element_getAttributeNodeNS => {
            let ns = inv.opt_str(0);
            let a = doc
                .borrow_mut()
                .get_attribute_node_ns(n, ns.as_deref(), &inv.str(1));
            wrap_opt(agent, &doc, a)
        }
        Op::Element_setAttributeNode | Op::Element_setAttributeNodeNS => {
            let a = arg_node(agent, inv, 0)?;
            let a = adopt_into(agent, &doc, a)?;
            let r = doc.borrow_mut().set_attribute_node(n, a);
            let old = ok(agent, r)?;
            wrap_opt(agent, &doc, old)
        }
        Op::Element_removeAttributeNode => {
            let a = arg_node(agent, inv, 0)?;
            let a = local_node(agent, &doc, a, DomException::NotFoundError)?;
            let r = doc.borrow_mut().remove_attribute_node(n, a);
            let a = ok(agent, r)?;
            wrap(agent, &doc, a)
        }
        Op::Element_attachShadow => {
            let Some(d) = inv.arg(0).as_dict() else {
                return agent.type_error("ShadowRootInit is required.");
            };
            let mode = if d.str("mode") == Some("closed") {
                ShadowRootMode::Closed
            } else {
                ShadowRootMode::Open
            };
            let mut init = ShadowRootInit::new(mode);
            init.delegates_focus = d.bool("delegatesFocus").unwrap_or(false);
            init.serializable = d.bool("serializable").unwrap_or(false);
            init.clonable = d.bool("clonable").unwrap_or(false);
            if d.str("slotAssignment") == Some("manual") {
                init.slot_assignment = SlotAssignmentMode::Manual;
            }
            let r = doc.borrow_mut().attach_shadow(n, init);
            let root = ok(agent, r)?;
            wrap(agent, &doc, root)
        }
        Op::Element_get_shadowRoot => {
            let r = doc.borrow().open_shadow_root(n);
            wrap_opt(agent, &doc, r)
        }
        Op::Element_get_customElementRegistry => IdlValue::Null,
        Op::Element_closest => {
            let r = doc.borrow().closest(n, &inv.str(0));
            let r = ok(agent, r)?;
            wrap_opt(agent, &doc, r)
        }
        Op::Element_matches | Op::Element_webkitMatchesSelector => {
            let r = doc.borrow().matches(n, &inv.str(0));
            IdlValue::Bool(ok(agent, r)?)
        }
        Op::Element_getElementsByTagName
        | Op::Element_getElementsByTagNameNS
        | Op::Element_getElementsByClassName => {
            let c = collection_query(inv, &doc.borrow(), n);
            new_object(
                agent,
                state,
                ids::HTMLCollection,
                Native::HtmlCollection(doc.clone(), c),
            )
        }
        Op::Element_insertAdjacentElement => {
            let e = arg_node(agent, inv, 1)?;
            let e = adopt_into(agent, &doc, e)?;
            let r = doc.borrow_mut().insert_adjacent_element(n, &inv.str(0), e);
            let r = ok(agent, r)?;
            wrap_opt(agent, &doc, r)
        }
        Op::Element_insertAdjacentText => {
            let r = doc
                .borrow_mut()
                .insert_adjacent_text(n, &inv.str(0), &inv.str(1));
            ok(agent, r)?;
            IdlValue::Undefined
        }
        Op::Element_get_innerHTML | Op::Element_set_innerHTML => {
            return inner_html(agent, state, inv);
        }
        Op::Element_get_outerHTML => {
            let script = scripting(state, &doc);
            let d = doc.borrow();
            string(crate::html::serialize::serialize_outer(&d, n, script))
        }
        Op::Element_set_outerHTML => {
            let script = scripting(state, &doc);
            let value = inv.str(0);
            let mut d = doc.borrow_mut();
            let Some(parent) = d.parent(n) else {
                return Ok(IdlValue::Undefined);
            };
            if d.node_type(parent) == 9 {
                drop(d);
                return dom(agent, DomException::NoModificationAllowedError);
            }
            let context = if d.is_element(parent) {
                parent
            } else {
                d.create_html_element("body")
            };
            let fragment = crate::html::parse_fragment(&mut d, context, &value, script);
            let r = d.replace_child(parent, fragment, n);
            drop(d);
            ok(agent, r)?;
            IdlValue::Undefined
        }
        Op::Element_insertAdjacentHTML => {
            let script = scripting(state, &doc);
            let r = doc
                .borrow_mut()
                .insert_adjacent_html(n, &inv.str(0), &inv.str(1), script);
            ok(agent, r)?;
            IdlValue::Undefined
        }

        // HTMLSlotElement
        Op::HTMLSlotElement_get_name => string(
            doc.borrow()
                .get_attribute(n, "name")
                .unwrap_or_default()
                .to_string(),
        ),
        Op::HTMLSlotElement_set_name => {
            doc.borrow_mut()
                .set_attribute_value(n, "name", &inv.str(0), None, None);
            IdlValue::Undefined
        }
        Op::HTMLSlotElement_assignedNodes | Op::HTMLSlotElement_assignedElements => {
            let flatten = inv
                .arg(0)
                .as_dict()
                .and_then(|d| d.bool("flatten"))
                .unwrap_or(false);
            let nodes = if inv.op == Op::HTMLSlotElement_assignedNodes {
                doc.borrow().assigned_nodes(n, flatten)
            } else {
                doc.borrow().assigned_elements(n, flatten)
            };
            wrap_list(agent, &doc, nodes)
        }
        Op::HTMLSlotElement_assign => {
            let mut nodes = Vec::new();
            for a in &inv.args {
                if let Some((d, id)) = a.as_object().and_then(|o| node_of(agent, o)) {
                    if Rc::ptr_eq(&d, &doc) {
                        nodes.push(id);
                    }
                }
            }
            doc.borrow_mut().slot_assign(n, &nodes);
            IdlValue::Undefined
        }
        _ => return node(agent, state, inv),
    })
}

/// The content attribute a reflecting `Element` attribute stands for.
fn reflected(op: Op) -> &'static str {
    match op {
        Op::Element_get_id | Op::Element_set_id => "id",
        Op::Element_get_className | Op::Element_set_className => "class",
        _ => "slot",
    }
}

fn character_data(agent: &mut Agent, state: &Rc<RealmState>, inv: &Invocation) -> R {
    match inv.op {
        Op::Text_constructor | Op::Comment_constructor => {
            let doc = window_document(agent, state)?;
            let data = inv.str(0);
            let n = if inv.op == Op::Text_constructor {
                doc.borrow_mut().create_text(&data)
            } else {
                doc.borrow_mut().create_comment(&data)
            };
            return constructed_node(agent, inv, &doc, n);
        }
        Op::ProcessingInstruction_constructor => {
            let doc = window_document(agent, state)?;
            let (target, data) = (inv.str(0), inv.str(1));
            if !is_xml_name(&target) || data.contains("?>") {
                return dom(agent, DomException::InvalidCharacterError);
            }
            let n = doc
                .borrow_mut()
                .create_processing_instruction(&target, &data);
            return constructed_node(agent, inv, &doc, n);
        }
        _ => {}
    }
    let (doc, n) = this_node(agent, inv)?;
    if !doc.borrow().is_character_data(n) {
        return illegal(agent);
    }
    Ok(match inv.op {
        Op::CharacterData_get_data => string(
            doc.borrow()
                .character_data(n)
                .unwrap_or_default()
                .to_string(),
        ),
        Op::CharacterData_set_data => {
            doc.borrow_mut().set_data(n, &inv.str(0));
            IdlValue::Undefined
        }
        Op::CharacterData_get_length => {
            IdlValue::Int(utf16_len(doc.borrow().character_data(n).unwrap_or_default()) as i64)
        }
        Op::CharacterData_substringData => {
            let r = doc.borrow().substring_data(n, inv.usize(0), inv.usize(1));
            string(ok(agent, r)?)
        }
        Op::CharacterData_appendData => {
            doc.borrow_mut().append_data(n, &inv.str(0));
            IdlValue::Undefined
        }
        Op::CharacterData_insertData => {
            let r = doc.borrow_mut().insert_data(n, inv.usize(0), &inv.str(1));
            ok(agent, r)?;
            IdlValue::Undefined
        }
        Op::CharacterData_deleteData => {
            let r = doc.borrow_mut().delete_data(n, inv.usize(0), inv.usize(1));
            ok(agent, r)?;
            IdlValue::Undefined
        }
        Op::CharacterData_replaceData => {
            let r = doc
                .borrow_mut()
                .replace_data(n, inv.usize(0), inv.usize(1), &inv.str(2));
            ok(agent, r)?;
            IdlValue::Undefined
        }
        Op::Text_splitText => {
            let r = doc.borrow_mut().split_text(n, inv.usize(0));
            let t = ok(agent, r)?;
            wrap(agent, &doc, t)
        }
        Op::Text_get_wholeText => string(doc.borrow().whole_text(n)),
        Op::ProcessingInstruction_get_target => {
            let d = doc.borrow();
            match &d.node(n).data {
                NodeData::ProcessingInstruction { target, .. } => string(target.clone()),
                _ => IdlValue::String(String::new()),
            }
        }
        _ => return node(agent, state, inv),
    })
}

// ----- collections ----------------------------------------------------------------

fn collection(agent: &mut Agent, inv: &Invocation) -> R {
    let native = with_this(inv, |n| match n {
        Native::NodeList(d, l) => Some(Coll::Nodes(d.clone(), l.clone())),
        Native::HtmlCollection(d, c) => Some(Coll::Html(d.clone(), c.clone())),
        Native::NamedNodeMap(d, m) => Some(Coll::Attrs(d.clone(), *m)),
        Native::TokenList(d, t) => Some(Coll::Tokens(d.clone(), *t)),
        _ => None,
    });
    let native = need(agent, native)?;
    Ok(match (inv.op, native) {
        (Op::NodeList_get_length | Op::NodeList_index_count, Coll::Nodes(d, l)) => {
            IdlValue::Int(l.length(&d.borrow()) as i64)
        }
        (Op::NodeList_item, Coll::Nodes(d, l)) => {
            let n = l.item(&d.borrow(), inv.usize(0));
            wrap_opt(agent, &d, n)
        }
        (Op::HTMLCollection_get_length | Op::HTMLCollection_index_count, Coll::Html(d, c)) => {
            IdlValue::Int(c.length(&d.borrow()) as i64)
        }
        (Op::HTMLCollection_item, Coll::Html(d, c)) => {
            let n = c.item(&d.borrow(), inv.usize(0));
            wrap_opt(agent, &d, n)
        }
        (Op::HTMLCollection_namedItem, Coll::Html(d, c)) => {
            let n = c.named_item(&d.borrow(), &inv.str(0));
            wrap_opt(agent, &d, n)
        }
        (Op::HTMLCollection_supported_names, Coll::Html(d, c)) => IdlValue::Sequence(
            c.supported_property_names(&d.borrow())
                .into_iter()
                .map(IdlValue::String)
                .collect(),
        ),
        (Op::NamedNodeMap_get_length | Op::NamedNodeMap_index_count, Coll::Attrs(d, m)) => {
            IdlValue::Int(m.length(&d.borrow()) as i64)
        }
        (Op::NamedNodeMap_item, Coll::Attrs(d, m)) => {
            let a = m.item(&mut d.borrow_mut(), inv.usize(0));
            wrap_opt(agent, &d, a)
        }
        (Op::NamedNodeMap_getNamedItem, Coll::Attrs(d, m)) => {
            let a = m.get_named_item(&mut d.borrow_mut(), &inv.str(0));
            wrap_opt(agent, &d, a)
        }
        (Op::NamedNodeMap_getNamedItemNS, Coll::Attrs(d, m)) => {
            let ns = inv.opt_str(0);
            let a = m.get_named_item_ns(&mut d.borrow_mut(), ns.as_deref(), &inv.str(1));
            wrap_opt(agent, &d, a)
        }
        (Op::NamedNodeMap_setNamedItem | Op::NamedNodeMap_setNamedItemNS, Coll::Attrs(d, m)) => {
            let a = arg_node(agent, inv, 0)?;
            let a = adopt_into(agent, &d, a)?;
            let r = m.set_named_item(&mut d.borrow_mut(), a);
            let old = ok(agent, r)?;
            wrap_opt(agent, &d, old)
        }
        (Op::NamedNodeMap_removeNamedItem, Coll::Attrs(d, m)) => {
            let r = m.remove_named_item(&mut d.borrow_mut(), &inv.str(0));
            let a = ok(agent, r)?;
            wrap(agent, &d, a)
        }
        (Op::NamedNodeMap_removeNamedItemNS, Coll::Attrs(d, m)) => {
            let ns = inv.opt_str(0);
            let r = m.remove_named_item_ns(&mut d.borrow_mut(), ns.as_deref(), &inv.str(1));
            let a = ok(agent, r)?;
            wrap(agent, &d, a)
        }
        (Op::NamedNodeMap_supported_names, Coll::Attrs(d, m)) => IdlValue::Sequence(
            m.supported_property_names(&d.borrow())
                .into_iter()
                .map(IdlValue::String)
                .collect(),
        ),
        (Op::DOMTokenList_get_length | Op::DOMTokenList_index_count, Coll::Tokens(d, t)) => {
            IdlValue::Int(t.length(&d.borrow()) as i64)
        }
        (Op::DOMTokenList_item, Coll::Tokens(d, t)) => {
            opt_string(t.item(&d.borrow(), inv.usize(0)))
        }
        (Op::DOMTokenList_contains, Coll::Tokens(d, t)) => {
            IdlValue::Bool(t.contains(&d.borrow(), &inv.str(0)))
        }
        (Op::DOMTokenList_add | Op::DOMTokenList_remove, Coll::Tokens(d, t)) => {
            let tokens = strings(&inv.args);
            let refs: Vec<&str> = tokens.iter().map(String::as_str).collect();
            let r = if inv.op == Op::DOMTokenList_add {
                t.add(&mut d.borrow_mut(), &refs)
            } else {
                t.remove(&mut d.borrow_mut(), &refs)
            };
            ok(agent, r)?;
            IdlValue::Undefined
        }
        (Op::DOMTokenList_toggle, Coll::Tokens(d, t)) => {
            let force = inv.arg(1).as_opt_bool();
            let r = t.toggle(&mut d.borrow_mut(), &inv.str(0), force);
            IdlValue::Bool(ok(agent, r)?)
        }
        (Op::DOMTokenList_replace, Coll::Tokens(d, t)) => {
            let r = t.replace(&mut d.borrow_mut(), &inv.str(0), &inv.str(1));
            IdlValue::Bool(ok(agent, r)?)
        }
        (Op::DOMTokenList_supports, Coll::Tokens(_, t)) => {
            let r = t.supports(&inv.str(0));
            IdlValue::Bool(ok_dom(agent, r)?)
        }
        (Op::DOMTokenList_get_value, Coll::Tokens(d, t)) => string(t.value(&d.borrow())),
        (Op::DOMTokenList_set_value, Coll::Tokens(d, t)) => {
            t.set_value(&mut d.borrow_mut(), &inv.str(0));
            IdlValue::Undefined
        }
        (op, _) => return not_implemented(agent, op),
    })
}

// ----- CSSStyleProperties of the style attribute (CSSOM §6.7) --------------------

/// The element whose `style` attribute `this` is the declaration block of.
fn inline_style_this(inv: &Invocation) -> Option<(DocRef, NodeId)> {
    with_this(inv, |n| match n {
        Native::InlineStyle(d, e) => Some((d.clone(), *e)),
        _ => None,
    })
}

/// The members of the `CSSStyleProperties` of an element's `style`
/// attribute. The block is parsed from the attribute on every access and
/// a change writes it back ("update style attribute for"), so it never
/// goes stale.
fn inline_style(agent: &mut Agent, inv: &Invocation) -> R {
    let (doc, el) = need(agent, inline_style_this(inv))?;
    let text = doc
        .borrow()
        .get_attribute(el, "style")
        .unwrap_or_default()
        .to_string();
    let mut block = DeclarationBlock::parse(&text);
    let write = |block: &DeclarationBlock| {
        doc.borrow_mut()
            .set_attribute_value(el, "style", &block.serialize(), None, None);
    };
    Ok(match inv.op {
        Op::CSSStyleDeclaration_get_cssText => string(block.serialize()),
        Op::CSSStyleDeclaration_set_cssText => {
            write(&DeclarationBlock::parse(&inv.str(0)));
            IdlValue::Undefined
        }
        Op::CSSStyleDeclaration_get_length | Op::CSSStyleDeclaration_index_count => {
            IdlValue::Int(block.len() as i64)
        }
        Op::CSSStyleDeclaration_item => {
            string(block.item(inv.usize(0)).unwrap_or_default().to_string())
        }
        Op::CSSStyleDeclaration_getPropertyValue => string(block.property_value(&inv.str(0))),
        Op::CSSStyleDeclaration_getPropertyPriority => {
            string(block.property_priority(&inv.str(0)).to_string())
        }
        Op::CSSStyleDeclaration_setProperty => {
            if block.set_property(&inv.str(0), &inv.str(1), &inv.str(2)) {
                write(&block);
            }
            IdlValue::Undefined
        }
        Op::CSSStyleDeclaration_removeProperty => match block.remove_property(&inv.str(0)) {
            Some(old) => {
                write(&block);
                string(old)
            }
            None => string(String::new()),
        },
        Op::CSSStyleDeclaration_get_parentRule => IdlValue::Null,
        Op::CSSStyleProperties_get_cssFloat => string(block.property_value("float")),
        Op::CSSStyleProperties_set_cssFloat => {
            if block.set_property("float", &inv.str(0), "") {
                write(&block);
            }
            IdlValue::Undefined
        }
        // The attributes of each property (CSSOM §6.7.1).
        op => {
            let name = op.name();
            if let Some(attr) = name.strip_prefix("CSSStyleProperties_get_") {
                string(block.property_value(&property_of_attribute(attr)))
            } else if let Some(attr) = name.strip_prefix("CSSStyleProperties_set_") {
                if block.set_property(&property_of_attribute(attr), &inv.str(0), "") {
                    write(&block);
                }
                IdlValue::Undefined
            } else {
                return not_implemented(agent, op);
            }
        }
    })
}

/// The native object of a collection.
enum Coll {
    Nodes(DocRef, NodeList),
    Html(DocRef, HtmlCollection),
    Attrs(DocRef, NamedNodeMap),
    Tokens(DocRef, DomTokenList),
}

// ----- ranges -------------------------------------------------------------------

fn range(agent: &mut Agent, state: &Rc<RealmState>, inv: &Invocation) -> R {
    match inv.op {
        Op::Range_constructor => {
            let doc = window_document(agent, state)?;
            let id = doc.borrow_mut().create_range();
            let proto = proto_from_new_target(agent, inv.new_target, inv.iface)?;
            return Ok(IdlValue::Object(new_range_object(
                agent, state, &doc, id, proto,
            )));
        }
        Op::StaticRange_constructor => {
            let Some(d) = inv.arg(0).as_dict() else {
                return agent.type_error("StaticRangeInit is required.");
            };
            let start = d.object("startContainer").and_then(|o| node_of(agent, o));
            let end = d.object("endContainer").and_then(|o| node_of(agent, o));
            let (Some((sd, sn)), Some(end)) = (start, end) else {
                return agent.type_error("The containers must be nodes.");
            };
            let en = local_node(agent, &sd, end, DomException::WrongDocumentError)?;
            let so = d.get("startOffset").map_or(0, IdlValue::as_usize);
            let eo = d.get("endOffset").map_or(0, IdlValue::as_usize);
            let r = StaticRange::new(
                &sd.borrow(),
                BoundaryPoint::new(sn, so),
                BoundaryPoint::new(en, eo),
            );
            let r = ok(agent, r)?;
            return constructed(agent, state, inv, Native::StaticRange(sd, r));
        }
        _ => {}
    }
    enum Which {
        Live(DocRef, RangeId),
        Static(DocRef, StaticRange),
    }
    let which = with_this(inv, |n| match n {
        Native::Range(d, id) => Some(Which::Live(d.clone(), *id)),
        Native::StaticRange(d, r) => Some(Which::Static(d.clone(), *r)),
        _ => None,
    });
    let which = need(agent, which)?;
    let (doc, start, end, id) = match &which {
        Which::Live(d, id) => {
            let r = d.borrow().range(*id);
            (d.clone(), r.start, r.end, Some(*id))
        }
        Which::Static(d, r) => (d.clone(), r.start, r.end, None),
    };
    match inv.op {
        Op::AbstractRange_get_startContainer => return Ok(wrap(agent, &doc, start.node)),
        Op::AbstractRange_get_startOffset => return Ok(IdlValue::Int(start.offset as i64)),
        Op::AbstractRange_get_endContainer => return Ok(wrap(agent, &doc, end.node)),
        Op::AbstractRange_get_endOffset => return Ok(IdlValue::Int(end.offset as i64)),
        Op::AbstractRange_get_collapsed => return Ok(IdlValue::Bool(start == end)),
        _ => {}
    }
    let Some(id) = id else {
        return illegal(agent);
    };
    // A node argument: ranges only reach nodes of their document.
    let node_arg = |agent: &mut Agent, i: usize| -> JsResult<NodeId> {
        let n = arg_node(agent, inv, i)?;
        local_node(agent, &doc, n, DomException::WrongDocumentError)
    };
    Ok(match inv.op {
        Op::Range_get_commonAncestorContainer => {
            let c = doc.borrow().common_ancestor_container(id);
            wrap(agent, &doc, c)
        }
        Op::Range_setStart | Op::Range_setEnd => {
            let n = node_arg(agent, 0)?;
            let offset = inv.usize(1);
            let r = if inv.op == Op::Range_setStart {
                doc.borrow_mut().range_set_start(id, n, offset)
            } else {
                doc.borrow_mut().range_set_end(id, n, offset)
            };
            ok(agent, r)?;
            IdlValue::Undefined
        }
        Op::Range_setStartBefore
        | Op::Range_setStartAfter
        | Op::Range_setEndBefore
        | Op::Range_setEndAfter
        | Op::Range_selectNode
        | Op::Range_selectNodeContents => {
            let n = node_arg(agent, 0)?;
            let r = {
                let mut d = doc.borrow_mut();
                match inv.op {
                    Op::Range_setStartBefore => d.range_set_start_before(id, n),
                    Op::Range_setStartAfter => d.range_set_start_after(id, n),
                    Op::Range_setEndBefore => d.range_set_end_before(id, n),
                    Op::Range_setEndAfter => d.range_set_end_after(id, n),
                    Op::Range_selectNode => d.range_select_node(id, n),
                    _ => d.range_select_node_contents(id, n),
                }
            };
            ok(agent, r)?;
            IdlValue::Undefined
        }
        Op::Range_collapse => {
            doc.borrow_mut().range_collapse(id, inv.bool(0));
            IdlValue::Undefined
        }
        Op::Range_compareBoundaryPoints => {
            let how = inv.arg(0).as_u64() as u16;
            let source = inv.object(1).and_then(|o| {
                with_object(agent, o, |n| match n {
                    Native::Range(d, id) => Some((d.clone(), *id)),
                    _ => None,
                })
            });
            let Some((sd, sid)) = source else {
                return agent.type_error("The value is not a Range.");
            };
            if !Rc::ptr_eq(&sd, &doc) {
                return dom(agent, DomException::WrongDocumentError);
            }
            let r = doc.borrow().range_compare_boundary_points(id, how, sid);
            IdlValue::Int(i64::from(ok(agent, r)?))
        }
        Op::Range_deleteContents => {
            let r = doc.borrow_mut().range_delete_contents(id);
            ok(agent, r)?;
            IdlValue::Undefined
        }
        Op::Range_extractContents | Op::Range_cloneContents => {
            let r = if inv.op == Op::Range_extractContents {
                doc.borrow_mut().range_extract_contents(id)
            } else {
                doc.borrow_mut().range_clone_contents(id)
            };
            let f = ok(agent, r)?;
            wrap(agent, &doc, f)
        }
        Op::Range_insertNode | Op::Range_surroundContents => {
            let n = arg_node(agent, inv, 0)?;
            let n = adopt_into(agent, &doc, n)?;
            let r = if inv.op == Op::Range_insertNode {
                doc.borrow_mut().range_insert_node(id, n)
            } else {
                doc.borrow_mut().range_surround_contents(id, n)
            };
            ok(agent, r)?;
            IdlValue::Undefined
        }
        Op::Range_cloneRange => {
            let c = doc.borrow_mut().clone_range(id);
            IdlValue::Object(new_range_object(agent, state, &doc, c, None))
        }
        Op::Range_detach => IdlValue::Undefined,
        Op::Range_isPointInRange => {
            let (nd, n) = arg_node(agent, inv, 0)?;
            if !Rc::ptr_eq(&nd, &doc) {
                return Ok(IdlValue::Bool(false));
            }
            let r = doc.borrow().range_is_point_in_range(id, n, inv.usize(1));
            IdlValue::Bool(ok(agent, r)?)
        }
        Op::Range_comparePoint => {
            let n = node_arg(agent, 0)?;
            let r = doc.borrow().range_compare_point(id, n, inv.usize(1));
            IdlValue::Int(i64::from(ok(agent, r)?))
        }
        Op::Range_intersectsNode => {
            let (nd, n) = arg_node(agent, inv, 0)?;
            IdlValue::Bool(Rc::ptr_eq(&nd, &doc) && doc.borrow().range_intersects_node(id, n))
        }
        Op::Range_stringifier => string(doc.borrow().range_to_string(id)),
        op => return not_implemented(agent, op),
    })
}

// ----- traversal ------------------------------------------------------------------

fn traversal(agent: &mut Agent, inv: &Invocation) -> R {
    enum Which {
        Iterator(DocRef, IteratorId),
        Walker(DocRef, WalkerRef),
    }
    let which = with_this(inv, |n| match n {
        Native::NodeIterator(d, id) => Some(Which::Iterator(d.clone(), *id)),
        Native::TreeWalker(d, w) => Some(Which::Walker(d.clone(), w.clone())),
        _ => None,
    });
    let which = need(agent, which)?;
    match which {
        Which::Iterator(doc, id) => {
            let (root, reference, before, what, filter) = {
                let d = doc.borrow();
                let it = d.node_iterator(id);
                (
                    it.root,
                    it.reference,
                    it.pointer_before_reference,
                    it.what_to_show,
                    it.filter,
                )
            };
            Ok(match inv.op {
                Op::NodeIterator_get_root => wrap(agent, &doc, root),
                Op::NodeIterator_get_referenceNode => wrap(agent, &doc, reference),
                Op::NodeIterator_get_pointerBeforeReferenceNode => IdlValue::Bool(before),
                Op::NodeIterator_get_whatToShow => IdlValue::Int(i64::from(what)),
                Op::NodeIterator_get_filter => host_ref_value(agent, filter),
                Op::NodeIterator_nextNode | Op::NodeIterator_previousNode => {
                    let r = {
                        let mut host = JsHost::new(agent, doc.clone())?;
                        if inv.op == Op::NodeIterator_nextNode {
                            iterator_next_node(&doc, &mut host, id)
                        } else {
                            iterator_previous_node(&doc, &mut host, id)
                        }
                    };
                    let n = ok_dom(agent, r)?;
                    wrap_opt(agent, &doc, n)
                }
                Op::NodeIterator_detach => IdlValue::Undefined,
                op => return not_implemented(agent, op),
            })
        }
        Which::Walker(doc, walker) => {
            let (root, current, what, filter) = {
                let w = walker.borrow();
                (w.root, w.current, w.what_to_show, w.filter)
            };
            Ok(match inv.op {
                Op::TreeWalker_get_root => wrap(agent, &doc, root),
                Op::TreeWalker_get_whatToShow => IdlValue::Int(i64::from(what)),
                Op::TreeWalker_get_filter => host_ref_value(agent, filter),
                Op::TreeWalker_get_currentNode => wrap(agent, &doc, current),
                Op::TreeWalker_set_currentNode => {
                    let n = arg_node(agent, inv, 0)?;
                    let n = local_node(agent, &doc, n, DomException::WrongDocumentError)?;
                    walker.borrow_mut().current = n;
                    IdlValue::Undefined
                }
                op => {
                    let r = {
                        let mut host = JsHost::new(agent, doc.clone())?;
                        match op {
                            Op::TreeWalker_parentNode => {
                                walker_parent_node(&doc, &mut host, &walker)
                            }
                            Op::TreeWalker_firstChild => {
                                walker_first_child(&doc, &mut host, &walker)
                            }
                            Op::TreeWalker_lastChild => walker_last_child(&doc, &mut host, &walker),
                            Op::TreeWalker_previousSibling => {
                                walker_previous_sibling(&doc, &mut host, &walker)
                            }
                            Op::TreeWalker_nextSibling => {
                                walker_next_sibling(&doc, &mut host, &walker)
                            }
                            Op::TreeWalker_previousNode => {
                                walker_previous_node(&doc, &mut host, &walker)
                            }
                            Op::TreeWalker_nextNode => walker_next_node(&doc, &mut host, &walker),
                            _ => {
                                drop(host);
                                return not_implemented(agent, op);
                            }
                        }
                    };
                    let n = ok_dom(agent, r)?;
                    wrap_opt(agent, &doc, n)
                }
            })
        }
    }
}

// ----- DOMException and Window ----------------------------------------------------------

fn dom_exception(agent: &mut Agent, state: &Rc<RealmState>, inv: &Invocation) -> R {
    if inv.op == Op::DOMException_constructor {
        let message = inv.str(0);
        let name = match inv.arg(1) {
            IdlValue::String(s) => s.clone(),
            _ => "Error".to_string(),
        };
        return constructed(agent, state, inv, Native::Exception { name, message });
    }
    let v = with_this(inv, |n| match n {
        Native::Exception { name, message } => Some((name.clone(), message.clone())),
        _ => None,
    });
    let (name, message) = need(agent, v)?;
    Ok(match inv.op {
        Op::DOMException_get_name => string(name),
        Op::DOMException_get_message => string(message),
        Op::DOMException_get_code => IdlValue::Int(i64::from(exception::legacy_code(&name))),
        op => return not_implemented(agent, op),
    })
}

fn window(agent: &mut Agent, state: &Rc<RealmState>, inv: &Invocation) -> R {
    let global = state.global.get().map_or(IdlValue::Null, IdlValue::Object);
    Ok(match inv.op {
        Op::Window_get_window
        | Op::Window_get_self
        | Op::Window_get_frames
        | Op::Window_get_top
        | Op::Window_get_parent => global,
        Op::Window_get_document => {
            let doc = window_document(agent, state)?;
            wrap(agent, &doc, NodeId::DOCUMENT)
        }
        Op::Window_get_event => state
            .current_event
            .get()
            .map_or(IdlValue::Undefined, IdlValue::Object),
        op => return generic(agent, state, inv, op),
    })
}

// ----- dispatch -----------------------------------------------------------------

fn not_implemented<T>(agent: &mut Agent, op: Op) -> JsResult<T> {
    agent.type_error(&format!("'{}' is not implemented yet.", op.name()))
}

/// Members handled by name: event handler attributes, empty legacy
/// collections and the constructors of event interfaces.
fn generic(agent: &mut Agent, state: &Rc<RealmState>, inv: &Invocation, op: Op) -> R {
    let name = op.name();
    if name.ends_with("_index_count") {
        return Ok(IdlValue::Int(0));
    }
    if name.ends_with("_supported_names") {
        return Ok(IdlValue::Sequence(Vec::new()));
    }
    if let Some(i) = name.find("_get_on") {
        let handler = &name[i + 5..];
        let v = inv.this.as_ref().and_then(|p| p.get().slot(handler));
        return Ok(match v {
            Some(Value::Object(o)) => IdlValue::Object(o),
            _ => IdlValue::Null,
        });
    }
    if let Some(i) = name.find("_set_on") {
        let handler = &name[i + 5..];
        if let Some(p) = &inv.this {
            let v = inv.object(0).map_or(Value::Null, Value::Object);
            p.get().set_slot(handler, v);
        }
        return Ok(IdlValue::Undefined);
    }
    if name.ends_with("_constructor") && implements(inv.iface, ids::Event) {
        return construct_event(agent, state, inv);
    }
    not_implemented(agent, op)
}

fn run(agent: &mut Agent, state: &Rc<RealmState>, inv: &Invocation) -> R {
    let name = inv.op.name();
    let (owner, _) = name.split_once('_').unwrap_or((name, ""));
    match owner {
        "EventTarget" => event_target(agent, state, inv),
        "Event" | "CustomEvent" => event(agent, state, inv),
        "AbortController" | "AbortSignal"
            if !name.contains("_get_on") && !name.contains("_set_on") =>
        {
            abort(agent, state, inv)
        }
        "MutationObserver" => mutation_observer(agent, state, inv),
        "MutationRecord" => mutation_record(agent, state, inv),
        "Node"
        | "ParentNode"
        | "NonElementParentNode"
        | "DocumentOrShadowRoot"
        | "NonDocumentTypeChildNode"
        | "ChildNode"
        | "Slottable" => node(agent, state, inv),
        "Document" if is_document_member(inv.op) => document(agent, state, inv),
        "DOMImplementation" => implementation(agent, state, inv),
        "DocumentType" => document_type(agent, inv),
        "DocumentFragment" | "ShadowRoot"
            if !name.contains("_get_on") && !name.contains("_set_on") =>
        {
            fragment(agent, state, inv)
        }
        "Element" | "HTMLSlotElement" if is_element_member(inv.op) => element(agent, state, inv),
        "Attr" => attr(agent, inv),
        "CharacterData" | "Text" | "Comment" | "ProcessingInstruction"
            if is_character_data_member(inv.op) =>
        {
            character_data(agent, state, inv)
        }
        "NodeList" | "HTMLCollection" | "NamedNodeMap" | "DOMTokenList" => collection(agent, inv),
        "ElementCSSInlineStyle" if inv.op == Op::ElementCSSInlineStyle_get_style => {
            let (doc, n) = this_node(agent, inv)?;
            Ok(new_object(
                agent,
                state,
                ids::CSSStyleProperties,
                Native::InlineStyle(doc, n),
            ))
        }
        "CSSStyleDeclaration" | "CSSStyleProperties" if inline_style_this(inv).is_some() => {
            inline_style(agent, inv)
        }
        "AbstractRange" | "Range" | "StaticRange" => range(agent, state, inv),
        "NodeIterator" | "TreeWalker" => traversal(agent, inv),
        "DOMException" => dom_exception(agent, state, inv),
        "Window" => window(agent, state, inv),
        _ => generic(agent, state, inv, inv.op),
    }
}

fn is_document_member(op: Op) -> bool {
    !matches!(op, Op::Document_named_getter | Op::Document_supported_names)
        && (dom_document_member(op)
            || matches!(
                op,
                Op::Document_get_body
                    | Op::Document_get_head
                    | Op::Document_get_title
                    | Op::Document_set_title
                    | Op::Document_get_defaultView
            ))
}

fn dom_document_member(op: Op) -> bool {
    matches!(
        op,
        Op::Document_constructor
            | Op::Document_get_implementation
            | Op::Document_get_URL
            | Op::Document_get_documentURI
            | Op::Document_get_compatMode
            | Op::Document_get_characterSet
            | Op::Document_get_charset
            | Op::Document_get_inputEncoding
            | Op::Document_get_contentType
            | Op::Document_get_doctype
            | Op::Document_get_documentElement
            | Op::Document_getElementsByTagName
            | Op::Document_getElementsByTagNameNS
            | Op::Document_getElementsByClassName
            | Op::Document_createElement
            | Op::Document_createElementNS
            | Op::Document_createDocumentFragment
            | Op::Document_createTextNode
            | Op::Document_createCDATASection
            | Op::Document_createComment
            | Op::Document_createProcessingInstruction
            | Op::Document_importNode
            | Op::Document_adoptNode
            | Op::Document_createAttribute
            | Op::Document_createAttributeNS
            | Op::Document_createEvent
            | Op::Document_createRange
            | Op::Document_createNodeIterator
            | Op::Document_createTreeWalker
    )
}

fn is_element_member(op: Op) -> bool {
    let name = op.name();
    if name.starts_with("HTMLSlotElement_") {
        return matches!(
            op,
            Op::HTMLSlotElement_get_name
                | Op::HTMLSlotElement_set_name
                | Op::HTMLSlotElement_assignedNodes
                | Op::HTMLSlotElement_assignedElements
                | Op::HTMLSlotElement_assign
        );
    }
    !name.contains("_get_on") && !name.contains("_set_on") && !is_unimplemented_element_member(op)
}

/// `Element` members of other specifications (CSSOM View, HTML's
/// `setHTML` family, …): handled generically.
fn is_unimplemented_element_member(op: Op) -> bool {
    !matches!(
        op,
        Op::Element_get_namespaceURI
            | Op::Element_get_prefix
            | Op::Element_get_localName
            | Op::Element_get_tagName
            | Op::Element_get_id
            | Op::Element_set_id
            | Op::Element_get_className
            | Op::Element_set_className
            | Op::Element_get_classList
            | Op::Element_get_slot
            | Op::Element_set_slot
            | Op::Element_hasAttributes
            | Op::Element_get_attributes
            | Op::Element_getAttributeNames
            | Op::Element_getAttribute
            | Op::Element_getAttributeNS
            | Op::Element_setAttribute
            | Op::Element_setAttributeNS
            | Op::Element_removeAttribute
            | Op::Element_removeAttributeNS
            | Op::Element_toggleAttribute
            | Op::Element_hasAttribute
            | Op::Element_hasAttributeNS
            | Op::Element_getAttributeNode
            | Op::Element_getAttributeNodeNS
            | Op::Element_setAttributeNode
            | Op::Element_setAttributeNodeNS
            | Op::Element_removeAttributeNode
            | Op::Element_attachShadow
            | Op::Element_get_shadowRoot
            | Op::Element_get_customElementRegistry
            | Op::Element_closest
            | Op::Element_matches
            | Op::Element_webkitMatchesSelector
            | Op::Element_getElementsByTagName
            | Op::Element_getElementsByTagNameNS
            | Op::Element_getElementsByClassName
            | Op::Element_insertAdjacentElement
            | Op::Element_insertAdjacentText
            | Op::Element_get_innerHTML
            | Op::Element_set_innerHTML
            | Op::Element_get_outerHTML
            | Op::Element_set_outerHTML
            | Op::Element_insertAdjacentHTML
    )
}

fn is_character_data_member(op: Op) -> bool {
    matches!(
        op,
        Op::Text_constructor
            | Op::Comment_constructor
            | Op::ProcessingInstruction_constructor
            | Op::ProcessingInstruction_get_target
            | Op::CharacterData_get_data
            | Op::CharacterData_set_data
            | Op::CharacterData_get_length
            | Op::CharacterData_substringData
            | Op::CharacterData_appendData
            | Op::CharacterData_insertData
            | Op::CharacterData_deleteData
            | Op::CharacterData_replaceData
            | Op::Text_splitText
            | Op::Text_get_wholeText
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xml_names() {
        assert!(is_xml_name("xml-stylesheet"));
        assert!(is_xml_name("a:b"));
        assert!(is_xml_name("\u{E9}t\u{E9}"));
        assert!(!is_xml_name(""));
        assert!(!is_xml_name("1a"));
        assert!(!is_xml_name("a b"));
        assert!(!is_xml_name("-a"));
    }

    #[test]
    fn title_whitespace_is_collapsed() {
        assert_eq!(collapse_whitespace("  a \n\t b  c "), "a b c");
        assert_eq!(collapse_whitespace(" \n "), "");
    }

    #[test]
    fn create_event_names_map_to_interfaces() {
        assert_eq!(event_interface_for(ids::Event), EventInterface::Event);
        let custom = crate::bindings::generated::interface_by_name("CustomEvent").unwrap();
        assert_eq!(event_interface_for(custom), EventInterface::CustomEvent);
    }

    #[test]
    fn op_owners_route_to_their_implementation() {
        assert!(is_document_member(Op::Document_createElement));
        assert!(is_document_member(Op::Document_get_body));
        assert!(!is_document_member(Op::Document_supported_names));
        assert!(is_element_member(Op::Element_getAttribute));
        assert!(is_element_member(Op::HTMLSlotElement_assign));
        assert!(is_character_data_member(Op::Text_splitText));
        assert_eq!(reflected(Op::Element_set_className), "class");
    }
}
