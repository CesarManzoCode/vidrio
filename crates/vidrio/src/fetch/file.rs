//! `Blob`, `File` and `FileReader` (File API §3–§6) and `FormData` (XHR
//! §5).

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use vjs::{Agent, JsResult, Object, Value};

use super::blob::{self, BlobData, EntryList, FileInfo, FormValue};
use super::{R, stream};
use crate::bindings::exception::dom;
use crate::bindings::platform::{self, Native};
use crate::bindings::{IdlValue, Invocation, RealmState};
use crate::browser::event_loop::queue_task;
use crate::browser::{TaskSource, in_realm};
use crate::dom::{DomException, Namespace, NodeData};
use crate::encoding::{BomHandling, Decoder, DecoderErrorMode, Encoding};

/// A `Blob` or `File` object for `data`.
pub fn blob_object(
    agent: &mut Agent,
    state: &RealmState,
    data: Rc<BlobData>,
    proto: Option<Object>,
) -> Object {
    let iface = if data.file.is_some() { "File" } else { "Blob" };
    super::create_object(agent, state, iface, Native::Data(data), proto)
}

/// The native state of a `FormData` object.
pub struct FormDataObject {
    pub entries: RefCell<EntryList>,
}

/// A `FormData` object holding `entries`.
pub fn form_data_object(
    agent: &mut Agent,
    state: &RealmState,
    entries: EntryList,
    proto: Option<Object>,
) -> Object {
    let data = Rc::new(FormDataObject {
        entries: RefCell::new(entries),
    });
    super::create_object(agent, state, "FormData", Native::Data(data), proto)
}

/// The bytes of a sequence of `BlobPart`s (File API §3.1.1, "process
/// blob parts").
fn blob_parts(agent: &mut Agent, parts: &IdlValue, native_endings: bool) -> JsResult<Vec<u8>> {
    let mut bytes = Vec::new();
    for part in parts.as_seq() {
        match part {
            IdlValue::String(s) => {
                if native_endings {
                    bytes.extend_from_slice(blob::native_line_endings(s).as_bytes());
                } else {
                    bytes.extend_from_slice(s.as_bytes());
                }
            }
            IdlValue::Object(o) | IdlValue::Any(Value::Object(o)) => {
                if let Some(b) = super::object_data::<BlobData>(agent, *o) {
                    bytes.extend_from_slice(&b.bytes);
                } else if let Some(b) = super::buffer_source_bytes(agent, *o) {
                    bytes.extend_from_slice(&b);
                } else {
                    let s = crate::bindings::convert::to_rust_string(agent, Value::Object(*o))?;
                    bytes.extend_from_slice(s.as_bytes());
                }
            }
            other => {
                let v = super::convert_any(agent, other)?;
                let s = crate::bindings::convert::to_rust_string(agent, v)?;
                bytes.extend_from_slice(s.as_bytes());
            }
        }
    }
    Ok(bytes)
}

fn opt_i64(v: &IdlValue) -> Option<i64> {
    match v {
        IdlValue::Missing | IdlValue::Undefined | IdlValue::Null => None,
        v => Some(v.as_i64()),
    }
}

/// Decodes `bytes` for `text()`/`readAsText`: a BOM wins, then
/// `encoding`, then the `charset` of `type_`, then UTF-8.
fn decode_text(bytes: &[u8], encoding: Option<&str>, type_: &str) -> String {
    let e = encoding
        .and_then(Encoding::for_label)
        .or_else(|| crate::browser::loader::charset_of(Some(type_)))
        .unwrap_or(Encoding::Utf8);
    let mut decoder = Decoder::new(e, DecoderErrorMode::Replacement, BomHandling::Sniff);
    let mut out = String::new();
    let _ = decoder.decode(bytes, true, &mut out);
    out
}

pub fn call(
    agent: &mut Agent,
    state: &Rc<RealmState>,
    inv: &Invocation,
    owner: &str,
    member: &str,
) -> Option<R> {
    match owner {
        "Blob" | "File" => blob_member(agent, state, inv, owner, member),
        "FileReader" => reader_member(agent, state, inv, member),
        "FormData" => Some(form_data_member(agent, state, inv, member)),
        _ => None,
    }
}

