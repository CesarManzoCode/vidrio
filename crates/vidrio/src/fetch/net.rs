//! The fetch algorithm (Fetch §4) over the network of
//! [`crate::browser::loader`]: main fetch with the request modes
//! (`same-origin`, `no-cors`, `cors`, `navigate`), response tainting,
//! CORS preflights (without a preflight cache) and the CORS check,
//! redirects with the `follow`, `error` and `manual` modes, the
//! `Origin`, `Referer` and `Cookie` request headers, `Set-Cookie`, and the
//! basic, CORS and opaque filtered responses.
//!
//! Each step that needs the network is one request of the loader with
//! manual redirects; [`start`] drives them asynchronously (events come as
//! networking tasks) and [`fetch_sync`] blocks the thread (synchronous
//! `XMLHttpRequest`). Both use the same decisions ([`decide`],
//! [`process_head`]).

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use vjs::Agent;

use super::headers::{self, HeaderList};
use crate::browser::UserAgent;
use crate::browser::loader::{self, FetchEvent, NetRequest, ResponseInfo};
use crate::dom::DocRef;
use crate::url::{Origin, Url};

/// The maximum number of redirects (Fetch §4.4, step 6).
pub const MAX_REDIRECTS: u32 = 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    SameOrigin,
    NoCors,
    Cors,
    Navigate,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::SameOrigin => "same-origin",
            Mode::NoCors => "no-cors",
            Mode::Cors => "cors",
            Mode::Navigate => "navigate",
        }
    }

    pub fn parse(s: &str) -> Option<Mode> {
        Some(match s {
            "same-origin" => Mode::SameOrigin,
            "no-cors" => Mode::NoCors,
            "cors" => Mode::Cors,
            "navigate" => Mode::Navigate,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Credentials {
    Omit,
    SameOrigin,
    Include,
}

impl Credentials {
    pub fn as_str(self) -> &'static str {
        match self {
            Credentials::Omit => "omit",
            Credentials::SameOrigin => "same-origin",
            Credentials::Include => "include",
        }
    }

    pub fn parse(s: &str) -> Option<Credentials> {
        Some(match s {
            "omit" => Credentials::Omit,
            "same-origin" => Credentials::SameOrigin,
            "include" => Credentials::Include,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Redirect {
    Follow,
    Error,
    Manual,
}

impl Redirect {
    pub fn as_str(self) -> &'static str {
        match self {
            Redirect::Follow => "follow",
            Redirect::Error => "error",
            Redirect::Manual => "manual",
        }
    }

    pub fn parse(s: &str) -> Option<Redirect> {
        Some(match s {
            "follow" => Redirect::Follow,
            "error" => Redirect::Error,
            "manual" => Redirect::Manual,
            _ => return None,
        })
    }
}

/// A request's response tainting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tainting {
    Basic,
    Cors,
    Opaque,
}

/// A response's type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResponseType {
    Basic,
    Cors,
    Default,
    Error,
    Opaque,
    OpaqueRedirect,
}

impl ResponseType {
    pub fn as_str(self) -> &'static str {
        match self {
            ResponseType::Basic => "basic",
            ResponseType::Cors => "cors",
            ResponseType::Default => "default",
            ResponseType::Error => "error",
            ResponseType::Opaque => "opaque",
            ResponseType::OpaqueRedirect => "opaqueredirect",
        }
    }
}

/// A request's referrer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Referrer {
    NoReferrer,
    Client,
    Url(Url),
}

/// A request as the fetch algorithm sees it.
#[derive(Clone, Debug)]
pub struct FetchRequest {
    pub method: String,
    /// The URL list: the last one is the current URL.
    pub url_list: Vec<Url>,
    pub headers: HeaderList,
    pub body: Option<Vec<u8>>,
    pub mode: Mode,
    pub credentials: Credentials,
    pub redirect: Redirect,
    pub referrer: Referrer,
    /// The referrer policy (`""` is the default,
    /// `strict-origin-when-cross-origin`).
    pub referrer_policy: String,
    /// The origin of the client (the document that fetches).
    pub origin: Origin,
    /// The URL of the client, the referrer of `Referrer::Client`.
    pub client_url: Option<Url>,
    pub use_cors_preflight: bool,
    pub tainting: Tainting,
    pub tainted_origin: bool,
    pub redirect_count: u32,
    /// The document whose networking tasks deliver the events.
    pub document: Option<DocRef>,
}

impl FetchRequest {
    /// A `GET` request of a client with `origin` at `client_url`.
    pub fn new(url: Url, origin: Origin, client_url: Option<Url>) -> FetchRequest {
        FetchRequest {
            method: "GET".to_string(),
            url_list: vec![url],
            headers: Vec::new(),
            body: None,
            mode: Mode::Cors,
            credentials: Credentials::SameOrigin,
            redirect: Redirect::Follow,
            referrer: Referrer::Client,
            referrer_policy: String::new(),
            origin,
            client_url,
            use_cors_preflight: false,
            tainting: Tainting::Basic,
            tainted_origin: false,
            redirect_count: 0,
            document: None,
        }
    }

    pub fn current_url(&self) -> &Url {
        self.url_list.last().expect("a URL list is never empty")
    }

    /// The serialized origin for `Origin` headers and the CORS check.
    fn serialized_origin(&self) -> String {
        if self.tainted_origin {
            "null".to_string()
        } else {
            self.origin.serialize()
        }
    }

    /// Whether cookies go with the request and come from its response.
    fn includes_credentials(&self) -> bool {
        match self.credentials {
            Credentials::Include => true,
            Credentials::SameOrigin => self.tainting == Tainting::Basic,
            Credentials::Omit => false,
        }
    }
}

/// The head of a (filtered) response.
#[derive(Clone, Debug)]
pub struct ResponseHead {
    pub kind: ResponseType,
    pub url_list: Vec<Url>,
    pub status: u16,
    pub status_text: String,
    pub headers: HeaderList,
    /// False for responses whose body is null (opaque ones).
    pub has_body: bool,
}

/// What a fetch reports: `Head`, then `Data`…, then `Done`; or `Failed`
/// (a network error) at any point.
#[derive(Clone, Debug)]
pub enum Event {
    Head(ResponseHead),
    Data(Vec<u8>),
    Done,
    Failed(String),
}

fn is_http(url: &Url) -> bool {
    matches!(url.scheme.as_str(), "http" | "https")
}

fn is_redirect_status(status: u16) -> bool {
    matches!(status, 301 | 302 | 303 | 307 | 308)
}

/// A "potentially trustworthy" URL for the referrer policies: `https`,
/// or a loopback host.
fn is_trustworthy(url: &Url) -> bool {
    if url.scheme == "https" || url.scheme == "file" {
        return true;
    }
    let host = url.host.as_ref().map(|h| h.to_string()).unwrap_or_default();
    host == "localhost"
        || host.ends_with(".localhost")
        || host.starts_with("127.")
        || host == "[::1]"
}

/// The `Referer` value of `req` (Referrer Policy §8.3).
pub fn referrer_value(req: &FetchRequest) -> Option<String> {
    let source = match &req.referrer {
        Referrer::NoReferrer => return None,
        Referrer::Client => req.client_url.clone()?,
        Referrer::Url(u) => u.clone(),
    };
    if !is_http(&source) {
        return None;
    }
    let mut stripped = source.clone();
    stripped.username.clear();
    stripped.password.clear();
    stripped.fragment = None;
    let full = stripped.serialize(true);
    let origin_only = format!("{}/", source.origin().serialize());
    let full = if full.len() > 4096 {
        origin_only.clone()
    } else {
        full
    };
    let target = req.current_url();
    let same = source.origin().same_origin(&target.origin());
    let downgrade = source.scheme == "https" && !is_trustworthy(target);
    match req.referrer_policy.as_str() {
        "no-referrer" => None,
        "origin" => Some(origin_only),
        "unsafe-url" => Some(full),
        "same-origin" => same.then_some(full),
        "strict-origin" => (!downgrade).then_some(origin_only),
        "origin-when-cross-origin" => Some(if same { full } else { origin_only }),
        "no-referrer-when-downgrade" => (!downgrade).then_some(full),
        _ => {
            if same {
                Some(full)
            } else if downgrade {
                None
            } else {
                Some(origin_only)
            }
        }
    }
}

/// The CORS check (Fetch §4.9) of a response with `headers`.
fn cors_check(req: &FetchRequest, headers: &HeaderList) -> bool {
    let Some(origin) = headers::get(headers, "access-control-allow-origin") else {
        return false;
    };
    if req.credentials != Credentials::Include && origin == "*" {
        return true;
    }
    if origin != req.serialized_origin() {
        return false;
    }
    if req.credentials != Credentials::Include {
        return true;
    }
    headers::get(headers, "access-control-allow-credentials").as_deref() == Some("true")
}

/// How main fetch goes on for the current URL.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    /// A scheme fetch (`about:`, `blob:`, `data:`, `file:`).
    Scheme,
    /// An HTTP fetch, after a CORS preflight if `preflight`.
    Http { preflight: bool },
}

