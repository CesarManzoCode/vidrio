//! `URL` and `URLSearchParams` (URL §6) over the URL parser of k3-06,
//! `URL.createObjectURL`/`revokeObjectURL` (File API §8.3), and
//! `TextEncoder`/`TextDecoder` (Encoding §7) over its encodings.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use vjs::{Agent, Value};

use super::R;
use super::blob::BlobData;
use crate::bindings::platform::Native;
use crate::bindings::{IdlValue, Invocation, RealmState};
use crate::encoding::{BomHandling, Decoder, DecoderErrorMode, Encoding};
use crate::url::Url;

/// The native state of a `URL` object.
pub struct UrlObject {
    pub url: RefCell<Url>,
    pub query: Rc<SearchParams>,
}

/// The native state of a `URLSearchParams` object: its list and the
/// `URL` object it belongs to, if any.
pub struct SearchParams {
    pub list: RefCell<Vec<(String, String)>>,
    pub url: RefCell<Weak<UrlObject>>,
}

impl SearchParams {
    pub fn serialize(&self) -> String {
        crate::url::form::serialize(
            self.list
                .borrow()
                .iter()
                .map(|(a, b)| (a.as_str(), b.as_str())),
        )
    }

    /// "Update": the URL's query becomes the serialization.
    fn update(&self) {
        let Some(u) = self.url.borrow().upgrade() else {
            return;
        };
        let s = self.serialize();
        u.url.borrow_mut().query = if s.is_empty() { None } else { Some(s) };
    }
}

fn query_list(url: &Url) -> Vec<(String, String)> {
    url.query
        .as_ref()
        .map(|q| crate::url::form::parse(q.as_bytes()))
        .unwrap_or_default()
}

/// The URL parser with an optional base string (URL §6.1, "API URL
/// parser").
fn api_parse(url: &str, base: Option<&str>) -> Option<Url> {
    let base = match base {
        Some(b) => Some(Url::parse(b, None).ok()?),
        None => None,
    };
    Url::parse(url, base.as_ref()).ok()
}

fn url_object(
    agent: &mut Agent,
    state: &RealmState,
    url: Url,
    proto: Option<vjs::Object>,
) -> vjs::Object {
    let query = Rc::new(SearchParams {
        list: RefCell::new(query_list(&url)),
        url: RefCell::new(Weak::new()),
    });
    let data = Rc::new(UrlObject {
        url: RefCell::new(url),
        query,
    });
    *data.query.url.borrow_mut() = Rc::downgrade(&data);
    super::create_object(agent, state, "URL", Native::Data(data), proto)
}

