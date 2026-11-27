//! The WebDriver server of vidrio (W3C WebDriver classic, JSON over
//! HTTP/1.1) with the subset of vault/CONTRACT.md §4: what the generic
//! WebDriver executors of wptrunner use.
//!
//! The HTTP side runs on its own thread ([`http`]); every command runs on
//! the thread that owns the [`Browser`] ([`run`]), between turns of the
//! event loop. Commands that wait (navigation with the `normal` page load
//! strategy, scripts that return a promise or use the asynchronous
//! callback, element searches with an implicit wait) leave a [`Pending`]
//! that the loop checks between tasks; the server waits for the answer,
//! so there is at most one.
//!
//! Window handles name top-level browsing contexts; the current browsing
//! context of the session is its current top-level context or a frame
//! inside it. Errors carry the codes and statuses of WebDriver §6.6
//! ([`error`]); any command outside the subset is `unknown command`.

pub mod error;
pub mod http;
pub mod input;
pub mod json;
pub mod script;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{RecvTimeoutError, Sender, TryRecvError};
use std::time::{Duration, Instant};

use vjs::Agent;
use vjs::gc::{Realm, RootSetId, Trace};

use crate::browser::document::Readiness;
use crate::browser::navigable::{self, NavigateParams};
use crate::browser::{Browser, ContextId, ua};
use crate::dom::NodeId;
use crate::paint::{Painter, VIEWPORT_HEIGHT, VIEWPORT_WIDTH};
use crate::url::Url;
use error::{ErrorCode, WdError, WdResult, invalid_argument};
use http::{Commands, Message, Response};
use json::Json;
use script::{ElementStore, ScriptOutcome, handle_of};

/// The largest integer a timeout may be (2^53 − 1).
const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

/// The page load strategy of a session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageLoadStrategy {
    None,
    Eager,
    Normal,
}

impl PageLoadStrategy {
    fn name(self) -> &'static str {
        match self {
            PageLoadStrategy::None => "none",
            PageLoadStrategy::Eager => "eager",
            PageLoadStrategy::Normal => "normal",
        }
    }
}

/// The timeouts of a session, in milliseconds (`None` for no script
/// timeout).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Timeouts {
    pub script: Option<u64>,
    pub page_load: u64,
    pub implicit: u64,
}

impl Default for Timeouts {
    fn default() -> Timeouts {
        Timeouts {
            script: Some(30_000),
            page_load: 300_000,
            implicit: 0,
        }
    }
}

impl Timeouts {
    fn to_json(self) -> Json {
        Json::object([
            (
                "script",
                self.script.map_or(Json::Null, |s| Json::Number(s as f64)),
            ),
            ("pageLoad", Json::Number(self.page_load as f64)),
            ("implicit", Json::Number(self.implicit as f64)),
        ])
    }

    /// Applies the members of a timeouts object (WebDriver §9 "JSON
    /// deserialization" of timeouts).
    fn update(&mut self, v: &Json) -> WdResult<()> {
        let Some(members) = v.as_object() else {
            return invalid_argument("timeouts must be an object");
        };
        let mut next = *self;
        for (k, value) in members {
            let ms = match value {
                Json::Null if k == "script" => None,
                Json::Number(n) if *n >= 0.0 && *n <= MAX_SAFE_INTEGER && n.fract() == 0.0 => {
                    Some(*n as u64)
                }
                _ => return invalid_argument(&format!("invalid value for timeout {k}")),
            };
            match k.as_str() {
                "script" => next.script = ms,
                "pageLoad" => next.page_load = ms.unwrap_or(0),
                "implicit" => next.implicit = ms.unwrap_or(0),
                _ => return invalid_argument(&format!("unknown timeout {k}")),
            }
        }
        *self = next;
        Ok(())
    }
}

/// A WebDriver session.
pub struct Session {
    pub id: String,
    pub timeouts: Timeouts,
    pub page_load: PageLoadStrategy,
    /// The current top-level browsing context.
    pub top: ContextId,
    /// The current browsing context, if it is a frame inside `top`.
    pub frame: Option<ContextId>,
    pub elements: Rc<RefCell<ElementStore>>,
    pub input: input::InputState,
}

impl Session {
    /// The current browsing context.
    pub fn current(&self) -> ContextId {
        self.frame.unwrap_or(self.top)
    }
}

/// What a pending command waits for.
enum Wait {
    /// A navigation of the context to finish loading.
    Load { ctx: ContextId },
    /// A history traversal to reach entry `index`, then load.
    Traverse { ctx: ContextId, index: usize },
    /// A script call to settle while the window keeps `realm`.
    Script {
        ctx: ContextId,
        realm: Realm,
        outcome: Rc<ScriptOutcome>,
        roots: RootSetId,
    },
    /// An element to match `selector` in the current browsing context.
    Find { selector: String },
}

/// A command waiting for the event loop.
pub struct Pending {
    wait: Wait,
    deadline: Option<Instant>,
    reply: Sender<Response>,
}

/// What a command handler gives back.
enum Outcome {
    Done(WdResult<Json>),
    Wait(Wait, Option<Instant>),
}

fn done(v: Json) -> WdResult<Outcome> {
    Ok(Outcome::Done(Ok(v)))
}

/// Values rooted while a script waits.
struct Keep(Vec<vjs::Value>);

impl Trace for Keep {
    fn trace(&self, tracer: &mut vjs::gc::Tracer) {
        for v in &self.0 {
            tracer.value(*v);
        }
    }
}

/// The server state on the main thread.
pub struct Driver {
    painter: Rc<dyn Painter>,
    pub session: Option<Session>,
    pending: Option<Pending>,
    /// Whether the browser runs without a window (reported as a
    /// capability).
    headless: bool,
}

