//! The data APIs of k3-11 on top of the network of
//! [`crate::browser::loader`]:
//!
//! - the Fetch Standard: header lists and guards ([`headers`]), the fetch
//!   algorithm with CORS, preflights, redirect modes, the cookie jar and
//!   referrers ([`net`], [`cookies`]), and `fetch()`, `Headers`,
//!   `Request`, `Response` and the body mixin ([`api`]);
//! - a minimal `ReadableStream` with a default reader ([`stream`]);
//! - `Blob`, `File`, `FileReader`, `FormData` and the blob URL store
//!   ([`blob`], [`file`]);
//! - `URL`, `URLSearchParams`, `TextEncoder` and `TextDecoder` over the
//!   URL parser and the encodings of k3-06 ([`url_api`]);
//! - `XMLHttpRequest`, synchronous and asynchronous ([`xhr`]);
//! - `DOMParser` and `XMLSerializer` over [`crate::xml`].
//!
//! The bindings route every member of these interfaces to [`call`].
//! JavaScript values that wait for the network (promises, objects whose
//! events are still to come) are kept alive by [`Kept`] handles: the
//! user agent's [`FetchState`] traces them until the handle drops.

pub mod api;
pub mod blob;
pub mod cookies;
pub mod file;
pub mod headers;
pub mod net;
pub mod stream;
pub mod url_api;
pub mod xhr;

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use vjs::buffer::TypedArrayKind;
use vjs::gc::{Realm, Trace, Tracer};
use vjs::object::ObjectKind;
use vjs::realm::Intrinsic;
use vjs::{Agent, JsResult, Object, Throw, Value};

use crate::bindings::generated::interface_by_name;
use crate::bindings::platform::{self, Native, PlatformRef};
use crate::bindings::{IdlValue, Invocation, RealmState};
use crate::browser::in_realm;

type R = JsResult<IdlValue>;

// ----- the user agent's state ------------------------------------------------------

/// What the user agent keeps for the data APIs: rooted values, the cookie
/// jar and the abort algorithms of pending fetches.
#[derive(Default)]
pub struct FetchState {
    roots: RefCell<HashMap<u64, Vec<Value>>>,
    next_root: Cell<u64>,
    pub cookies: RefCell<cookies::CookieJar>,
    abort_algorithms: RefCell<HashMap<u64, Box<dyn FnOnce(&mut Agent)>>>,
    next_algorithm: Cell<u64>,
}

impl Trace for FetchState {
    fn trace(&self, tracer: &mut Tracer) {
        for values in self.roots.borrow().values() {
            for v in values {
                tracer.value(*v);
            }
        }
    }
}

/// Values kept alive (garbage collector roots) while the handle lives.
pub struct Kept(u64);

/// Roots `values` until the returned handle drops.
pub fn keep(values: Vec<Value>) -> Kept {
    let Some(ua) = crate::browser::ua() else {
        return Kept(0);
    };
    let s = &ua.fetch;
    let id = s.next_root.get() + 1;
    s.next_root.set(id);
    s.roots.borrow_mut().insert(id, values);
    Kept(id)
}

impl Drop for Kept {
    fn drop(&mut self) {
        if self.0 == 0 {
            return;
        }
        if let Some(ua) = crate::browser::ua() {
            if let Ok(mut roots) = ua.fetch.roots.try_borrow_mut() {
                roots.remove(&self.0);
            }
        }
    }
}

/// Registers an abort algorithm run by the `JsHost` when its signal
/// aborts; returns its id for `AbortAlgorithm::Host`.
pub fn add_abort_algorithm(f: Box<dyn FnOnce(&mut Agent)>) -> u64 {
    let Some(ua) = crate::browser::ua() else {
        return 0;
    };
    let id = ua.fetch.next_algorithm.get() + 1;
    ua.fetch.next_algorithm.set(id);
    ua.fetch.abort_algorithms.borrow_mut().insert(id, f);
    id
}

/// Forgets an abort algorithm that is no longer needed.
pub fn remove_abort_algorithm(id: u64) {
    if let Some(ua) = crate::browser::ua() {
        ua.fetch.abort_algorithms.borrow_mut().remove(&id);
    }
}

/// Runs the abort algorithm `id` (from `JsHost::run_abort_algorithm`).
pub fn run_abort_algorithm(agent: &mut Agent, id: u64) {
    let f = crate::browser::ua().and_then(|ua| ua.fetch.abort_algorithms.borrow_mut().remove(&id));
    if let Some(f) = f {
        f(agent);
    }
}

// ----- helpers ---------------------------------------------------------------------

