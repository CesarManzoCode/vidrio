//! `XMLHttpRequest` (XHR Standard §3), synchronous and asynchronous, over
//! the fetch algorithm of [`super::net`]: states and `readystatechange`,
//! progress events on the object and its `upload`, `responseType`
//! (`""`, `text`, `arraybuffer`, `blob`, `document`, `json`),
//! `overrideMimeType`, `responseXML` (HTML or XML documents) and the
//! `ProgressEvent` attributes. `timeout` is kept but not enforced
//! (LIMITATIONS.md).

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use vjs::{Agent, JsResult, Throw, Value};

use super::headers::{self, HeaderList};
use super::net::{self, Credentials, FetchHandle, FetchRequest, Mode, ResponseHead};
use super::{R, fire_event};
use crate::bindings::exception::{self, dom};
use crate::bindings::platform::{self, Native};
use crate::bindings::{IdlValue, Invocation, RealmState};
use crate::browser::in_realm;
use crate::dom::events::Target;
use crate::dom::{DomException, NodeId};
use crate::mime::MimeType;
use crate::url::Url;

const UNSENT: u16 = 0;
const OPENED: u16 = 1;
const HEADERS_RECEIVED: u16 = 2;
const LOADING: u16 = 3;
const DONE: u16 = 4;

/// The native state of an `XMLHttpRequest`.
pub struct Xhr {
    target: u64,
    upload_target: Cell<Option<u64>>,
    state: Cell<u16>,
    send_flag: Cell<bool>,
    sync: Cell<bool>,
    method: RefCell<String>,
    url: RefCell<Option<Url>>,
    headers: RefCell<HeaderList>,
    timeout: Cell<u32>,
    with_credentials: Cell<bool>,
    upload_complete: Cell<bool>,
    upload_listener: Cell<bool>,
    /// The response head; `None` is a network error.
    response: RefCell<Option<ResponseHead>>,
    received: RefCell<Vec<u8>>,
    response_type: RefCell<String>,
    override_mime: RefCell<Option<String>>,
    handle: RefCell<Option<FetchHandle>>,
    /// Bumped by `open()` and `abort()`: events of an older fetch stop.
    generation: Cell<u64>,
    request_length: Cell<usize>,
}

fn throw_named<T>(agent: &mut Agent, name: &str, message: &str) -> JsResult<T> {
    Err(Throw(Value::Object(exception::create(
        agent, name, message,
    ))))
}

impl Xhr {
    fn terminate(&self) {
        self.generation.set(self.generation.get() + 1);
        if let Some(h) = self.handle.borrow_mut().take() {
            h.abort();
        }
    }

    /// The final MIME type (XHR §3.6.7).
    fn final_mime(&self) -> MimeType {
        if let Some(m) = self
            .override_mime
            .borrow()
            .as_deref()
            .and_then(MimeType::parse)
        {
            return m;
        }
        self.response
            .borrow()
            .as_ref()
            .and_then(|r| headers::get(&r.headers, "content-type"))
            .and_then(|v| MimeType::parse(&v))
            .unwrap_or_else(|| MimeType::parse("text/xml").expect("text/xml parses"))
    }

    /// The final encoding: the `charset` of the override, then of the
    /// response.
    fn final_charset(&self) -> Option<crate::encoding::Encoding> {
        let from = |m: Option<MimeType>| {
            m.and_then(|m| {
                m.parameter("charset")
                    .and_then(crate::encoding::Encoding::for_label)
            })
        };
        if let Some(o) = self.override_mime.borrow().as_deref() {
            if let Some(e) = from(MimeType::parse(o)) {
                return Some(e);
            }
        }
        let ct = self
            .response
            .borrow()
            .as_ref()
            .and_then(|r| headers::get(&r.headers, "content-type"));
        from(ct.as_deref().and_then(MimeType::parse))
    }

    /// The text response (XHR §3.6.9).
    fn text(&self) -> String {
        if self.response.borrow().is_none() {
            return String::new();
        }
        let bytes = self.received.borrow();
        let charset = self.final_charset();
        if charset.is_none() && self.response_type.borrow().is_empty() && self.final_mime().is_xml()
        {
            return crate::xml::decode(&bytes, None).0;
        }
        crate::encoding::decode(&bytes, charset.unwrap_or(crate::encoding::Encoding::Utf8)).0
    }
}