/// Main fetch's choice for the current URL (Fetch §4.1, step 12); it may
/// set the response tainting.
pub fn decide(req: &mut FetchRequest) -> Result<Route, String> {
    let url = req.current_url().clone();
    let plain = |url: &Url| {
        if is_http(url) {
            Route::Http { preflight: false }
        } else {
            Route::Scheme
        }
    };
    let file_to_file =
        url.scheme == "file" && req.client_url.as_ref().is_some_and(|c| c.scheme == "file");
    let same = req.origin.same_origin(&url.origin()) || file_to_file;
    if (same && req.tainting == Tainting::Basic)
        || url.scheme == "data"
        || req.mode == Mode::Navigate
    {
        return Ok(plain(&url));
    }
    match req.mode {
        Mode::SameOrigin => Err("a cross-origin request in same-origin mode".to_string()),
        Mode::NoCors => {
            if req.redirect != Redirect::Follow {
                return Err("a no-cors request must follow redirects".to_string());
            }
            req.tainting = Tainting::Opaque;
            Ok(plain(&url))
        }
        _ => {
            if !is_http(&url) {
                return Err(format!("the {} scheme does not support CORS", url.scheme));
            }
            req.tainting = Tainting::Cors;
            let preflight = req.use_cors_preflight
                || !headers::is_cors_safelisted_method(&req.method)
                || !headers::cors_unsafe_names(&req.headers).is_empty();
            Ok(Route::Http { preflight })
        }
    }
}

