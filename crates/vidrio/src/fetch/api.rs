//! `fetch()`, `Headers`, `Request`, `Response` and the body mixin (Fetch
//! §5–§6) over [`super::net`].

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use vjs::gc::Realm;
use vjs::{Agent, JsResult, Object, Value};

use super::blob::BlobData;
use super::headers::{Guard, Headers};
use super::net::{self, Credentials, FetchRequest, Mode, Redirect, Referrer, ResponseType};
use super::stream::{self, StreamRef};
use super::{Deferred, R, keep};
use crate::bindings::platform::{self, Native};
use crate::bindings::{IdlValue, Invocation, RealmState};
use crate::browser::in_realm;
use crate::dom::DocRef;
use crate::dom::abort::{AbortAlgorithm, SignalId};
use crate::mime::MimeType;
use crate::url::Url;

/// The native state of a `Headers` object (shared with its `Request` or
/// `Response`).
pub struct HeadersObject {
    pub headers: RefCell<Headers>,
}

impl HeadersObject {
    pub fn new(guard: Guard) -> Rc<HeadersObject> {
        Rc::new(HeadersObject {
            headers: RefCell::new(Headers::new(guard)),
        })
    }
}

/// A body: its stream and, when known, its bytes (to send it again or
/// to clone it without reading).
#[derive(Clone)]
pub struct Body {
    pub stream: StreamRef,
    pub source: Option<Vec<u8>>,
}

/// The native state of a `Request` object.
pub struct RequestObject {
    pub req: RefCell<FetchRequest>,
    pub headers: Rc<HeadersObject>,
    pub body: RefCell<Option<Body>>,
    pub signal: (DocRef, SignalId),
    pub destination: String,
    pub cache: RefCell<String>,
    pub integrity: RefCell<String>,
    pub keepalive: Cell<bool>,
}

/// The native state of a `Response` object.
pub struct ResponseObject {
    pub kind: Cell<ResponseType>,
    pub url_list: RefCell<Vec<Url>>,
    pub status: Cell<u16>,
    pub status_text: RefCell<String>,
    pub headers: Rc<HeadersObject>,
    pub body: RefCell<Option<Body>>,
}

/// The document base URL of the window of `state`.
pub fn base_url(state: &RealmState) -> Url {
    state
        .document()
        .map(|d| crate::browser::document::base_url(&d))
        .unwrap_or_else(|| Url::parse("about:blank", None).expect("about:blank parses"))
}

/// The origin and URL of the window of `state` (the fetch client).
pub fn client(state: &RealmState) -> (crate::url::Origin, Option<Url>) {
    match state.document() {
        Some(d) => {
            let d = d.borrow();
            (d.origin.clone(), Some(d.url.clone()))
        }
        None => (crate::url::Origin::new_opaque(), None),
    }
}

/// The header pairs of a `HeadersInit` value.
fn headers_init(agent: &mut Agent, v: &IdlValue) -> JsResult<Vec<(String, String)>> {
    match v {
        IdlValue::Sequence(items) => {
            let mut out = Vec::new();
            for item in items {
                let pair = item.as_seq();
                if pair.len() != 2 {
                    return agent.type_error("A header pair must have exactly two items.");
                }
                out.push((pair[0].as_str().to_string(), pair[1].as_str().to_string()));
            }
            Ok(out)
        }
        IdlValue::Record(entries) => Ok(entries
            .iter()
            .map(|(k, v)| (k.clone(), v.as_str().to_string()))
            .collect()),
        _ => Ok(Vec::new()),
    }
}

fn headers_error<T>(agent: &mut Agent, r: Result<T, String>) -> JsResult<T> {
    match r {
        Ok(v) => Ok(v),
        Err(m) => agent.type_error(&m),
    }
}

/// "Extract" a body from a `BodyInit` value (Fetch §5.2): the body and
/// its `Content-Type`; `None` for `null`.
pub fn extract(
    agent: &mut Agent,
    realm: Realm,
    v: &IdlValue,
) -> JsResult<Option<(Body, Option<String>)>> {
    let (bytes, type_) = match v {
        IdlValue::String(s) => (
            s.as_bytes().to_vec(),
            Some("text/plain;charset=UTF-8".to_string()),
        ),
        IdlValue::Object(o) | IdlValue::Any(Value::Object(o)) => {
            let o = *o;
            if let Some(b) = super::object_data::<BlobData>(agent, o) {
                let t = (!b.type_.is_empty()).then(|| b.type_.clone());
                (b.bytes.to_vec(), t)
            } else if let Some(f) = super::object_data::<super::file::FormDataObject>(agent, o) {
                let (body, t) = super::blob::multipart_encode(&f.entries.borrow());
                (body, Some(t))
            } else if let Some(p) = super::object_data::<super::url_api::SearchParams>(agent, o) {
                let s = p.serialize();
                (
                    s.into_bytes(),
                    Some("application/x-www-form-urlencoded;charset=UTF-8".to_string()),
                )
            } else if let Some(s) = stream::stream_of(agent, o) {
                if s.borrow().locked || s.borrow().disturbed {
                    return agent.type_error("The stream is locked or disturbed.");
                }
                let source = stream::queued_bytes(&s);
                return Ok(Some((Body { stream: s, source }, None)));
            } else if let Some(b) = super::buffer_source_bytes(agent, o) {
                (b, None)
            } else {
                let s = crate::bindings::convert::to_rust_string(agent, Value::Object(o))?;
                (s.into_bytes(), Some("text/plain;charset=UTF-8".to_string()))
            }
        }
        _ => return Ok(None),
    };
    Ok(Some((
        Body {
            stream: stream::from_bytes(realm, bytes.clone()),
            source: Some(bytes),
        },
        type_,
    )))
}