/// Whether the event target `target` of `state` has listeners.
fn has_listeners(state: &RealmState, target: u64) -> bool {
    state
        .listeners
        .borrow()
        .get(&Target::Other(target))
        .is_some_and(|l| !l.is_empty())
}

fn fire_progress(
    agent: &mut Agent,
    state: &Rc<RealmState>,
    target: u64,
    type_: &str,
    loaded: usize,
    total: usize,
) {
    fire_event(
        agent,
        state,
        target,
        type_,
        Some((total != 0, loaded as f64, total as f64)),
    );
}

/// The "request error steps" (XHR §3.5.6) without the throw, which the
/// synchronous caller does itself.
fn request_error(agent: &mut Agent, state: &Rc<RealmState>, x: &Rc<Xhr>, event: &str) {
    x.state.set(DONE);
    x.send_flag.set(false);
    *x.response.borrow_mut() = None;
    fire_event(agent, state, x.target, "readystatechange", None);
    if !x.upload_complete.get() {
        x.upload_complete.set(true);
        if let (true, Some(u)) = (x.upload_listener.get(), x.upload_target.get()) {
            fire_progress(agent, state, u, event, 0, 0);
            fire_progress(agent, state, u, "loadend", 0, 0);
        }
    }
    fire_progress(agent, state, x.target, event, 0, 0);
    fire_progress(agent, state, x.target, "loadend", 0, 0);
}

/// The request body is sent: upload events (XHR §3.5.6, "process
/// request end-of-body").
fn upload_done(agent: &mut Agent, state: &Rc<RealmState>, x: &Rc<Xhr>) {
    if x.upload_complete.get() {
        return;
    }
    x.upload_complete.set(true);
    if let (true, Some(u)) = (x.upload_listener.get(), x.upload_target.get()) {
        let n = x.request_length.get();
        fire_progress(agent, state, u, "progress", n, n);
        fire_progress(agent, state, u, "load", n, n);
        fire_progress(agent, state, u, "loadend", n, n);
    }
}

fn content_length(x: &Xhr) -> usize {
    x.response
        .borrow()
        .as_ref()
        .and_then(|r| headers::get(&r.headers, "content-length"))
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0)
}

/// "Handle response end-of-body" (XHR §3.5.6).
fn end_of_body(agent: &mut Agent, state: &Rc<RealmState>, x: &Rc<Xhr>) {
    let transmitted = x.received.borrow().len();
    let length = content_length(x);
    if !x.sync.get() {
        fire_progress(agent, state, x.target, "progress", transmitted, length);
    }
    x.state.set(DONE);
    x.send_flag.set(false);
    fire_event(agent, state, x.target, "readystatechange", None);
    fire_progress(agent, state, x.target, "load", transmitted, length);
    fire_progress(agent, state, x.target, "loadend", transmitted, length);
}

/// Clears the cached `response` and `responseXML` objects.
fn clear_cache(inv: &Invocation) {
    if let Some(p) = &inv.this {
        p.get().set_slot("[[response]]", Value::Undefined);
        p.get().set_slot("[[responseXML]]", Value::Undefined);
    }
}

