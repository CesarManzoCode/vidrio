//! The members of the HTML interfaces of k3-10, which the bindings route
//! here before their own implementation ([`call`] answers `None` for the
//! members it does not handle): `Window` and `WindowProxy` navigation
//! (`window`, `self`, `frames`, `parent`, `top`, `opener`, `length`,
//! `frameElement`, `name`, `closed`, `open`, `close`, `postMessage`),
//! timers and the rest of `WindowOrWorkerGlobalScope` (`atob`, `btoa`,
//! `queueMicrotask`, `reportError`, `structuredClone`, `origin`,
//! `performance`), animation frames, dialogs without a user interface,
//! `console`, `Location`, `History`, `Navigator`, `Performance`, the
//! dynamic markup insertion and lifecycle members of `Document`,
//! `HTMLScriptElement`, `HTMLIFrameElement`, `MessageChannel`,
//! `MessagePort`, the attributes of the HTML event interfaces and every
//! event handler IDL attribute.

use std::rc::Rc;

use vjs::function::is_callable;
use vjs::{Agent, JsResult, Object, Value};

use super::event_loop::{self, TimerHandler};
use super::navigable::{self, BrowsingContext, NavigateParams};
use super::window::{WindowData, proxy_of, window_now};
use crate::bindings::convert::to_js_any;
use crate::bindings::exception::dom;
use crate::bindings::generated::interface_by_name;
use crate::bindings::install::proto_from_new_target;
use crate::bindings::platform::{self, Native, PlatformRef};
use crate::bindings::{IdlValue, Invocation, RealmState};
use crate::dom::{DocRef, DomException, NodeId};
use crate::url::Url;

type R = JsResult<IdlValue>;

/// Runs `inv` if it is a member of this module.
pub fn call(agent: &mut Agent, state: &Rc<RealmState>, inv: &Invocation) -> Option<R> {
    let name = inv.op.name();
    let (owner, member) = name.split_once('_')?;
    if let Some(handler) = member.strip_prefix("get_") {
        if is_handler_name(handler) {
            return Some(handler_get(agent, inv, handler));
        }
    }
    if let Some(handler) = member.strip_prefix("set_") {
        if is_handler_name(handler) {
            return Some(handler_set(agent, inv, handler));
        }
    }
    match owner {
        "Window" => window(agent, state, inv, member),
        "AnimationFrameProvider" => animation_frames(agent, state, inv, member),
        "WindowOrWorkerGlobalScope" => global_scope(agent, state, inv, member),
        "console" => Some(console(agent, state, inv, member)),
        "Location" => location(agent, state, inv, member),
        "History" => history(agent, state, inv, member),
        "Performance" => performance(agent, state, inv, member),
        o if o.starts_with("Navigator") => navigator(agent, state, member),
        "Document" | "DocumentOrShadowRoot" => document(agent, state, inv, member),
        "HTMLScriptElement" => script_element(agent, inv, member),
        "HTMLIFrameElement" => iframe_element(agent, inv, member),
        "MessageChannel" => message_channel(agent, state, inv, member),
        "MessagePort" => message_port(agent, inv, member),
        "MessageEvent"
        | "ErrorEvent"
        | "PromiseRejectionEvent"
        | "PageTransitionEvent"
        | "PopStateEvent"
        | "HashChangeEvent"
        | "BeforeUnloadEvent" => event_attribute(agent, inv, owner, member),
        _ => None,
    }
}

/// An event handler name: `on` and lowercase letters (not `onLine`).
fn is_handler_name(name: &str) -> bool {
    name.len() > 2 && name.starts_with("on") && name.bytes().all(|b| b.is_ascii_lowercase())
}

fn handler_get(agent: &mut Agent, inv: &Invocation, name: &str) -> R {
    let Some(this) = inv.this.as_ref() else {
        return Ok(IdlValue::Null);
    };
    let v = super::handlers::idl_get(agent, this, name)?;
    Ok(match v {
        Value::Object(o) => IdlValue::Object(o),
        _ => IdlValue::Null,
    })
}

fn handler_set(agent: &mut Agent, inv: &Invocation, name: &str) -> R {
    if let Some(this) = inv.this.as_ref() {
        let v = inv.object(0).map_or(Value::Null, Value::Object);
        super::handlers::idl_set(agent, this, name, v);
    }
    Ok(IdlValue::Undefined)
}

// ----- helpers ---------------------------------------------------------------------

fn string(agent: &mut Agent, s: &str) -> Value {
    Value::String(agent.heap.alloc_str(s))
}

fn obj(v: Value) -> IdlValue {
    match v {
        Value::Object(o) => IdlValue::Object(o),
        Value::Null => IdlValue::Null,
        Value::Undefined => IdlValue::Undefined,
        v => IdlValue::Any(v),
    }
}

/// The realm state of the platform object `this` (the window a
/// `Location` or `History` belongs to), or `state`.
fn this_state(agent: &Agent, inv: &Invocation, state: &Rc<RealmState>) -> Rc<RealmState> {
    inv.this
        .as_ref()
        .and_then(|p| crate::bindings::state_of(agent, p.get().realm))
        .unwrap_or_else(|| state.clone())
}

/// The browsing context whose active window is the window of `state`.
pub fn context_of(state: &RealmState) -> Option<Rc<BrowsingContext>> {
    let ua = super::ua()?;
    let ctx = ua.context(state.html.context.get()?)?;
    let w = ctx.active_window()?;
    (w.realm == state.realm).then_some(ctx)
}

/// A platform object of interface `name` for the window of `state`,
/// cached in `cell`.
fn cached_object(
    agent: &mut Agent,
    state: &RealmState,
    cell: &std::cell::Cell<Option<Object>>,
    name: &str,
) -> IdlValue {
    if let Some(o) = cell.get() {
        return IdlValue::Object(o);
    }
    let Some(iface) = interface_by_name(name) else {
        return IdlValue::Null;
    };
    let o = platform::create(agent, state, iface, Native::None, None);
    cell.set(Some(o));
    IdlValue::Object(o)
}