/// The CORS preflight request of `req` (Fetch §4.8).
pub fn preflight_request(req: &FetchRequest) -> NetRequest {
    let mut h: HeaderList = vec![
        ("Accept".to_string(), "*/*".to_string()),
        (
            "Access-Control-Request-Method".to_string(),
            req.method.clone(),
        ),
    ];
    let names = headers::cors_unsafe_names(&req.headers);
    if !names.is_empty() {
        h.push((
            "Access-Control-Request-Headers".to_string(),
            names.join(","),
        ));
    }
    h.push(("Origin".to_string(), req.serialized_origin()));
    if let Some(r) = referrer_value(req) {
        h.push(("Referer".to_string(), r));
    }
    NetRequest {
        method: "OPTIONS".to_string(),
        headers: h,
        body: Vec::new(),
        manual_redirects: true,
    }
}

/// Whether the preflight response with `info` allows `req`.
pub fn check_preflight(req: &FetchRequest, info: &ResponseInfo) -> Result<(), String> {
    if !cors_check(req, &info.headers) {
        return Err("the CORS preflight was not allowed".to_string());
    }
    if !(200..300).contains(&info.status) {
        return Err(format!("the CORS preflight answered {}", info.status));
    }
    let wildcard_ok = req.credentials != Credentials::Include;
    let methods = headers::get(&info.headers, "access-control-allow-methods")
        .map(|v| headers::split_values(&v))
        .unwrap_or_default();
    let allowed_headers = headers::get(&info.headers, "access-control-allow-headers")
        .map(|v| headers::split_values(&v))
        .unwrap_or_default();
    let method_ok = methods.iter().any(|m| *m == req.method)
        || headers::is_cors_safelisted_method(&req.method)
        || (wildcard_ok && methods.iter().any(|m| m == "*"));
    if !method_ok {
        return Err(format!("the method {} is not allowed by CORS", req.method));
    }
    for name in headers::cors_unsafe_names(&req.headers) {
        let listed = allowed_headers
            .iter()
            .any(|h| h.eq_ignore_ascii_case(&name));
        let star =
            wildcard_ok && name != "authorization" && allowed_headers.iter().any(|h| h == "*");
        if !listed && !star {
            return Err(format!("the header {name} is not allowed by CORS"));
        }
    }
    Ok(())
}