/// Runs the commands that arrive through `commands` on this thread,
/// together with the event loop of `browser`, until the server thread
/// stops.
pub fn run(browser: &mut Browser, commands: Commands, painter: Rc<dyn Painter>, headless: bool) {
    let mut d = Driver::new(painter, headless);
    loop {
        if d.pending.is_none() {
            match commands.rx.try_recv() {
                Ok(m) => {
                    commands.wake.store(false, Ordering::SeqCst);
                    d.handle(browser, m);
                    continue;
                }
                Err(TryRecvError::Disconnected) => break,
                Err(TryRecvError::Empty) => {}
            }
        }
        d.poll_pending(browser);
        if d.pending.is_some() {
            let limit = Duration::from_millis(10);
            browser.run_until(limit, |agent| d.pending_ready(agent));
            if !d.pending_ready(&mut browser.agent) {
                // Nothing to run (or the loop went idle): do not spin.
                std::thread::sleep(Duration::from_millis(1));
            }
        } else {
            // The loop sleeps until its next timer without looking at
            // `wake`: a short slice keeps a command from waiting for it.
            let wake = commands.wake.clone();
            browser.run_until(Duration::from_millis(1), |_| wake.load(Ordering::SeqCst));
            match commands.rx.recv_timeout(Duration::from_millis(2)) {
                Ok(m) => {
                    commands.wake.store(false, Ordering::SeqCst);
                    d.handle(browser, m);
                }
                Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => {}
            }
        }
    }
    d.cancel_pending(browser);
}

fn respond(reply: &Sender<Response>, r: WdResult<Json>) {
    let (status, value) = match r {
        Ok(v) => (200, v),
        Err(e) => {
            crate::log::info(&format!(
                "webdriver: error {}: {}",
                e.code.name(),
                e.message
            ));
            (e.code.status(), e.to_json())
        }
    };
    let body = Json::Object(vec![("value".to_string(), value)]).serialize();
    let _ = reply.send(Response { status, body });
}

fn no_such_window() -> WdError {
    WdError::new(ErrorCode::NoSuchWindow, "no such window")
}

fn unknown_command(method: &str, path: &str) -> WdError {
    WdError::new(
        ErrorCode::UnknownCommand,
        format!("unknown command: {method} {path}"),
    )
}

/// The commands of the subset (vault/CONTRACT.md §4).
#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    Status,
    NewSession,
    DeleteSession,
    GetTimeouts,
    SetTimeouts,
    NavigateTo,
    GetUrl,
    GetTitle,
    Back,
    Forward,
    Refresh,
    GetWindowHandle,
    SwitchToWindow,
    CloseWindow,
    GetWindowHandles,
    NewWindow,
    GetWindowRect,
    SetWindowRect,
    SwitchToFrame,
    SwitchToParentFrame,
    ExecuteSync,
    ExecuteAsync,
    Screenshot,
    PerformActions,
    ReleaseActions,
    FindElement,
    ElementClick(String),
    ElementSendKeys(String),
    GetCookies,
    DeleteCookies,
}

/// The command and session id of a request, or `None` for an unknown
/// command.
pub fn route(method: &str, path: &str) -> Option<(Command, Option<String>)> {
    let segs: Vec<&str> = path.trim_matches('/').split('/').collect();
    use Command::*;
    match (method, segs.as_slice()) {
        ("GET", ["status"]) => Some((Status, None)),
        ("POST", ["session"]) => Some((NewSession, None)),
        (m, ["session", id, rest @ ..]) if !id.is_empty() => {
            let c = match (m, rest) {
                ("DELETE", []) => DeleteSession,
                ("GET", ["timeouts"]) => GetTimeouts,
                ("POST", ["timeouts"]) => SetTimeouts,
                ("POST", ["url"]) => NavigateTo,
                ("GET", ["url"]) => GetUrl,
                ("GET", ["title"]) => GetTitle,
                ("POST", ["back"]) => Back,
                ("POST", ["forward"]) => Forward,
                ("POST", ["refresh"]) => Refresh,
                ("GET", ["window"]) => GetWindowHandle,
                ("POST", ["window"]) => SwitchToWindow,
                ("DELETE", ["window"]) => CloseWindow,
                ("GET", ["window", "handles"]) => GetWindowHandles,
                ("POST", ["window", "new"]) => NewWindow,
                ("GET", ["window", "rect"]) => GetWindowRect,
                ("POST", ["window", "rect"]) => SetWindowRect,
                ("POST", ["frame"]) => SwitchToFrame,
                ("POST", ["frame", "parent"]) => SwitchToParentFrame,
                ("POST", ["execute", "sync"]) => ExecuteSync,
                ("POST", ["execute", "async"]) => ExecuteAsync,
                ("GET", ["screenshot"]) => Screenshot,
                ("POST", ["actions"]) => PerformActions,
                ("DELETE", ["actions"]) => ReleaseActions,
                ("POST", ["element"]) => FindElement,
                ("POST", ["element", e, "click"]) => ElementClick(e.to_string()),
                ("POST", ["element", e, "value"]) => ElementSendKeys(e.to_string()),
                ("GET", ["cookie"]) => GetCookies,
                ("DELETE", ["cookie"]) => DeleteCookies,
                _ => return None,
            };
            Some((c, Some(id.to_string())))
        }
        _ => None,
    }
}