fn headers_object(agent: &mut Agent, state: &RealmState, h: &Rc<HeadersObject>) -> Object {
    super::create_object(agent, state, "Headers", Native::Data(h.clone()), None)
}

/// A `Response` object in the realm of `state`.
pub fn response_object(
    agent: &mut Agent,
    state: &RealmState,
    r: ResponseObject,
    proto: Option<Object>,
) -> Object {
    super::create_object(agent, state, "Response", Native::Data(Rc::new(r)), proto)
}

fn null_body_status(status: u16) -> bool {
    matches!(status, 101 | 103 | 204 | 205 | 304)
}

/// A reason phrase: tabs, spaces, visible ASCII and obs-text.
fn is_reason_phrase(s: &str) -> bool {
    s.chars().all(|c| {
        c == '\t' || c == ' ' || ('!'..='~').contains(&c) || ('\u{80}'..='\u{FF}').contains(&c)
    })
}

/// "Initialize a response" (Fetch §6.4) from a `ResponseInit` and an
/// extracted body.
fn init_response(
    agent: &mut Agent,
    init: Option<&crate::bindings::Dict>,
    body: Option<(Body, Option<String>)>,
) -> JsResult<ResponseObject> {
    let status = init
        .and_then(|d| d.get("status"))
        .map_or(200, |v| v.as_u64() as u16);
    if !(200..=599).contains(&status) {
        return agent.range_error("The status must be in the range 200 to 599.");
    }
    let status_text = init
        .and_then(|d| d.str("statusText"))
        .unwrap_or("")
        .to_string();
    if !is_reason_phrase(&status_text) {
        return agent.type_error("Invalid status text.");
    }
    let headers = HeadersObject::new(Guard::Response);
    if let Some(h) = init.and_then(|d| d.get("headers")) {
        let pairs = headers_init(agent, h)?;
        let r = headers.headers.borrow_mut().fill(&pairs);
        headers_error(agent, r)?;
    }
    let mut out_body = None;
    if let Some((b, t)) = body {
        if null_body_status(status) {
            return agent.type_error("A null body status cannot have a body.");
        }
        if let Some(t) = t {
            let mut h = headers.headers.borrow_mut();
            if !h.has("content-type").unwrap_or(true) {
                super::headers::append(&mut h.list, "Content-Type", &t);
            }
        }
        out_body = Some(b);
    }
    Ok(ResponseObject {
        kind: Cell::new(ResponseType::Default),
        url_list: RefCell::new(Vec::new()),
        status: Cell::new(status),
        status_text: RefCell::new(status_text),
        headers,
        body: RefCell::new(out_body),
    })
}

// ----- the Request constructor ---------------------------------------------------------