/// The request of an HTTP-network-or-cache fetch: `req`'s headers with
/// `Origin`, `Referer` and `Cookie`.
pub fn http_request(ua: &UserAgent, req: &FetchRequest) -> NetRequest {
    let mut h = req.headers.clone();
    let cors_like = req.tainting == Tainting::Cors;
    if cors_like || !matches!(req.method.as_str(), "GET" | "HEAD") {
        headers::set(&mut h, "Origin", &req.serialized_origin());
    }
    if let Some(r) = referrer_value(req) {
        headers::set(&mut h, "Referer", &r);
    }
    if req.includes_credentials() {
        if let Some(c) = ua.fetch.cookies.borrow_mut().header_for(req.current_url()) {
            headers::set(&mut h, "Cookie", &c);
        }
    }
    NetRequest {
        method: req.method.clone(),
        headers: h,
        body: req.body.clone().unwrap_or_default(),
        manual_redirects: true,
    }
}

/// What a response head means for the fetch.
#[derive(Debug)]
pub enum HopOutcome {
    /// The request now has a new current URL: run main fetch again.
    Redirect,
    Final(ResponseHead),
    Error(String),
}

/// The filtered head of the final response `info`.
fn filtered(req: &FetchRequest, info: &ResponseInfo, local: bool) -> ResponseHead {
    let url_list = req.url_list.clone();
    let without_cookies: HeaderList = info
        .headers
        .iter()
        .filter(|(n, _)| !headers::is_forbidden_response_header_name(n))
        .cloned()
        .collect();
    let (kind, headers, status, status_text, has_body) = match req.tainting {
        _ if local && req.tainting != Tainting::Opaque => (
            ResponseType::Basic,
            without_cookies,
            info.status,
            info.status_text.clone(),
            true,
        ),
        Tainting::Basic => (
            ResponseType::Basic,
            without_cookies,
            info.status,
            info.status_text.clone(),
            true,
        ),
        Tainting::Cors => {
            let exposed = headers::get(&info.headers, "access-control-expose-headers")
                .map(|v| headers::split_values(&v))
                .unwrap_or_default();
            let all = req.credentials != Credentials::Include && exposed.iter().any(|e| e == "*");
            let list = without_cookies
                .into_iter()
                .filter(|(n, _)| {
                    all || headers::is_cors_safelisted_response_header_name(n, &exposed)
                })
                .collect();
            (
                ResponseType::Cors,
                list,
                info.status,
                info.status_text.clone(),
                true,
            )
        }
        Tainting::Opaque => (ResponseType::Opaque, Vec::new(), 0, String::new(), false),
    };
    let url_list = if kind == ResponseType::Opaque {
        Vec::new()
    } else {
        url_list
    };
    ResponseHead {
        kind,
        url_list,
        status,
        status_text,
        headers,
        has_body,
    }
}

/// Follows a redirect to `location` (HTTP-redirect fetch, Fetch §4.4).
fn follow_redirect(req: &mut FetchRequest, status: u16, location: &str) -> Result<(), String> {
    let current = req.current_url().clone();
    let next =
        Url::parse(location, Some(&current)).map_err(|_| "an invalid redirect URL".to_string())?;
    if !is_http(&next) {
        return Err("a redirect to a URL that is not HTTP(S)".to_string());
    }
    if req.redirect_count >= MAX_REDIRECTS {
        return Err("too many redirects".to_string());
    }
    req.redirect_count += 1;
    let next_origin = next.origin();
    if req.mode == Mode::Cors
        && next.includes_credentials()
        && !req.origin.same_origin(&next_origin)
    {
        return Err("a CORS redirect to a URL with credentials".to_string());
    }
    if req.tainting == Tainting::Cors && next.includes_credentials() {
        return Err("a CORS redirect to a URL with credentials".to_string());
    }
    let current_origin = current.origin();
    if !current_origin.same_origin(&next_origin) && !req.origin.same_origin(&current_origin) {
        req.tainted_origin = true;
    }
    if ((status == 301 || status == 302) && req.method == "POST")
        || (status == 303 && !matches!(req.method.as_str(), "GET" | "HEAD"))
    {
        req.method = "GET".to_string();
        req.body = None;
        for name in [
            "content-encoding",
            "content-language",
            "content-location",
            "content-type",
        ] {
            headers::delete(&mut req.headers, name);
        }
    }
    if !current_origin.same_origin(&next_origin) {
        headers::delete(&mut req.headers, "authorization");
    }
    req.url_list.push(next);
    Ok(())
}