/// A new id with the shape of a UUID.
fn new_uuid() -> String {
    use std::sync::atomic::AtomicU64;
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let mix = t ^ (u64::from(std::process::id()) << 32) ^ n.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    format!(
        "{:08x}-{:04x}-4{:03x}-a{:03x}-{:012x}",
        (mix >> 32) as u32,
        (mix >> 16) & 0xFFFF,
        mix & 0xFFF,
        n & 0xFFF,
        (t ^ n) & 0xFFFF_FFFF_FFFF
    )
}

// ----- capabilities --------------------------------------------------------

/// The capabilities a session was created with.
struct Matched {
    page_load: PageLoadStrategy,
    timeouts: Timeouts,
}

/// Validates one capabilities object (WebDriver §7.2 "validate
/// capabilities"); `null` members are dropped.
fn validate_capabilities(v: &Json) -> WdResult<Vec<(String, Json)>> {
    let Some(members) = v.as_object() else {
        return invalid_argument("capabilities must be an object");
    };
    let mut out = Vec::new();
    for (k, value) in members {
        if value.is_null() {
            continue;
        }
        let ok = match k.as_str() {
            "acceptInsecureCerts"
            | "setWindowRect"
            | "strictFileInteractability"
            | "webSocketUrl" => matches!(value, Json::Bool(_)),
            "browserName" | "browserVersion" | "platformName" => value.as_str().is_some(),
            "pageLoadStrategy" => {
                matches!(value.as_str(), Some("none" | "eager" | "normal"))
            }
            "proxy" => value.as_object().is_some(),
            "timeouts" => Timeouts::default().update(value).is_ok(),
            "unhandledPromptBehavior" => value.as_str().is_some() || value.as_object().is_some(),
            k if k.contains(':') => true,
            _ => false,
        };
        if !ok {
            return invalid_argument(&format!("invalid capability {k}"));
        }
        out.push((k.clone(), value.clone()));
    }
    Ok(out)
}

/// Whether vidrio can give the merged capabilities `caps` (WebDriver
/// §7.2 "match capabilities").
fn match_capabilities(caps: &[(String, Json)]) -> Option<Matched> {
    let mut m = Matched {
        page_load: PageLoadStrategy::Normal,
        timeouts: Timeouts::default(),
    };
    for (k, v) in caps {
        let ok = match k.as_str() {
            "browserName" => v.as_str() == Some("vidrio"),
            "browserVersion" => v.as_str() == Some(env!("CARGO_PKG_VERSION")),
            "platformName" => v.as_str() == Some("linux"),
            // Certificates are always checked; window rects are fixed;
            // there is no BiDi.
            "acceptInsecureCerts" | "setWindowRect" | "webSocketUrl" => *v == Json::Bool(false),
            "strictFileInteractability" => true,
            "pageLoadStrategy" => {
                m.page_load = match v.as_str() {
                    Some("none") => PageLoadStrategy::None,
                    Some("eager") => PageLoadStrategy::Eager,
                    _ => PageLoadStrategy::Normal,
                };
                true
            }
            "proxy" => match v.as_object() {
                Some([]) => true,
                Some(_) => matches!(
                    v.get("proxyType").and_then(Json::as_str),
                    Some("direct" | "system")
                ),
                None => false,
            },
            "timeouts" => m.timeouts.update(v).is_ok(),
            "unhandledPromptBehavior" => true,
            // Extension capabilities of other browsers are ignored.
            _ => true,
        };
        if !ok {
            return None;
        }
    }
    Some(m)
}

/// "Process capabilities" of a New Session request.
fn process_capabilities(body: &Json) -> WdResult<Matched> {
    let Some(caps) = body.get("capabilities") else {
        return invalid_argument("missing capabilities");
    };
    if caps.as_object().is_none() {
        return invalid_argument("capabilities must be an object");
    }
    let always = match caps.get("alwaysMatch") {
        None | Some(Json::Null) => Vec::new(),
        Some(v) => validate_capabilities(v)?,
    };
    let first: Vec<Vec<(String, Json)>> = match caps.get("firstMatch") {
        None | Some(Json::Null) => vec![Vec::new()],
        Some(Json::Array(list)) if !list.is_empty() => {
            let mut out = Vec::new();
            for f in list {
                let v = validate_capabilities(f)?;
                if v.iter().any(|(k, _)| always.iter().any(|(a, _)| a == k)) {
                    return invalid_argument("firstMatch repeats a key of alwaysMatch");
                }
                out.push(v);
            }
            out
        }
        Some(_) => return invalid_argument("firstMatch must be a non-empty array"),
    };
    for f in first {
        let mut merged = always.clone();
        merged.extend(f);
        if let Some(m) = match_capabilities(&merged) {
            return Ok(m);
        }
    }
    Err(WdError::new(
        ErrorCode::SessionNotCreated,
        "no matching capabilities",
    ))
}

// ----- the driver ------------------------------------------------------------

impl Driver {
    pub fn new(painter: Rc<dyn Painter>, headless: bool) -> Driver {
        Driver {
            painter,
            session: None,
            pending: None,
            headless,
        }
    }

    /// Runs one request and answers it (now or once its wait is over).
    pub fn handle(&mut self, browser: &mut Browser, m: Message) {
        let Message { request, reply } = m;
        crate::log::info(&format!("webdriver: {} {}", request.method, request.path));
        let outcome = self.dispatch(browser, &request.method, &request.path, &request.body);
        match outcome {
            Ok(Outcome::Done(r)) => respond(&reply, r),
            Err(e) => respond(&reply, Err(e)),
            Ok(Outcome::Wait(wait, deadline)) => {
                self.pending = Some(Pending {
                    wait,
                    deadline,
                    reply,
                });
                self.poll_pending(browser);
            }
        }
    }