/// The document of the platform object `this`, if it is a node.
fn this_node(inv: &Invocation) -> Option<(DocRef, NodeId)> {
    let p = inv.this.as_ref()?;
    match &*p.get().native.borrow() {
        Native::Node(d, n) => Some((d.clone(), *n)),
        _ => None,
    }
}

fn dom_result(agent: &mut Agent, r: Result<(), DomException>) -> R {
    match r {
        Ok(()) => Ok(IdlValue::Undefined),
        Err(e) => dom(agent, e),
    }
}

/// The document base URL of the entry window (the current realm).
fn entry_base(state: &RealmState) -> Url {
    state
        .document()
        .map(|d| super::document::base_url(&d))
        .unwrap_or_else(|| Url::parse("about:blank", None).expect("about:blank parses"))
}

// ----- Window ------------------------------------------------------------------------

fn window(agent: &mut Agent, state: &Rc<RealmState>, inv: &Invocation, member: &str) -> Option<R> {
    let w = this_state(agent, inv, state);
    let ua = super::ua()?;
    let ctx = context_of(&w);
    let proxy_value = |c: &Option<Rc<BrowsingContext>>| match c {
        Some(c) => IdlValue::Object(c.proxy),
        None => IdlValue::Null,
    };
    Some(Ok(match member {
        "get_window" | "get_self" | "get_frames" => obj(proxy_of(&w)),
        "get_length" => IdlValue::Int(match &ctx {
            Some(c) => navigable::child_contexts(&ua, c.id).len() as i64,
            None => 0,
        }),
        "get_top" => {
            let mut c = ctx.clone();
            while let Some(p) = c
                .as_ref()
                .and_then(|x| x.parent)
                .and_then(|p| ua.context(p))
            {
                c = Some(p);
            }
            proxy_value(&c)
        }
        "get_parent" => match &ctx {
            Some(c) => match c.parent.and_then(|p| ua.context(p)) {
                Some(p) => IdlValue::Object(p.proxy),
                None => IdlValue::Object(c.proxy),
            },
            None => IdlValue::Null,
        },
        "get_opener" => {
            let o = ctx
                .as_ref()
                .and_then(|c| c.opener.get())
                .and_then(|o| ua.context(o));
            proxy_value(&o)
        }
        "set_opener" => {
            if inv.arg(0).is_nullish() {
                if let Some(c) = &ctx {
                    c.opener.set(None);
                }
            }
            IdlValue::Undefined
        }
        "get_frameElement" => match ctx.as_ref().and_then(|c| c.container_element()) {
            Some((d, n)) => IdlValue::Object(crate::bindings::wrap_node(agent, &d, n)),
            None => IdlValue::Null,
        },
        "get_name" => IdlValue::String(
            ctx.as_ref()
                .map(|c| c.name.borrow().clone())
                .unwrap_or_default(),
        ),
        "set_name" => {
            if let Some(c) = &ctx {
                *c.name.borrow_mut() = inv.str(0);
            }
            IdlValue::Undefined
        }
        "get_closed" => IdlValue::Bool(match ua.context_any(w.html.context.get().unwrap_or(0)) {
            Some(c) => c.discarded.get() || c.closing.get(),
            None => true,
        }),
        "close" => {
            if let Some(c) = &ctx {
                navigable::close(agent, c.id);
            }
            IdlValue::Undefined
        }
        "stop" => {
            if let Some(c) = &ctx {
                c.navigation.set(0);
            }
            IdlValue::Undefined
        }
        "focus" | "blur" | "print" | "captureEvents" | "releaseEvents" => IdlValue::Undefined,
        "get_location" => cached_object(agent, &w, &w.html.location, "Location"),
        "get_history" => cached_object(agent, &w, &w.html.history, "History"),
        "get_navigator" | "get_clientInformation" => {
            cached_object(agent, &w, &w.html.navigator, "Navigator")
        }
        "get_status" => IdlValue::String(w.html.status.borrow().clone()),
        "set_status" => {
            *w.html.status.borrow_mut() = inv.str(0);
            IdlValue::Undefined
        }
        "open" => {
            let url = inv.str(0);
            let target = match inv.arg(1) {
                IdlValue::String(s) => s.clone(),
                _ => "_blank".to_string(),
            };
            let features = inv.str(2);
            return Some(
                match navigable::window_open(agent, &w, &url, &target, &features) {
                    Ok(v) => Ok(obj(v)),
                    Err(e) => dom(agent, e),
                },
            );
        }
        "postMessage" => {
            let message = inv.arg(0).as_value();
            let (target_origin, transfer) = match inv.arg(1) {
                IdlValue::String(s) => (s.clone(), sequence_values(inv.arg(2))),
                IdlValue::Dictionary(d) => (
                    d.str("targetOrigin").unwrap_or("/").to_string(),
                    d.get("transfer").map(sequence_values).unwrap_or_default(),
                ),
                _ => ("/".to_string(), Vec::new()),
            };
            return Some(
                super::messaging::window_post_message(
                    agent,
                    &w,
                    message,
                    &target_origin,
                    &transfer,
                )
                .map(|()| IdlValue::Undefined),
            );
        }
        "alert" => {
            eprintln!("alert: {}", inv.str(0));
            IdlValue::Undefined
        }
        "confirm" => {
            eprintln!("confirm: {}", inv.str(0));
            IdlValue::Bool(false)
        }
        "prompt" => {
            eprintln!("prompt: {}", inv.str(0));
            IdlValue::Null
        }
        _ => return None,
    }))
}

/// The objects of a `sequence<object>` value.
fn sequence_values(v: &IdlValue) -> Vec<Value> {
    v.as_seq().iter().map(IdlValue::as_value).collect()
}