/// The `Request` constructor (Fetch §5.4) without the object: the native
/// state.
pub fn new_request(
    agent: &mut Agent,
    state: &Rc<RealmState>,
    input: &IdlValue,
    init: Option<&crate::bindings::Dict>,
) -> JsResult<RequestObject> {
    let realm = state.realm;
    let (origin, client_url) = client(state);
    let base = base_url(state);
    let input_request = input
        .as_object()
        .and_then(|o| super::object_data::<RequestObject>(agent, o));
    let mut fallback_mode = None;
    let mut req: FetchRequest;
    let mut parent_signal: Option<(DocRef, SignalId)> = None;
    let (mut cache, mut integrity, mut keepalive) = (String::from("default"), String::new(), false);
    match &input_request {
        None => {
            let text = match input {
                IdlValue::String(s) => s.clone(),
                other => {
                    let v = crate::bindings::convert::to_js_any(agent, other)?;
                    crate::bindings::convert::to_rust_string(agent, v)?
                }
            };
            let Ok(url) = Url::parse(&text, Some(&base)) else {
                return agent.type_error(&format!("Failed to parse URL from {text}"));
            };
            if url.includes_credentials() {
                return agent.type_error("The URL includes credentials.");
            }
            req = FetchRequest::new(url, origin.clone(), client_url.clone());
            fallback_mode = Some(Mode::Cors);
        }
        Some(r) => {
            req = r.req.borrow().clone();
            req.headers = r.headers.headers.borrow().list.clone();
            parent_signal = Some(r.signal.clone());
            cache = r.cache.borrow().clone();
            integrity = r.integrity.borrow().clone();
            keepalive = r.keepalive.get();
        }
    }
    req.origin = origin;
    req.client_url = client_url;
    let has = |name: &str| init.and_then(|d| d.get(name)).is_some();
    let init_present = init.is_some_and(|d| !d.members.is_empty());
    if let Some(d) = init {
        if d.get("window").is_some_and(|w| !w.is_nullish()) {
            return agent.type_error("'window' can only be null.");
        }
    }
    if init_present {
        if req.mode == Mode::Navigate {
            req.mode = Mode::SameOrigin;
        }
        req.referrer = Referrer::Client;
        req.referrer_policy = String::new();
        req.url_list = vec![req.current_url().clone()];
    }
    let d = init;
    if let Some(r) = d.and_then(|d| d.str("referrer")) {
        if r.is_empty() {
            req.referrer = Referrer::NoReferrer;
        } else {
            let Ok(parsed) = Url::parse(r, Some(&base)) else {
                return agent.type_error("The referrer is not a valid URL.");
            };
            req.referrer =
                if parsed.href() == "about:client" || !parsed.origin().same_origin(&req.origin) {
                    Referrer::Client
                } else {
                    Referrer::Url(parsed)
                };
        }
    }
    if let Some(p) = d.and_then(|d| d.str("referrerPolicy")) {
        req.referrer_policy = p.to_string();
    }
    let mode = d.and_then(|d| d.str("mode")).map(str::to_string);
    if let Some(m) = &mode {
        if m == "navigate" {
            return agent.type_error("The mode cannot be 'navigate'.");
        }
        req.mode = Mode::parse(m).unwrap_or(Mode::Cors);
    } else if let Some(m) = fallback_mode {
        req.mode = m;
    }
    if let Some(c) = d.and_then(|d| d.str("credentials")) {
        req.credentials = Credentials::parse(c).unwrap_or(Credentials::SameOrigin);
    }
    if let Some(c) = d.and_then(|d| d.str("cache")) {
        cache = c.to_string();
    }
    if cache == "only-if-cached" && req.mode != Mode::SameOrigin {
        return agent.type_error("'only-if-cached' needs the 'same-origin' mode.");
    }
    if let Some(r) = d.and_then(|d| d.str("redirect")) {
        req.redirect = Redirect::parse(r).unwrap_or(Redirect::Follow);
    }
    if let Some(i) = d.and_then(|d| d.str("integrity")) {
        integrity = i.to_string();
    }
    if let Some(k) = d.and_then(|d| d.bool("keepalive")) {
        keepalive = k;
    }
    if let Some(m) = d.and_then(|d| d.str("method")) {
        if !super::headers::is_header_name(m) {
            return agent.type_error(&format!("'{m}' is not a valid HTTP method."));
        }
        if super::headers::is_forbidden_method(m) {
            return agent.type_error(&format!("'{m}' HTTP method is unsupported."));
        }
        req.method = super::headers::normalize_method(m);
    }

    // The signal follows the input's and init's signals.
    let init_signal = d.and_then(|d| d.object("signal")).and_then(|o| {
        let p = platform::platform_ref(agent, Value::Object(o))?;
        match &*p.get().native.borrow() {
            Native::AbortSignal(doc, s) => Some((doc.clone(), *s)),
            _ => None,
        }
    });
    if has("signal") {
        parent_signal = init_signal;
    }
    let signal = match parent_signal {
        Some((doc, s)) => {
            let id = doc.borrow_mut().abort_signal_any(&[s]);
            (doc, id)
        }
        None => {
            let doc = match state.document() {
                Some(doc) => doc,
                None => return agent.type_error("no document"),
            };
            let id = doc.borrow_mut().create_abort_signal();
            (doc, id)
        }
    };

    // Headers.
    let guard = if req.mode == Mode::NoCors {
        if !super::headers::is_cors_safelisted_method(&req.method) {
            return agent.type_error(&format!("'{}' is unsupported in no-cors mode.", req.method));
        }
        Guard::RequestNoCors
    } else {
        Guard::Request
    };
    let headers = HeadersObject::new(guard);
    let pairs = match d.and_then(|d| d.get("headers")) {
        Some(h) => headers_init(agent, h)?,
        None => req.headers.clone(),
    };
    if d.and_then(|d| d.get("headers")).is_some() || input_request.is_some() {
        let r = headers.headers.borrow_mut().fill(&pairs);
        headers_error(agent, r)?;
    }

    // The body.
    let input_body = input_request.as_ref().and_then(|r| r.body.borrow().clone());
    let init_body = d.and_then(|d| d.get("body")).filter(|b| !b.is_nullish());
    if (init_body.is_some() || input_body.is_some())
        && matches!(req.method.as_str(), "GET" | "HEAD")
    {
        return agent.type_error("Request with GET/HEAD method cannot have body.");
    }
    let mut body = input_body.clone();
    if let Some(b) = init_body {
        if let Some((extracted, type_)) = extract(agent, realm, b)? {
            if extracted.source.is_none() && !has("duplex") {
                return agent.type_error("'duplex' is required for a stream body.");
            }
            if let Some(t) = type_ {
                let mut h = headers.headers.borrow_mut();
                if !super::headers::contains(&h.list, "content-type") {
                    super::headers::append(&mut h.list, "Content-Type", &t);
                }
            }
            body = Some(extracted);
        }
    } else if let Some(b) = &input_body {
        let (locked, disturbed) = {
            let s = b.stream.borrow();
            (s.locked, s.disturbed)
        };
        if locked || disturbed {
            return agent.type_error("The input request's body is unusable.");
        }
        // The input's body moves into the new request.
        if let Some(r) = &input_request {
            b.stream.borrow_mut().disturbed = true;
            let fresh = match &b.source {
                Some(bytes) => stream::from_bytes(realm, bytes.clone()),
                None => stream::tee(&b.stream),
            };
            body = Some(Body {
                stream: fresh,
                source: b.source.clone(),
            });
            let _ = r;
        }
    }
    req.headers = Vec::new();
    Ok(RequestObject {
        req: RefCell::new(req),
        headers,
        body: RefCell::new(body),
        signal,
        destination: String::new(),
        cache: RefCell::new(cache),
        integrity: RefCell::new(integrity),
        keepalive: Cell::new(keepalive),
    })
}