    fn dispatch(
        &mut self,
        browser: &mut Browser,
        method: &str,
        path: &str,
        body: &[u8],
    ) -> WdResult<Outcome> {
        let Some((command, id)) = route(method, path) else {
            return Err(unknown_command(method, path));
        };
        let body = if method == "POST" {
            let text = std::str::from_utf8(body)
                .map_err(|_| WdError::new(ErrorCode::InvalidArgument, "the body is not UTF-8"))?;
            let v = Json::parse(text).map_err(|e| {
                WdError::new(ErrorCode::InvalidArgument, format!("invalid JSON: {e}"))
            })?;
            if v.as_object().is_none() {
                return invalid_argument("the body must be a JSON object");
            }
            v
        } else {
            Json::Object(Vec::new())
        };
        match command {
            Command::Status => {
                let ready = self.session.is_none();
                return done(Json::object([
                    ("ready", Json::Bool(ready)),
                    (
                        "message",
                        Json::str(if ready {
                            "ready for a new session"
                        } else {
                            "a session already exists"
                        }),
                    ),
                ]));
            }
            Command::NewSession => return self.new_session(browser, &body).map(Outcome::Done),
            _ => {}
        }
        let id = id.unwrap_or_default();
        if self.session.as_ref().is_none_or(|s| s.id != id) {
            return Err(WdError::new(
                ErrorCode::InvalidSessionId,
                format!("no session {id}"),
            ));
        }
        self.session_command(browser, command, &body)
    }

    fn new_session(&mut self, browser: &mut Browser, body: &Json) -> WdResult<WdResult<Json>> {
        if self.session.is_some() {
            return Err(WdError::new(
                ErrorCode::SessionNotCreated,
                "only one session at a time",
            ));
        }
        let m = process_capabilities(body)?;
        let top = match browser.top_level_contexts().first() {
            Some(id) => *id,
            None => navigable::create_top_level(&mut browser.agent, None, ""),
        };
        let id = new_uuid();
        let caps = Json::object([
            ("browserName", Json::str("vidrio")),
            ("browserVersion", Json::str(env!("CARGO_PKG_VERSION"))),
            ("platformName", Json::str("linux")),
            ("acceptInsecureCerts", Json::Bool(false)),
            ("pageLoadStrategy", Json::str(m.page_load.name())),
            ("proxy", Json::Object(Vec::new())),
            ("setWindowRect", Json::Bool(false)),
            ("strictFileInteractability", Json::Bool(false)),
            ("timeouts", m.timeouts.to_json()),
            ("unhandledPromptBehavior", Json::str("dismiss and notify")),
            ("vidrio:headless", Json::Bool(self.headless)),
        ]);
        self.session = Some(Session {
            id: id.clone(),
            timeouts: m.timeouts,
            page_load: m.page_load,
            top,
            frame: None,
            elements: Rc::new(RefCell::new(ElementStore::new())),
            input: input::InputState::default(),
        });
        crate::log::info(&format!("webdriver: new session {id}"));
        Ok(Ok(Json::object([
            ("sessionId", Json::String(id)),
            ("capabilities", caps),
        ])))
    }

    fn session(&mut self) -> &mut Session {
        self.session.as_mut().expect("a session")
    }

    /// The current top-level browsing context, if it is still open.
    fn top_context(&mut self) -> WdResult<ContextId> {
        let top = self.session().top;
        let ua = ua().ok_or_else(no_such_window)?;
        match ua.context(top) {
            Some(c) if c.parent.is_none() => Ok(top),
            _ => Err(no_such_window()),
        }
    }

    /// The current browsing context, if it (and its top-level context)
    /// are still open.
    fn current_context(&mut self) -> WdResult<ContextId> {
        self.top_context()?;
        let cur = self.session().current();
        let ua = ua().ok_or_else(no_such_window)?;
        match ua.context(cur) {
            Some(c) if c.active_document().is_some() => Ok(cur),
            _ => Err(no_such_window()),
        }
    }

    fn deadline_after(ms: u64) -> Option<Instant> {
        Instant::now().checked_add(Duration::from_millis(ms))
    }

    /// The wait of a navigation of `ctx` under the session's page load
    /// strategy.
    fn load_wait(&mut self, ctx: ContextId) -> Outcome {
        let s = self.session();
        if s.page_load == PageLoadStrategy::None {
            return Outcome::Done(Ok(Json::Null));
        }
        let deadline = Self::deadline_after(s.timeouts.page_load);
        Outcome::Wait(Wait::Load { ctx }, deadline)
    }