fn animation_frames(
    _agent: &mut Agent,
    state: &Rc<RealmState>,
    inv: &Invocation,
    member: &str,
) -> Option<R> {
    let html: &WindowData = &state.html;
    Some(Ok(match member {
        "requestAnimationFrame" => {
            let id = html.animation_frame_id.get() + 1;
            html.animation_frame_id.set(id);
            let cb = inv.object(0).map_or(Value::Undefined, Value::Object);
            html.animation_frames.borrow_mut().push((id, cb));
            IdlValue::Int(i64::from(id))
        }
        "cancelAnimationFrame" => {
            let id = inv.u32(0);
            html.animation_frames.borrow_mut().retain(|(h, _)| *h != id);
            IdlValue::Undefined
        }
        _ => return None,
    }))
}

// ----- WindowOrWorkerGlobalScope --------------------------------------------------------

fn timer_handler(agent: &mut Agent, v: &IdlValue) -> JsResult<TimerHandler> {
    let value = match v {
        IdlValue::String(s) => return Ok(TimerHandler::Source(s.clone())),
        v => v.as_value(),
    };
    if is_callable(agent, value) {
        return Ok(TimerHandler::Function(value));
    }
    let s = vjs::ops::to_string(agent, value)?;
    Ok(TimerHandler::Source(agent.heap.string_to_rust(s)))
}

fn global_scope(
    agent: &mut Agent,
    state: &Rc<RealmState>,
    inv: &Invocation,
    member: &str,
) -> Option<R> {
    let ua = super::ua()?;
    Some(match member {
        "setTimeout" | "setInterval" => (|| {
            let handler = timer_handler(agent, inv.arg(0))?;
            let timeout = inv.arg(1).as_f64();
            let args: Vec<Value> = inv.args.iter().skip(2).map(IdlValue::as_value).collect();
            let id = event_loop::initialize_timer(
                &ua,
                state.realm,
                handler,
                timeout,
                args,
                member == "setInterval",
                None,
            );
            Ok(IdlValue::Int(i64::from(id)))
        })(),
        "clearTimeout" | "clearInterval" => {
            event_loop::clear_timer(&ua, state.realm, inv.arg(0).as_i64() as i32);
            Ok(IdlValue::Undefined)
        }
        "queueMicrotask" => {
            let cb = inv.object(0).map_or(Value::Undefined, Value::Object);
            event_loop::queue_microtask(agent, state.realm, cb);
            Ok(IdlValue::Undefined)
        }
        "reportError" => {
            let v = match inv.arg(0) {
                IdlValue::Missing => Value::Undefined,
                v => match to_js_any(agent, v) {
                    Ok(v) => v,
                    Err(e) => return Some(Err(e)),
                },
            };
            super::errors::report_exception(agent, state, v);
            Ok(IdlValue::Undefined)
        }
        "btoa" => match btoa(&inv.str(0)) {
            Some(s) => Ok(IdlValue::String(s)),
            None => dom(agent, DomException::InvalidCharacterError),
        },
        "atob" => match forgiving_base64_decode(&inv.str(0)) {
            Some(bytes) => Ok(IdlValue::String(
                bytes.into_iter().map(char::from).collect(),
            )),
            None => dom(agent, DomException::InvalidCharacterError),
        },
        "structuredClone" => (|| {
            let value = to_js_any(agent, inv.arg(0))?;
            let transfer = inv
                .arg(1)
                .as_dict()
                .and_then(|d| d.get("transfer"))
                .map(sequence_values)
                .unwrap_or_default();
            let s = super::clone::serialize_with_transfer(agent, value, &transfer)?;
            let (v, _) = super::clone::deserialize_with_transfer(agent, state.realm, &s)?;
            Ok(IdlValue::Any(v))
        })(),
        "get_origin" => Ok(IdlValue::String(
            state
                .document()
                .map(|d| d.borrow().origin.serialize())
                .unwrap_or_else(|| "null".to_string()),
        )),
        "get_isSecureContext" => Ok(IdlValue::Bool(state.options.secure_context)),
        "get_crossOriginIsolated" => Ok(IdlValue::Bool(false)),
        "get_performance" => Ok(cached_object(
            agent,
            state,
            &state.html.performance,
            "Performance",
        )),
        _ => return None,
    })
}

/// `btoa`: `None` if a code point is above U+00FF.
pub fn btoa(s: &str) -> Option<String> {
    let mut bytes = Vec::with_capacity(s.len());
    for c in s.chars() {
        let n = u32::from(c);
        if n > 0xFF {
            return None;
        }
        bytes.push(n as u8);
    }
    Some(vjs::base64::to_base64(&bytes, false, false))
}

/// The forgiving-base64 decode of the Infra standard.
pub fn forgiving_base64_decode(input: &str) -> Option<Vec<u8>> {
    let mut data: Vec<u8> = input
        .bytes()
        .filter(|b| !matches!(b, b'\t' | b'\n' | b'\x0c' | b'\r' | b' '))
        .collect();
    if !input.is_ascii() {
        return None;
    }
    if data.len() % 4 == 0 {
        if data.ends_with(b"==") {
            data.truncate(data.len() - 2);
        } else if data.ends_with(b"=") {
            data.truncate(data.len() - 1);
        }
    }
    if data.len() % 4 == 1 {
        return None;
    }
    let value = |b: u8| -> Option<u32> {
        Some(match b {
            b'A'..=b'Z' => u32::from(b - b'A'),
            b'a'..=b'z' => u32::from(b - b'a') + 26,
            b'0'..=b'9' => u32::from(b - b'0') + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        })
    };
    let mut out = Vec::with_capacity(data.len() * 3 / 4);
    let mut buffer: u32 = 0;
    let mut bits = 0;
    for b in data {
        buffer = (buffer << 6) | value(b)?;
        bits += 6;
        if bits == 24 {
            out.extend_from_slice(&[(buffer >> 16) as u8, (buffer >> 8) as u8, buffer as u8]);
            buffer = 0;
            bits = 0;
        }
    }
    match bits {
        12 => out.push((buffer >> 4) as u8),
        18 => {
            out.push((buffer >> 10) as u8);
            out.push((buffer >> 2) as u8);
        }
        _ => {}
    }
    Some(out)
}