/// The native state of type `T` of a platform object.
pub fn data_of<T: 'static>(p: &PlatformRef) -> Option<Rc<T>> {
    let d: Rc<dyn Any> = match &*p.get().native.borrow() {
        Native::Data(d) | Native::TargetData(_, d) => d.clone(),
        _ => return None,
    };
    d.downcast::<T>().ok()
}

/// The native state of type `T` of `o`, if it is such a platform object.
pub fn object_data<T: 'static>(agent: &Agent, o: Object) -> Option<Rc<T>> {
    data_of(&platform::platform_ref(agent, Value::Object(o))?)
}

/// The native state of type `T` of `this`; a `TypeError` otherwise.
pub fn this_data<T: 'static>(agent: &mut Agent, inv: &Invocation) -> JsResult<Rc<T>> {
    match inv.this.as_ref().and_then(data_of::<T>) {
        Some(d) => Ok(d),
        None => agent.type_error("Illegal invocation"),
    }
}

/// A new platform object of interface `iface` with native state `data`.
pub fn create_object(
    agent: &mut Agent,
    state: &RealmState,
    iface: &str,
    native: Native,
    proto: Option<Object>,
) -> Object {
    let id = interface_by_name(iface).unwrap_or(crate::bindings::generated::ids::EventTarget);
    platform::create(agent, state, id, native, proto)
}

/// The value a thrown error carries.
pub fn thrown<T>(r: JsResult<T>) -> Value {
    match r {
        Err(Throw(v)) => v,
        Ok(_) => Value::Undefined,
    }
}

/// A new `TypeError` of `realm`.
pub fn type_error_value(agent: &mut Agent, realm: Realm, message: &str) -> Value {
    in_realm(agent, realm, |agent| {
        thrown(agent.type_error::<()>(message))
    })
}

/// A new `DOMException` named `name` of `realm`.
pub fn dom_exception_value(agent: &mut Agent, realm: Realm, name: &str, message: &str) -> Value {
    in_realm(agent, realm, |agent| {
        Value::Object(crate::bindings::exception::create(agent, name, message))
    })
}

/// A pending promise of `realm` with its resolving functions.
pub struct Deferred {
    pub promise: Object,
    resolve: Value,
    reject: Value,
}

impl Deferred {
    pub fn new(agent: &mut Agent, realm: Realm) -> Deferred {
        let cap = vjs::interp::promise::new_intrinsic_capability(agent, realm);
        Deferred {
            promise: cap.promise.as_object().expect("a promise"),
            resolve: cap.resolve,
            reject: cap.reject,
        }
    }

    /// The values to keep alive while the promise is pending.
    pub fn values(&self) -> Vec<Value> {
        vec![Value::Object(self.promise), self.resolve, self.reject]
    }

    pub fn resolve(&self, agent: &mut Agent, v: Value) {
        let _ = vjs::ops::call(agent, self.resolve, Value::Undefined, &[v]);
    }

    pub fn reject(&self, agent: &mut Agent, v: Value) {
        let _ = vjs::ops::call(agent, self.reject, Value::Undefined, &[v]);
    }
}

/// A promise of `realm` resolved with `v`.
pub fn resolved(agent: &mut Agent, realm: Realm, v: Value) -> Object {
    let d = Deferred::new(agent, realm);
    d.resolve(agent, v);
    d.promise
}

/// A promise of `realm` rejected with `v`.
pub fn rejected(agent: &mut Agent, realm: Realm, v: Value) -> Object {
    let d = Deferred::new(agent, realm);
    d.reject(agent, v);
    d.promise
}

/// A promise of `realm` rejected with a new `TypeError`.
pub fn rejected_type_error(agent: &mut Agent, realm: Realm, message: &str) -> R {
    let e = type_error_value(agent, realm, message);
    Ok(IdlValue::Object(rejected(agent, realm, e)))
}

fn intrinsic(agent: &Agent, realm: Realm, which: Intrinsic) -> Object {
    agent.heap.realm(realm).intrinsics.get(which)
}

/// A new `ArrayBuffer` of `realm` holding `bytes`.
pub fn array_buffer(agent: &mut Agent, realm: Realm, bytes: Vec<u8>) -> Object {
    let proto = intrinsic(agent, realm, Intrinsic::ArrayBufferPrototype);
    vjs::buffer::array_buffer_from_bytes(agent, Some(proto), bytes, None)
}

/// A new `Uint8Array` of `realm` over a new buffer holding `bytes`.
pub fn uint8_array(agent: &mut Agent, realm: Realm, bytes: Vec<u8>) -> Object {
    let len = bytes.len();
    let buffer = array_buffer(agent, realm, bytes);
    let proto = intrinsic(agent, realm, Intrinsic::Uint8ArrayPrototype);
    vjs::buffer::typed_array_create(
        agent,
        Some(proto),
        TypedArrayKind::Uint8,
        buffer,
        0,
        Some(len),
    )
}