pub fn call(
    agent: &mut Agent,
    state: &Rc<RealmState>,
    inv: &Invocation,
    owner: &str,
    member: &str,
) -> Option<R> {
    if owner == "ProgressEvent" {
        let attr = member.strip_prefix("get_")?;
        let this = inv.this.as_ref()?;
        let v = this.get().slot(attr).unwrap_or(match attr {
            "lengthComputable" => Value::Boolean(false),
            _ => Value::Number(0.0),
        });
        return Some(Ok(IdlValue::Any(v)));
    }
    if member == "constructor" {
        return Some((|| {
            let target = super::new_target_id(state);
            let x = Rc::new(Xhr {
                target,
                upload_target: Cell::new(None),
                state: Cell::new(UNSENT),
                send_flag: Cell::new(false),
                sync: Cell::new(false),
                method: RefCell::new(String::new()),
                url: RefCell::new(None),
                headers: RefCell::new(Vec::new()),
                timeout: Cell::new(0),
                with_credentials: Cell::new(false),
                upload_complete: Cell::new(false),
                upload_listener: Cell::new(false),
                response: RefCell::new(None),
                received: RefCell::new(Vec::new()),
                response_type: RefCell::new(String::new()),
                override_mime: RefCell::new(None),
                handle: RefCell::new(None),
                generation: Cell::new(0),
                request_length: Cell::new(0),
            });
            let proto =
                crate::bindings::install::proto_from_new_target(agent, inv.new_target, inv.iface)?;
            let o = super::create_object(
                agent,
                state,
                "XMLHttpRequest",
                Native::TargetData(target, x),
                proto,
            );
            state.targets.borrow_mut().insert(target, o);
            Ok(IdlValue::Object(o))
        })());
    }
    let x = match super::this_data::<Xhr>(agent, inv) {
        Ok(x) => x,
        Err(e) => return Some(Err(e)),
    };
    Some(match member {
        "get_readyState" => Ok(IdlValue::Int(i64::from(x.state.get()))),
        "open" => open(agent, state, inv, &x),
        "setRequestHeader" => (|| {
            if x.state.get() != OPENED || x.send_flag.get() {
                return dom(agent, DomException::InvalidStateError);
            }
            let name = inv.str(0);
            let value = headers::normalize(&inv.str(1));
            if !headers::is_header_name(&name) || !headers::is_header_value(&value) {
                return dom(agent, DomException::SyntaxError);
            }
            if !headers::is_forbidden_request_header(&name, &value) {
                headers::combine(&mut x.headers.borrow_mut(), &name, &value);
            }
            Ok(IdlValue::Undefined)
        })(),
        "get_timeout" => Ok(IdlValue::Int(i64::from(x.timeout.get()))),
        "set_timeout" => (|| {
            if x.sync.get() && x.state.get() != UNSENT {
                return throw_named(
                    agent,
                    "InvalidAccessError",
                    "Synchronous requests cannot have a timeout.",
                );
            }
            x.timeout.set(inv.u32(0));
            Ok(IdlValue::Undefined)
        })(),
        "get_withCredentials" => Ok(IdlValue::Bool(x.with_credentials.get())),
        "set_withCredentials" => (|| {
            if !matches!(x.state.get(), UNSENT | OPENED) || x.send_flag.get() {
                return dom(agent, DomException::InvalidStateError);
            }
            x.with_credentials.set(inv.bool(0));
            Ok(IdlValue::Undefined)
        })(),
        "get_upload" => {
            let id = match x.upload_target.get() {
                Some(id) => id,
                None => {
                    let id = super::new_target_id(state);
                    x.upload_target.set(Some(id));
                    id
                }
            };
            let existing = state.targets.borrow().get(&id).copied();
            let o = match existing {
                Some(o) => o,
                None => {
                    let o = super::create_object(
                        agent,
                        state,
                        "XMLHttpRequestUpload",
                        Native::EventTarget(id),
                        None,
                    );
                    state.targets.borrow_mut().insert(id, o);
                    o
                }
            };
            Ok(IdlValue::Object(o))
        }
        "send" => send(agent, state, inv, &x),
        "abort" => {
            x.terminate();
            let s = x.state.get();
            if (s == OPENED && x.send_flag.get()) || s == HEADERS_RECEIVED || s == LOADING {
                request_error(agent, state, &x, "abort");
            }
            if x.state.get() == DONE {
                x.state.set(UNSENT);
                *x.response.borrow_mut() = None;
            }
            Ok(IdlValue::Undefined)
        }
        "get_responseURL" => Ok(IdlValue::String(
            x.response
                .borrow()
                .as_ref()
                .and_then(|r| r.url_list.last().map(|u| u.serialize(true)))
                .unwrap_or_default(),
        )),
        "get_status" => Ok(IdlValue::Int(
            x.response
                .borrow()
                .as_ref()
                .map_or(0, |r| i64::from(r.status)),
        )),
        "get_statusText" => Ok(IdlValue::String(
            x.response
                .borrow()
                .as_ref()
                .map(|r| r.status_text.clone())
                .unwrap_or_default(),
        )),
        "getResponseHeader" => Ok(IdlValue::opt_string(
            x.response
                .borrow()
                .as_ref()
                .and_then(|r| headers::get(&r.headers, &inv.str(0))),
        )),
        "getAllResponseHeaders" => {
            let mut out = String::new();
            if let Some(r) = x.response.borrow().as_ref() {
                for (n, v) in headers::sort_and_combine(&r.headers) {
                    out.push_str(&n);
                    out.push_str(": ");
                    out.push_str(&v);
                    out.push_str("\r\n");
                }
            }
            Ok(IdlValue::String(out))
        }
        "overrideMimeType" => (|| {
            if matches!(x.state.get(), LOADING | DONE) {
                return dom(agent, DomException::InvalidStateError);
            }
            let m = MimeType::parse(&inv.str(0))
                .map(|m| m.serialize())
                .unwrap_or_else(|| "application/octet-stream".to_string());
            *x.override_mime.borrow_mut() = Some(m);
            Ok(IdlValue::Undefined)
        })(),
        "get_responseType" => Ok(IdlValue::String(x.response_type.borrow().clone())),
        "set_responseType" => (|| {
            if matches!(x.state.get(), LOADING | DONE) {
                return dom(agent, DomException::InvalidStateError);
            }
            if x.sync.get() && x.state.get() != UNSENT {
                return throw_named(
                    agent,
                    "InvalidAccessError",
                    "Synchronous requests cannot set a response type.",
                );
            }
            *x.response_type.borrow_mut() = inv.str(0);
            Ok(IdlValue::Undefined)
        })(),
        "get_responseText" => (|| {
            if !matches!(x.response_type.borrow().as_str(), "" | "text") {
                return dom(agent, DomException::InvalidStateError);
            }
            if !matches!(x.state.get(), LOADING | DONE) {
                return Ok(IdlValue::String(String::new()));
            }
            Ok(IdlValue::String(x.text()))
        })(),
        "get_response" => response(agent, state, inv, &x),
        "get_responseXML" => (|| {
            if !matches!(x.response_type.borrow().as_str(), "" | "document") {
                return dom(agent, DomException::InvalidStateError);
            }
            if x.state.get() != DONE {
                return Ok(IdlValue::Null);
            }
            Ok(IdlValue::Any(document_response(
                agent,
                state,
                inv,
                &x,
                "[[responseXML]]",
            )))
        })(),
        _ => return None,
    })
}