// ----- console ---------------------------------------------------------------------

/// The text of console arguments: strings as they are, other values as
/// their description.
fn console_text(agent: &mut Agent, args: &[IdlValue]) -> String {
    let mut parts = Vec::with_capacity(args.len());
    for a in args {
        match a {
            IdlValue::String(s) => parts.push(s.clone()),
            IdlValue::Missing => {}
            a => {
                let v = a.as_value();
                let text = match v {
                    Value::String(s) => agent.heap.string_to_rust(s),
                    v => super::errors::describe(agent, v),
                };
                parts.push(text);
            }
        }
    }
    parts.join(" ")
}

fn console_print(state: &RealmState, level: &str, text: &str) {
    let indent = "  ".repeat(state.html.console_group.get());
    eprintln!("console.{level}: {indent}{text}");
}

fn console(agent: &mut Agent, state: &Rc<RealmState>, inv: &Invocation, member: &str) -> R {
    let html = &state.html;
    match member {
        "log" | "info" | "warn" | "error" | "debug" | "trace" | "dirxml" => {
            let text = console_text(agent, &inv.args);
            console_print(state, member, &text);
        }
        "dir" | "table" => {
            let text = console_text(agent, &inv.args[..inv.args.len().min(1)]);
            console_print(state, member, &text);
        }
        "assert" => {
            if !inv.bool(0) {
                let text = console_text(agent, inv.args.get(1..).unwrap_or(&[]));
                console_print(state, "assert", &format!("Assertion failed {text}"));
            }
        }
        "clear" => {}
        "count" | "countReset" => {
            let label = inv.str(0);
            let mut counts = html.console_counts.borrow_mut();
            let i = match counts.iter().position(|(l, _)| *l == label) {
                Some(i) => i,
                None => {
                    counts.push((label.clone(), 0));
                    counts.len() - 1
                }
            };
            if member == "count" {
                counts[i].1 += 1;
                let n = counts[i].1;
                drop(counts);
                console_print(state, "count", &format!("{label}: {n}"));
            } else {
                counts[i].1 = 0;
            }
        }
        "group" | "groupCollapsed" => {
            let text = console_text(agent, &inv.args);
            if !text.is_empty() {
                console_print(state, member, &text);
            }
            html.console_group.set(html.console_group.get() + 1);
        }
        "groupEnd" => html
            .console_group
            .set(html.console_group.get().saturating_sub(1)),
        "time" => {
            let label = inv.str(0);
            let now = window_now(state);
            let mut timers = html.console_timers.borrow_mut();
            if !timers.iter().any(|(l, _)| *l == label) {
                timers.push((label, now));
            }
        }
        "timeLog" | "timeEnd" => {
            let label = inv.str(0);
            let now = window_now(state);
            let start = {
                let mut timers = html.console_timers.borrow_mut();
                let i = timers.iter().position(|(l, _)| *l == label);
                let start = i.map(|i| timers[i].1);
                if member == "timeEnd" {
                    if let Some(i) = i {
                        timers.remove(i);
                    }
                }
                start
            };
            if let Some(start) = start {
                let extra = console_text(agent, inv.args.get(1..).unwrap_or(&[]));
                console_print(
                    state,
                    member,
                    &format!("{label}: {}ms {extra}", now - start),
                );
            }
        }
        _ => return not_implemented(agent, member),
    }
    Ok(IdlValue::Undefined)
}

fn not_implemented<T>(agent: &mut Agent, member: &str) -> JsResult<T> {
    agent.type_error(&format!("'{member}' is not implemented yet."))
}

// ----- Location ----------------------------------------------------------------------

/// The URL of the active document of the window of `w`.
fn window_url(w: &RealmState) -> Option<Url> {
    w.document().map(|d| d.borrow().url.clone())
}

/// "Location-object navigate" to `url`.
fn location_navigate(agent: &mut Agent, w: &RealmState, url: Url, replace: bool) {
    let Some(ctx) = context_of(w) else {
        return;
    };
    let mut p = NavigateParams::new(url);
    // A navigation before the load event replaces the entry.
    p.replace = replace;
    p.source_origin = w.document().map(|d| d.borrow().origin.clone());
    navigable::navigate(agent, ctx.id, p);
}

fn location(
    agent: &mut Agent,
    state: &Rc<RealmState>,
    inv: &Invocation,
    member: &str,
) -> Option<R> {
    let w = this_state(agent, inv, state);
    let url = window_url(&w)
        .unwrap_or_else(|| Url::parse("about:blank", None).expect("about:blank parses"));
    let s = |v: String| Ok(IdlValue::String(v));
    Some(match member {
        "get_href" => s(url.href()),
        "get_origin" => s(url.origin().serialize()),
        "get_protocol" => s(format!("{}:", url.scheme)),
        "get_host" => s(match (&url.host, url.port) {
            (Some(h), Some(p)) => format!("{h}:{p}"),
            (Some(h), None) => h.to_string(),
            _ => String::new(),
        }),
        "get_hostname" => s(url.host.as_ref().map(|h| h.to_string()).unwrap_or_default()),
        "get_port" => s(url.port.map(|p| p.to_string()).unwrap_or_default()),
        "get_pathname" => s(url.path_string()),
        "get_search" => s(match &url.query {
            Some(q) if !q.is_empty() => format!("?{q}"),
            _ => String::new(),
        }),
        "get_hash" => s(match &url.fragment {
            Some(f) if !f.is_empty() => format!("#{f}"),
            _ => String::new(),
        }),
        "set_href" | "assign" | "replace" => {
            let base = entry_base(state);
            match Url::parse(&inv.str(0), Some(&base)) {
                Ok(u) => {
                    location_navigate(agent, &w, u, member == "replace");
                    Ok(IdlValue::Undefined)
                }
                Err(_) => dom(agent, DomException::SyntaxError),
            }
        }
        "reload" => {
            location_navigate(agent, &w, url, true);
            Ok(IdlValue::Undefined)
        }
        m if m.starts_with("set_") => {
            let value = inv.str(0);
            let mut copy = url.clone();
            match m {
                "set_protocol" => copy.set_protocol(&value),
                "set_host" => copy.set_host(&value),
                "set_hostname" => copy.set_hostname(&value),
                "set_port" => copy.set_port(&value),
                "set_pathname" => copy.set_pathname(&value),
                "set_search" => copy.set_search(&value),
                "set_hash" => copy.set_hash(&value),
                _ => return None,
            }
            location_navigate(agent, &w, copy, false);
            Ok(IdlValue::Undefined)
        }
        _ => return None,
    })
}