// ----- fetch() ----------------------------------------------------------------------------

/// `fetch(input, init)` (Fetch §6.5).
pub fn fetch(agent: &mut Agent, state: &Rc<RealmState>, inv: &Invocation) -> R {
    let realm = state.realm;
    let request = match new_request(agent, state, inv.arg(0), inv.arg(1).as_dict()) {
        Ok(r) => Rc::new(r),
        Err(e) => return Ok(IdlValue::Object(super::rejected(agent, realm, e.0))),
    };
    let deferred = Deferred::new(agent, realm);
    let promise = deferred.promise;
    let (sdoc, sid) = request.signal.clone();
    if sdoc.borrow().is_aborted(sid) {
        let reason = sdoc
            .borrow()
            .abort_reason(sid)
            .map_or(Value::Undefined, |r| crate::bindings::host_value(agent, r));
        deferred.reject(agent, reason);
        return Ok(IdlValue::Object(promise));
    }
    let mut req = request.req.borrow().clone();
    req.headers = request.headers.headers.borrow().list.clone();
    req.document = state.document();
    let body = request.body.borrow().clone();
    let state = state.clone();
    // The promise's functions stay alive while a stream body is read.
    let early = keep(deferred.values());
    let start = move |agent: &mut Agent, req: FetchRequest| {
        start_fetch(agent, &state, req, deferred, (sdoc, sid));
    };
    match body {
        None => {
            drop(early);
            start(agent, req)
        }
        Some(b) => match b.source {
            Some(bytes) => {
                drop(early);
                b.stream.borrow_mut().disturbed = true;
                req.body = Some(bytes);
                start(agent, req);
            }
            None => {
                let mut start = Some(start);
                stream::read_all(
                    agent,
                    &b.stream,
                    Box::new(move |agent, r| {
                        let _early = early;
                        let mut req = req;
                        req.body = Some(r.unwrap_or_default());
                        if let Some(f) = start.take() {
                            f(agent, req);
                        }
                    }),
                );
            }
        },
    }
    Ok(IdlValue::Object(promise))
}

