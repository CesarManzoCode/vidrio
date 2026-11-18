//! Event handlers (HTML §8.1.8): the `on…` IDL attributes and content
//! attributes of elements, documents, windows and other event targets.
//!
//! The value of a handler lives in a slot of its target's platform object
//! named after the handler (`onclick`): a callable object, `null`, or a
//! string — the body of a content attribute not compiled yet. The first
//! time a handler becomes non-null, an event listener is added to the
//! target whose callback is a native function ([`handler_listener`]);
//! it gets the handler's current value (compiling the body with the scope
//! chain of HTML §8.1.8.1: the element, its form owner, the document) and
//! runs the event handler processing algorithm (the five arguments and
//! the return value of `window.onerror`, `false` canceling other events).
//!
//! The handlers of `body` and `frameset` named in
//! [`WINDOW_REFLECTING`] are the window's.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use vjs::function::{NativeCall, is_callable, native_slots};
use vjs::{Agent, JsResult, Object, Throw, Value};

use crate::bindings::RealmState;
use crate::bindings::generated::INTERFACES;
use crate::bindings::platform::{Native, PlatformRef, platform_ref};
use crate::dom::events::{ListenerOptions, Target, add_event_listener};
use crate::dom::{DocRef, NodeId};

/// The event handlers of `body` and `frameset` elements that are the
/// window's (HTML §8.1.8.2).
pub const WINDOW_REFLECTING: &[&str] = &[
    "onblur",
    "onerror",
    "onfocus",
    "onload",
    "onresize",
    "onscroll",
    "onafterprint",
    "onbeforeprint",
    "onbeforeunload",
    "onhashchange",
    "onlanguagechange",
    "onmessage",
    "onmessageerror",
    "onoffline",
    "ononline",
    "onpagehide",
    "onpagereveal",
    "onpageshow",
    "onpageswap",
    "onpopstate",
    "onrejectionhandled",
    "onstorage",
    "onunhandledrejection",
    "onunload",
];

thread_local! {
    /// The `&'static str` of every handler name of the IDL files, and the
    /// slot keys of their listeners.
    static NAMES: RefCell<HashMap<String, (&'static str, &'static str)>> =
        RefCell::new(HashMap::new());
}

/// The static name of handler `name` and the slot key of its listener,
/// if some interface has an `EventHandler` attribute of that name.
fn handler_names(name: &str) -> Option<(&'static str, &'static str)> {
    if let Some(n) = NAMES.with(|m| m.borrow().get(name).copied()) {
        return Some(n);
    }
    let found = INTERFACES
        .iter()
        .flat_map(|i| i.attributes.iter())
        .find(|a| a.name == name && a.name.starts_with("on"))
        .map(|a| a.name)?;
    let key: &'static str = Box::leak(format!("[[listener {found}]]").into_boxed_str());
    NAMES.with(|m| m.borrow_mut().insert(name.to_string(), (found, key)));
    Some((found, key))
}

/// True if the interface of `p` (or an ancestor) has the handler `name`.
fn supports_handler(p: &PlatformRef, name: &str) -> bool {
    let mut next = Some(p.get().iface);
    while let Some(i) = next {
        let def = &INTERFACES[i as usize];
        if def.attributes.iter().any(|a| a.name == name) {
            return true;
        }
        next = def.parent;
    }
    false
}

/// The realm state of a platform object.
fn state_of_object(agent: &Agent, p: &PlatformRef) -> Option<Rc<RealmState>> {
    crate::bindings::state_of(agent, p.get().realm)
}

/// The document and event target of a platform object.
fn target_of(p: &PlatformRef, state: &RealmState) -> Option<(DocRef, Target)> {
    match &*p.get().native.borrow() {
        Native::Node(d, n) => Some((d.clone(), Target::Node(*n))),
        Native::AbortSignal(d, s) => Some((d.clone(), Target::Signal(*s))),
        Native::Window => state.document().map(|d| (d, Target::Window)),
        Native::EventTarget(id) => state.document().map(|d| (d, Target::Other(*id))),
        _ => None,
    }
}

/// The platform object whose slot holds handler `name` of `p`: the window
/// for the window-reflecting handlers of `body` and `frameset`.
fn handler_owner(agent: &Agent, p: &PlatformRef, name: &str) -> Option<PlatformRef> {
    let reflecting = match &*p.get().native.borrow() {
        Native::Node(d, n) => {
            let d = d.borrow();
            (d.is_html_element(*n, "body") || d.is_html_element(*n, "frameset"))
                && WINDOW_REFLECTING.contains(&name)
        }
        _ => false,
    };
    if !reflecting {
        return Some(p.clone());
    }
    let state = state_of_object(agent, p)?;
    let global = state.global.get()?;
    platform_ref(agent, Value::Object(global))
}