// ----- History -----------------------------------------------------------------------

fn history(agent: &mut Agent, state: &Rc<RealmState>, inv: &Invocation, member: &str) -> Option<R> {
    let w = this_state(agent, inv, state);
    let ctx = context_of(&w);
    Some(match member {
        "get_length" => Ok(IdlValue::Int(
            ctx.as_ref()
                .map_or(0, |c| c.session.borrow().entries.len() as i64),
        )),
        "get_scrollRestoration" => {
            let v = w.html.scroll_restoration.borrow().clone();
            Ok(IdlValue::String(if v.is_empty() {
                "auto".to_string()
            } else {
                v
            }))
        }
        "set_scrollRestoration" => {
            *w.html.scroll_restoration.borrow_mut() = inv.str(0);
            Ok(IdlValue::Undefined)
        }
        "get_state" => Ok(obj(w.html.history_state.get().unwrap_or(Value::Null))),
        "go" | "back" | "forward" => {
            let delta = match member {
                "back" => -1,
                "forward" => 1,
                _ => inv.arg(0).as_i64(),
            };
            match &ctx {
                Some(c) => {
                    navigable::traverse_history(agent, c.id, delta);
                    Ok(IdlValue::Undefined)
                }
                None => dom(agent, DomException::SecurityError),
            }
        }
        "pushState" | "replaceState" => push_state(agent, &w, ctx, inv, member == "replaceState"),
        _ => return None,
    })
}

/// The URL and history update steps of `pushState`/`replaceState`.
fn push_state(
    agent: &mut Agent,
    w: &Rc<RealmState>,
    ctx: Option<Rc<BrowsingContext>>,
    inv: &Invocation,
    replace: bool,
) -> R {
    let (Some(ctx), Some(doc)) = (ctx, w.document()) else {
        return dom(agent, DomException::SecurityError);
    };
    let data = to_js_any(agent, inv.arg(0))?;
    let serialized = Rc::new(super::clone::serialize(agent, data)?);
    let current = doc.borrow().url.clone();
    let new_url = match inv.arg(2).as_opt_str() {
        Some(u) => match Url::parse(u, Some(&super::document::base_url(&doc))) {
            Ok(u) => u,
            Err(_) => return dom(agent, DomException::SecurityError),
        },
        None => current.clone(),
    };
    // "Can have its URL rewritten".
    let rewritable = new_url.scheme == current.scheme
        && new_url.host == current.host
        && new_url.port == current.port
        && new_url.username == current.username
        && new_url.password == current.password
        && (matches!(new_url.scheme.as_str(), "http" | "https")
            || new_url.serialize(true) == current.serialize(true)
            || new_url.scheme == "file" && new_url.path_string() == current.path_string()
            || new_url.scheme == "file");
    if !rewritable {
        return dom(agent, DomException::SecurityError);
    }
    doc.borrow_mut().url = new_url.clone();
    {
        let mut s = ctx.session.borrow_mut();
        let entry = navigable::HistoryEntry {
            url: new_url,
            state: Some(serialized.clone()),
            document: Some(Rc::downgrade(&doc)),
        };
        navigable::push_or_replace(&mut s, entry, replace);
    }
    let value = super::clone::deserialize(agent, w.realm, &serialized)?;
    super::window::set_history_state(agent, w, value);
    Ok(IdlValue::Undefined)
}

// ----- Navigator and Performance --------------------------------------------------------------

fn navigator(_agent: &mut Agent, state: &Rc<RealmState>, member: &str) -> Option<R> {
    let ua = super::ua()?;
    let s = |v: &str| IdlValue::String(v.to_string());
    Some(Ok(match member {
        "get_userAgent" => IdlValue::String(ua.options.user_agent.clone()),
        "get_appCodeName" => s("Mozilla"),
        "get_appName" => s("Netscape"),
        "get_appVersion" => IdlValue::String(
            ua.options
                .user_agent
                .strip_prefix("Mozilla/")
                .unwrap_or(&ua.options.user_agent)
                .to_string(),
        ),
        "get_platform" => s("Linux x86_64"),
        "get_product" => s("Gecko"),
        "get_productSub" => s("20030107"),
        "get_vendor" | "get_vendorSub" | "get_oscpu" | "get_buildID" => s(""),
        "taintEnabled" | "javaEnabled" => IdlValue::Bool(false),
        "get_language" => s("en-US"),
        "get_languages" => IdlValue::Sequence(vec![s("en-US")]),
        "get_onLine" => IdlValue::Bool(true),
        "get_cookieEnabled" => IdlValue::Bool(false),
        "get_pdfViewerEnabled" => IdlValue::Bool(false),
        "get_webdriver" => IdlValue::Bool(false),
        "get_hardwareConcurrency" => {
            IdlValue::Int(std::thread::available_parallelism().map_or(1, |n| n.get() as i64))
        }
        _ => {
            let _ = state;
            return None;
        }
    }))
}