fn start_fetch(
    agent: &mut Agent,
    state: &Rc<RealmState>,
    req: FetchRequest,
    deferred: Deferred,
    signal: (DocRef, SignalId),
) {
    let Some(ua) = crate::browser::ua() else {
        return;
    };
    let realm = state.realm;
    let kept = Rc::new(keep(deferred.values()));
    let deferred = Rc::new(deferred);
    // The response's stream once the head arrived.
    let body: Rc<RefCell<Option<StreamRef>>> = Rc::new(RefCell::new(None));
    let st = state.clone();
    let (d2, b2, k2) = (deferred.clone(), body.clone(), kept.clone());
    let (sdoc, sid) = signal;
    // The abort algorithm is dropped once the fetch is over.
    let algorithm_id = Rc::new(Cell::new(0u64));
    let (a2, sdoc2) = (algorithm_id.clone(), sdoc.clone());
    let forget = move || {
        super::remove_abort_algorithm(a2.get());
        sdoc2.borrow_mut().remove_abort_algorithm(sid, a2.get());
    };
    let handle = net::start(
        &ua,
        req,
        Box::new(move |agent, event| {
            let _keep = &k2;
            match event {
                net::Event::Head(head) => {
                    let s = stream::new_stream(realm);
                    if !head.has_body {
                        // An opaque response: a null body.
                    } else {
                        *b2.borrow_mut() = Some(s.clone());
                    }
                    let headers = HeadersObject::new(Guard::Immutable);
                    headers.headers.borrow_mut().list = head.headers.clone();
                    let r = ResponseObject {
                        kind: Cell::new(head.kind),
                        url_list: RefCell::new(head.url_list.clone()),
                        status: Cell::new(head.status),
                        status_text: RefCell::new(head.status_text.clone()),
                        headers,
                        body: RefCell::new(head.has_body.then(|| Body {
                            stream: s,
                            source: None,
                        })),
                    };
                    let o = in_realm(agent, realm, |agent| response_object(agent, &st, r, None));
                    d2.resolve(agent, Value::Object(o));
                    super::settle(agent);
                }
                net::Event::Data(bytes) => {
                    if let Some(s) = b2.borrow().clone() {
                        stream::enqueue(agent, &s, bytes);
                        super::settle(agent);
                    }
                }
                net::Event::Done => {
                    forget();
                    if let Some(s) = b2.borrow_mut().take() {
                        stream::close(agent, &s);
                        super::settle(agent);
                    }
                }
                net::Event::Failed(message) => {
                    forget();
                    let s = b2.borrow_mut().take();
                    match s {
                        Some(s) => stream::error(agent, &s, &message),
                        None => {
                            let e = super::type_error_value(
                                agent,
                                realm,
                                &format!("Failed to fetch: {message}"),
                            );
                            d2.reject(agent, e);
                        }
                    }
                    super::settle(agent);
                }
            }
        }),
    );
    // Aborting the signal stops the fetch.
    let doc_for_reason = sdoc.clone();
    let algorithm = super::add_abort_algorithm(Box::new(move |agent| {
        let _keep = &kept;
        handle.abort();
        let reason = doc_for_reason
            .borrow()
            .abort_reason(sid)
            .map_or(Value::Undefined, |r| crate::bindings::host_value(agent, r));
        let s = body.borrow_mut().take();
        match s {
            Some(s) => stream::error(agent, &s, "The operation was aborted."),
            None => deferred.reject(agent, reason),
        }
    }));
    algorithm_id.set(algorithm);
    sdoc.borrow_mut()
        .add_abort_algorithm(sid, AbortAlgorithm::Host(algorithm));
}

// ----- the members ----------------------------------------------------------------------------

/// The body and `Content-Type` of a `Request` or `Response` object.
fn body_of(agent: &Agent, inv: &Invocation) -> Option<(Option<Body>, Option<String>)> {
    let p = inv.this.as_ref()?;
    if let Some(r) = super::data_of::<RequestObject>(p) {
        let t = super::headers::get(&r.headers.headers.borrow().list, "content-type");
        return Some((r.body.borrow().clone(), t));
    }
    if let Some(r) = super::data_of::<ResponseObject>(p) {
        let t = super::headers::get(&r.headers.headers.borrow().list, "content-type");
        return Some((r.body.borrow().clone(), t));
    }
    let _ = agent;
    None
}

/// What a body mixin method makes of the bytes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Package {
    ArrayBuffer,
    Blob,
    Bytes,
    FormData,
    Json,
    Text,
}

fn package(
    agent: &mut Agent,
    state: &RealmState,
    kind: Package,
    bytes: Vec<u8>,
    mime: Option<&str>,
) -> JsResult<Value> {
    let realm = state.realm;
    in_realm(agent, realm, |agent| {
        Ok(match kind {
            Package::ArrayBuffer => Value::Object(super::array_buffer(agent, realm, bytes)),
            Package::Bytes => Value::Object(super::uint8_array(agent, realm, bytes)),
            Package::Text => {
                let s = crate::encoding::utf8_decode(&bytes);
                super::js_string(agent, &s)
            }
            Package::Json => {
                let s = crate::encoding::utf8_decode(&bytes);
                super::json_parse(agent, &s)?
            }
            Package::Blob => {
                let t = mime
                    .and_then(MimeType::parse)
                    .map(|m| m.serialize())
                    .unwrap_or_default();
                let data = Rc::new(BlobData::new(bytes, &t));
                Value::Object(super::file::blob_object(agent, state, data, None))
            }
            Package::FormData => {
                let m = mime.and_then(MimeType::parse);
                let entries = match m.as_ref().map(MimeType::essence).as_deref() {
                    Some("multipart/form-data") => {
                        let boundary = m.as_ref().and_then(|m| m.parameter("boundary"));
                        match boundary.and_then(|b| super::blob::multipart_parse(&bytes, b)) {
                            Some(e) => e,
                            None => return agent.type_error("Could not parse the form data."),
                        }
                    }
                    Some("application/x-www-form-urlencoded") => crate::url::form::parse(&bytes)
                        .into_iter()
                        .map(|(k, v)| (k, super::blob::FormValue::Text(v)))
                        .collect(),
                    _ => return agent.type_error("The body is not form data."),
                };
                Value::Object(super::file::form_data_object(agent, state, entries, None))
            }
        })
    })
}

