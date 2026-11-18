//! A minimal readable byte stream (Streams Standard §4) for the bodies of
//! `Request`, `Response` and `Blob.stream()`: a queue of chunks filled by
//! the network or by a body source, read with a default reader
//! (`getReader().read()`) or in full by the body mixin ("fully read").
//! Chunks are `Uint8Array`s. Streams cannot be built from script, and
//! there is no BYOB reader, `pipeTo` or `tee` from script
//! (LIMITATIONS.md); `Response.clone()` tees natively ([`tee`]).

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use vjs::gc::Realm;
use vjs::{Agent, Value};

use super::{Deferred, Kept, R, keep};
use crate::bindings::platform::Native;
use crate::bindings::{IdlValue, Invocation, RealmState};
use crate::browser::in_realm;

type Waiter = Box<dyn FnOnce(&mut Agent, Result<Vec<u8>, String>)>;

/// A pending read or `closed` promise with the handle that keeps it.
struct Pending {
    deferred: Deferred,
    _kept: Kept,
}

impl Pending {
    fn new(agent: &mut Agent, realm: Realm) -> Pending {
        let deferred = Deferred::new(agent, realm);
        let kept = keep(deferred.values());
        Pending {
            deferred,
            _kept: kept,
        }
    }
}

/// The state of a stream.
pub struct Stream {
    /// The realm its chunks and read results are created in.
    pub realm: Realm,
    chunks: VecDeque<Vec<u8>>,
    pub closed: bool,
    pub errored: Option<String>,
    /// A reader holds the stream (or "fully read" took it).
    pub locked: bool,
    /// Something was read from it or it was cancelled.
    pub disturbed: bool,
    read_requests: VecDeque<Pending>,
    closed_promises: Vec<Pending>,
    waiters: Vec<Waiter>,
    tees: Vec<StreamRef>,
    /// What cancelling the stream stops (the fetch that fills it).
    on_cancel: Option<Box<dyn FnOnce()>>,
}

pub type StreamRef = Rc<RefCell<Stream>>;

/// The native state of a `ReadableStream` object.
pub struct StreamObject(pub StreamRef);

/// The native state of a `ReadableStreamDefaultReader` object.
pub struct ReaderObject {
    pub stream: RefCell<Option<StreamRef>>,
}

/// A new readable stream of `realm`, empty and open.
pub fn new_stream(realm: Realm) -> StreamRef {
    Rc::new(RefCell::new(Stream {
        realm,
        chunks: VecDeque::new(),
        closed: false,
        errored: None,
        locked: false,
        disturbed: false,
        read_requests: VecDeque::new(),
        closed_promises: Vec::new(),
        waiters: Vec::new(),
        tees: Vec::new(),
        on_cancel: None,
    }))
}

/// A closed stream holding `bytes` ("extract" of a byte sequence).
pub fn from_bytes(realm: Realm, bytes: Vec<u8>) -> StreamRef {
    let s = new_stream(realm);
    {
        let mut st = s.borrow_mut();
        if !bytes.is_empty() {
            st.chunks.push_back(bytes);
        }
        st.closed = true;
    }
    s
}

/// Sets what cancelling `s` stops.
pub fn set_on_cancel(s: &StreamRef, f: Box<dyn FnOnce()>) {
    s.borrow_mut().on_cancel = Some(f);
}

fn read_result(agent: &mut Agent, realm: Realm, chunk: Option<Vec<u8>>) -> Value {
    in_realm(agent, realm, |agent| {
        let (value, done) = match chunk {
            Some(bytes) => (
                Value::Object(super::uint8_array(agent, realm, bytes)),
                false,
            ),
            None => (Value::Undefined, true),
        };
        Value::Object(vjs::ops::create_iter_result_object(agent, value, done))
    })
}

/// Enqueues `bytes`: a waiting read takes them at once.
pub fn enqueue(agent: &mut Agent, s: &StreamRef, bytes: Vec<u8>) {
    let tees = s.borrow().tees.clone();
    for t in &tees {
        enqueue(agent, t, bytes.clone());
    }
    let (request, realm) = {
        let mut st = s.borrow_mut();
        if st.closed || st.errored.is_some() || bytes.is_empty() {
            return;
        }
        match st.read_requests.pop_front() {
            Some(r) => (r, st.realm),
            None => {
                st.chunks.push_back(bytes);
                return;
            }
        }
    };
    let result = read_result(agent, realm, Some(bytes));
    request.deferred.resolve(agent, result);
}