fn performance(
    agent: &mut Agent,
    state: &Rc<RealmState>,
    inv: &Invocation,
    member: &str,
) -> Option<R> {
    let w = this_state(agent, inv, state);
    let origin = || {
        let since_epoch = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0.0, |d| d.as_secs_f64() * 1000.0);
        since_epoch - window_now(&w)
    };
    Some(match member {
        "now" => Ok(IdlValue::Double(window_now(&w))),
        "get_timeOrigin" => Ok(IdlValue::Double(origin())),
        "toJSON" => (|| {
            let o = vjs::ops::object_create(agent);
            let key = agent.heap.key_from_str("timeOrigin");
            vjs::ops::create_data_property_or_throw(agent, o, key, Value::Number(origin()))?;
            Ok(IdlValue::Object(o))
        })(),
        _ => return None,
    })
}

// ----- Document ----------------------------------------------------------------------

fn document(
    agent: &mut Agent,
    _state: &Rc<RealmState>,
    inv: &Invocation,
    member: &str,
) -> Option<R> {
    let (doc, node) = this_node(inv)?;
    if node != NodeId::DOCUMENT {
        return None;
    }
    let ua = super::ua()?;
    let info = ua.doc_info(&doc);
    Some(match member {
        "get_readyState" => Ok(IdlValue::String(
            info.as_ref()
                .map_or("complete", |i| i.readiness.get().as_str())
                .to_string(),
        )),
        "get_currentScript" => Ok(match info.as_ref().and_then(|i| i.current_script.get()) {
            Some(n) => IdlValue::Object(crate::bindings::wrap_node(agent, &doc, n)),
            None => IdlValue::Null,
        }),
        "write" | "writeln" => {
            let text: String = inv.args.iter().map(|a| a.as_str().to_string()).collect();
            let r = super::document::document_write(agent, &doc, &text, member == "writeln");
            dom_result(agent, r)
        }
        "open" => {
            let window_form = inv.args.len() >= 3 && !inv.arg(2).is_missing();
            if window_form {
                let Some(state) = super::window::state_of_document(agent, &ua, &doc) else {
                    return Some(Ok(IdlValue::Null));
                };
                return Some(
                    match navigable::window_open(
                        agent,
                        &state,
                        &inv.str(0),
                        &inv.str(1),
                        &inv.str(2),
                    ) {
                        Ok(v) => Ok(obj(v)),
                        Err(e) => dom(agent, e),
                    },
                );
            }
            match super::document::document_open(agent, &doc) {
                Ok(()) => Ok(IdlValue::Object(crate::bindings::wrap_node(
                    agent,
                    &doc,
                    NodeId::DOCUMENT,
                ))),
                Err(e) => dom(agent, e),
            }
        }
        "close" => {
            let r = super::document::document_close(agent, &doc);
            dom_result(agent, r)
        }
        "get_defaultView" => Ok(match super::window::state_of_document(agent, &ua, &doc) {
            Some(s) if info.as_ref().is_some_and(|i| i.context.get().is_some()) => {
                obj(proxy_of(&s))
            }
            _ => IdlValue::Null,
        }),
        "get_location" => Ok(match super::window::state_of_document(agent, &ua, &doc) {
            Some(s) if info.as_ref().is_some_and(|i| i.context.get().is_some()) => {
                cached_object(agent, &s, &s.html.location, "Location")
            }
            _ => IdlValue::Null,
        }),
        "get_referrer" => Ok(IdlValue::String(
            info.as_ref()
                .map(|i| i.referrer.borrow().clone())
                .unwrap_or_default(),
        )),
        "get_cookie" => Ok(IdlValue::String(
            info.as_ref()
                .map(|i| i.cookie.borrow().clone())
                .unwrap_or_default(),
        )),
        "set_cookie" => {
            if let Some(i) = &info {
                let pair = inv.str(0);
                let pair = pair.split(';').next().unwrap_or("").trim().to_string();
                let mut jar: Vec<String> = i
                    .cookie
                    .borrow()
                    .split("; ")
                    .filter(|c| !c.is_empty())
                    .map(str::to_string)
                    .collect();
                let name = pair.split('=').next().unwrap_or("").to_string();
                jar.retain(|c| c.split('=').next().unwrap_or("") != name);
                if !pair.is_empty() {
                    jar.push(pair);
                }
                *i.cookie.borrow_mut() = jar.join("; ");
            }
            Ok(IdlValue::Undefined)
        }
        "get_lastModified" => Ok(IdlValue::String(
            info.as_ref()
                .map(|i| i.last_modified.borrow().clone())
                .unwrap_or_default(),
        )),
        "get_visibilityState" => Ok(IdlValue::String("visible".to_string())),
        "get_hidden" => Ok(IdlValue::Bool(false)),
        "hasFocus" => Ok(IdlValue::Bool(
            info.as_ref().is_some_and(|i| i.context.get().is_some()),
        )),
        "get_domain" => Ok(IdlValue::String(
            doc.borrow()
                .url
                .host
                .as_ref()
                .map(|h| h.to_string())
                .unwrap_or_default(),
        )),
        _ => return None,
    })
}

// ----- HTMLScriptElement and HTMLIFrameElement -------------------------------------------------

/// A reflected URL attribute: the attribute parsed against the document
/// base URL, the attribute itself if that fails, `""` without one.
fn reflect_url(doc: &DocRef, el: NodeId, attr: &str) -> String {
    let v = doc.borrow().get_attribute(el, attr).map(str::to_string);
    match v {
        Some(v) => match Url::parse(&v, Some(&super::document::base_url(doc))) {
            Ok(u) => u.href(),
            Err(_) => v,
        },
        None => String::new(),
    }
}

fn reflect_string(doc: &DocRef, el: NodeId, attr: &str) -> IdlValue {
    IdlValue::String(
        doc.borrow()
            .get_attribute(el, attr)
            .unwrap_or("")
            .to_string(),
    )
}

fn set_attr(agent: &mut Agent, doc: &DocRef, el: NodeId, attr: &str, value: &str) {
    doc.borrow_mut().set_attribute(el, attr, value);
    super::document::process_html_changes(agent, doc);
}

