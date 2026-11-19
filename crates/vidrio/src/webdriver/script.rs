//! Script execution of WebDriver §13 ("Executing script"): the function
//! body becomes a function (through the realm's `Function` constructor, so
//! it sees `arguments`), the arguments are deserialized from JSON, and
//! the result goes through the JSON clone of §13.1 ("internal JSON
//! clone") with element and window references and cycle detection.
//!
//! The result of a call is kept as JSON in a [`ScriptOutcome`]: the
//! asynchronous callback and the reactions to a returned promise clone
//! their value at once, so no JavaScript value has to outlive the
//! garbage collector's safe points outside a root set.

use std::cell::RefCell;
use std::rc::{Rc, Weak};

use vjs::function::{create_native_function, is_callable, native_host_data};
use vjs::gc::Realm;
use vjs::internal;
use vjs::object::ObjectKind;
use vjs::realm::Intrinsic;
use vjs::{Agent, JsResult, Object, PropertyKey, Throw, Value};

use super::error::{ErrorCode, WdError, WdResult};
use super::json::Json;
use crate::bindings::platform::{Native, platform_ref};
use crate::browser::{ContextId, in_realm, ua};
use crate::dom::{DocRef, Document, NodeId};

/// The key of a web element reference (WebDriver §12.1).
pub const ELEMENT_KEY: &str = "element-6066-11e4-a52e-4f735466cecf";
/// The key of a shadow root reference.
pub const SHADOW_KEY: &str = "shadow-6066-11e4-a52e-4f735466cecf";
/// The key of a web window reference (WebDriver §11.1).
pub const WINDOW_KEY: &str = "window-fcc6-11e5-b4f8-330a88ab9d7f";
/// The key of a web frame reference.
pub const FRAME_KEY: &str = "frame-075b-4da1-b6ba-e579c2d3230a";

/// The deepest nesting the JSON clone follows.
const MAX_CLONE_DEPTH: usize = 1000;

/// The window handle of browsing context `id`: a string with the shape
/// of a UUID that carries the id.
pub fn handle_of(id: ContextId) -> String {
    format!("00000000-0000-4000-8000-{id:012x}")
}

/// The browsing context a window handle names.
pub fn context_of_handle(handle: &str) -> Option<ContextId> {
    let rest = handle.strip_prefix("00000000-0000-4000-8000-")?;
    if rest.len() != 12 {
        return None;
    }
    u32::from_str_radix(rest, 16).ok()
}

/// The known elements of a session: each reference names a node of a
/// document (WebDriver §12.1 "list of known elements").
#[derive(Default)]
pub struct ElementStore {
    entries: Vec<(String, Weak<RefCell<Document>>, NodeId)>,
    seed: u32,
    next: u64,
}

/// What [`ElementStore::resolve`] found.
#[derive(Debug)]
pub enum Known {
    Unknown,
    /// The document is gone.
    Gone,
    Node(DocRef, NodeId),
}

impl ElementStore {
    pub fn new() -> ElementStore {
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos() ^ (d.as_secs() as u32))
            .unwrap_or(0);
        ElementStore {
            entries: Vec::new(),
            seed,
            next: 1,
        }
    }

    /// The reference of `node` of `doc`, created on first use.
    pub fn reference(&mut self, doc: &DocRef, node: NodeId) -> String {
        if let Some((id, _, _)) = self
            .entries
            .iter()
            .find(|(_, d, n)| *n == node && d.upgrade().is_some_and(|d| Rc::ptr_eq(&d, doc)))
        {
            return id.clone();
        }
        // Drop the entries of documents that are gone.
        self.entries.retain(|(_, d, _)| d.strong_count() > 0);
        let id = format!(
            "{:08x}-{:04x}-4{:03x}-8{:03x}-{:012x}",
            self.seed,
            (self.next >> 48) & 0xFFFF,
            (self.next >> 36) & 0xFFF,
            (self.next >> 24) & 0xFFF,
            self.next & 0xFFFF_FFFF_FFFF
        );
        self.next += 1;
        self.entries.push((id.clone(), Rc::downgrade(doc), node));
        id
    }

    pub fn resolve(&self, id: &str) -> Known {
        match self.entries.iter().find(|(x, _, _)| x == id) {
            None => Known::Unknown,
            Some((_, d, n)) => match d.upgrade() {
                Some(d) => Known::Node(d, *n),
                None => Known::Gone,
            },
        }
    }
}

