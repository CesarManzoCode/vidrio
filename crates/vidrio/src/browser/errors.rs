//! Runtime script errors (HTML §8.1.4.6, "report the exception") and
//! unhandled promise rejections (HTML §8.1.4.7): the `error` event
//! (`ErrorEvent`, which `window.onerror` receives as five arguments),
//! `unhandledrejection` and `rejectionhandled` (`PromiseRejectionEvent`),
//! and the console messages of errors nobody handled.

use std::rc::Rc;

use vjs::agent::RejectionOperation;
use vjs::object::ObjectKind;
use vjs::{Agent, Object, Value};

use super::event_loop::{TaskSource, queue_value_task};
use super::navigable::fire_window_event;
use crate::bindings::RealmState;
use crate::dom::events::{EventInit, EventInterface};

/// The property `name` of `o` as a string, if it is not `undefined` and
/// can be read without throwing.
fn string_property(agent: &mut Agent, o: Object, name: &str) -> Option<String> {
    let key = agent.heap.key_from_str(name);
    let v = vjs::ops::get(agent, o, key).ok()?;
    if v.is_undefined() {
        return None;
    }
    let s = vjs::ops::to_string(agent, v).ok()?;
    Some(agent.heap.string_to_rust(s))
}

/// A one-line description of a thrown value: `Name: message` for error
/// objects, `ToString(value)` otherwise.
pub fn describe(agent: &mut Agent, v: Value) -> String {
    if let Value::Object(o) = v {
        if matches!(agent.heap.object(o).kind, ObjectKind::Error) {
            let name = string_property(agent, o, "name").unwrap_or_else(|| "Error".to_string());
            let message = string_property(agent, o, "message").unwrap_or_default();
            return if message.is_empty() {
                name
            } else {
                format!("{name}: {message}")
            };
        }
    }
    match vjs::ops::to_string(agent, v) {
        Ok(s) => agent.heap.string_to_rust(s),
        Err(_) => "exception".to_string(),
    }
}

/// The line and column of an error object (vjs keeps neither yet: 0).
fn position(agent: &mut Agent, v: Value) -> (f64, f64) {
    let Value::Object(o) = v else {
        return (0.0, 0.0);
    };
    let mut out = [0.0, 0.0];
    for (i, name) in ["lineNumber", "columnNumber"].iter().enumerate() {
        let key = agent.heap.key_from_str(name);
        if let Ok(Value::Number(n)) = vjs::ops::get(agent, o, key) {
            out[i] = n;
        }
    }
    (out[0], out[1])
}

/// "Report the exception" `value` in the window of `state`: an `error`
/// event at the window and, unless canceled, the message on stderr.
pub fn report_exception(agent: &mut Agent, state: &Rc<RealmState>, value: Value) {
    state.report(value);
    if state.html.error_reporting.get() {
        eprintln!("Uncaught {}", describe(agent, value));
        return;
    }
    state.html.error_reporting.set(true);
    let message = format!("Uncaught {}", describe(agent, value));
    let filename = state
        .document()
        .map(|d| d.borrow().url.href())
        .unwrap_or_default();
    let (line, column) = position(agent, value);
    let m = Value::String(agent.heap.alloc_str(&message));
    let f = Value::String(agent.heap.alloc_str(&filename));
    let keep = Rc::new(super::window::Roots(vec![value, m, f]));
    let root = agent
        .heap
        .add_root_set(keep.clone() as Rc<dyn vjs::gc::Trace>);
    let init = EventInit {
        cancelable: true,
        ..EventInit::default()
    };
    let not_canceled = if state.document().is_some() {
        fire_window_event(
            agent,
            state,
            "error",
            EventInterface::Other("ErrorEvent"),
            init,
            &[
                ("message", m),
                ("filename", f),
                ("lineno", Value::Number(line)),
                ("colno", Value::Number(column)),
                ("error", value),
            ],
            false,
        )
    } else {
        true
    };
    agent.heap.remove_root_set(root);
    state.html.error_reporting.set(false);
    if not_canceled {
        eprintln!("{message}");
    }
}

/// "Report the exception" in the current realm's window.
pub fn report_exception_in_current(agent: &mut Agent, value: Value) {
    match crate::bindings::current(agent) {
        Some(state) => report_exception(agent, &state, value),
        None => eprintln!("Uncaught {}", describe(agent, value)),
    }
}

/// HostPromiseRejectionTracker (HTML §8.1.5.7).
pub fn promise_rejection_tracker(
    agent: &mut Agent,
    promise: Object,
    operation: RejectionOperation,
) {
    let Some(state) = crate::bindings::current(agent) else {
        return;
    };
    match operation {
        RejectionOperation::Reject => {
            state.html.about_to_be_notified.borrow_mut().push(promise);
        }
        RejectionOperation::Handle => {
            {
                let mut list = state.html.about_to_be_notified.borrow_mut();
                if let Some(i) = list.iter().position(|p| *p == promise) {
                    list.remove(i);
                    return;
                }
            }
            {
                let mut outstanding = state.html.outstanding_rejections.borrow_mut();
                match outstanding.iter().position(|p| *p == promise) {
                    Some(i) => {
                        outstanding.remove(i);
                    }
                    None => return,
                }
            }
            let doc = state.document();
            let s = state.clone();
            queue_value_task(
                TaskSource::DomManipulation,
                doc,
                vec![Value::Object(promise)],
                move |agent, values| {
                    let p = values[0];
                    let reason = match p {
                        Value::Object(o) => vjs::interp::promise::promise_state(agent, o)
                            .map_or(Value::Undefined, |(_, r)| r),
                        _ => Value::Undefined,
                    };
                    fire_window_event(
                        agent,
                        &s,
                        "rejectionhandled",
                        EventInterface::Other("PromiseRejectionEvent"),
                        EventInit::default(),
                        &[("promise", p), ("reason", reason)],
                        false,
                    );
                },
            );
        }
    }
}

/// "Notify about rejected promises" for every window (step of each
/// microtask checkpoint).
pub fn notify_about_rejected_promises(agent: &mut Agent) {
    let realms: Vec<vjs::gc::Realm> = agent.realms().to_vec();
    for realm in realms {
        let Some(state) = crate::bindings::state_of(agent, realm) else {
            continue;
        };
        let list: Vec<Object> = std::mem::take(&mut *state.html.about_to_be_notified.borrow_mut());
        if list.is_empty() {
            continue;
        }
        let doc = state.document();
        let s = state.clone();
        let values = list.into_iter().map(Value::Object).collect();
        queue_value_task(
            TaskSource::DomManipulation,
            doc,
            values,
            move |agent, values| {
                for p in values {
                    let Value::Object(o) = p else {
                        continue;
                    };
                    if vjs::interp::promise::promise_is_handled(agent, o) {
                        continue;
                    }
                    let reason = vjs::interp::promise::promise_state(agent, o)
                        .map_or(Value::Undefined, |(_, r)| r);
                    let init = EventInit {
                        cancelable: true,
                        ..EventInit::default()
                    };
                    let not_canceled = fire_window_event(
                        agent,
                        &s,
                        "unhandledrejection",
                        EventInterface::Other("PromiseRejectionEvent"),
                        init,
                        &[("promise", p), ("reason", reason)],
                        false,
                    );
                    if not_canceled {
                        eprintln!("Uncaught (in promise) {}", describe(agent, reason));
                    }
                    if !vjs::interp::promise::promise_is_handled(agent, o) {
                        s.html.outstanding_rejections.borrow_mut().push(o);
                    }
                }
            },
        );
    }
}