/// "Consume body" (Fetch §5.3).
fn consume(agent: &mut Agent, state: &Rc<RealmState>, inv: &Invocation, kind: Package) -> R {
    let realm = state.realm;
    let Some((body, mime)) = body_of(agent, inv) else {
        return agent.type_error("Illegal invocation");
    };
    if let Some(b) = &body {
        let s = b.stream.borrow();
        if s.disturbed || s.locked {
            drop(s);
            return super::rejected_type_error(agent, realm, "The body has already been consumed.");
        }
    }
    let deferred = Deferred::new(agent, realm);
    let promise = deferred.promise;
    match body {
        None => match package(agent, state, kind, Vec::new(), mime.as_deref()) {
            Ok(v) => deferred.resolve(agent, v),
            Err(e) => deferred.reject(agent, e.0),
        },
        Some(b) => {
            let kept = keep(deferred.values());
            let state = state.clone();
            stream::read_all(
                agent,
                &b.stream,
                Box::new(move |agent, r| {
                    let _keep = kept;
                    match r {
                        Ok(bytes) => match package(agent, &state, kind, bytes, mime.as_deref()) {
                            Ok(v) => deferred.resolve(agent, v),
                            Err(e) => deferred.reject(agent, e.0),
                        },
                        Err(m) => {
                            let e = super::type_error_value(agent, realm, &m);
                            deferred.reject(agent, e);
                        }
                    }
                }),
            );
        }
    }
    Ok(IdlValue::Object(promise))
}

fn this_object(inv: &Invocation) -> Option<Object> {
    inv.this_object()
}

/// The `ReadableStream` of a body, cached on the object.
fn body_stream(agent: &mut Agent, state: &RealmState, inv: &Invocation) -> R {
    let Some((body, _)) = body_of(agent, inv) else {
        return agent.type_error("Illegal invocation");
    };
    let Some(b) = body else {
        return Ok(IdlValue::Null);
    };
    let this = this_object(inv).expect("a body object");
    let p = platform::platform_ref(agent, Value::Object(this)).expect("a platform object");
    if let Some(Value::Object(o)) = p.get().slot("[[body]]") {
        if stream::stream_of(agent, o).is_some_and(|s| Rc::ptr_eq(&s, &b.stream)) {
            return Ok(IdlValue::Object(o));
        }
    }
    let o = stream::stream_object(agent, state, &b.stream);
    p.get().set_slot("[[body]]", Value::Object(o));
    Ok(IdlValue::Object(o))
}

fn url_string(url: Option<&Url>) -> String {
    url.map(|u| u.serialize(true)).unwrap_or_default()
}

pub fn call(
    agent: &mut Agent,
    state: &Rc<RealmState>,
    inv: &Invocation,
    owner: &str,
    member: &str,
) -> Option<R> {
    Some(match owner {
        "Headers" => headers_member(agent, state, inv, member),
        "Body" => match member {
            "get_body" => body_stream(agent, state, inv),
            "get_bodyUsed" => match body_of(agent, inv) {
                Some((b, _)) => Ok(IdlValue::Bool(
                    b.is_some_and(|b| b.stream.borrow().disturbed),
                )),
                None => agent.type_error("Illegal invocation"),
            },
            "arrayBuffer" => consume(agent, state, inv, Package::ArrayBuffer),
            "blob" => consume(agent, state, inv, Package::Blob),
            "bytes" => consume(agent, state, inv, Package::Bytes),
            "formData" => consume(agent, state, inv, Package::FormData),
            "json" => consume(agent, state, inv, Package::Json),
            "text" => consume(agent, state, inv, Package::Text),
            _ => return None,
        },
        "Request" => request_member(agent, state, inv, member)?,
        "Response" => response_member(agent, state, inv, member)?,
        _ => return None,
    })
}