    fn session_command(
        &mut self,
        browser: &mut Browser,
        command: Command,
        body: &Json,
    ) -> WdResult<Outcome> {
        use Command::*;
        match command {
            Status | NewSession => unreachable!("handled before"),
            DeleteSession => {
                self.session = None;
                done(Json::Null)
            }
            GetTimeouts => done(self.session().timeouts.to_json()),
            SetTimeouts => {
                self.session().timeouts.update(body)?;
                done(Json::Null)
            }
            NavigateTo => {
                let ctx = self.top_context()?;
                let Some(url) = body.get("url").and_then(Json::as_str) else {
                    return invalid_argument("url must be a string");
                };
                let Ok(url) = Url::parse(url, None) else {
                    return invalid_argument("invalid URL");
                };
                self.session().frame = None;
                navigable::navigate(&mut browser.agent, ctx, NavigateParams::new(url));
                Ok(self.load_wait(ctx))
            }
            GetUrl => {
                let ctx = self.top_context()?;
                let doc = browser.document(ctx).ok_or_else(no_such_window)?;
                let href = doc.borrow().url.href();
                done(Json::String(href))
            }
            GetTitle => {
                let ctx = self.top_context()?;
                let title = browser.evaluate_to_string(ctx, "document.title");
                done(Json::String(title))
            }
            Back => self.traverse(browser, -1),
            Forward => self.traverse(browser, 1),
            Refresh => {
                let ctx = self.top_context()?;
                self.session().frame = None;
                navigable::traverse_history(&mut browser.agent, ctx, 0);
                Ok(self.load_wait(ctx))
            }
            GetWindowHandle => {
                let ctx = self.top_context()?;
                done(Json::String(handle_of(ctx)))
            }
            SwitchToWindow => {
                let Some(handle) = body.get("handle").and_then(Json::as_str) else {
                    return invalid_argument("handle must be a string");
                };
                let ua = ua().ok_or_else(no_such_window)?;
                let c = script::context_of_handle(handle)
                    .and_then(|id| ua.context(id))
                    .filter(|c| c.parent.is_none())
                    .ok_or_else(no_such_window)?;
                let s = self.session();
                s.top = c.id;
                s.frame = None;
                done(Json::Null)
            }
            CloseWindow => {
                let ctx = self.top_context()?;
                let ua = ua().ok_or_else(no_such_window)?;
                if let Some(c) = ua.context(ctx) {
                    navigable::discard_context(&mut browser.agent, &ua, &c);
                }
                self.session().frame = None;
                let handles = self.handles(browser);
                if handles.as_array().is_some_and(<[Json]>::is_empty) {
                    self.session = None;
                }
                done(handles)
            }
            GetWindowHandles => done(self.handles(browser)),
            NewWindow => {
                match body.get("type") {
                    None | Some(Json::Null) | Some(Json::String(_)) => {}
                    Some(_) => return invalid_argument("type must be a string"),
                }
                self.top_context()?;
                let id = navigable::create_top_level(&mut browser.agent, None, "");
                done(Json::object([
                    ("handle", Json::String(handle_of(id))),
                    ("type", Json::str("tab")),
                ]))
            }
            GetWindowRect => {
                self.top_context()?;
                done(window_rect())
            }
            SetWindowRect => {
                set_window_rect_args(body)?;
                self.top_context()?;
                // The window stays at 800×600 in (0, 0): that is "as close
                // as possible" to any rect for a window of fixed size.
                done(window_rect())
            }
            SwitchToFrame => self
                .switch_to_frame(body)
                .map(|_| Outcome::Done(Ok(Json::Null))),
            SwitchToParentFrame => {
                let cur = self.current_context()?;
                let ua = ua().ok_or_else(no_such_window)?;
                let parent = ua.context(cur).and_then(|c| c.parent);
                let s = self.session();
                s.frame = match parent {
                    Some(p) if p != s.top => Some(p),
                    _ => None,
                };
                done(Json::Null)
            }
            ExecuteSync => self.execute(browser, body, false),
            ExecuteAsync => self.execute(browser, body, true),
            Screenshot => {
                let ctx = self.top_context()?;
                let doc = browser.document(ctx).ok_or_else(no_such_window)?;
                let fb = self.painter.paint(&mut browser.agent, &doc);
                let png = fb.to_png();
                done(Json::String(vjs::base64::to_base64(&png, false, false)))
            }
            PerformActions => {
                let ctx = self.current_context()?;
                let session = self.session.as_mut().expect("a session");
                input::perform_actions(browser, session, ctx, body)?;
                done(Json::Null)
            }
            ReleaseActions => {
                let ctx = self.current_context()?;
                let session = self.session.as_mut().expect("a session");
                input::release_actions(browser, session, ctx);
                done(Json::Null)
            }
            FindElement => self.find_element(browser, body),
            ElementClick(id) => {
                let ctx = self.current_context()?;
                let session = self.session.as_mut().expect("a session");
                input::element_click(browser, session, ctx, &id)?;
                // A click that started a navigation waits for it.
                let top = session.top;
                let navigating = ua()
                    .and_then(|u| u.context(top))
                    .is_some_and(|c| c.navigation.get() != 0);
                if navigating {
                    Ok(self.load_wait(top))
                } else {
                    done(Json::Null)
                }
            }
            ElementSendKeys(id) => {
                let ctx = self.current_context()?;
                let Some(text) = body.get("text").and_then(Json::as_str) else {
                    return invalid_argument("text must be a string");
                };
                let text = text.to_string();
                let session = self.session.as_mut().expect("a session");
                input::element_send_keys(browser, session, ctx, &id, &text)?;
                done(Json::Null)
            }
            GetCookies => {
                let ctx = self.current_context()?;
                done(cookies(browser, ctx, false))
            }
            DeleteCookies => {
                let ctx = self.current_context()?;
                cookies(browser, ctx, true);
                done(Json::Null)
            }
        }
    }

    /// Back (`delta` −1) or Forward (+1) in the current top-level
    /// browsing context, waiting for the load.
    fn traverse(&mut self, browser: &mut Browser, delta: i64) -> WdResult<Outcome> {
        let ctx = self.top_context()?;
        self.session().frame = None;
        let ua = ua().ok_or_else(no_such_window)?;
        let c = ua.context(ctx).ok_or_else(no_such_window)?;
        let (index, len) = {
            let s = c.session.borrow();
            (s.index as i64, s.entries.len() as i64)
        };
        let target = index + delta;
        if target < 0 || target >= len {
            return done(Json::Null);
        }
        navigable::traverse_history(&mut browser.agent, ctx, delta);
        if self.session().page_load == PageLoadStrategy::None {
            return done(Json::Null);
        }
        let deadline = Self::deadline_after(self.session().timeouts.page_load);
        Ok(Outcome::Wait(
            Wait::Traverse {
                ctx,
                index: target as usize,
            },
            deadline,
        ))
    }