fn open(agent: &mut Agent, state: &Rc<RealmState>, inv: &Invocation, x: &Rc<Xhr>) -> R {
    let method = inv.str(0);
    if !headers::is_header_name(&method) {
        return dom(agent, DomException::SyntaxError);
    }
    if headers::is_forbidden_method(&method) {
        return dom(agent, DomException::SecurityError);
    }
    let method = headers::normalize_method(&method);
    let base = super::api::base_url(state);
    let Ok(mut url) = Url::parse(&inv.str(1), Some(&base)) else {
        return dom(agent, DomException::SyntaxError);
    };
    let asynchronous = if inv.overload == 0 { true } else { inv.bool(2) };
    if inv.overload == 1 && url.host.is_some() {
        if let Some(user) = inv.opt_str(3) {
            url.set_username(&user);
        }
        if let Some(password) = inv.opt_str(4) {
            url.set_password(&password);
        }
    }
    if !asynchronous && (x.timeout.get() != 0 || !x.response_type.borrow().is_empty()) {
        return throw_named(
            agent,
            "InvalidAccessError",
            "Synchronous requests cannot have a timeout or a response type.",
        );
    }
    x.terminate();
    x.send_flag.set(false);
    x.upload_listener.set(false);
    *x.method.borrow_mut() = method;
    *x.url.borrow_mut() = Some(url);
    x.sync.set(!asynchronous);
    x.headers.borrow_mut().clear();
    *x.response.borrow_mut() = None;
    x.received.borrow_mut().clear();
    clear_cache(inv);
    if x.state.get() != OPENED {
        x.state.set(OPENED);
        fire_event(agent, state, x.target, "readystatechange", None);
    }
    Ok(IdlValue::Undefined)
}

/// Sets the `charset` of an author `Content-Type` to UTF-8 (XHR §3.5.6,
/// step 4.4.2).
fn utf8_charset(value: &str) -> Option<String> {
    let mut m = MimeType::parse(value)?;
    let current = m.parameter("charset")?;
    if current.eq_ignore_ascii_case("utf-8") {
        return None;
    }
    m.set_parameter("charset", "UTF-8");
    Some(m.serialize())
}