/// A copy of the bytes a buffer source (`ArrayBuffer`,
/// `SharedArrayBuffer`, a typed array or a `DataView`) holds; `None` for
/// other objects. A detached buffer holds no bytes.
pub fn buffer_source_bytes(agent: &Agent, o: Object) -> Option<Vec<u8>> {
    match &agent.heap.object(o).kind {
        ObjectKind::ArrayBuffer(_) => {
            let len = vjs::buffer::buffer_byte_length(agent, o);
            Some(vjs::buffer::read_bytes(agent, o, 0, len))
        }
        ObjectKind::TypedArray(_) => {
            let (kind, buffer, offset, _) = vjs::buffer::typed_array_slots(agent, o)?;
            let len = vjs::buffer::typed_array_length(agent, o).unwrap_or(0) * kind.element_size();
            if len == 0 {
                return Some(Vec::new());
            }
            Some(vjs::buffer::read_bytes(agent, buffer, offset, len))
        }
        ObjectKind::DataView(d) => {
            let (buffer, offset, length) = (d.buffer, d.byte_offset, d.byte_length);
            if vjs::buffer::is_detached(agent, buffer) {
                return Some(Vec::new());
            }
            let total = vjs::buffer::buffer_byte_length(agent, buffer);
            let len = length.unwrap_or(total.saturating_sub(offset));
            if offset + len > total {
                return Some(Vec::new());
            }
            Some(vjs::buffer::read_bytes(agent, buffer, offset, len))
        }
        _ => None,
    }
}

/// `JSON.stringify(v)` of the current realm; `None` when it gives
/// `undefined`.
pub fn json_stringify(agent: &mut Agent, v: Value) -> JsResult<Option<String>> {
    let json = agent.intrinsic(Intrinsic::Json);
    let key = agent.heap.key_from_str("stringify");
    let f = vjs::ops::get(agent, json, key)?;
    let r = vjs::ops::call(agent, f, Value::Object(json), &[v])?;
    match r {
        Value::String(s) => Ok(Some(agent.heap.string_to_rust(s))),
        _ => Ok(None),
    }
}

/// Parses JSON text into a value of the current realm.
pub fn json_parse(agent: &mut Agent, text: &str) -> JsResult<Value> {
    let units: Vec<u16> = text.encode_utf16().collect();
    vjs::json::parse_json(agent, &units)
}

/// An IDL value of no particular type as an ECMAScript value.
pub fn convert_any(agent: &mut Agent, v: &IdlValue) -> JsResult<Value> {
    crate::bindings::convert::to_js_any(agent, v)
}

/// A JavaScript string value.
pub fn js_string(agent: &mut Agent, s: &str) -> Value {
    Value::String(agent.heap.alloc_str(s))
}

/// Runs the queued microtasks when no script is running (after a task
/// of the networking or file reading task sources settled promises).
pub fn settle(agent: &mut Agent) {
    crate::browser::event_loop::clean_up_after_running_script(agent);
}

// ----- the bindings' entry point ------------------------------------------------------

/// Runs `inv` if it is a member of the data APIs.
pub fn call(agent: &mut Agent, state: &Rc<RealmState>, inv: &Invocation) -> Option<R> {
    let name = inv.op.name();
    let (owner, member) = name.split_once('_')?;
    if member.starts_with("get_on") || member.starts_with("set_on") {
        return None;
    }
    match owner {
        "Headers" | "Request" | "Response" | "Body" => api::call(agent, state, inv, owner, member),
        "WindowOrWorkerGlobalScope" if member == "fetch" => Some(api::fetch(agent, state, inv)),
        "ReadableStream" | "ReadableStreamDefaultReader" => {
            stream::call(agent, state, inv, owner, member)
        }
        "Blob" | "File" | "FileReader" | "FormData" => file::call(agent, state, inv, owner, member),
        "URL" | "URLSearchParams" => url_api::call(agent, state, inv, owner, member),
        "TextEncoder" | "TextEncoderCommon" | "TextDecoder" | "TextDecoderCommon" => {
            url_api::encoding_call(agent, state, inv, owner, member)
        }
        "XMLHttpRequest" | "ProgressEvent" => xhr::call(agent, state, inv, owner, member),
        "DOMParser" | "XMLSerializer" => xml_call(agent, state, inv, owner, member),
        _ => None,
    }
}

// ----- DOMParser and XMLSerializer -----------------------------------------------------