fn blob_member(
    agent: &mut Agent,
    state: &Rc<RealmState>,
    inv: &Invocation,
    owner: &str,
    member: &str,
) -> Option<R> {
    let realm = state.realm;
    if member == "constructor" {
        return Some((|| {
            let (parts, options) = if owner == "File" {
                (inv.arg(0), inv.arg(2).as_dict())
            } else {
                (inv.arg(0), inv.arg(1).as_dict())
            };
            let native = options
                .and_then(|d| d.str("endings"))
                .is_some_and(|e| e == "native");
            let bytes = blob_parts(agent, parts, native)?;
            let type_ = blob::normalize_type(options.and_then(|d| d.str("type")).unwrap_or(""));
            let file = (owner == "File").then(|| FileInfo {
                name: inv.str(1),
                last_modified: options
                    .and_then(|d| d.get("lastModified"))
                    .map_or_else(blob::now_ms, IdlValue::as_i64),
            });
            let data = Rc::new(BlobData {
                bytes: bytes.into(),
                type_,
                file,
            });
            let proto =
                crate::bindings::install::proto_from_new_target(agent, inv.new_target, inv.iface)?;
            Ok(IdlValue::Object(blob_object(agent, state, data, proto)))
        })());
    }
    let b = match super::this_data::<BlobData>(agent, inv) {
        Ok(b) => b,
        Err(e) => return Some(Err(e)),
    };
    Some(match member {
        "get_size" => Ok(IdlValue::UInt(b.bytes.len() as u64)),
        "get_type" => Ok(IdlValue::String(b.type_.clone())),
        "get_name" => Ok(IdlValue::String(
            b.file.as_ref().map(|f| f.name.clone()).unwrap_or_default(),
        )),
        "get_lastModified" => Ok(IdlValue::Int(
            b.file.as_ref().map_or(0, |f| f.last_modified),
        )),
        "slice" => {
            let (start, end) =
                blob::relative_range(b.bytes.len(), opt_i64(inv.arg(0)), opt_i64(inv.arg(1)));
            let type_ = match inv.arg(2) {
                IdlValue::String(t) => blob::normalize_type(t),
                _ => String::new(),
            };
            let data = Rc::new(b.slice(start, end, &type_));
            Ok(IdlValue::Object(blob_object(agent, state, data, None)))
        }
        "stream" => {
            let s = stream::from_bytes(realm, b.bytes.to_vec());
            Ok(IdlValue::Object(stream::stream_object(agent, state, &s)))
        }
        "text" => {
            let s = crate::encoding::utf8_decode(&b.bytes);
            let v = super::js_string(agent, &s);
            Ok(IdlValue::Object(super::resolved(agent, realm, v)))
        }
        "arrayBuffer" => {
            let o = super::array_buffer(agent, realm, b.bytes.to_vec());
            Ok(IdlValue::Object(super::resolved(
                agent,
                realm,
                Value::Object(o),
            )))
        }
        "bytes" => {
            let o = super::uint8_array(agent, realm, b.bytes.to_vec());
            Ok(IdlValue::Object(super::resolved(
                agent,
                realm,
                Value::Object(o),
            )))
        }
        _ => return None,
    })
}

// ----- FileReader ------------------------------------------------------------------------

/// The native state of a `FileReader` (its host id is its event target).
pub struct ReaderState {
    target: u64,
    state: Cell<u16>,
    /// Bumped by each read and abort: tasks of an older read stop.
    generation: Cell<u64>,
}

const EMPTY: u16 = 0;
const LOADING: u16 = 1;
const DONE: u16 = 2;

#[derive(Clone, Copy)]
enum ReadKind {
    ArrayBuffer,
    BinaryString,
    Text,
    DataUrl,
}