fn headers_member(agent: &mut Agent, state: &Rc<RealmState>, inv: &Invocation, member: &str) -> R {
    if member == "constructor" {
        let h = HeadersObject::new(Guard::None);
        let pairs = headers_init(agent, inv.arg(0))?;
        let r = h.headers.borrow_mut().fill(&pairs);
        headers_error(agent, r)?;
        let proto =
            crate::bindings::install::proto_from_new_target(agent, inv.new_target, inv.iface)?;
        let o = super::create_object(agent, state, "Headers", Native::Data(h), proto);
        return Ok(IdlValue::Object(o));
    }
    let h = super::this_data::<HeadersObject>(agent, inv)?;
    let (name, value) = (inv.str(0), inv.str(1));
    let r: Result<IdlValue, String> = match member {
        "append" => h
            .headers
            .borrow_mut()
            .append(&name, &value)
            .map(|_| IdlValue::Undefined),
        "delete" => h
            .headers
            .borrow_mut()
            .delete(&name)
            .map(|_| IdlValue::Undefined),
        "get" => h.headers.borrow().get(&name).map(IdlValue::opt_string),
        "getSetCookie" => Ok(IdlValue::Sequence(
            h.headers
                .borrow()
                .get_set_cookie()
                .into_iter()
                .map(IdlValue::String)
                .collect(),
        )),
        "has" => h.headers.borrow().has(&name).map(IdlValue::Bool),
        "set" => h
            .headers
            .borrow_mut()
            .set(&name, &value)
            .map(|_| IdlValue::Undefined),
        "iterable_pairs" => Ok(IdlValue::Sequence(
            h.headers
                .borrow()
                .iteration_pairs()
                .into_iter()
                .map(|(k, v)| IdlValue::Sequence(vec![IdlValue::String(k), IdlValue::String(v)]))
                .collect(),
        )),
        _ => return agent.type_error(&format!("Headers.{member} is not supported")),
    };
    headers_error(agent, r)
}

fn request_member(
    agent: &mut Agent,
    state: &Rc<RealmState>,
    inv: &Invocation,
    member: &str,
) -> Option<R> {
    if member == "constructor" {
        return Some((|| {
            let r = new_request(agent, state, inv.arg(0), inv.arg(1).as_dict())?;
            let proto =
                crate::bindings::install::proto_from_new_target(agent, inv.new_target, inv.iface)?;
            let o = super::create_object(agent, state, "Request", Native::Data(Rc::new(r)), proto);
            Ok(IdlValue::Object(o))
        })());
    }
    let r = match super::this_data::<RequestObject>(agent, inv) {
        Ok(r) => r,
        Err(e) => return Some(Err(e)),
    };
    let s = |v: &str| Ok(IdlValue::String(v.to_string()));
    Some(match member {
        "get_method" => s(&r.req.borrow().method),
        "get_url" => s(&r.req.borrow().current_url().serialize(false)),
        "get_headers" => Ok(IdlValue::Object(headers_object(agent, state, &r.headers))),
        "get_destination" => s(&r.destination),
        "get_referrer" => match &r.req.borrow().referrer {
            Referrer::NoReferrer => s(""),
            Referrer::Client => s("about:client"),
            Referrer::Url(u) => s(&u.href()),
        },
        "get_referrerPolicy" => s(&r.req.borrow().referrer_policy),
        "get_mode" => s(r.req.borrow().mode.as_str()),
        "get_credentials" => s(r.req.borrow().credentials.as_str()),
        "get_cache" => s(&r.cache.borrow()),
        "get_redirect" => s(r.req.borrow().redirect.as_str()),
        "get_integrity" => s(&r.integrity.borrow()),
        "get_keepalive" => Ok(IdlValue::Bool(r.keepalive.get())),
        "get_isReloadNavigation" | "get_isHistoryNavigation" => Ok(IdlValue::Bool(false)),
        "get_duplex" => s("half"),
        "get_signal" => {
            let (doc, id) = r.signal.clone();
            Ok(IdlValue::Object(crate::bindings::host::signal_wrapper(
                agent, state, &doc, id,
            )))
        }
        "clone" => (|| {
            if let Some(b) = &*r.body.borrow() {
                let st = b.stream.borrow();
                if st.disturbed || st.locked {
                    drop(st);
                    return agent.type_error("The body has already been used.");
                }
            }
            let body = r.body.borrow().as_ref().map(|b| Body {
                stream: stream::tee(&b.stream),
                source: b.source.clone(),
            });
            let headers = HeadersObject::new(r.headers.headers.borrow().guard);
            headers.headers.borrow_mut().list = r.headers.headers.borrow().list.clone();
            let (doc, sid) = r.signal.clone();
            let id = doc.borrow_mut().abort_signal_any(&[sid]);
            let copy = RequestObject {
                req: RefCell::new(r.req.borrow().clone()),
                headers,
                body: RefCell::new(body),
                signal: (doc, id),
                destination: r.destination.clone(),
                cache: RefCell::new(r.cache.borrow().clone()),
                integrity: RefCell::new(r.integrity.borrow().clone()),
                keepalive: Cell::new(r.keepalive.get()),
            };
            let o =
                super::create_object(agent, state, "Request", Native::Data(Rc::new(copy)), None);
            Ok(IdlValue::Object(o))
        })(),
        _ => return None,
    })
}