fn send(agent: &mut Agent, state: &Rc<RealmState>, inv: &Invocation, x: &Rc<Xhr>) -> R {
    if x.state.get() != OPENED || x.send_flag.get() {
        return dom(agent, DomException::InvalidStateError);
    }
    let realm = state.realm;
    let method = x.method.borrow().clone();
    let mut body_arg = inv.arg(0).clone();
    if matches!(method.as_str(), "GET" | "HEAD") {
        body_arg = IdlValue::Null;
    }
    let mut body: Option<Vec<u8>> = None;
    if !body_arg.is_nullish() {
        let document = body_arg.as_object().and_then(|o| {
            let p = platform::platform_ref(agent, Value::Object(o))?;
            match &*p.get().native.borrow() {
                Native::Node(d, n) if *n == NodeId::DOCUMENT => Some(d.clone()),
                _ => None,
            }
        });
        let is_text = matches!(body_arg, IdlValue::String(_)) || document.is_some();
        let (bytes, type_) = match document {
            Some(d) => {
                let doc = d.borrow();
                let (text, t) = if doc.is_html() {
                    (
                        crate::html::serialize_fragment(&doc, NodeId::DOCUMENT, false),
                        "text/html;charset=UTF-8",
                    )
                } else {
                    (
                        crate::xml::serialize(&doc, NodeId::DOCUMENT),
                        "application/xml;charset=UTF-8",
                    )
                };
                (text.into_bytes(), Some(t.to_string()))
            }
            None => match super::api::extract(agent, realm, &body_arg)? {
                Some((b, t)) => (b.source.unwrap_or_default(), t),
                None => (Vec::new(), None),
            },
        };
        let mut h = x.headers.borrow_mut();
        match headers::get(&h, "content-type") {
            Some(author) => {
                if is_text {
                    if let Some(v) = utf8_charset(&author) {
                        headers::set(&mut h, "Content-Type", &v);
                    }
                }
            }
            None => {
                if let Some(t) = type_ {
                    headers::set(&mut h, "Content-Type", &t);
                }
            }
        }
        body = Some(bytes);
    }
    let url = x.url.borrow().clone().expect("open() set the URL");
    let (origin, client_url) = super::api::client(state);
    let mut req = FetchRequest::new(url, origin, client_url);
    req.method = method;
    req.headers = x.headers.borrow().clone();
    req.mode = Mode::Cors;
    req.credentials = if x.with_credentials.get() {
        Credentials::Include
    } else {
        Credentials::SameOrigin
    };
    req.document = state.document();
    let upload_listener = x
        .upload_target
        .get()
        .is_some_and(|u| has_listeners(state, u));
    x.upload_listener.set(upload_listener);
    req.use_cors_preflight = upload_listener;
    x.request_length.set(body.as_ref().map_or(0, Vec::len));
    x.upload_complete.set(body.is_none());
    req.body = body;
    x.send_flag.set(true);
    let Some(ua) = crate::browser::ua() else {
        return agent.type_error("no user agent");
    };

    if x.sync.get() {
        let result = net::fetch_sync(&ua, req);
        match result {
            Err(e) => {
                x.state.set(DONE);
                x.send_flag.set(false);
                *x.response.borrow_mut() = None;
                return throw_named(
                    agent,
                    "NetworkError",
                    &format!("A network error occurred: {e}"),
                );
            }
            Ok((head, bytes)) => {
                x.upload_complete.set(true);
                *x.response.borrow_mut() = Some(head);
                *x.received.borrow_mut() = bytes;
                end_of_body(agent, state, x);
            }
        }
        return Ok(IdlValue::Undefined);
    }

    fire_progress(agent, state, x.target, "loadstart", 0, 0);
    if let (false, true, Some(u)) = (
        x.upload_complete.get(),
        upload_listener,
        x.upload_target.get(),
    ) {
        fire_progress(agent, state, u, "loadstart", 0, x.request_length.get());
    }
    if x.state.get() != OPENED || !x.send_flag.get() {
        return Ok(IdlValue::Undefined);
    }
    let generation = x.generation.get();
    let (st, xx) = (state.clone(), x.clone());
    let handle = net::start(
        &ua,
        req,
        Box::new(move |agent, event| {
            if xx.generation.get() != generation {
                return;
            }
            match event {
                net::Event::Head(head) => {
                    upload_done(agent, &st, &xx);
                    if xx.generation.get() != generation {
                        return;
                    }
                    *xx.response.borrow_mut() = Some(head);
                    xx.state.set(HEADERS_RECEIVED);
                    fire_event(agent, &st, xx.target, "readystatechange", None);
                }
                net::Event::Data(bytes) => {
                    xx.received.borrow_mut().extend_from_slice(&bytes);
                    if xx.state.get() == HEADERS_RECEIVED {
                        xx.state.set(LOADING);
                    }
                    fire_event(agent, &st, xx.target, "readystatechange", None);
                    if xx.generation.get() != generation {
                        return;
                    }
                    let n = xx.received.borrow().len();
                    fire_progress(agent, &st, xx.target, "progress", n, content_length(&xx));
                }
                net::Event::Done => {
                    xx.handle.borrow_mut().take();
                    end_of_body(agent, &st, &xx);
                }
                net::Event::Failed(_) => {
                    xx.handle.borrow_mut().take();
                    request_error(agent, &st, &xx, "error");
                }
            }
        }),
    );
    *x.handle.borrow_mut() = Some(handle);
    Ok(IdlValue::Undefined)
}