fn reader_member(
    agent: &mut Agent,
    state: &Rc<RealmState>,
    inv: &Invocation,
    member: &str,
) -> Option<R> {
    if member == "constructor" {
        return Some((|| {
            let target = super::new_target_id(state);
            let data = Rc::new(ReaderState {
                target,
                state: Cell::new(EMPTY),
                generation: Cell::new(0),
            });
            let proto =
                crate::bindings::install::proto_from_new_target(agent, inv.new_target, inv.iface)?;
            let o = super::create_object(
                agent,
                state,
                "FileReader",
                Native::TargetData(target, data),
                proto,
            );
            state.targets.borrow_mut().insert(target, o);
            Ok(IdlValue::Object(o))
        })());
    }
    let r = match super::this_data::<ReaderState>(agent, inv) {
        Ok(r) => r,
        Err(e) => return Some(Err(e)),
    };
    let this = inv.this.clone().expect("a FileReader");
    let kind = match member {
        "readAsArrayBuffer" => Some(ReadKind::ArrayBuffer),
        "readAsBinaryString" => Some(ReadKind::BinaryString),
        "readAsText" => Some(ReadKind::Text),
        "readAsDataURL" => Some(ReadKind::DataUrl),
        _ => None,
    };
    if let Some(kind) = kind {
        return Some(read(agent, state, inv, &r, kind));
    }
    Some(match member {
        "get_readyState" => Ok(IdlValue::Int(i64::from(r.state.get()))),
        "get_result" => Ok(IdlValue::Any(
            this.get().slot("[[result]]").unwrap_or(Value::Null),
        )),
        "get_error" => Ok(IdlValue::Any(
            this.get().slot("[[error]]").unwrap_or(Value::Null),
        )),
        "abort" => {
            match r.state.get() {
                EMPTY | DONE => this.get().set_slot("[[result]]", Value::Null),
                _ => {
                    r.state.set(DONE);
                    this.get().set_slot("[[result]]", Value::Null);
                    r.generation.set(r.generation.get() + 1);
                    super::fire_event(agent, state, r.target, "abort", Some((false, 0.0, 0.0)));
                    if r.state.get() != LOADING {
                        super::fire_event(
                            agent,
                            state,
                            r.target,
                            "loadend",
                            Some((false, 0.0, 0.0)),
                        );
                    }
                }
            }
            Ok(IdlValue::Undefined)
        }
        _ => return None,
    })
}

/// "Read operation" (File API §6.4): `loadstart` in a task, then
/// `progress`, `load` and `loadend` in another.
fn read(
    agent: &mut Agent,
    state: &Rc<RealmState>,
    inv: &Invocation,
    r: &Rc<ReaderState>,
    kind: ReadKind,
) -> R {
    if r.state.get() == LOADING {
        return dom(agent, DomException::InvalidStateError);
    }
    let Some(b) = inv
        .object(0)
        .and_then(|o| super::object_data::<BlobData>(agent, o))
    else {
        return agent.type_error("The value is not of type 'Blob'.");
    };
    let this = inv.this.clone().expect("a FileReader");
    r.state.set(LOADING);
    this.get().set_slot("[[result]]", Value::Null);
    this.get().set_slot("[[error]]", Value::Null);
    let generation = r.generation.get() + 1;
    r.generation.set(generation);
    let encoding = match kind {
        ReadKind::Text => inv.opt_str(1),
        _ => None,
    };
    let doc = state.document();
    let (st, rs) = (state.clone(), r.clone());
    queue_task(TaskSource::DomManipulation, doc.clone(), move |agent| {
        if rs.generation.get() != generation {
            return;
        }
        let total = b.bytes.len() as f64;
        super::fire_event(agent, &st, rs.target, "loadstart", Some((true, 0.0, total)));
        let (st2, rs2) = (st.clone(), rs.clone());
        queue_task(TaskSource::DomManipulation, doc, move |agent| {
            if rs2.generation.get() != generation {
                return;
            }
            super::fire_event(
                agent,
                &st2,
                rs2.target,
                "progress",
                Some((true, total, total)),
            );
            if rs2.generation.get() != generation {
                return;
            }
            let realm = st2.realm;
            let result = in_realm(agent, realm, |agent| match kind {
                ReadKind::ArrayBuffer => {
                    Value::Object(super::array_buffer(agent, realm, b.bytes.to_vec()))
                }
                ReadKind::BinaryString => {
                    let s = crate::encoding::isomorphic_decode(&b.bytes);
                    super::js_string(agent, &s)
                }
                ReadKind::Text => {
                    let s = decode_text(&b.bytes, encoding.as_deref(), &b.type_);
                    super::js_string(agent, &s)
                }
                ReadKind::DataUrl => {
                    let type_ = if b.type_.is_empty() {
                        "application/octet-stream"
                    } else {
                        b.type_.as_str()
                    };
                    let encoded = vjs::base64::to_base64(&b.bytes, false, false);
                    super::js_string(agent, &format!("data:{type_};base64,{encoded}"))
                }
            });
            rs2.state.set(DONE);
            if let Some(o) = st2.targets.borrow().get(&rs2.target).copied() {
                if let Some(p) = platform::platform_ref(agent, Value::Object(o)) {
                    p.get().set_slot("[[result]]", result);
                }
            }
            super::fire_event(agent, &st2, rs2.target, "load", Some((true, total, total)));
            if rs2.state.get() != LOADING {
                super::fire_event(
                    agent,
                    &st2,
                    rs2.target,
                    "loadend",
                    Some((true, total, total)),
                );
            }
        });
    });
    Ok(IdlValue::Undefined)
}