/// What the response head `info` of the current hop means (HTTP fetch,
/// Fetch §4.3, from step 4).
pub fn process_head(ua: &UserAgent, req: &mut FetchRequest, info: &ResponseInfo) -> HopOutcome {
    if req.includes_credentials() {
        let mut jar = ua.fetch.cookies.borrow_mut();
        for (n, v) in &info.headers {
            if n.eq_ignore_ascii_case("set-cookie") {
                jar.set_from_header(req.current_url(), v);
            }
        }
    }
    if req.tainting == Tainting::Cors && !cors_check(req, &info.headers) {
        return HopOutcome::Error("the CORS check failed".to_string());
    }
    let location = headers::get(&info.headers, "location");
    if is_redirect_status(info.status) {
        if let Some(location) = location {
            match req.redirect {
                Redirect::Error => {
                    return HopOutcome::Error("a redirect with redirect mode error".to_string());
                }
                Redirect::Manual => {
                    if req.mode == Mode::Navigate {
                        return HopOutcome::Final(filtered(req, info, false));
                    }
                    return HopOutcome::Final(ResponseHead {
                        kind: ResponseType::OpaqueRedirect,
                        url_list: req.url_list.clone(),
                        status: 0,
                        status_text: String::new(),
                        headers: Vec::new(),
                        has_body: false,
                    });
                }
                Redirect::Follow => {
                    return match follow_redirect(req, info.status, &location) {
                        Ok(()) => HopOutcome::Redirect,
                        Err(e) => HopOutcome::Error(e),
                    };
                }
            }
        }
    }
    HopOutcome::Final(filtered(req, info, false))
}

// ----- the asynchronous driver -------------------------------------------------------------

/// What hears the events of a fetch.
pub type Sink = Box<dyn FnMut(&mut Agent, Event)>;

struct Driver {
    req: FetchRequest,
    /// The loader request of the current hop.
    current: Option<u64>,
    /// Bumped at each hop: events of older hops are ignored.
    hop: u64,
    /// The final head was reported; body events go to the sink.
    streaming: bool,
    has_body: bool,
    finished: bool,
}

/// A running fetch.
#[derive(Clone)]
pub struct FetchHandle {
    driver: Rc<RefCell<Driver>>,
    sink: Rc<RefCell<Option<Sink>>>,
    aborted: Rc<Cell<bool>>,
}

impl FetchHandle {
    /// Stops the fetch: the sink hears nothing more.
    pub fn abort(&self) {
        self.aborted.set(true);
        let current = {
            let mut d = self.driver.borrow_mut();
            d.finished = true;
            d.current.take()
        };
        *self.sink.borrow_mut() = None;
        if let (Some(id), Some(ua)) = (current, crate::browser::ua()) {
            loader::cancel(&ua, id);
        }
    }

    /// The URL list so far.
    pub fn url_list(&self) -> Vec<Url> {
        self.driver.borrow().req.url_list.clone()
    }
}

impl FetchHandle {
    fn emit(&self, agent: &mut Agent, event: Event) {
        if self.aborted.get() {
            return;
        }
        let last = matches!(event, Event::Done | Event::Failed(_));
        if last {
            let mut d = self.driver.borrow_mut();
            if d.finished {
                return;
            }
            d.finished = true;
            d.current = None;
        }
        let taken = self.sink.borrow_mut().take();
        if let Some(mut f) = taken {
            f(agent, event);
            if !last && !self.aborted.get() {
                let mut slot = self.sink.borrow_mut();
                if slot.is_none() {
                    *slot = Some(f);
                }
            }
        }
    }

    /// Reports a network error from a task (never synchronously).
    fn fail_soon(&self, message: String) {
        let h = self.clone();
        let doc = self.driver.borrow().req.document.clone();
        crate::browser::event_loop::queue_task(
            crate::browser::TaskSource::Networking,
            doc,
            move |agent| h.emit(agent, Event::Failed(message)),
        );
    }
}