/// "Get a known element" for browsing context `ctx`: the node of
/// reference `id`, if it is in the active document of `ctx` and
/// connected.
pub fn known_element(
    elements: &RefCell<ElementStore>,
    ctx: ContextId,
    id: &str,
) -> WdResult<(DocRef, NodeId)> {
    let found = elements.borrow().resolve(id);
    let (doc, node) = match found {
        Known::Unknown => {
            return Err(WdError::new(
                ErrorCode::NoSuchElement,
                format!("no element with reference {id}"),
            ));
        }
        Known::Gone => {
            return Err(WdError::new(
                ErrorCode::StaleElementReference,
                "the element's document is gone",
            ));
        }
        Known::Node(d, n) => (d, n),
    };
    let ua = ua().ok_or_else(|| WdError::new(ErrorCode::UnknownError, "no user agent"))?;
    let active = ua.context(ctx).and_then(|c| c.active_document());
    match active {
        Some(a) if Rc::ptr_eq(&a, &doc) => {
            if doc.borrow().is_connected(node) {
                Ok((doc, node))
            } else {
                Err(WdError::new(
                    ErrorCode::StaleElementReference,
                    "the element is not connected",
                ))
            }
        }
        _ if !ua.is_fully_active(&doc) => Err(WdError::new(
            ErrorCode::StaleElementReference,
            "the element's document is not active",
        )),
        _ => Err(WdError::new(
            ErrorCode::NoSuchElement,
            "the element belongs to another browsing context",
        )),
    }
}

/// The element reference JSON of `node`.
pub fn element_json(elements: &RefCell<ElementStore>, doc: &DocRef, node: NodeId) -> Json {
    let id = elements.borrow_mut().reference(doc, node);
    Json::Object(vec![(ELEMENT_KEY.to_string(), Json::String(id))])
}

/// The window (or frame) reference JSON of context `id`.
pub fn window_json(id: ContextId) -> Json {
    let top = ua()
        .and_then(|u| u.context_any(id))
        .is_none_or(|c| c.parent.is_none());
    let key = if top { WINDOW_KEY } else { FRAME_KEY };
    Json::Object(vec![(key.to_string(), Json::String(handle_of(id)))])
}

fn js_error(message: impl Into<String>) -> WdError {
    WdError::new(ErrorCode::JavascriptError, message)
}

/// The message of a thrown value: `ToString` of it (for an error object,
/// `Name: message`).
pub fn thrown_message(agent: &mut Agent, v: Value) -> String {
    crate::browser::value_to_string(agent, v)
}

fn get(agent: &mut Agent, o: Object, name: &str) -> WdResult<Value> {
    let key = agent.heap.key_from_str(name);
    vjs::ops::get(agent, o, key).map_err(|Throw(e)| js_error(thrown_message(agent, e)))
}

/// The browsing context whose `Window` or `WindowProxy` `o` is.
fn window_context(agent: &Agent, o: Object) -> Option<ContextId> {
    if let Some(id) = crate::browser::window::proxy_context(agent, o) {
        return Some(id);
    }
    let p = platform_ref(agent, Value::Object(o))?;
    let obj = p.get();
    if !matches!(&*obj.native.borrow(), Native::Window) {
        return None;
    }
    let realm = obj.realm;
    let ua = ua()?;
    let found = ua
        .contexts
        .borrow()
        .iter()
        .find(|c| c.active_window().is_some_and(|w| w.realm == realm))
        .map(|c| c.id);
    found
}

/// How the clone treats an object.
enum Shape {
    Element(DocRef, NodeId),
    Window(ContextId),
    /// Arrays, `arguments`, `NodeList` and `HTMLCollection`.
    List,
    Other,
}

fn shape_of(agent: &Agent, o: Object) -> Shape {
    if let Some(id) = window_context(agent, o) {
        return Shape::Window(id);
    }
    if let Some(p) = platform_ref(agent, Value::Object(o)) {
        match &*p.get().native.borrow() {
            Native::Node(doc, n) if doc.borrow().is_element(*n) => {
                return Shape::Element(doc.clone(), *n);
            }
            Native::NodeList(..) | Native::HtmlCollection(..) => return Shape::List,
            _ => {}
        }
    }
    match &agent.heap.object(o).kind {
        ObjectKind::Array | ObjectKind::Arguments(_) => Shape::List,
        _ => Shape::Other,
    }
}