// ----- FormData ---------------------------------------------------------------------------

/// The value of an entry as an IDL value (`File` entries become new
/// `File` objects).
fn entry_value(agent: &mut Agent, state: &RealmState, v: &FormValue) -> IdlValue {
    match v {
        FormValue::Text(s) => IdlValue::String(s.clone()),
        FormValue::File(f) => IdlValue::Object(blob_object(agent, state, f.clone(), None)),
    }
}

/// The entry of `append`/`set`: overload 0 takes a string, overload 1 a
/// blob and an optional file name.
fn new_entry(agent: &mut Agent, inv: &Invocation) -> JsResult<(String, FormValue)> {
    let name = inv.str(0);
    if inv.overload == 0 {
        return Ok((name, FormValue::Text(inv.str(1))));
    }
    let Some(b) = inv
        .object(1)
        .and_then(|o| super::object_data::<BlobData>(agent, o))
    else {
        return agent.type_error("The value is not of type 'Blob'.");
    };
    let filename = inv.opt_str(2);
    Ok((
        name,
        FormValue::File(blob::file_entry(&b, filename.as_deref())),
    ))
}

/// The entry list of a form (HTML §4.10.21.4, simplified): its named,
/// enabled `input`, `select` and `textarea` descendants by their content
/// attributes.
fn form_entries(doc: &crate::dom::Document, form: crate::dom::NodeId) -> EntryList {
    let mut out = Vec::new();
    for n in doc.inclusive_descendants(form) {
        let Some(e) = doc.element(n) else {
            continue;
        };
        if e.name.ns != Some(Namespace::Html) || e.attr("disabled").is_some() {
            continue;
        }
        let Some(name) = e.attr("name").filter(|s| !s.is_empty()) else {
            continue;
        };
        match e.name.local.as_str() {
            "input" => {
                let t = e.attr("type").unwrap_or("text").to_ascii_lowercase();
                match t.as_str() {
                    "submit" | "button" | "reset" | "image" | "file" => {}
                    "checkbox" | "radio" => {
                        if e.attr("checked").is_some() {
                            let v = e.attr("value").unwrap_or("on");
                            out.push((name.to_string(), FormValue::Text(v.to_string())));
                        }
                    }
                    _ => {
                        let v = e.attr("value").unwrap_or("");
                        out.push((name.to_string(), FormValue::Text(v.to_string())));
                    }
                }
            }
            "textarea" => {
                let v = doc.child_text_content(n);
                out.push((name.to_string(), FormValue::Text(v)));
            }
            "select" => {
                let options: Vec<_> = doc
                    .descendant_elements(n)
                    .into_iter()
                    .filter(|o| doc.is_html_element(*o, "option"))
                    .collect();
                let option_value = |o: crate::dom::NodeId| {
                    doc.get_attribute(o, "value")
                        .map(str::to_string)
                        .unwrap_or_else(|| doc.child_text_content(o))
                };
                let selected: Vec<_> = options
                    .iter()
                    .filter(|o| doc.get_attribute(**o, "selected").is_some())
                    .copied()
                    .collect();
                if selected.is_empty() {
                    if e.attr("multiple").is_none() {
                        if let Some(first) = options.first() {
                            out.push((name.to_string(), FormValue::Text(option_value(*first))));
                        }
                    }
                } else {
                    for o in selected {
                        out.push((name.to_string(), FormValue::Text(option_value(o))));
                    }
                }
            }
            _ => {}
        }
    }
    out
}