fn response_member(
    agent: &mut Agent,
    state: &Rc<RealmState>,
    inv: &Invocation,
    member: &str,
) -> Option<R> {
    let realm = state.realm;
    match member {
        "constructor" => {
            return Some((|| {
                let body = if inv.arg(0).is_nullish() {
                    None
                } else {
                    extract(agent, realm, inv.arg(0))?
                };
                let r = init_response(agent, inv.arg(1).as_dict(), body)?;
                let proto = crate::bindings::install::proto_from_new_target(
                    agent,
                    inv.new_target,
                    inv.iface,
                )?;
                Ok(IdlValue::Object(response_object(agent, state, r, proto)))
            })());
        }
        "static_error" => {
            let r = ResponseObject {
                kind: Cell::new(ResponseType::Error),
                url_list: RefCell::new(Vec::new()),
                status: Cell::new(0),
                status_text: RefCell::new(String::new()),
                headers: HeadersObject::new(Guard::Immutable),
                body: RefCell::new(None),
            };
            return Some(Ok(IdlValue::Object(response_object(agent, state, r, None))));
        }
        "static_redirect" => {
            return Some((|| {
                let base = base_url(state);
                let Ok(url) = Url::parse(&inv.str(0), Some(&base)) else {
                    return agent.type_error("Failed to parse the redirect URL.");
                };
                let status = if inv.arg(1).is_missing() {
                    302
                } else {
                    inv.arg(1).as_u64() as u16
                };
                if !matches!(status, 301 | 302 | 303 | 307 | 308) {
                    return agent.range_error("Invalid redirect status.");
                }
                let headers = HeadersObject::new(Guard::Immutable);
                headers
                    .headers
                    .borrow_mut()
                    .list
                    .push(("Location".to_string(), url.serialize(false)));
                let r = ResponseObject {
                    kind: Cell::new(ResponseType::Default),
                    url_list: RefCell::new(Vec::new()),
                    status: Cell::new(status),
                    status_text: RefCell::new(String::new()),
                    headers,
                    body: RefCell::new(None),
                };
                Ok(IdlValue::Object(response_object(agent, state, r, None)))
            })());
        }
        "static_json" => {
            return Some((|| {
                let v = super::convert_any(agent, inv.arg(0))?;
                let Some(text) = super::json_stringify(agent, v)? else {
                    return agent.type_error("The data is not JSON-serializable.");
                };
                let bytes = text.into_bytes();
                let body = Body {
                    stream: stream::from_bytes(realm, bytes.clone()),
                    source: Some(bytes),
                };
                let r = init_response(
                    agent,
                    inv.arg(1).as_dict(),
                    Some((body, Some("application/json".to_string()))),
                )?;
                Ok(IdlValue::Object(response_object(agent, state, r, None)))
            })());
        }
        _ => {}
    }
    let r = match super::this_data::<ResponseObject>(agent, inv) {
        Ok(r) => r,
        Err(e) => return Some(Err(e)),
    };
    Some(match member {
        "get_type" => Ok(IdlValue::string(r.kind.get().as_str())),
        "get_url" => Ok(IdlValue::String(url_string(r.url_list.borrow().last()))),
        "get_redirected" => Ok(IdlValue::Bool(r.url_list.borrow().len() > 1)),
        "get_status" => Ok(IdlValue::Int(i64::from(r.status.get()))),
        "get_ok" => Ok(IdlValue::Bool((200..300).contains(&r.status.get()))),
        "get_statusText" => Ok(IdlValue::String(r.status_text.borrow().clone())),
        "get_headers" => Ok(IdlValue::Object(headers_object(agent, state, &r.headers))),
        "clone" => (|| {
            if let Some(b) = &*r.body.borrow() {
                let st = b.stream.borrow();
                if st.disturbed || st.locked {
                    drop(st);
                    return agent.type_error("The body has already been used.");
                }
            }
            let body = r.body.borrow().as_ref().map(|b| Body {
                stream: stream::tee(&b.stream),
                source: b.source.clone(),
            });
            let headers = HeadersObject::new(r.headers.headers.borrow().guard);
            headers.headers.borrow_mut().list = r.headers.headers.borrow().list.clone();
            let copy = ResponseObject {
                kind: Cell::new(r.kind.get()),
                url_list: RefCell::new(r.url_list.borrow().clone()),
                status: Cell::new(r.status.get()),
                status_text: RefCell::new(r.status_text.borrow().clone()),
                headers,
                body: RefCell::new(body),
            };
            Ok(IdlValue::Object(response_object(agent, state, copy, None)))
        })(),
        _ => return None,
    })
}