/// Closes `s`: waiting reads end, "fully read" gets the bytes.
pub fn close(agent: &mut Agent, s: &StreamRef) {
    let tees = s.borrow().tees.clone();
    for t in &tees {
        close(agent, t);
    }
    let (requests, waiters, closed, realm, all) = {
        let mut st = s.borrow_mut();
        if st.closed || st.errored.is_some() {
            return;
        }
        st.closed = true;
        st.on_cancel = None;
        let waiters = std::mem::take(&mut st.waiters);
        let all = if waiters.is_empty() {
            Vec::new()
        } else {
            st.chunks.drain(..).flatten().collect()
        };
        (
            std::mem::take(&mut st.read_requests),
            waiters,
            std::mem::take(&mut st.closed_promises),
            st.realm,
            all,
        )
    };
    for r in requests {
        let result = read_result(agent, realm, None);
        r.deferred.resolve(agent, result);
    }
    for c in closed {
        c.deferred.resolve(agent, Value::Undefined);
    }
    let mut all = Some(all);
    for w in waiters {
        w(agent, Ok(all.take().unwrap_or_default()));
    }
}

/// Errors `s` with a `TypeError` carrying `message`.
pub fn error(agent: &mut Agent, s: &StreamRef, message: &str) {
    let tees = s.borrow().tees.clone();
    for t in &tees {
        error(agent, t, message);
    }
    let (requests, waiters, closed, realm) = {
        let mut st = s.borrow_mut();
        if st.closed || st.errored.is_some() {
            return;
        }
        st.errored = Some(message.to_string());
        st.chunks.clear();
        st.on_cancel = None;
        (
            std::mem::take(&mut st.read_requests),
            std::mem::take(&mut st.waiters),
            std::mem::take(&mut st.closed_promises),
            st.realm,
        )
    };
    for r in requests.into_iter().chain(closed) {
        let e = super::type_error_value(agent, realm, message);
        r.deferred.reject(agent, e);
    }
    for w in waiters {
        w(agent, Err(message.to_string()));
    }
}

/// Reads one chunk: a promise for `{value, done}`.
pub fn read(agent: &mut Agent, s: &StreamRef) -> Value {
    let realm = s.borrow().realm;
    s.borrow_mut().disturbed = true;
    let (chunk, closed, errored) = {
        let mut st = s.borrow_mut();
        (st.chunks.pop_front(), st.closed, st.errored.clone())
    };
    if let Some(bytes) = chunk {
        let r = read_result(agent, realm, Some(bytes));
        return Value::Object(super::resolved(agent, realm, r));
    }
    if closed {
        let r = read_result(agent, realm, None);
        return Value::Object(super::resolved(agent, realm, r));
    }
    if let Some(m) = errored {
        let e = super::type_error_value(agent, realm, &m);
        return Value::Object(super::rejected(agent, realm, e));
    }
    let p = Pending::new(agent, realm);
    let promise = p.deferred.promise;
    s.borrow_mut().read_requests.push_back(p);
    Value::Object(promise)
}

/// "Fully read" `s`: `f` gets every byte once it closes (at once if it
/// already did), or the error. The stream becomes locked and disturbed.
pub fn read_all(agent: &mut Agent, s: &StreamRef, f: Waiter) {
    let ready = {
        let mut st = s.borrow_mut();
        st.locked = true;
        st.disturbed = true;
        if let Some(e) = &st.errored {
            Some(Err(e.clone()))
        } else if st.closed {
            Some(Ok(st.chunks.drain(..).flatten().collect::<Vec<u8>>()))
        } else {
            None
        }
    };
    match ready {
        Some(r) => f(agent, r),
        None => s.borrow_mut().waiters.push(f),
    }
}

/// Cancels `s`: the queue empties, it closes and its source stops.
pub fn cancel(agent: &mut Agent, s: &StreamRef) {
    let stop = {
        let mut st = s.borrow_mut();
        st.disturbed = true;
        st.chunks.clear();
        st.on_cancel.take()
    };
    if let Some(stop) = stop {
        stop();
    }
    close(agent, s);
}

/// A second stream with the same chunks, present and future (the tee of
/// `Response.clone()` and `Request.clone()`).
pub fn tee(s: &StreamRef) -> StreamRef {
    let st = s.borrow();
    let t = new_stream(st.realm);
    {
        let mut tt = t.borrow_mut();
        tt.chunks = st.chunks.clone();
        tt.closed = st.closed;
        tt.errored = st.errored.clone();
    }
    drop(st);
    if !t.borrow().closed && t.borrow().errored.is_none() {
        s.borrow_mut().tees.push(t.clone());
    }
    t
}

/// The bytes queued in a closed stream, without reading it (for bodies
/// that must be sent, such as the body of a `Request` built from a
/// string).
pub fn queued_bytes(s: &StreamRef) -> Option<Vec<u8>> {
    let st = s.borrow();
    if !st.closed {
        return None;
    }
    Some(st.chunks.iter().flatten().copied().collect())
}

/// The `ReadableStream` object for `s` in the realm of `state`.
pub fn stream_object(agent: &mut Agent, state: &RealmState, s: &StreamRef) -> vjs::Object {
    super::create_object(
        agent,
        state,
        "ReadableStream",
        Native::Data(Rc::new(StreamObject(s.clone()))),
        None,
    )
}

/// The stream of a `ReadableStream` object.
pub fn stream_of(agent: &Agent, o: vjs::Object) -> Option<StreamRef> {
    super::object_data::<StreamObject>(agent, o).map(|d| d.0.clone())
}