/// Sets handler `name` of `owner` to `value` (a callable object, `null`
/// or an uncompiled body) and adds its listener the first time it is not
/// `null`.
fn set_handler(
    agent: &mut Agent,
    owner: &PlatformRef,
    name: &'static str,
    key: &'static str,
    value: Value,
) {
    owner.get().set_slot(name, value);
    if value == Value::Null || owner.get().slot(key).is_some() {
        return;
    }
    let Some(state) = state_of_object(agent, owner) else {
        return;
    };
    let Some((doc, target)) = target_of(owner, &state) else {
        return;
    };
    let name_value = Value::String(agent.heap.alloc_str(name));
    let fname = agent.heap.key_from_str("");
    let f = crate::bindings::install::make_fn(
        agent,
        state.realm,
        handler_listener,
        1,
        fname,
        None,
        vec![Value::Object(owner.object), name_value],
    );
    owner.get().set_slot(key, Value::Object(f));
    let callback = state.host_ref(Value::Object(f));
    let mut host = crate::bindings::host::JsHost::with_state(agent, doc.clone(), state.clone());
    add_event_listener(
        &mut doc.borrow_mut(),
        &mut host,
        target,
        &name[2..],
        Some(callback),
        ListenerOptions::default(),
    );
}

/// The getter of an `on…` IDL attribute of `this`.
pub fn idl_get(agent: &mut Agent, this: &PlatformRef, name: &str) -> JsResult<Value> {
    let Some((name, _)) = handler_names(name) else {
        return Ok(Value::Null);
    };
    let Some(owner) = handler_owner(agent, this, name) else {
        return Ok(Value::Null);
    };
    current_value(agent, &owner, name)
}

/// The setter of an `on…` IDL attribute of `this` (`value` is the
/// converted callback or `null`).
pub fn idl_set(agent: &mut Agent, this: &PlatformRef, name: &str, value: Value) {
    let Some((name, key)) = handler_names(name) else {
        return;
    };
    let Some(owner) = handler_owner(agent, this, name) else {
        return;
    };
    let value = match value {
        Value::Object(_) => value,
        _ => Value::Null,
    };
    set_handler(agent, &owner, name, key, value);
    // Setting `onmessage` of a MessagePort starts it.
    if name == "onmessage" {
        super::messaging::implicitly_started(agent, &owner);
    }
}

/// The attribute change steps of an event handler content attribute
/// `name` of `element`.
pub fn content_attribute_changed(agent: &mut Agent, doc: &DocRef, element: NodeId, name: &str) {
    let Some((static_name, key)) = handler_names(name) else {
        return;
    };
    let value = doc
        .borrow()
        .get_attribute(element, name)
        .map(str::to_string);
    let wrapper = crate::bindings::wrap_node(agent, doc, element);
    let Some(p) = platform_ref(agent, Value::Object(wrapper)) else {
        return;
    };
    if !supports_handler(&p, static_name) {
        return;
    }
    let Some(owner) = handler_owner(agent, &p, static_name) else {
        return;
    };
    let v = match value {
        Some(body) => Value::String(agent.heap.alloc_str(&body)),
        None => Value::Null,
    };
    set_handler(agent, &owner, static_name, key, v);
}

/// The event handler content attributes an element already had when it
/// was inserted (the parser creates elements with their attributes).
pub fn element_inserted(agent: &mut Agent, doc: &DocRef, element: NodeId) {
    let names: Vec<String> = {
        let d = doc.borrow();
        match d.element(element) {
            Some(e) => e
                .attrs
                .iter()
                .filter(|a| a.ns.is_none() && a.local.as_str().starts_with("on"))
                .map(|a| a.local.as_str().to_string())
                .collect(),
            None => return,
        }
    };
    for name in names {
        let already = {
            let wrapper = crate::bindings::wrappers::existing_wrapper(doc, element);
            wrapper
                .and_then(|w| platform_ref(agent, Value::Object(w)))
                .and_then(|p| handler_names(&name).map(|(n, _)| (p, n)))
                .and_then(|(p, n)| handler_owner(agent, &p, n).map(|o| (o, n)))
                .is_some_and(|(o, n)| o.get().slot(n).is_some())
        };
        if !already {
            content_attribute_changed(agent, doc, element, &name);
        }
    }
}

/// "Getting the current value of the event handler" `name` of `owner`:
/// compiles an uncompiled body first.
fn current_value(agent: &mut Agent, owner: &PlatformRef, name: &'static str) -> JsResult<Value> {
    let v = owner.get().slot(name).unwrap_or(Value::Null);
    let Value::String(body) = v else {
        return Ok(match v {
            Value::Object(_) => v,
            _ => Value::Null,
        });
    };
    let body = agent.heap.string_to_rust(body);
    let compiled = compile(agent, owner, name, &body);
    owner
        .get()
        .set_slot(name, compiled.map_or(Value::Null, Value::Object));
    Ok(compiled.map_or(Value::Null, Value::Object))
}