/// The JSON clone of `v` (WebDriver §13.1 "internal JSON clone").
pub fn clone_value(
    agent: &mut Agent,
    v: Value,
    elements: &RefCell<ElementStore>,
) -> WdResult<Json> {
    let mut seen = Vec::new();
    clone_inner(agent, v, elements, &mut seen)
}

fn clone_inner(
    agent: &mut Agent,
    v: Value,
    elements: &RefCell<ElementStore>,
    seen: &mut Vec<Object>,
) -> WdResult<Json> {
    let o = match v {
        Value::Undefined | Value::Null => return Ok(Json::Null),
        Value::Boolean(b) => return Ok(Json::Bool(b)),
        Value::Number(n) => return Ok(Json::Number(n)),
        Value::String(s) => return Ok(Json::String(agent.heap.string_to_rust(s))),
        Value::Symbol(_) => return Err(js_error("cannot serialize a symbol")),
        Value::BigInt(_) => return Err(js_error("cannot serialize a bigint")),
        Value::Object(o) => o,
    };
    match shape_of(agent, o) {
        Shape::Element(doc, node) => return Ok(element_json(elements, &doc, node)),
        Shape::Window(id) => return Ok(window_json(id)),
        _ => {}
    }
    if seen.contains(&o) {
        return Err(js_error("cyclic object value"));
    }
    if seen.len() >= MAX_CLONE_DEPTH {
        return Err(js_error("object nested too deeply"));
    }
    seen.push(o);
    let r = clone_object(agent, o, elements, seen);
    seen.pop();
    r
}

fn clone_object(
    agent: &mut Agent,
    o: Object,
    elements: &RefCell<ElementStore>,
    seen: &mut Vec<Object>,
) -> WdResult<Json> {
    if let Shape::List = shape_of(agent, o) {
        let len = get(agent, o, "length")?;
        let len = vjs::ops::to_length(agent, len)
            .map_err(|Throw(e)| js_error(thrown_message(agent, e)))?;
        let mut out = Vec::new();
        for i in 0..len as u64 {
            let key = vjs::ops::index_key(agent, i);
            let item = vjs::ops::get(agent, o, key)
                .map_err(|Throw(e)| js_error(thrown_message(agent, e)))?;
            out.push(clone_inner(agent, item, elements, seen)?);
        }
        return Ok(Json::Array(out));
    }
    let to_json = get(agent, o, "toJSON")?;
    if is_callable(agent, to_json) {
        let r = vjs::ops::call(agent, to_json, Value::Object(o), &[])
            .map_err(|Throw(e)| js_error(thrown_message(agent, e)))?;
        return match r {
            Value::Object(x) if x == o => Ok(Json::Object(Vec::new())),
            r => clone_inner(agent, r, elements, seen),
        };
    }
    let keys = internal::own_property_keys(agent, o)
        .map_err(|Throw(e)| js_error(thrown_message(agent, e)))?;
    let mut out = Vec::new();
    for key in keys {
        if matches!(key, PropertyKey::Symbol(_)) {
            continue;
        }
        let desc = internal::get_own_property(agent, o, key)
            .map_err(|Throw(e)| js_error(thrown_message(agent, e)))?;
        if desc.is_none_or(|d| d.enumerable != Some(true)) {
            continue;
        }
        let item =
            vjs::ops::get(agent, o, key).map_err(|Throw(e)| js_error(thrown_message(agent, e)))?;
        let name = agent.heap.key_to_rust(key);
        out.push((name, clone_inner(agent, item, elements, seen)?));
    }
    Ok(Json::Object(out))
}