pub fn call(
    agent: &mut Agent,
    state: &Rc<RealmState>,
    inv: &Invocation,
    owner: &str,
    member: &str,
) -> Option<R> {
    if owner == "URLSearchParams" {
        return Some(search_params(agent, state, inv, member));
    }
    match member {
        "constructor" => {
            return Some((|| {
                let Some(url) = api_parse(&inv.str(0), inv.opt_str(1).as_deref()) else {
                    return agent.type_error(&format!("Invalid URL: {}", inv.str(0)));
                };
                let proto = crate::bindings::install::proto_from_new_target(
                    agent,
                    inv.new_target,
                    inv.iface,
                )?;
                Ok(IdlValue::Object(url_object(agent, state, url, proto)))
            })());
        }
        "static_parse" => {
            return Some(Ok(
                match api_parse(&inv.str(0), inv.opt_str(1).as_deref()) {
                    Some(url) => IdlValue::Object(url_object(agent, state, url, None)),
                    None => IdlValue::Null,
                },
            ));
        }
        "static_canParse" => {
            return Some(Ok(IdlValue::Bool(
                api_parse(&inv.str(0), inv.opt_str(1).as_deref()).is_some(),
            )));
        }
        "static_createObjectURL" => {
            return Some((|| {
                let Some(b) = inv
                    .object(0)
                    .and_then(|o| super::object_data::<BlobData>(agent, o))
                else {
                    return agent.type_error("The value is not of type 'Blob'.");
                };
                let origin = super::api::client(state).0;
                Ok(IdlValue::String(super::blob::add_url(b, &origin)))
            })());
        }
        "static_revokeObjectURL" => {
            if let Ok(u) = Url::parse(&inv.str(0), None) {
                if u.scheme == "blob" {
                    super::blob::revoke_url(&u.serialize(true));
                }
            }
            return Some(Ok(IdlValue::Undefined));
        }
        _ => {}
    }
    let u = match super::this_data::<UrlObject>(agent, inv) {
        Ok(u) => u,
        Err(e) => return Some(Err(e)),
    };
    let s = |v: String| Ok(IdlValue::String(v));
    let value = inv.str(0);
    Some(match member {
        "get_href" | "toJSON" | "stringifier" => s(u.url.borrow().href()),
        "get_origin" => s(u.url.borrow().origin().serialize()),
        "get_protocol" => s(format!("{}:", u.url.borrow().scheme)),
        "get_username" => s(u.url.borrow().username.clone()),
        "get_password" => s(u.url.borrow().password.clone()),
        "get_host" => {
            let url = u.url.borrow();
            s(match (&url.host, url.port) {
                (Some(h), Some(p)) => format!("{h}:{p}"),
                (Some(h), None) => h.to_string(),
                _ => String::new(),
            })
        }
        "get_hostname" => s(u
            .url
            .borrow()
            .host
            .as_ref()
            .map(|h| h.to_string())
            .unwrap_or_default()),
        "get_port" => s(u
            .url
            .borrow()
            .port
            .map(|p| p.to_string())
            .unwrap_or_default()),
        "get_pathname" => s(u.url.borrow().path_string()),
        "get_search" => s(match &u.url.borrow().query {
            Some(q) if !q.is_empty() => format!("?{q}"),
            _ => String::new(),
        }),
        "get_hash" => s(match &u.url.borrow().fragment {
            Some(f) if !f.is_empty() => format!("#{f}"),
            _ => String::new(),
        }),
        "get_searchParams" => Ok(IdlValue::Object(super::create_object(
            agent,
            state,
            "URLSearchParams",
            Native::Data(u.query.clone()),
            None,
        ))),
        "set_href" => {
            let Ok(parsed) = Url::parse(&value, None) else {
                return Some(agent.type_error(&format!("Invalid URL: {value}")));
            };
            *u.query.list.borrow_mut() = query_list(&parsed);
            *u.url.borrow_mut() = parsed;
            Ok(IdlValue::Undefined)
        }
        "set_protocol" | "set_username" | "set_password" | "set_host" | "set_hostname"
        | "set_port" | "set_pathname" | "set_hash" => {
            let mut url = u.url.borrow_mut();
            match member {
                "set_protocol" => url.set_protocol(&value),
                "set_username" => url.set_username(&value),
                "set_password" => url.set_password(&value),
                "set_host" => url.set_host(&value),
                "set_hostname" => url.set_hostname(&value),
                "set_port" => url.set_port(&value),
                "set_pathname" => url.set_pathname(&value),
                _ => url.set_hash(&value),
            }
            Ok(IdlValue::Undefined)
        }
        "set_search" => {
            u.url.borrow_mut().set_search(&value);
            let list = query_list(&u.url.borrow());
            *u.query.list.borrow_mut() = list;
            Ok(IdlValue::Undefined)
        }
        _ => return None,
    })
}