/// The `response` getter (XHR §3.6.9).
fn response(agent: &mut Agent, state: &Rc<RealmState>, inv: &Invocation, x: &Rc<Xhr>) -> R {
    let kind = x.response_type.borrow().clone();
    if kind.is_empty() || kind == "text" {
        if !matches!(x.state.get(), LOADING | DONE) {
            return Ok(IdlValue::String(String::new()));
        }
        return Ok(IdlValue::String(x.text()));
    }
    if x.state.get() != DONE {
        return Ok(IdlValue::Null);
    }
    let this = inv.this.clone().expect("an XMLHttpRequest");
    if let Some(v) = this
        .get()
        .slot("[[response]]")
        .filter(|v| !v.is_undefined())
    {
        return Ok(IdlValue::Any(v));
    }
    if kind == "document" {
        return Ok(IdlValue::Any(document_response(
            agent,
            state,
            inv,
            x,
            "[[response]]",
        )));
    }
    if x.response.borrow().is_none() {
        return Ok(IdlValue::Null);
    }
    let realm = state.realm;
    let bytes = x.received.borrow().clone();
    let v = in_realm(agent, realm, |agent| -> JsResult<Value> {
        Ok(match kind.as_str() {
            "arraybuffer" => Value::Object(super::array_buffer(agent, realm, bytes)),
            "blob" => {
                let t = x.final_mime().serialize();
                let data = Rc::new(super::blob::BlobData::new(bytes, &t));
                Value::Object(super::file::blob_object(agent, state, data, None))
            }
            "json" => {
                let text = crate::encoding::utf8_decode(&bytes);
                super::json_parse(agent, &text).unwrap_or(Value::Null)
            }
            _ => Value::Null,
        })
    })?;
    this.get().set_slot("[[response]]", v);
    Ok(IdlValue::Any(v))
}

/// The document response (XHR §3.6.8), cached in `slot`.
fn document_response(
    agent: &mut Agent,
    state: &Rc<RealmState>,
    inv: &Invocation,
    x: &Rc<Xhr>,
    slot: &'static str,
) -> Value {
    let this = inv.this.clone().expect("an XMLHttpRequest");
    if let Some(v) = this.get().slot(slot).filter(|v| !v.is_undefined()) {
        return v;
    }
    let v = build_document(agent, state, x).unwrap_or(Value::Null);
    this.get().set_slot(slot, v);
    v
}

fn build_document(agent: &mut Agent, state: &Rc<RealmState>, x: &Rc<Xhr>) -> Option<Value> {
    use crate::dom::{Document, DocumentKind};
    let head = x.response.borrow().clone()?;
    let mime = x.final_mime();
    let html = mime.is_html();
    if !html && !mime.is_xml() {
        return None;
    }
    if x.response_type.borrow().is_empty() && html {
        return None;
    }
    let bytes = x.received.borrow().clone();
    let mut doc = if html {
        let encoding = x
            .final_charset()
            .or_else(|| crate::browser::loader::prescan(&bytes))
            .unwrap_or(crate::encoding::Encoding::Utf8);
        let (text, used) = crate::encoding::decode(&bytes, encoding);
        let mut d = Document::new_html();
        crate::html::parse_into(&mut d, &text, false);
        d.encoding = used;
        d
    } else {
        let content_type = x
            .final_charset()
            .map(|e| format!("text/xml;charset={}", e.name()));
        let (text, used) = crate::xml::decode(&bytes, content_type.as_deref());
        let mut d = Document::new(DocumentKind::Xml, &mime.essence());
        crate::xml::parse_into(&mut d, &text).ok()?;
        d.encoding = used;
        d
    };
    doc.content_type = mime.essence();
    if let Some(u) = head.url_list.last() {
        doc.url = u.clone();
    }
    doc.origin = super::api::client(state).0;
    let doc = Rc::new(RefCell::new(doc));
    Some(Value::Object(crate::bindings::wrap_node(
        agent,
        &doc,
        NodeId::DOCUMENT,
    )))
}