fn set_bool_attr(agent: &mut Agent, doc: &DocRef, el: NodeId, attr: &str, on: bool) {
    if on {
        set_attr(agent, doc, el, attr, "");
    } else {
        let _ = doc.borrow_mut().remove_attribute_by_name(el, attr);
        super::document::process_html_changes(agent, doc);
    }
}

fn script_element(agent: &mut Agent, inv: &Invocation, member: &str) -> Option<R> {
    if member == "static_supports" {
        return Some(Ok(IdlValue::Bool(super::script::supports(&inv.str(0)))));
    }
    let (doc, el) = this_node(inv)?;
    let has = |a: &str| doc.borrow().get_attribute(el, a).is_some();
    Some(Ok(match member {
        "get_src" => IdlValue::String(reflect_url(&doc, el, "src")),
        "set_src" => {
            set_attr(agent, &doc, el, "src", &inv.str(0));
            IdlValue::Undefined
        }
        "get_type" | "get_charset" | "get_event" | "get_integrity" => {
            reflect_string(&doc, el, &member[4..])
        }
        "set_type" | "set_charset" | "set_event" | "set_integrity" => {
            set_attr(agent, &doc, el, &member[4..], &inv.str(0));
            IdlValue::Undefined
        }
        "get_htmlFor" => reflect_string(&doc, el, "for"),
        "set_htmlFor" => {
            set_attr(agent, &doc, el, "for", &inv.str(0));
            IdlValue::Undefined
        }
        "get_noModule" => IdlValue::Bool(has("nomodule")),
        "set_noModule" => {
            set_bool_attr(agent, &doc, el, "nomodule", inv.bool(0));
            IdlValue::Undefined
        }
        "get_defer" => IdlValue::Bool(has("defer")),
        "set_defer" => {
            set_bool_attr(agent, &doc, el, "defer", inv.bool(0));
            IdlValue::Undefined
        }
        "get_async" => {
            let force = doc
                .borrow()
                .element(el)
                .is_some_and(|e| e.script.force_async);
            IdlValue::Bool(force || has("async"))
        }
        "set_async" => {
            if let Some(e) = doc.borrow_mut().element_mut(el) {
                e.script.force_async = false;
            }
            set_bool_attr(agent, &doc, el, "async", inv.bool(0));
            IdlValue::Undefined
        }
        "get_crossOrigin" => match doc.borrow().get_attribute(el, "crossorigin") {
            None => IdlValue::Null,
            Some(v) if v.eq_ignore_ascii_case("use-credentials") => {
                IdlValue::string("use-credentials")
            }
            Some(_) => IdlValue::string("anonymous"),
        },
        "set_crossOrigin" => {
            match inv.arg(0).as_opt_str() {
                Some(v) => set_attr(agent, &doc, el, "crossorigin", v),
                None => set_bool_attr(agent, &doc, el, "crossorigin", false),
            }
            IdlValue::Undefined
        }
        "get_referrerPolicy" => reflect_string(&doc, el, "referrerpolicy"),
        "set_referrerPolicy" => {
            set_attr(agent, &doc, el, "referrerpolicy", &inv.str(0));
            IdlValue::Undefined
        }
        "get_fetchPriority" => {
            let v = doc
                .borrow()
                .get_attribute(el, "fetchpriority")
                .map(str::to_ascii_lowercase);
            IdlValue::String(
                match v.as_deref() {
                    Some("high") => "high",
                    Some("low") => "low",
                    _ => "auto",
                }
                .to_string(),
            )
        }
        "set_fetchPriority" => {
            set_attr(agent, &doc, el, "fetchpriority", &inv.str(0));
            IdlValue::Undefined
        }
        "get_text" => IdlValue::String(doc.borrow().child_text_content(el)),
        "get_textContent" | "get_innerText" => IdlValue::opt_string(doc.borrow().text_content(el)),
        "set_text" | "set_textContent" | "set_innerText" => {
            let v = inv.arg(0).as_opt_str().unwrap_or("").to_string();
            doc.borrow_mut().set_text_content(el, Some(&v));
            crate::bindings::host::queue_observer_microtask(agent, &doc);
            super::document::process_html_changes(agent, &doc);
            IdlValue::Undefined
        }
        _ => return None,
    }))
}

fn iframe_element(agent: &mut Agent, inv: &Invocation, member: &str) -> Option<R> {
    let (doc, el) = this_node(inv)?;
    let ua = super::ua()?;
    let child = || navigable::context_of_container(&ua, &doc, el);
    Some(Ok(match member {
        "get_src" => IdlValue::String(reflect_url(&doc, el, "src")),
        "get_contentWindow" => match child() {
            Some(c) => IdlValue::Object(c.proxy),
            None => IdlValue::Null,
        },
        "get_contentDocument" => match child().and_then(|c| c.active_document()) {
            Some(d) if d.borrow().origin.same_origin(&doc.borrow().origin) => {
                IdlValue::Object(crate::bindings::wrap_node(agent, &d, NodeId::DOCUMENT))
            }
            _ => IdlValue::Null,
        },
        "get_allowFullscreen" => {
            IdlValue::Bool(doc.borrow().get_attribute(el, "allowfullscreen").is_some())
        }
        "set_allowFullscreen" => {
            set_bool_attr(agent, &doc, el, "allowfullscreen", inv.bool(0));
            IdlValue::Undefined
        }
        "get_referrerPolicy" => reflect_string(&doc, el, "referrerpolicy"),
        "get_loading" => {
            let lazy = doc
                .borrow()
                .get_attribute(el, "loading")
                .is_some_and(|v| v.eq_ignore_ascii_case("lazy"));
            IdlValue::string(if lazy { "lazy" } else { "eager" })
        }
        m if m.starts_with("get_") => {
            let attr = m[4..].to_ascii_lowercase();
            reflect_string(&doc, el, &attr)
        }
        m if m.starts_with("set_") => {
            let attr = m[4..].to_ascii_lowercase();
            set_attr(agent, &doc, el, &attr, &inv.str(0));
            IdlValue::Undefined
        }
        _ => return None,
    }))
}