/// Compiles the body of a content attribute handler; a syntax error is
/// reported and gives `None`.
fn compile(agent: &mut Agent, owner: &PlatformRef, name: &str, body: &str) -> Option<Object> {
    let state = state_of_object(agent, owner)?;
    let is_window = matches!(&*owner.get().native.borrow(), Native::Window);
    // The scope chain: the document, the form owner, the element.
    let (doc, element) = match &*owner.get().native.borrow() {
        Native::Node(d, n) => (Some(d.clone()), Some(*n)),
        Native::Window => {
            let d = state.document();
            let body_el = d.as_ref().and_then(|d| d.borrow().body());
            (d, body_el)
        }
        _ => (None, None),
    };
    let mut scopes: Vec<Value> = Vec::new();
    if let Some(d) = &doc {
        scopes.push(Value::Object(crate::bindings::wrap_node(
            agent,
            d,
            NodeId::DOCUMENT,
        )));
        if let Some(el) = element {
            let form = d.borrow().element(el).and_then(|e| e.form_owner);
            if let Some(f) = form {
                scopes.push(Value::Object(crate::bindings::wrap_node(agent, d, f)));
            }
            scopes.push(Value::Object(crate::bindings::wrap_node(agent, d, el)));
        }
    }
    let params = if is_window && name == "onerror" {
        "event, source, lineno, colno, error"
    } else {
        "event"
    };
    let withs: String = (0..scopes.len())
        .map(|i| format!("with (this[{i}]) "))
        .collect();
    let source =
        format!("(function () {{ {withs}return function {name}({params}) {{\n{body}\n}}; }})");
    let realm = state.realm;
    let keep = Rc::new(super::window::Roots(scopes.clone()));
    let root = agent
        .heap
        .add_root_set(keep.clone() as Rc<dyn vjs::gc::Trace>);
    let result = (|| -> JsResult<Value> {
        let factory = vjs::evaluate_script_in(agent, realm, &source)?;
        let list = vjs::ops::create_array_from_list(agent, &scopes);
        super::in_realm(agent, realm, |agent| {
            vjs::ops::call(agent, factory, Value::Object(list), &[])
        })
    })();
    agent.heap.remove_root_set(root);
    match result {
        Ok(Value::Object(f)) => Some(f),
        Ok(_) => None,
        Err(Throw(e)) => {
            super::errors::report_exception(agent, &state, e);
            None
        }
    }
}

/// The callback of a handler's listener: the event handler processing
/// algorithm (HTML §8.1.8.1). Slots: the owner and the handler's name.
fn handler_listener(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let slots = native_slots(agent, call.callee).to_vec();
    let (Some(Value::Object(owner)), Some(Value::String(name))) = (slots.first(), slots.get(1))
    else {
        return Ok(Value::Undefined);
    };
    let name = agent.heap.string_to_rust(*name);
    let Some((name, _)) = handler_names(&name) else {
        return Ok(Value::Undefined);
    };
    let Some(owner) = platform_ref(agent, Value::Object(*owner)) else {
        return Ok(Value::Undefined);
    };
    let callback = current_value(agent, &owner, name)?;
    if !is_callable(agent, callback) {
        return Ok(Value::Undefined);
    }
    let event = call.arg(0);
    let Some(ev) = platform_ref(agent, event) else {
        return Ok(Value::Undefined);
    };
    let ev_ref = match &*ev.get().native.borrow() {
        Native::Event(e) => Some(e.clone()),
        _ => None,
    };
    let Some(ev_ref) = ev_ref else {
        return Ok(Value::Undefined);
    };
    let is_window = matches!(&*owner.get().native.borrow(), Native::Window);
    let interface = ev_ref.borrow().interface;
    let error_special = is_window
        && name == "onerror"
        && interface == crate::dom::events::EventInterface::Other("ErrorEvent");
    let ret = if error_special {
        let slot = |k: &str| ev.get().slot(k).unwrap_or(Value::Undefined);
        let args = [
            slot("message"),
            slot("filename"),
            slot("lineno"),
            slot("colno"),
            slot("error"),
        ];
        vjs::ops::call(agent, callback, call.this, &args)?
    } else {
        vjs::ops::call(agent, callback, call.this, &[event])?
    };
    let type_ = ev_ref.borrow().type_.clone();
    if error_special {
        if ret == Value::Boolean(true) {
            ev_ref.borrow_mut().prevent_default();
        }
    } else if type_ == "beforeunload"
        && interface == crate::dom::events::EventInterface::Other("BeforeUnloadEvent")
    {
        if !ret.is_nullish() {
            ev_ref.borrow_mut().prevent_default();
        }
    } else if ret == Value::Boolean(false) {
        ev_ref.borrow_mut().prevent_default();
    }
    Ok(Value::Undefined)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handler_names_are_interned() {
        let (a, ka) = handler_names("onclick").expect("onclick is a handler");
        let (b, kb) = handler_names("onclick").expect("onclick is a handler");
        assert!(std::ptr::eq(a, b));
        assert!(std::ptr::eq(ka, kb));
        assert_eq!(ka, "[[listener onclick]]");
        assert!(handler_names("onnotanevent").is_none());
        assert!(handler_names("title").is_none());
    }

    #[test]
    fn body_reflects_window_handlers() {
        assert!(WINDOW_REFLECTING.contains(&"onload"));
        assert!(WINDOW_REFLECTING.contains(&"onmessage"));
        assert!(!WINDOW_REFLECTING.contains(&"onclick"));
    }
}