/// Starts fetching `req`; `sink` hears the events as networking tasks.
pub fn start(ua: &UserAgent, req: FetchRequest, sink: Sink) -> FetchHandle {
    let handle = FetchHandle {
        driver: Rc::new(RefCell::new(Driver {
            req,
            current: None,
            hop: 0,
            streaming: false,
            has_body: true,
            finished: false,
        })),
        sink: Rc::new(RefCell::new(Some(sink))),
        aborted: Rc::new(Cell::new(false)),
    };
    main_fetch(ua, &handle);
    handle
}

fn main_fetch(ua: &UserAgent, h: &FetchHandle) {
    let route = {
        let mut d = h.driver.borrow_mut();
        d.hop += 1;
        decide(&mut d.req)
    };
    match route {
        Err(e) => h.fail_soon(e),
        Ok(Route::Scheme) => scheme_fetch(ua, h),
        Ok(Route::Http { preflight: true }) => preflight(ua, h),
        Ok(Route::Http { preflight: false }) => http_fetch(ua, h),
    }
}

/// Issues one loader request of the current hop.
fn issue(
    ua: &UserAgent,
    h: &FetchHandle,
    request: NetRequest,
    mut on_event: impl FnMut(&mut Agent, &FetchHandle, FetchEvent) + 'static,
) {
    let (url, doc, hop) = {
        let d = h.driver.borrow();
        (d.req.current_url().clone(), d.req.document.clone(), d.hop)
    };
    let handle = h.clone();
    let id = loader::fetch_request(
        ua,
        url,
        request,
        doc,
        Box::new(move |agent, event| {
            if handle.aborted.get() || handle.driver.borrow().hop != hop {
                return;
            }
            on_event(agent, &handle, event);
        }),
    );
    h.driver.borrow_mut().current = Some(id);
}

fn scheme_fetch(ua: &UserAgent, h: &FetchHandle) {
    let method = h.driver.borrow().req.method.clone();
    let request = NetRequest {
        method,
        headers: Vec::new(),
        body: Vec::new(),
        manual_redirects: true,
    };
    issue(ua, h, request, |agent, h, event| match event {
        FetchEvent::Head(info) => {
            let head = filtered(&h.driver.borrow().req, &info, true);
            {
                let mut d = h.driver.borrow_mut();
                d.streaming = true;
                d.has_body = head.has_body;
            }
            h.emit(agent, Event::Head(head));
        }
        other => body_event(agent, h, other),
    });
}

/// Forwards a body event of the final response.
fn body_event(agent: &mut Agent, h: &FetchHandle, event: FetchEvent) {
    let (streaming, has_body) = {
        let d = h.driver.borrow();
        (d.streaming, d.has_body)
    };
    match event {
        FetchEvent::Data(bytes) => {
            if streaming && has_body {
                h.emit(agent, Event::Data(bytes));
            }
        }
        FetchEvent::Done => h.emit(agent, Event::Done),
        FetchEvent::Failed(e) => h.emit(agent, Event::Failed(e)),
        FetchEvent::Head(_) => {}
    }
}

fn preflight(ua: &UserAgent, h: &FetchHandle) {
    let request = preflight_request(&h.driver.borrow().req);
    let mut checked = false;
    issue(ua, h, request, move |agent, h, event| {
        if checked {
            return;
        }
        match event {
            FetchEvent::Head(info) => {
                checked = true;
                if let (Some(id), Some(ua)) =
                    (h.driver.borrow_mut().current.take(), crate::browser::ua())
                {
                    loader::cancel(&ua, id);
                }
                let r = check_preflight(&h.driver.borrow().req, &info);
                match r {
                    Ok(()) => {
                        if let Some(ua) = crate::browser::ua() {
                            h.driver.borrow_mut().hop += 1;
                            http_fetch(&ua, h);
                        }
                    }
                    Err(e) => h.emit(agent, Event::Failed(e)),
                }
            }
            FetchEvent::Failed(e) => h.emit(agent, Event::Failed(e)),
            _ => {}
        }
    });
}