fn acquire_reader(
    agent: &mut Agent,
    state: &RealmState,
    s: &StreamRef,
    proto: Option<vjs::Object>,
) -> R {
    if s.borrow().locked {
        return agent.type_error("The stream is locked.");
    }
    s.borrow_mut().locked = true;
    let reader = Rc::new(ReaderObject {
        stream: RefCell::new(Some(s.clone())),
    });
    let o = super::create_object(
        agent,
        state,
        "ReadableStreamDefaultReader",
        Native::Data(reader),
        proto,
    );
    Ok(IdlValue::Object(o))
}

pub fn call(
    agent: &mut Agent,
    state: &Rc<RealmState>,
    inv: &Invocation,
    owner: &str,
    member: &str,
) -> Option<R> {
    let realm = state.realm;
    Some(match (owner, member) {
        ("ReadableStream", "get_locked") => (|| {
            let s = super::this_data::<StreamObject>(agent, inv)?;
            Ok(IdlValue::Bool(s.0.borrow().locked))
        })(),
        ("ReadableStream", "cancel") => (|| {
            let s = super::this_data::<StreamObject>(agent, inv)?;
            if s.0.borrow().locked {
                return super::rejected_type_error(agent, realm, "The stream is locked.");
            }
            cancel(agent, &s.0);
            Ok(IdlValue::Object(super::resolved(
                agent,
                realm,
                Value::Undefined,
            )))
        })(),
        ("ReadableStream", "getReader") => (|| {
            let s = super::this_data::<StreamObject>(agent, inv)?;
            if inv
                .arg(0)
                .as_dict()
                .and_then(|d| d.str("mode"))
                .is_some_and(|m| m == "byob")
            {
                return agent.type_error("BYOB readers are not supported.");
            }
            acquire_reader(agent, state, &s.0, None)
        })(),
        ("ReadableStreamDefaultReader", "constructor") => (|| {
            let Some(s) = inv.object(0).and_then(|o| stream_of(agent, o)) else {
                return agent.type_error("The value is not a ReadableStream.");
            };
            let proto =
                crate::bindings::install::proto_from_new_target(agent, inv.new_target, inv.iface)?;
            acquire_reader(agent, state, &s, proto)
        })(),
        ("ReadableStreamDefaultReader", "read") => (|| {
            let r = super::this_data::<ReaderObject>(agent, inv)?;
            let s = r.stream.borrow().clone();
            match s {
                Some(s) => Ok(IdlValue::Any(read(agent, &s))),
                None => super::rejected_type_error(agent, realm, "The reader was released."),
            }
        })(),
        ("ReadableStreamDefaultReader", "releaseLock") => (|| {
            let r = super::this_data::<ReaderObject>(agent, inv)?;
            if let Some(s) = r.stream.borrow_mut().take() {
                s.borrow_mut().locked = false;
                // Pending reads fail (Streams §4.3.7, release).
                let pending: Vec<Pending> = s.borrow_mut().read_requests.drain(..).collect();
                for p in pending {
                    let e = super::type_error_value(agent, realm, "The reader was released.");
                    p.deferred.reject(agent, e);
                }
            }
            Ok(IdlValue::Undefined)
        })(),
        ("ReadableStreamDefaultReader", "get_closed") => (|| {
            let r = super::this_data::<ReaderObject>(agent, inv)?;
            let this = inv.this_object().expect("a reader");
            let p = crate::bindings::platform::platform_ref(agent, Value::Object(this))
                .expect("a platform object");
            if let Some(v) = p.get().slot("[[closed]]") {
                return Ok(IdlValue::Any(v));
            }
            let s = r.stream.borrow().clone();
            let promise = match s {
                None => {
                    let e = super::type_error_value(agent, realm, "The reader was released.");
                    super::rejected(agent, realm, e)
                }
                Some(s) => {
                    let (closed, errored) = {
                        let st = s.borrow();
                        (st.closed, st.errored.clone())
                    };
                    if closed {
                        super::resolved(agent, realm, Value::Undefined)
                    } else if let Some(m) = errored {
                        let e = super::type_error_value(agent, realm, &m);
                        super::rejected(agent, realm, e)
                    } else {
                        let pending = Pending::new(agent, realm);
                        let promise = pending.deferred.promise;
                        s.borrow_mut().closed_promises.push(pending);
                        promise
                    }
                }
            };
            p.get().set_slot("[[closed]]", Value::Object(promise));
            Ok(IdlValue::Object(promise))
        })(),
        ("ReadableStreamDefaultReader", "cancel") => (|| {
            let r = super::this_data::<ReaderObject>(agent, inv)?;
            let s = r.stream.borrow().clone();
            match s {
                Some(s) => {
                    cancel(agent, &s);
                    Ok(IdlValue::Object(super::resolved(
                        agent,
                        realm,
                        Value::Undefined,
                    )))
                }
                None => super::rejected_type_error(agent, realm, "The reader was released."),
            }
        })(),
        _ => return None,
    })
}