    /// The handles of the open top-level browsing contexts.
    fn handles(&self, browser: &Browser) -> Json {
        Json::Array(
            browser
                .top_level_contexts()
                .into_iter()
                .map(|id| Json::String(handle_of(id)))
                .collect(),
        )
    }

    fn switch_to_frame(&mut self, body: &Json) -> WdResult<()> {
        let Some(id) = body.get("id") else {
            return invalid_argument("missing id");
        };
        let ua = ua().ok_or_else(no_such_window)?;
        match id {
            Json::Null => {
                self.top_context()?;
                self.session().frame = None;
                Ok(())
            }
            Json::Number(n) => {
                if *n < 0.0 || *n > 65535.0 || n.fract() != 0.0 {
                    return invalid_argument("frame index out of range");
                }
                let cur = self.current_context()?;
                let children = navigable::child_contexts(&ua, cur);
                let child = children
                    .get(*n as usize)
                    .ok_or_else(|| WdError::new(ErrorCode::NoSuchFrame, "no such frame"))?;
                self.session().frame = Some(child.id);
                Ok(())
            }
            Json::Object(_) => {
                let Some(eid) = id.get(script::ELEMENT_KEY).and_then(Json::as_str) else {
                    return invalid_argument("id must be null, a number or an element");
                };
                let cur = self.current_context()?;
                let elements = self.session().elements.clone();
                let (doc, node) = script::known_element(&elements, cur, eid)?;
                let is_frame = {
                    let d = doc.borrow();
                    d.is_html_element(node, "iframe") || d.is_html_element(node, "frame")
                };
                if !is_frame {
                    return Err(WdError::new(ErrorCode::NoSuchFrame, "not a frame element"));
                }
                let child = navigable::context_of_container(&ua, &doc, node).ok_or_else(|| {
                    WdError::new(ErrorCode::NoSuchFrame, "the frame has no browsing context")
                })?;
                self.session().frame = Some(child.id);
                Ok(())
            }
            _ => invalid_argument("id must be null, a number or an element"),
        }
    }

    fn execute(
        &mut self,
        browser: &mut Browser,
        body: &Json,
        asynchronous: bool,
    ) -> WdResult<Outcome> {
        let Some(source) = body.get("script").and_then(Json::as_str) else {
            return invalid_argument("script must be a string");
        };
        let Some(args) = body.get("args").and_then(Json::as_array) else {
            return invalid_argument("args must be an array");
        };
        let ctx = self.current_context()?;
        let s = self.session();
        let elements = s.elements.clone();
        let timeout = s.timeouts.script;
        let realm = browser.realm(ctx).ok_or_else(no_such_window)?;
        let started = script::start(
            &mut browser.agent,
            ctx,
            source,
            args,
            asynchronous,
            &elements,
        )?;
        if let Some(r) = started.outcome.result.borrow_mut().take() {
            return Ok(Outcome::Done(r));
        }
        let roots = browser
            .agent
            .heap
            .add_root_set(Rc::new(Keep(started.keep)) as Rc<dyn Trace>);
        let deadline = timeout.and_then(Self::deadline_after);
        Ok(Outcome::Wait(
            Wait::Script {
                ctx,
                realm,
                outcome: started.outcome,
                roots,
            },
            deadline,
        ))
    }

    /// The first element that matches `selector` in the current browsing
    /// context.
    fn find_now(&mut self, browser: &Browser, selector: &str) -> WdResult<Option<Json>> {
        let ctx = self.current_context()?;
        let doc = browser.document(ctx).ok_or_else(no_such_window)?;
        let found = doc
            .borrow()
            .query_selector(NodeId::DOCUMENT, selector)
            .map_err(|_| {
                WdError::new(
                    ErrorCode::InvalidSelector,
                    format!("invalid selector {selector}"),
                )
            })?;
        let elements = self.session().elements.clone();
        Ok(found.map(|n| script::element_json(&elements, &doc, n)))
    }

    fn find_element(&mut self, browser: &mut Browser, body: &Json) -> WdResult<Outcome> {
        let Some(using) = body.get("using").and_then(Json::as_str) else {
            return invalid_argument("using must be a string");
        };
        let Some(value) = body.get("value").and_then(Json::as_str) else {
            return invalid_argument("value must be a string");
        };
        let selector = match using {
            "css selector" => value.to_string(),
            // A tag name is a type selector.
            "tag name" => value.to_string(),
            "link text" | "partial link text" | "xpath" => {
                return invalid_argument(&format!("unsupported location strategy {using}"));
            }
            _ => return invalid_argument(&format!("invalid location strategy {using}")),
        };
        if let Some(found) = self.find_now(browser, &selector)? {
            return done(found);
        }
        let implicit = self.session().timeouts.implicit;
        if implicit == 0 {
            return Err(WdError::new(
                ErrorCode::NoSuchElement,
                format!("no element matches {value}"),
            ));
        }
        Ok(Outcome::Wait(
            Wait::Find { selector },
            Self::deadline_after(implicit),
        ))
    }

    /// Whether the pending command can be answered now (checked between
    /// tasks; cheap).
    fn pending_ready(&self, agent: &mut Agent) -> bool {
        let _ = agent;
        let Some(p) = &self.pending else {
            return false;
        };
        if p.deadline.is_some_and(|d| Instant::now() >= d) {
            return true;
        }
        match &p.wait {
            Wait::Script {
                outcome,
                ctx,
                realm,
                ..
            } => outcome.result.borrow().is_some() || !script_window_alive(*ctx, *realm),
            Wait::Load { ctx } => load_finished(*ctx, self.strategy()),
            Wait::Traverse { ctx, index } => traversal_finished(*ctx, *index, self.strategy()),
            // Searched for in poll_pending, after each turn of the loop.
            Wait::Find { .. } => false,
        }
    }