fn http_fetch(ua: &UserAgent, h: &FetchHandle) {
    let request = http_request(ua, &h.driver.borrow().req);
    issue(ua, h, request, |agent, h, event| match event {
        FetchEvent::Head(info) => {
            let Some(ua) = crate::browser::ua() else {
                return;
            };
            let outcome = process_head(&ua, &mut h.driver.borrow_mut().req, &info);
            match outcome {
                HopOutcome::Redirect => {
                    let current = h.driver.borrow_mut().current.take();
                    if let Some(id) = current {
                        loader::cancel(&ua, id);
                    }
                    main_fetch(&ua, h);
                }
                HopOutcome::Final(head) => {
                    {
                        let mut d = h.driver.borrow_mut();
                        d.streaming = true;
                        d.has_body = head.has_body;
                    }
                    h.emit(agent, Event::Head(head));
                }
                HopOutcome::Error(e) => {
                    let current = h.driver.borrow_mut().current.take();
                    if let Some(id) = current {
                        loader::cancel(&ua, id);
                    }
                    h.emit(agent, Event::Failed(e));
                }
            }
        }
        other => body_event(agent, h, other),
    });
}

// ----- the synchronous driver ------------------------------------------------------------

/// One blocking HTTP request.
fn http_sync(
    ua: &UserAgent,
    url: &Url,
    net: &NetRequest,
) -> Result<(ResponseInfo, Vec<u8>), String> {
    let request = loader::http_request(&ua.options.user_agent, url, Some(net))?;
    let mut client = vnet::http::Client::from_env();
    let _ = client.enable_https(ua.options.ca_cert.as_deref());
    let response = client.fetch(&request).map_err(|e| e.to_string())?;
    let head = response.head;
    let info = ResponseInfo {
        url: url.clone(),
        status: head.status,
        status_text: head.reason.clone(),
        content_type: head.headers.get("content-type").map(str::to_string),
        headers: head
            .headers
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
    };
    Ok((info, response.body))
}