/// Deserializes a JSON argument into a value of `realm` (WebDriver §13.1
/// "JSON deserialize"): element references become the element's wrapper,
/// window and frame references the `WindowProxy`.
pub fn deserialize(
    agent: &mut Agent,
    realm: Realm,
    ctx: ContextId,
    v: &Json,
    elements: &RefCell<ElementStore>,
) -> WdResult<Value> {
    Ok(match v {
        Json::Null => Value::Null,
        Json::Bool(b) => Value::Boolean(*b),
        Json::Number(n) => Value::Number(*n),
        Json::String(s) => Value::String(agent.heap.alloc_str(s)),
        Json::Array(items) => {
            let mut values = Vec::with_capacity(items.len());
            for item in items {
                values.push(deserialize(agent, realm, ctx, item, elements)?);
            }
            Value::Object(in_realm(agent, realm, |agent| {
                vjs::ops::create_array_from_list(agent, &values)
            }))
        }
        Json::Object(members) => {
            if let Some(Json::String(id)) = v.get(ELEMENT_KEY) {
                let (doc, node) = known_element(elements, ctx, id)?;
                return Ok(Value::Object(crate::bindings::wrap_node(agent, &doc, node)));
            }
            if v.get(SHADOW_KEY).is_some() {
                return Err(WdError::new(
                    ErrorCode::NoSuchShadowRoot,
                    "shadow root references are not supported",
                ));
            }
            for key in [WINDOW_KEY, FRAME_KEY] {
                if let Some(handle) = v.get(key) {
                    let ua =
                        ua().ok_or_else(|| WdError::new(ErrorCode::UnknownError, "no user agent"))?;
                    let c = handle
                        .as_str()
                        .and_then(context_of_handle)
                        .and_then(|id| ua.context(id))
                        .filter(|c| (key == WINDOW_KEY) == c.parent.is_none());
                    return match c {
                        Some(c) => Ok(Value::Object(c.proxy)),
                        None => Err(WdError::new(ErrorCode::NoSuchWindow, "no such window")),
                    };
                }
            }
            let o = in_realm(agent, realm, vjs::ops::object_create);
            for (k, item) in members {
                let value = deserialize(agent, realm, ctx, item, elements)?;
                let key = agent.heap.key_from_str(k);
                vjs::ops::create_data_property_or_throw(agent, o, key, value)
                    .map_err(|Throw(e)| js_error(thrown_message(agent, e)))?;
            }
            Value::Object(o)
        }
    })
}

/// Where the result of a script call ends up.
pub struct ScriptOutcome {
    pub result: RefCell<Option<WdResult<Json>>>,
    pub elements: Rc<RefCell<ElementStore>>,
}

impl ScriptOutcome {
    fn settle(&self, r: WdResult<Json>) {
        let mut slot = self.result.borrow_mut();
        if slot.is_none() {
            *slot = Some(r);
        }
    }

    fn is_settled(&self) -> bool {
        self.result.borrow().is_some()
    }
}

fn outcome_of(agent: &Agent, f: Object) -> Option<Rc<ScriptOutcome>> {
    native_host_data(agent, f).and_then(|d| d.downcast::<ScriptOutcome>().ok())
}

/// The callback of `execute/async` and the fulfillment reaction of a
/// returned promise: clones its first argument into the outcome.
fn resolve_behavior(agent: &mut Agent, call: vjs::function::NativeCall<'_>) -> JsResult<Value> {
    if let Some(out) = outcome_of(agent, call.callee) {
        if !out.is_settled() {
            let r = clone_value(agent, call.arg(0), &out.elements);
            out.settle(r);
        }
    }
    Ok(Value::Undefined)
}

/// The rejection reaction of a returned promise.
fn reject_behavior(agent: &mut Agent, call: vjs::function::NativeCall<'_>) -> JsResult<Value> {
    if let Some(out) = outcome_of(agent, call.callee) {
        if !out.is_settled() {
            let message = thrown_message(agent, call.arg(0));
            out.settle(Err(js_error(message)));
        }
    }
    Ok(Value::Undefined)
}

/// A script call that has started: its outcome and the values that must
/// stay alive while it waits (the function, arguments and callbacks).
pub struct Started {
    pub outcome: Rc<ScriptOutcome>,
    pub keep: Vec<Value>,
}