    fn strategy(&self) -> PageLoadStrategy {
        self.session
            .as_ref()
            .map_or(PageLoadStrategy::Normal, |s| s.page_load)
    }

    /// Answers the pending command if it is ready.
    fn poll_pending(&mut self, browser: &mut Browser) {
        let Some(p) = self.pending.take() else {
            return;
        };
        let expired = p.deadline.is_some_and(|d| Instant::now() >= d);
        let strategy = self.strategy();
        let result: Option<WdResult<Json>> = match &p.wait {
            Wait::Load { ctx } => {
                if ua().and_then(|u| u.context(*ctx)).is_none() {
                    Some(Err(no_such_window()))
                } else if load_finished(*ctx, strategy) {
                    Some(Ok(Json::Null))
                } else if expired {
                    Some(Err(WdError::new(ErrorCode::Timeout, "page load timed out")))
                } else {
                    None
                }
            }
            Wait::Traverse { ctx, index } => {
                if ua().and_then(|u| u.context(*ctx)).is_none() {
                    Some(Err(no_such_window()))
                } else if traversal_finished(*ctx, *index, strategy) {
                    Some(Ok(Json::Null))
                } else if expired {
                    Some(Err(WdError::new(ErrorCode::Timeout, "page load timed out")))
                } else {
                    None
                }
            }
            Wait::Script {
                ctx,
                realm,
                outcome,
                ..
            } => {
                if let Some(r) = outcome.result.borrow_mut().take() {
                    Some(r)
                } else if ua().and_then(|u| u.context(*ctx)).is_none() {
                    Some(Err(no_such_window()))
                } else if !script_window_alive(*ctx, *realm) {
                    Some(Err(WdError::new(
                        ErrorCode::JavascriptError,
                        "document unloaded while waiting for result",
                    )))
                } else if expired {
                    Some(Err(WdError::new(
                        ErrorCode::ScriptTimeout,
                        "script timed out",
                    )))
                } else {
                    None
                }
            }
            Wait::Find { selector } => {
                let selector = selector.clone();
                match self.find_now(browser, &selector) {
                    Err(e) => Some(Err(e)),
                    Ok(Some(found)) => Some(Ok(found)),
                    Ok(None) if expired => Some(Err(WdError::new(
                        ErrorCode::NoSuchElement,
                        format!("no element matches {selector}"),
                    ))),
                    Ok(None) => None,
                }
            }
        };
        match result {
            None => self.pending = Some(p),
            Some(r) => {
                if let Wait::Script { roots, .. } = &p.wait {
                    browser.agent.heap.remove_root_set(*roots);
                }
                let navigation = matches!(p.wait, Wait::Load { .. } | Wait::Traverse { .. });
                if navigation && r.is_ok() {
                    // A finished navigation leaves the frame.
                    if let Some(s) = self.session.as_mut() {
                        s.frame = None;
                    }
                }
                respond(&p.reply, r);
            }
        }
    }

    /// Answers a pending command with an error when the server stops.
    fn cancel_pending(&mut self, browser: &mut Browser) {
        if let Some(p) = self.pending.take() {
            if let Wait::Script { roots, .. } = &p.wait {
                browser.agent.heap.remove_root_set(*roots);
            }
            respond(
                &p.reply,
                Err(WdError::new(ErrorCode::UnknownError, "the server stopped")),
            );
        }
    }
}

/// The fixed window rect (the viewport, vault/CONTRACT.md §3.5).
fn window_rect() -> Json {
    Json::object([
        ("x", Json::Number(0.0)),
        ("y", Json::Number(0.0)),
        ("width", Json::Number(f64::from(VIEWPORT_WIDTH))),
        ("height", Json::Number(f64::from(VIEWPORT_HEIGHT))),
    ])
}

/// Validates the parameters of Set Window Rect (WebDriver §11.8.2):
/// `width` and `height` are null or a number from 0 to 2^31−1, `x` and
/// `y` null or a number from −2^31 to 2^31−1.
fn set_window_rect_args(body: &Json) -> WdResult<()> {
    let max = f64::from(i32::MAX);
    for (name, min) in [
        ("width", 0.0),
        ("height", 0.0),
        ("x", f64::from(i32::MIN)),
        ("y", f64::from(i32::MIN)),
    ] {
        match body.get(name) {
            None | Some(Json::Null) => {}
            Some(Json::Number(n)) if (min..=max).contains(n) => {}
            Some(_) => {
                return invalid_argument(&format!(
                    "{name} must be null or a number from {min} to {max}"
                ));
            }
        }
    }
    Ok(())
}

/// Whether the window of `ctx` still has the realm the script ran in.
fn script_window_alive(ctx: ContextId, realm: Realm) -> bool {
    ua().and_then(|u| u.context(ctx))
        .and_then(|c| c.active_window())
        .is_some_and(|w| w.realm == realm)
}

/// Whether `ctx` has no navigation under way and its document reached
/// the readiness the strategy waits for.
fn load_finished(ctx: ContextId, strategy: PageLoadStrategy) -> bool {
    let Some(ua) = ua() else {
        return true;
    };
    let Some(c) = ua.context(ctx) else {
        return true;
    };
    if c.navigation.get() != 0 {
        return false;
    }
    let Some(doc) = c.active_document() else {
        return false;
    };
    let Some(info) = ua.doc_info(&doc) else {
        return false;
    };
    match strategy {
        PageLoadStrategy::None => true,
        PageLoadStrategy::Eager => info.readiness.get() != Readiness::Loading,
        PageLoadStrategy::Normal => info.readiness.get() == Readiness::Complete,
    }
}