/// Fetches `req` blocking the thread: the final head and the whole body,
/// or the network error.
pub fn fetch_sync(
    ua: &UserAgent,
    mut req: FetchRequest,
) -> Result<(ResponseHead, Vec<u8>), String> {
    loop {
        match decide(&mut req)? {
            Route::Scheme => {
                let (info, body) = loader::local_fetch(req.current_url(), &req.method)?;
                let head = filtered(&req, &info, true);
                let body = if head.has_body { body } else { Vec::new() };
                return Ok((head, body));
            }
            Route::Http { preflight } => {
                let url = req.current_url().clone();
                if preflight {
                    let (info, _) = http_sync(ua, &url, &preflight_request(&req))?;
                    check_preflight(&req, &info)?;
                }
                let (info, body) = http_sync(ua, &url, &http_request(ua, &req))?;
                match process_head(ua, &mut req, &info) {
                    HopOutcome::Redirect => continue,
                    HopOutcome::Final(head) => {
                        let body = if head.has_body { body } else { Vec::new() };
                        return Ok((head, body));
                    }
                    HopOutcome::Error(e) => return Err(e),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(s: &str) -> Url {
        Url::parse(s, None).unwrap()
    }

    fn request(target: &str, client: &str) -> FetchRequest {
        let c = url(client);
        FetchRequest::new(url(target), c.origin(), Some(c))
    }

    fn info(status: u16, headers: &[(&str, &str)]) -> ResponseInfo {
        ResponseInfo {
            url: url("http://x.test/"),
            status,
            status_text: String::new(),
            content_type: None,
            headers: headers
                .iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect(),
        }
    }

    #[test]
    fn modes_choose_routes_and_tainting() {
        let mut r = request("http://a.test/x", "http://a.test/");
        assert_eq!(decide(&mut r), Ok(Route::Http { preflight: false }));
        assert_eq!(r.tainting, Tainting::Basic);

        let mut r = request("http://b.test/x", "http://a.test/");
        assert_eq!(decide(&mut r), Ok(Route::Http { preflight: false }));
        assert_eq!(r.tainting, Tainting::Cors);

        let mut r = request("http://b.test/x", "http://a.test/");
        r.method = "PUT".to_string();
        assert_eq!(decide(&mut r), Ok(Route::Http { preflight: true }));

        let mut r = request("http://b.test/x", "http://a.test/");
        r.headers.push(("X-Test".to_string(), "1".to_string()));
        assert_eq!(decide(&mut r), Ok(Route::Http { preflight: true }));

        let mut r = request("http://b.test/x", "http://a.test/");
        r.mode = Mode::SameOrigin;
        assert!(decide(&mut r).is_err());

        let mut r = request("http://b.test/x", "http://a.test/");
        r.mode = Mode::NoCors;
        assert_eq!(decide(&mut r), Ok(Route::Http { preflight: false }));
        assert_eq!(r.tainting, Tainting::Opaque);

        let mut r = request("data:,x", "http://a.test/");
        assert_eq!(decide(&mut r), Ok(Route::Scheme));
    }

    #[test]
    fn cors_check_and_filtering() {
        let ua_less = |r: &FetchRequest, i: &ResponseInfo| cors_check(r, &i.headers);
        let mut r = request("http://b.test/x", "http://a.test/");
        decide(&mut r).unwrap();
        assert!(!ua_less(&r, &info(200, &[])));
        assert!(ua_less(
            &r,
            &info(200, &[("Access-Control-Allow-Origin", "*")])
        ));
        assert!(ua_less(
            &r,
            &info(200, &[("Access-Control-Allow-Origin", "http://a.test")])
        ));
        r.credentials = Credentials::Include;
        assert!(!ua_less(
            &r,
            &info(200, &[("Access-Control-Allow-Origin", "*")])
        ));
        let i = info(
            200,
            &[
                ("Access-Control-Allow-Origin", "http://a.test"),
                ("Access-Control-Allow-Credentials", "true"),
                ("X-Secret", "1"),
                ("X-Exposed", "2"),
                ("Access-Control-Expose-Headers", "X-Exposed"),
                ("Content-Type", "text/plain"),
            ],
        );
        assert!(ua_less(&r, &i));
        let head = filtered(&r, &i, false);
        assert_eq!(head.kind, ResponseType::Cors);
        let names: Vec<&str> = head.headers.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["X-Exposed", "Content-Type"]);
    }

    #[test]
    fn preflight_requests_and_checks() {
        let mut r = request("http://b.test/x", "http://a.test/");
        r.method = "DELETE".to_string();
        r.headers.push(("X-B".to_string(), "1".to_string()));
        r.headers.push(("X-A".to_string(), "1".to_string()));
        decide(&mut r).unwrap();
        let p = preflight_request(&r);
        assert_eq!(p.method, "OPTIONS");
        assert_eq!(
            headers::get(&p.headers, "access-control-request-headers").as_deref(),
            Some("x-a,x-b")
        );
        let ok = info(
            204,
            &[
                ("Access-Control-Allow-Origin", "http://a.test"),
                ("Access-Control-Allow-Methods", "DELETE"),
                ("Access-Control-Allow-Headers", "X-A, x-b"),
            ],
        );
        assert!(check_preflight(&r, &ok).is_ok());
        let missing = info(
            204,
            &[
                ("Access-Control-Allow-Origin", "http://a.test"),
                ("Access-Control-Allow-Methods", "DELETE"),
            ],
        );
        assert!(check_preflight(&r, &missing).is_err());
    }

    #[test]
    fn redirects_change_method_and_taint_origin() {
        let mut r = request("http://a.test/x", "http://a.test/");
        r.method = "POST".to_string();
        r.body = Some(b"x".to_vec());
        r.headers
            .push(("Content-Type".to_string(), "text/plain".to_string()));
        follow_redirect(&mut r, 303, "http://b.test/y").unwrap();
        assert_eq!(r.method, "GET");
        assert!(r.body.is_none());
        assert!(!headers::contains(&r.headers, "content-type"));
        assert_eq!(r.url_list.len(), 2);
        follow_redirect(&mut r, 307, "http://c.test/z").unwrap();
        assert!(r.tainted_origin);
        assert!(follow_redirect(&mut r, 302, "data:,x").is_err());
    }

    #[test]
    fn referrer_policies() {
        let mut r = request("http://b.test/x", "http://a.test/page?q#f");
        assert_eq!(referrer_value(&r).as_deref(), Some("http://a.test/"));
        r.url_list = vec![url("http://a.test/other")];
        assert_eq!(referrer_value(&r).as_deref(), Some("http://a.test/page?q"));
        r.referrer_policy = "no-referrer".to_string();
        assert_eq!(referrer_value(&r), None);
        let mut s = request("http://b.test/x", "https://a.test/");
        assert_eq!(referrer_value(&s), None);
        s.referrer_policy = "unsafe-url".to_string();
        assert_eq!(referrer_value(&s).as_deref(), Some("https://a.test/"));
    }
}