/// "Execute a function body" `body` in the active window of `ctx` with
/// `args`; with `asynchronous`, a callback that settles the result is
/// appended to the arguments. A returned promise (or thenable) settles
/// the result when it settles.
pub fn start(
    agent: &mut Agent,
    ctx: ContextId,
    body: &str,
    args: &[Json],
    asynchronous: bool,
    elements: &Rc<RefCell<ElementStore>>,
) -> WdResult<Started> {
    let ua = ua().ok_or_else(|| WdError::new(ErrorCode::UnknownError, "no user agent"))?;
    let c = ua
        .context(ctx)
        .ok_or_else(|| WdError::new(ErrorCode::NoSuchWindow, "no such window"))?;
    let w = c
        .active_window()
        .ok_or_else(|| WdError::new(ErrorCode::NoSuchWindow, "the window has no document"))?;
    let realm = w.realm;
    let this = Value::Object(c.proxy);
    let mut values = Vec::with_capacity(args.len() + 1);
    for a in args {
        values.push(deserialize(agent, realm, ctx, a, elements)?);
    }
    let outcome = Rc::new(ScriptOutcome {
        result: RefCell::new(None),
        elements: elements.clone(),
    });
    let source = Value::String(agent.heap.alloc_str(body));
    let function_ctor = agent.heap.realm(realm).intrinsics.get(Intrinsic::Function);
    let f = in_realm(agent, realm, |agent| {
        vjs::ops::call(
            agent,
            Value::Object(function_ctor),
            Value::Undefined,
            &[source],
        )
    });
    let f = match f {
        Ok(f) => f,
        Err(Throw(e)) => return Err(js_error(thrown_message(agent, e))),
    };
    let mut keep = vec![f, this];
    keep.extend(values.iter().copied());
    if asynchronous {
        let data = outcome.clone() as Rc<dyn std::any::Any>;
        let callback = in_realm(agent, realm, |agent| {
            create_native_function(agent, resolve_behavior, 1, "", Vec::new(), Some(data))
        });
        values.push(Value::Object(callback));
        keep.push(Value::Object(callback));
    }
    // The values stay reachable from the stack while the call runs.
    let result = vjs::ops::call(agent, f, this, &values);
    crate::browser::event_loop::clean_up_after_running_script(agent);
    match result {
        Err(Throw(e)) => {
            let message = thrown_message(agent, e);
            outcome.settle(Err(js_error(message)));
        }
        Ok(v) => {
            let then = match v {
                Value::Object(o) => match get(agent, o, "then") {
                    Ok(t) if is_callable(agent, t) => Some(t),
                    Ok(_) => None,
                    Err(e) => {
                        outcome.settle(Err(e));
                        None
                    }
                },
                _ => None,
            };
            if let Some(then) = then {
                let data = outcome.clone() as Rc<dyn std::any::Any>;
                let (on_ok, on_err) = in_realm(agent, realm, |agent| {
                    (
                        create_native_function(
                            agent,
                            resolve_behavior,
                            1,
                            "",
                            Vec::new(),
                            Some(data.clone()),
                        ),
                        create_native_function(
                            agent,
                            reject_behavior,
                            1,
                            "",
                            Vec::new(),
                            Some(data),
                        ),
                    )
                });
                keep.push(Value::Object(on_ok));
                keep.push(Value::Object(on_err));
                keep.push(v);
                let r = vjs::ops::call(
                    agent,
                    then,
                    v,
                    &[Value::Object(on_ok), Value::Object(on_err)],
                );
                crate::browser::event_loop::clean_up_after_running_script(agent);
                if let Err(Throw(e)) = r {
                    let message = thrown_message(agent, e);
                    outcome.settle(Err(js_error(message)));
                }
            } else if !asynchronous {
                let r = clone_value(agent, v, elements);
                outcome.settle(r);
            }
        }
    }
    Ok(Started { outcome, keep })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_handles_round_trip() {
        assert_eq!(handle_of(7), "00000000-0000-4000-8000-000000000007");
        assert_eq!(context_of_handle(&handle_of(300)), Some(300));
        assert_eq!(context_of_handle("nope"), None);
        assert_eq!(context_of_handle("00000000-0000-4000-8000-zz"), None);
    }

    #[test]
    fn element_store_reuses_references_per_node() {
        let doc: DocRef = Rc::new(RefCell::new(Document::new_html()));
        let mut store = ElementStore::new();
        let a = store.reference(&doc, NodeId::DOCUMENT);
        assert_eq!(store.reference(&doc, NodeId::DOCUMENT), a);
        assert!(matches!(
            store.resolve(&a),
            Known::Node(_, NodeId::DOCUMENT)
        ));
        assert!(matches!(store.resolve("other"), Known::Unknown));
        drop(doc);
        assert!(matches!(store.resolve(&a), Known::Gone));
    }
}