fn search_params(agent: &mut Agent, state: &Rc<RealmState>, inv: &Invocation, member: &str) -> R {
    if member == "constructor" {
        let list = match inv.arg(0) {
            IdlValue::Sequence(items) => {
                let mut out = Vec::new();
                for item in items {
                    let pair = item.as_seq();
                    if pair.len() != 2 {
                        return agent.type_error("A pair must have exactly two items.");
                    }
                    out.push((pair[0].as_str().to_string(), pair[1].as_str().to_string()));
                }
                out
            }
            IdlValue::Record(entries) => entries
                .iter()
                .map(|(k, v)| (k.clone(), v.as_str().to_string()))
                .collect(),
            IdlValue::String(s) => {
                let s = s.strip_prefix('?').unwrap_or(s);
                crate::url::form::parse(s.as_bytes())
            }
            _ => Vec::new(),
        };
        let data = Rc::new(SearchParams {
            list: RefCell::new(list),
            url: RefCell::new(Weak::new()),
        });
        let proto =
            crate::bindings::install::proto_from_new_target(agent, inv.new_target, inv.iface)?;
        let o = super::create_object(agent, state, "URLSearchParams", Native::Data(data), proto);
        return Ok(IdlValue::Object(o));
    }
    let p = super::this_data::<SearchParams>(agent, inv)?;
    let name = inv.str(0);
    let value = inv.opt_str(1);
    let matches = |(n, v): &(String, String)| *n == name && value.as_ref().is_none_or(|x| x == v);
    let r = match member {
        "get_size" => IdlValue::UInt(p.list.borrow().len() as u64),
        "append" => {
            p.list.borrow_mut().push((name.clone(), inv.str(1)));
            p.update();
            IdlValue::Undefined
        }
        "delete" => {
            p.list.borrow_mut().retain(|e| !matches(e));
            p.update();
            IdlValue::Undefined
        }
        "get" => IdlValue::opt_string(
            p.list
                .borrow()
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, v)| v.clone()),
        ),
        "getAll" => IdlValue::Sequence(
            p.list
                .borrow()
                .iter()
                .filter(|(n, _)| *n == name)
                .map(|(_, v)| IdlValue::String(v.clone()))
                .collect(),
        ),
        "has" => IdlValue::Bool(p.list.borrow().iter().any(matches)),
        "set" => {
            let v = inv.str(1);
            {
                let mut list = p.list.borrow_mut();
                match list.iter().position(|(n, _)| *n == name) {
                    Some(i) => {
                        list[i].1 = v;
                        let mut seen = false;
                        list.retain(|(n, _)| {
                            if *n != name {
                                return true;
                            }
                            let keep = !seen;
                            seen = true;
                            keep
                        });
                    }
                    None => list.push((name.clone(), v)),
                }
            }
            p.update();
            IdlValue::Undefined
        }
        "sort" => {
            p.list
                .borrow_mut()
                .sort_by(|a, b| a.0.encode_utf16().cmp(b.0.encode_utf16()));
            p.update();
            IdlValue::Undefined
        }
        "stringifier" => IdlValue::String(p.serialize()),
        "iterable_pairs" => IdlValue::Sequence(
            p.list
                .borrow()
                .iter()
                .map(|(k, v)| {
                    IdlValue::Sequence(vec![
                        IdlValue::String(k.clone()),
                        IdlValue::String(v.clone()),
                    ])
                })
                .collect(),
        ),
        _ => return agent.type_error(&format!("URLSearchParams.{member} is not supported")),
    };
    Ok(r)
}

// ----- TextEncoder and TextDecoder ------------------------------------------------------------

/// The native state of a `TextDecoder`.
pub struct DecoderObject {
    encoding: Encoding,
    fatal: bool,
    ignore_bom: bool,
    decoder: RefCell<Decoder>,
    /// The "do not flush" flag.
    streaming: Cell<bool>,
}

impl DecoderObject {
    fn fresh(&self) -> Decoder {
        Decoder::new(
            self.encoding,
            if self.fatal {
                DecoderErrorMode::Fatal
            } else {
                DecoderErrorMode::Replacement
            },
            if self.ignore_bom {
                BomHandling::Keep
            } else {
                BomHandling::Strip
            },
        )
    }
}

/// The `TextEncoder` marker state.
pub struct EncoderObject;