fn traversal_finished(ctx: ContextId, index: usize, strategy: PageLoadStrategy) -> bool {
    let reached = ua()
        .and_then(|u| u.context(ctx))
        .is_none_or(|c| c.session.borrow().index == index);
    reached && load_finished(ctx, strategy)
}

/// The cookies of the active document of `ctx` as WebDriver cookie
/// objects (the jar of fetch, then the pairs of `document.cookie`), or
/// deletes them all with `delete`.
fn cookies(browser: &Browser, ctx: ContextId, delete: bool) -> Json {
    let Some(ua) = ua() else {
        return Json::Array(Vec::new());
    };
    let Some(doc) = browser.document(ctx) else {
        return Json::Array(Vec::new());
    };
    let url = doc.borrow().url.clone();
    let info = ua.doc_info(&doc);
    if delete {
        ua.fetch.cookies.borrow_mut().delete_matching(&url);
        if let Some(i) = &info {
            i.cookie.borrow_mut().clear();
        }
        return Json::Null;
    }
    let host = url.host.as_ref().map(|h| h.to_string()).unwrap_or_default();
    let mut out: Vec<Json> = Vec::new();
    let mut names: Vec<String> = Vec::new();
    for c in ua.fetch.cookies.borrow_mut().matching(&url) {
        names.push(c.name.clone());
        out.push(Json::object([
            ("name", Json::String(c.name)),
            ("value", Json::String(c.value)),
            ("path", Json::String(c.path)),
            ("domain", Json::String(c.domain)),
            ("secure", Json::Bool(c.secure)),
            ("httpOnly", Json::Bool(c.http_only)),
            ("sameSite", Json::str("None")),
        ]));
    }
    if let Some(i) = &info {
        for pair in i.cookie.borrow().split("; ").filter(|p| !p.is_empty()) {
            let (name, value) = pair.split_once('=').unwrap_or(("", pair));
            if names.iter().any(|n| n == name) {
                continue;
            }
            out.push(Json::object([
                ("name", Json::str(name)),
                ("value", Json::str(value)),
                ("path", Json::str("/")),
                ("domain", Json::String(host.clone())),
                ("secure", Json::Bool(false)),
                ("httpOnly", Json::Bool(false)),
                ("sameSite", Json::str("None")),
            ]));
        }
    }
    Json::Array(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_the_subset_and_nothing_else() {
        assert_eq!(route("GET", "/status"), Some((Command::Status, None)));
        assert_eq!(
            route("POST", "/session/abc/execute/async"),
            Some((Command::ExecuteAsync, Some("abc".to_string())))
        );
        assert_eq!(
            route("POST", "/session/abc/element/e1/click"),
            Some((
                Command::ElementClick("e1".to_string()),
                Some("abc".to_string())
            ))
        );
        assert_eq!(route("GET", "/session/abc/element/e1/text"), None);
        assert_eq!(route("POST", "/session/abc/print"), None);
        assert_eq!(route("PUT", "/session/abc/url"), None);
        assert_eq!(route("GET", "/"), None);
    }

    #[test]
    fn capabilities_of_wptrunner_match() {
        let body = Json::parse(r#"{"capabilities":{"alwaysMatch":{}}}"#).unwrap();
        let m = process_capabilities(&body).unwrap();
        assert_eq!(m.page_load, PageLoadStrategy::Normal);
        let body = Json::parse(
            r#"{"capabilities":{"alwaysMatch":{"pageLoadStrategy":"eager","timeouts":{"script":null},"goog:x":1},"firstMatch":[{"browserName":"firefox"},{"browserName":"vidrio"}]}}"#,
        )
        .unwrap();
        let m = process_capabilities(&body).unwrap();
        assert_eq!(m.page_load, PageLoadStrategy::Eager);
        assert_eq!(m.timeouts.script, None);
    }

    #[test]
    fn invalid_or_unmatched_capabilities_fail() {
        let bad = Json::parse(r#"{"capabilities":{"alwaysMatch":{"nonsense":true}}}"#).unwrap();
        assert_eq!(
            process_capabilities(&bad).err().map(|e| e.code),
            Some(ErrorCode::InvalidArgument)
        );
        let none =
            Json::parse(r#"{"capabilities":{"alwaysMatch":{"browserName":"chrome"}}}"#).unwrap();
        assert_eq!(
            process_capabilities(&none).err().map(|e| e.code),
            Some(ErrorCode::SessionNotCreated)
        );
        assert_eq!(
            process_capabilities(&Json::Object(Vec::new()))
                .err()
                .map(|e| e.code),
            Some(ErrorCode::InvalidArgument)
        );
    }

    #[test]
    fn timeouts_update_and_validate() {
        let mut t = Timeouts::default();
        t.update(&Json::parse(r#"{"script":5,"implicit":10}"#).unwrap())
            .unwrap();
        assert_eq!(t.script, Some(5));
        assert_eq!(t.implicit, 10);
        assert!(
            t.update(&Json::parse(r#"{"pageLoad":-1}"#).unwrap())
                .is_err()
        );
        assert!(
            t.update(&Json::parse(r#"{"implicit":null}"#).unwrap())
                .is_err()
        );
        assert_eq!(t.page_load, 300_000);
        assert_eq!(
            t.to_json().serialize(),
            r#"{"script":5,"pageLoad":300000,"implicit":10}"#
        );
    }
}