fn form_data_member(
    agent: &mut Agent,
    state: &Rc<RealmState>,
    inv: &Invocation,
    member: &str,
) -> R {
    if member == "constructor" {
        let entries = match inv.object(0) {
            Some(o) => {
                let node =
                    platform::platform_ref(agent, Value::Object(o)).and_then(|p| {
                        match &*p.get().native.borrow() {
                            Native::Node(d, n) => Some((d.clone(), *n)),
                            _ => None,
                        }
                    });
                match node {
                    Some((d, n)) if matches!(d.borrow().data(n), NodeData::Element(_)) => {
                        form_entries(&d.borrow(), n)
                    }
                    _ => return agent.type_error("The value is not of type 'HTMLFormElement'."),
                }
            }
            None => Vec::new(),
        };
        let proto =
            crate::bindings::install::proto_from_new_target(agent, inv.new_target, inv.iface)?;
        return Ok(IdlValue::Object(form_data_object(
            agent, state, entries, proto,
        )));
    }
    let f = super::this_data::<FormDataObject>(agent, inv)?;
    let name = inv.str(0);
    match member {
        "append" => {
            let entry = new_entry(agent, inv)?;
            f.entries.borrow_mut().push(entry);
            Ok(IdlValue::Undefined)
        }
        "set" => {
            let entry = new_entry(agent, inv)?;
            let mut entries = f.entries.borrow_mut();
            match entries.iter().position(|(n, _)| *n == entry.0) {
                Some(i) => {
                    entries[i] = entry.clone();
                    let mut seen = false;
                    entries.retain(|(n, _)| {
                        if *n != entry.0 {
                            return true;
                        }
                        let keep = !seen;
                        seen = true;
                        keep
                    });
                }
                None => entries.push(entry),
            }
            Ok(IdlValue::Undefined)
        }
        "delete" => {
            f.entries.borrow_mut().retain(|(n, _)| *n != name);
            Ok(IdlValue::Undefined)
        }
        "get" => {
            let v = f
                .entries
                .borrow()
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, v)| v.clone());
            Ok(match v {
                Some(v) => entry_value(agent, state, &v),
                None => IdlValue::Null,
            })
        }
        "getAll" => {
            let values: Vec<FormValue> = f
                .entries
                .borrow()
                .iter()
                .filter(|(n, _)| *n == name)
                .map(|(_, v)| v.clone())
                .collect();
            Ok(IdlValue::Sequence(
                values
                    .iter()
                    .map(|v| entry_value(agent, state, v))
                    .collect(),
            ))
        }
        "has" => Ok(IdlValue::Bool(
            f.entries.borrow().iter().any(|(n, _)| *n == name),
        )),
        "iterable_pairs" => {
            let entries = f.entries.borrow().clone();
            Ok(IdlValue::Sequence(
                entries
                    .iter()
                    .map(|(n, v)| {
                        IdlValue::Sequence(vec![
                            IdlValue::String(n.clone()),
                            entry_value(agent, state, v),
                        ])
                    })
                    .collect(),
            ))
        }
        _ => agent.type_error(&format!("FormData.{member} is not supported")),
    }
}