pub fn encoding_call(
    agent: &mut Agent,
    state: &Rc<RealmState>,
    inv: &Invocation,
    owner: &str,
    member: &str,
) -> Option<R> {
    let realm = state.realm;
    Some(match (owner, member) {
        ("TextEncoder", "constructor") => (|| {
            let proto =
                crate::bindings::install::proto_from_new_target(agent, inv.new_target, inv.iface)?;
            let o = super::create_object(
                agent,
                state,
                "TextEncoder",
                Native::Data(Rc::new(EncoderObject)),
                proto,
            );
            Ok(IdlValue::Object(o))
        })(),
        ("TextEncoderCommon", "get_encoding") => Ok(IdlValue::string("utf-8")),
        ("TextEncoder", "encode") => {
            let bytes = inv.str(0).into_bytes();
            Ok(IdlValue::Object(super::uint8_array(agent, realm, bytes)))
        }
        ("TextEncoder", "encodeInto") => encode_into(agent, inv),
        ("TextDecoder", "constructor") => (|| {
            let label = inv.str(0);
            let encoding = match Encoding::for_label(&label) {
                Some(e) if e != Encoding::Replacement => e,
                _ => {
                    return agent.range_error(&format!("The encoding label '{label}' is invalid."));
                }
            };
            let options = inv.arg(1).as_dict();
            let fatal = options.and_then(|d| d.bool("fatal")).unwrap_or(false);
            let ignore_bom = options.and_then(|d| d.bool("ignoreBOM")).unwrap_or(false);
            let mut data = DecoderObject {
                encoding,
                fatal,
                ignore_bom,
                decoder: RefCell::new(Decoder::new(
                    Encoding::Utf8,
                    DecoderErrorMode::Replacement,
                    BomHandling::Keep,
                )),
                streaming: Cell::new(false),
            };
            data.decoder = RefCell::new(data.fresh());
            let proto =
                crate::bindings::install::proto_from_new_target(agent, inv.new_target, inv.iface)?;
            let o = super::create_object(
                agent,
                state,
                "TextDecoder",
                Native::Data(Rc::new(data)),
                proto,
            );
            Ok(IdlValue::Object(o))
        })(),
        ("TextDecoderCommon", m) => (|| {
            let d = super::this_data::<DecoderObject>(agent, inv)?;
            Ok(match m {
                "get_encoding" => IdlValue::String(d.encoding.name().to_ascii_lowercase()),
                "get_fatal" => IdlValue::Bool(d.fatal),
                "get_ignoreBOM" => IdlValue::Bool(d.ignore_bom),
                _ => return agent.type_error("Illegal invocation"),
            })
        })(),
        ("TextDecoder", "decode") => (|| {
            let d = super::this_data::<DecoderObject>(agent, inv)?;
            if !d.streaming.get() {
                *d.decoder.borrow_mut() = d.fresh();
            }
            let stream = inv
                .arg(1)
                .as_dict()
                .and_then(|o| o.bool("stream"))
                .unwrap_or(false);
            d.streaming.set(stream);
            let bytes = match inv.object(0) {
                Some(o) => super::buffer_source_bytes(agent, o).unwrap_or_default(),
                None => Vec::new(),
            };
            let mut out = String::new();
            let r = d.decoder.borrow_mut().decode(&bytes, !stream, &mut out);
            if r.is_err() {
                *d.decoder.borrow_mut() = d.fresh();
                d.streaming.set(false);
                return agent.type_error("The encoded data was not valid.");
            }
            Ok(IdlValue::String(out))
        })(),
        _ => return None,
    })
}

/// `encodeInto(source, destination)`: as many whole scalar values as fit.
fn encode_into(agent: &mut Agent, inv: &Invocation) -> R {
    let source = inv.str(0);
    let Some(dest) = inv.object(1) else {
        return agent.type_error("The destination is not a Uint8Array.");
    };
    let Some((_, buffer, offset, _)) = vjs::buffer::typed_array_slots(agent, dest) else {
        return agent.type_error("The destination is not a Uint8Array.");
    };
    let available = vjs::buffer::typed_array_length(agent, dest).unwrap_or(0);
    let mut out = Vec::new();
    let mut read = 0u64;
    for c in source.chars() {
        let mut buf = [0u8; 4];
        let encoded = c.encode_utf8(&mut buf);
        if out.len() + encoded.len() > available {
            break;
        }
        out.extend_from_slice(encoded.as_bytes());
        read += c.len_utf16() as u64;
    }
    if !out.is_empty() {
        vjs::buffer::write_bytes(agent, buffer, offset, &out);
    }
    let o = vjs::ops::object_create(agent);
    for (k, v) in [("read", read as f64), ("written", out.len() as f64)] {
        let key = agent.heap.key_from_str(k);
        vjs::ops::create_data_property_or_throw(agent, o, key, Value::Number(v))?;
    }
    Ok(IdlValue::Any(Value::Object(o)))
}