// ----- messaging -------------------------------------------------------------------------

fn message_channel(
    agent: &mut Agent,
    state: &Rc<RealmState>,
    inv: &Invocation,
    member: &str,
) -> Option<R> {
    Some(match member {
        "constructor" => (|| {
            let proto = proto_from_new_target(agent, inv.new_target, inv.iface)?;
            let o = platform::create(agent, state, inv.iface, Native::None, proto);
            let (a, b) = super::messaging::new_channel(agent, state);
            if let Some(p) = platform::platform_ref(agent, Value::Object(o)) {
                p.get().set_slot("port1", Value::Object(a));
                p.get().set_slot("port2", Value::Object(b));
            }
            Ok(IdlValue::Object(o))
        })(),
        "get_port1" | "get_port2" => Ok(inv
            .this
            .as_ref()
            .and_then(|p| p.get().slot(&member[4..]))
            .map_or(IdlValue::Null, obj)),
        _ => return None,
    })
}

fn message_port(agent: &mut Agent, inv: &Invocation, member: &str) -> Option<R> {
    let this = inv.this_object()?;
    Some(match member {
        "postMessage" => (|| {
            let message = to_js_any(agent, inv.arg(0))?;
            let transfer = match inv.arg(1) {
                IdlValue::Dictionary(d) => {
                    d.get("transfer").map(sequence_values).unwrap_or_default()
                }
                v => sequence_values(v),
            };
            super::messaging::port_post_message(agent, this, message, &transfer)?;
            Ok(IdlValue::Undefined)
        })(),
        "start" => {
            super::messaging::start(agent, this);
            Ok(IdlValue::Undefined)
        }
        "close" => {
            super::messaging::close(agent, this);
            Ok(IdlValue::Undefined)
        }
        _ => return None,
    })
}

// ----- event interfaces ----------------------------------------------------------------------

/// The default of an attribute of an HTML event interface that was not
/// initialized.
fn event_default(agent: &mut Agent, member: &str) -> Value {
    match member {
        "origin" | "lastEventId" | "message" | "filename" | "oldURL" | "newURL" | "returnValue" => {
            string(agent, "")
        }
        "lineno" | "colno" => Value::Number(0.0),
        "persisted" | "hasUAVisualTransition" => Value::Boolean(false),
        "data" | "source" | "state" => Value::Null,
        "ports" => {
            let a = vjs::ops::create_array_from_list(agent, &[]);
            let _ = vjs::ops::set_integrity_level(agent, a, vjs::ops::IntegrityLevel::Frozen);
            Value::Object(a)
        }
        _ => Value::Undefined,
    }
}

fn event_attribute(agent: &mut Agent, inv: &Invocation, owner: &str, member: &str) -> Option<R> {
    let this: &PlatformRef = inv.this.as_ref()?;
    if let Some(attr) = member.strip_prefix("get_") {
        let v = match this.get().slot(attr) {
            Some(v) => v,
            None => event_default(agent, attr),
        };
        return Some(Ok(IdlValue::Any(v)));
    }
    if owner == "BeforeUnloadEvent" && member == "set_returnValue" {
        let v = string(agent, &inv.str(0));
        this.get().set_slot("returnValue", v);
        return Some(Ok(IdlValue::Undefined));
    }
    if owner == "MessageEvent" && member == "initMessageEvent" {
        let ev = match &*this.get().native.borrow() {
            Native::Event(e) => Some(e.clone()),
            _ => None,
        };
        if let Some(ev) = ev {
            if ev.borrow().is_dispatching() {
                return Some(Ok(IdlValue::Undefined));
            }
            ev.borrow_mut()
                .init_event(&inv.str(0), inv.bool(1), inv.bool(2));
        }
        let r = (|| -> JsResult<()> {
            let data = to_js_any(agent, inv.arg(3))?;
            let origin = string(agent, &inv.str(4));
            let last = string(agent, &inv.str(5));
            let source = inv.arg(6).as_value();
            let ports: Vec<Value> = sequence_values(inv.arg(7));
            let a = vjs::ops::create_array_from_list(agent, &ports);
            vjs::ops::set_integrity_level(agent, a, vjs::ops::IntegrityLevel::Frozen)?;
            let p = this.get();
            p.set_slot("data", data);
            p.set_slot("origin", origin);
            p.set_slot("lastEventId", last);
            p.set_slot(
                "source",
                if source.is_undefined() {
                    Value::Null
                } else {
                    source
                },
            );
            p.set_slot("ports", Value::Object(a));
            Ok(())
        })();
        return Some(r.map(|()| IdlValue::Undefined));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trips() {
        assert_eq!(btoa("hello").as_deref(), Some("aGVsbG8="));
        assert_eq!(btoa("\u{FF}\u{0}").as_deref(), Some("/wA="));
        assert_eq!(btoa("\u{100}"), None);
        assert_eq!(forgiving_base64_decode("aGVsbG8="), Some(b"hello".to_vec()));
        assert_eq!(
            forgiving_base64_decode(" aGVs bG8 "),
            Some(b"hello".to_vec())
        );
        assert_eq!(forgiving_base64_decode("aGVsbG8"), Some(b"hello".to_vec()));
        assert_eq!(forgiving_base64_decode("a"), None);
        assert_eq!(forgiving_base64_decode("a==="), None);
        assert_eq!(forgiving_base64_decode("ab*c"), None);
        assert_eq!(forgiving_base64_decode(""), Some(Vec::new()));
    }

    #[test]
    fn handler_names_exclude_other_attributes() {
        assert!(is_handler_name("onclick"));
        assert!(is_handler_name("onmessage"));
        assert!(!is_handler_name("onLine"));
        assert!(!is_handler_name("on"));
        assert!(!is_handler_name("open"));
    }
}