/// Parses `text` as a document of MIME type `mime` for `state`'s window:
/// HTML with scripting disabled, or XML (a not-well-formed one becomes a
/// `parsererror` document).
pub fn parse_document(state: &RealmState, text: &str, mime: &str) -> crate::dom::DocRef {
    use crate::dom::{Document, DocumentKind};
    let (url, origin) = match state.document() {
        Some(d) => {
            let d = d.borrow();
            (d.url.clone(), d.origin.clone())
        }
        None => (
            crate::url::Url::parse("about:blank", None).expect("about:blank parses"),
            crate::url::Origin::new_opaque(),
        ),
    };
    let mut doc = if mime == "text/html" {
        let mut d = Document::new_html();
        crate::html::parse_into(&mut d, text, false);
        d
    } else {
        let mut d = Document::new(DocumentKind::Xml, mime);
        if let Err(e) = crate::xml::parse_into(&mut d, text) {
            crate::xml::replace_with_error(&mut d, &e);
        }
        d
    };
    doc.content_type = mime.to_string();
    doc.url = url;
    doc.origin = origin;
    Rc::new(RefCell::new(doc))
}

fn xml_call(
    agent: &mut Agent,
    state: &Rc<RealmState>,
    inv: &Invocation,
    owner: &str,
    member: &str,
) -> Option<R> {
    Some(match (owner, member) {
        ("DOMParser", "constructor") => (|| {
            let proto =
                crate::bindings::install::proto_from_new_target(agent, inv.new_target, inv.iface)?;
            Ok(IdlValue::Object(create_object(
                agent,
                state,
                "DOMParser",
                Native::None,
                proto,
            )))
        })(),
        ("DOMParser", "parseFromString") => {
            let text = match inv.arg(0) {
                IdlValue::String(s) => s.clone(),
                other => match crate::bindings::convert::to_js_any(agent, other) {
                    Ok(v) => match crate::bindings::convert::to_rust_string(agent, v) {
                        Ok(s) => s,
                        Err(e) => return Some(Err(e)),
                    },
                    Err(e) => return Some(Err(e)),
                },
            };
            let doc = parse_document(state, &text, &inv.str(1));
            Ok(IdlValue::Object(crate::bindings::wrap_node(
                agent,
                &doc,
                crate::dom::NodeId::DOCUMENT,
            )))
        }
        ("XMLSerializer", "constructor") => (|| {
            let proto =
                crate::bindings::install::proto_from_new_target(agent, inv.new_target, inv.iface)?;
            Ok(IdlValue::Object(create_object(
                agent,
                state,
                "XMLSerializer",
                Native::None,
                proto,
            )))
        })(),
        ("XMLSerializer", "serializeToString") => {
            let node = inv.object(0).and_then(|o| {
                let p = platform::platform_ref(agent, Value::Object(o))?;
                match &*p.get().native.borrow() {
                    Native::Node(d, n) => Some((d.clone(), *n)),
                    _ => None,
                }
            });
            match node {
                Some((d, n)) => Ok(IdlValue::String(crate::xml::serialize(&d.borrow(), n))),
                None => agent.type_error("The value is not of type 'Node'."),
            }
        }
        _ => return None,
    })
}

// ----- events --------------------------------------------------------------------------

/// A new host id for an event target of the realm of `state`.
pub fn new_target_id(state: &RealmState) -> u64 {
    let id = state.next_target.get();
    state.next_target.set(id + 1);
    id
}

/// Fires an event named `type_` at the event target `target` of the
/// window of `state`: a `ProgressEvent` with `lengthComputable`, `loaded`
/// and `total` when `progress` is given, an `Event` otherwise.
pub fn fire_event(
    agent: &mut Agent,
    state: &Rc<RealmState>,
    target: u64,
    type_: &str,
    progress: Option<(bool, f64, f64)>,
) {
    use crate::dom::events::{Event, EventInit, EventInterface, Target, dispatch};
    let Some(doc) = state.document() else {
        return;
    };
    let now = crate::browser::window::window_now(state);
    let mut e = Event::new_trusted(type_, EventInit::default(), now);
    if progress.is_some() {
        e.interface = EventInterface::Other("ProgressEvent");
    }
    let ev = e.into_ref();
    let o = crate::bindings::host::event_wrapper(agent, state, &ev);
    if let Some((computable, loaded, total)) = progress {
        if let Some(p) = platform::platform_ref(agent, Value::Object(o)) {
            p.get()
                .set_slot("lengthComputable", Value::Boolean(computable));
            p.get().set_slot("loaded", Value::Number(loaded));
            p.get().set_slot("total", Value::Number(total));
        }
    }
    let mut host = crate::bindings::host::JsHost::with_state(agent, doc.clone(), state.clone());
    dispatch(&doc, &mut host, &ev, Target::Other(target), false);
    drop(host);
    settle(agent);
}
