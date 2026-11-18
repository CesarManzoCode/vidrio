//! Structured serialization (HTML §2.7): StructuredSerializeWithTransfer
//! and StructuredDeserializeWithTransfer for `postMessage`,
//! `structuredClone` and `history.pushState`.
//!
//! Supported: primitives except symbols; `Boolean`, `Number`, `String`
//! and `BigInt` objects; `Date`; `RegExp`; `ArrayBuffer` (transferable),
//! typed arrays and `DataView`; `Map`; `Set`; error objects; arrays and
//! ordinary objects (own enumerable string-keyed properties), with cycles
//! and shared references kept; and `MessagePort` (transferable only).
//! Everything else (functions, symbols, proxies, other platform objects,
//! shared buffers) throws a `DataCloneError`.

use std::collections::HashMap;

use vjs::buffer::BufferStorage;
use vjs::gc::{Realm, Trace, Tracer};
use vjs::internal;
use vjs::object::ObjectKind;
use vjs::realm::Intrinsic;
use vjs::{Agent, JsResult, Object, PropertyKey, Value};

use super::in_realm;
use super::messaging::PortId;
use crate::bindings::exception::dom;
use crate::dom::DomException;

/// One serialized value. Objects refer to other records by index.
#[derive(Clone, Debug)]
pub enum Record {
    /// `undefined`, `null`, a boolean, a number, a string or a bigint.
    Primitive(Value),
    BooleanObject(bool),
    NumberObject(f64),
    StringObject(Value),
    BigIntObject(Value),
    Date(f64),
    RegExp {
        source: String,
        flags: String,
    },
    ArrayBuffer(Vec<u8>),
    /// A transferred object: index in [`Serialized::transfers`].
    Transferred(usize),
    View {
        /// The constructor's name (`Uint8Array`, `DataView`, …).
        constructor: &'static str,
        buffer: usize,
        byte_offset: usize,
        length: Option<usize>,
    },
    Map(Vec<(usize, usize)>),
    Set(Vec<usize>),
    Error {
        name: String,
        message: Option<String>,
    },
    Array {
        length: f64,
        properties: Vec<(String, usize)>,
    },
    Object(Vec<(String, usize)>),
}

/// A transferred object's data.
#[derive(Clone, Debug)]
pub enum Transferred {
    ArrayBuffer(Vec<u8>),
    Port(PortId),
}

/// The output of StructuredSerializeWithTransfer: the records (the root
/// is the first) and the transferred objects.
#[derive(Clone, Debug, Default)]
pub struct Serialized {
    pub records: Vec<Record>,
    pub transfers: Vec<Transferred>,
}

impl Trace for Serialized {
    fn trace(&self, tracer: &mut Tracer) {
        for r in &self.records {
            match r {
                Record::Primitive(v) | Record::StringObject(v) | Record::BigIntObject(v) => {
                    tracer.value(*v);
                }
                _ => {}
            }
        }
    }
}

impl Serialized {
    /// The ports this message carries.
    pub fn ports(&self) -> Vec<PortId> {
        self.transfers
            .iter()
            .filter_map(|t| match t {
                Transferred::Port(p) => Some(*p),
                Transferred::ArrayBuffer(_) => None,
            })
            .collect()
    }
}

struct Serializer {
    out: Serialized,
    memory: HashMap<Object, usize>,
}

fn data_clone_error<T>(agent: &mut Agent) -> JsResult<T> {
    dom(agent, DomException::DataCloneError)
}

impl Serializer {
    fn push(&mut self, r: Record) -> usize {
        self.out.records.push(r);
        self.out.records.len() - 1
    }

    fn serialize(&mut self, agent: &mut Agent, value: Value) -> JsResult<usize> {
        let o = match value {
            Value::Symbol(_) => return data_clone_error(agent),
            Value::Object(o) => o,
            v => return Ok(self.push(Record::Primitive(v))),
        };
        if let Some(i) = self.memory.get(&o) {
            return Ok(*i);
        }
        let index = self.push(Record::Primitive(Value::Undefined));
        self.memory.insert(o, index);
        let record = self.serialize_object(agent, o)?;
        self.out.records[index] = record;
        Ok(index)
    }

    fn serialize_object(&mut self, agent: &mut Agent, o: Object) -> JsResult<Record> {
        enum K {
            Bool(bool),
            Num(f64),
            Str(Value),
            Big(Value),
            Date(f64),
            RegExp,
            Buffer(Option<Vec<u8>>),
            View(&'static str, Object, usize, Option<usize>),
            DataView,
            Map,
            Set,
            Error,
            Array,
            Ordinary,
            Other,
        }
        let k = match &agent.heap.object(o).kind {
            ObjectKind::Boolean(b) => K::Bool(*b),
            ObjectKind::Number(n) => K::Num(*n),
            ObjectKind::String(s) => K::Str(Value::String(*s)),
            ObjectKind::BigInt(b) => K::Big(Value::BigInt(*b)),
            ObjectKind::Date(t) => K::Date(*t),
            ObjectKind::RegExp(_) => K::RegExp,
            ObjectKind::ArrayBuffer(d) => {
                if d.is_shared() || d.is_detached() {
                    K::Buffer(None)
                } else {
                    let mut bytes = vec![0; d.byte_length()];
                    d.read(0, &mut bytes);
                    K::Buffer(Some(bytes))
                }
            }
            ObjectKind::TypedArray(t) => K::View(t.kind.name(), t.buffer, t.byte_offset, t.length),
            ObjectKind::DataView(_) => K::DataView,
            ObjectKind::Map(_) => K::Map,
            ObjectKind::Set(_) => K::Set,
            ObjectKind::Error => K::Error,
            ObjectKind::Array => K::Array,
            ObjectKind::Ordinary => K::Ordinary,
            _ => K::Other,
        };
        Ok(match k {
            K::Bool(b) => Record::BooleanObject(b),
            K::Num(n) => Record::NumberObject(n),
            K::Str(s) => Record::StringObject(s),
            K::Big(b) => Record::BigIntObject(b),
            K::Date(t) => Record::Date(t),
            K::RegExp => {
                let source = string_of(agent, o, "source")?;
                let flags = string_of(agent, o, "flags")?;
                Record::RegExp { source, flags }
            }
            K::Buffer(Some(bytes)) => Record::ArrayBuffer(bytes),
            K::Buffer(None) => return data_clone_error(agent),
            K::View(constructor, buffer, byte_offset, length) => {
                let buffer = self.serialize(agent, Value::Object(buffer))?;
                Record::View {
                    constructor,
                    buffer,
                    byte_offset,
                    length,
                }
            }
            K::DataView => {
                let key = agent.heap.key_from_str("buffer");
                let b = vjs::ops::get(agent, o, key)?;
                let off_key = agent.heap.key_from_str("byteOffset");
                let off = vjs::ops::get(agent, o, off_key)?.as_number().unwrap_or(0.0) as usize;
                let len_key = agent.heap.key_from_str("byteLength");
                let len = vjs::ops::get(agent, o, len_key)?.as_number().unwrap_or(0.0) as usize;
                let buffer = self.serialize(agent, b)?;
                Record::View {
                    constructor: "DataView",
                    buffer,
                    byte_offset: off,
                    length: Some(len),
                }
            }
            K::Map => {
                let entries =
                    vjs::builtins::collections::collection_entries(agent, o).unwrap_or_default();
                let mut out = Vec::with_capacity(entries.len());
                for (k, v) in entries {
                    let k = self.serialize(agent, k)?;
                    let v = self.serialize(agent, v)?;
                    out.push((k, v));
                }
                Record::Map(out)
            }
            K::Set => {
                let entries =
                    vjs::builtins::collections::collection_entries(agent, o).unwrap_or_default();
                let mut out = Vec::with_capacity(entries.len());
                for (k, _) in entries {
                    out.push(self.serialize(agent, k)?);
                }
                Record::Set(out)
            }
            K::Error => {
                let name = match string_of(agent, o, "name") {
                    Ok(n) if is_error_name(&n) => n,
                    _ => "Error".to_string(),
                };
                let message = own_string(agent, o, "message")?;
                Record::Error { name, message }
            }
            K::Array => {
                let key = agent.heap.key_from_str("length");
                let length = vjs::ops::get(agent, o, key)?.as_number().unwrap_or(0.0);
                let properties = self.properties(agent, o)?;
                Record::Array { length, properties }
            }
            K::Ordinary => Record::Object(self.properties(agent, o)?),
            K::Other => return data_clone_error(agent),
        })
    }

    /// The own enumerable string-keyed properties of `o`, serialized.
    fn properties(&mut self, agent: &mut Agent, o: Object) -> JsResult<Vec<(String, usize)>> {
        let keys = internal::own_property_keys(agent, o)?;
        let mut out = Vec::new();
        for key in keys {
            if matches!(key, PropertyKey::Symbol(_)) {
                continue;
            }
            let Some(desc) = internal::get_own_property(agent, o, key)? else {
                continue;
            };
            if desc.enumerable != Some(true) {
                continue;
            }
            let v = vjs::ops::get(agent, o, key)?;
            let name = agent.heap.key_to_rust(key);
            let index = self.serialize(agent, v)?;
            out.push((name, index));
        }
        Ok(out)
    }
}

fn is_error_name(n: &str) -> bool {
    matches!(
        n,
        "Error"
            | "EvalError"
            | "RangeError"
            | "ReferenceError"
            | "SyntaxError"
            | "TypeError"
            | "URIError"
    )
}

fn string_of(agent: &mut Agent, o: Object, name: &str) -> JsResult<String> {
    let key = agent.heap.key_from_str(name);
    let v = vjs::ops::get(agent, o, key)?;
    let s = vjs::ops::to_string(agent, v)?;
    Ok(agent.heap.string_to_rust(s))
}

/// An own data property of `o` as a string, if present.
fn own_string(agent: &mut Agent, o: Object, name: &str) -> JsResult<Option<String>> {
    let key = agent.heap.key_from_str(name);
    let Some(desc) = internal::get_own_property(agent, o, key)? else {
        return Ok(None);
    };
    match desc.value {
        Some(v) => {
            let s = vjs::ops::to_string(agent, v)?;
            Ok(Some(agent.heap.string_to_rust(s)))
        }
        None => Ok(None),
    }
}

/// StructuredSerializeWithTransfer(value, transferList).
pub fn serialize_with_transfer(
    agent: &mut Agent,
    value: Value,
    transfer: &[Value],
) -> JsResult<Serialized> {
    let mut s = Serializer {
        out: Serialized::default(),
        memory: HashMap::new(),
    };
    // The transfer list: ArrayBuffers and MessagePorts, each once.
    let mut transferables: Vec<(Object, bool)> = Vec::new();
    for t in transfer {
        let Value::Object(o) = *t else {
            return data_clone_error(agent);
        };
        if transferables.iter().any(|(x, _)| *x == o) {
            return data_clone_error(agent);
        }
        let is_buffer = match &agent.heap.object(o).kind {
            ObjectKind::ArrayBuffer(d) => {
                if d.is_shared() || d.is_detached() {
                    return data_clone_error(agent);
                }
                true
            }
            _ => false,
        };
        if !is_buffer && super::messaging::port_of_object(agent, o).is_none() {
            return data_clone_error(agent);
        }
        if !is_buffer && super::messaging::port_is_detached(agent, o) {
            return data_clone_error(agent);
        }
        transferables.push((o, is_buffer));
    }
    // The root is record 0; the transferred objects follow.
    s.push(Record::Primitive(Value::Undefined));
    for (i, (o, _)) in transferables.iter().enumerate() {
        let index = s.push(Record::Transferred(i));
        s.memory.insert(*o, index);
    }
    match value {
        Value::Symbol(_) => return data_clone_error(agent),
        Value::Object(o) => match s.memory.get(&o).copied() {
            Some(i) => s.out.records[0] = s.out.records[i].clone(),
            None => {
                s.memory.insert(o, 0);
                let r = s.serialize_object(agent, o)?;
                s.out.records[0] = r;
            }
        },
        v => s.out.records[0] = Record::Primitive(v),
    }
    // Transfer.
    let mut transfers = Vec::with_capacity(transferables.len());
    for (o, is_buffer) in &transferables {
        if *is_buffer {
            let bytes = match &mut agent.heap.object_mut(*o).kind {
                ObjectKind::ArrayBuffer(d) => {
                    match std::mem::replace(&mut d.storage, BufferStorage::Detached) {
                        BufferStorage::Owned(v) => v,
                        other => {
                            d.storage = other;
                            Vec::new()
                        }
                    }
                }
                _ => Vec::new(),
            };
            transfers.push(Transferred::ArrayBuffer(bytes));
        } else {
            match super::messaging::ship_port(agent, *o) {
                Some(id) => transfers.push(Transferred::Port(id)),
                None => return data_clone_error(agent),
            }
        }
    }
    s.out.transfers = transfers;
    Ok(s.out)
}

struct Deserializer<'a> {
    input: &'a Serialized,
    memory: Vec<Option<Value>>,
    transferred: Vec<Value>,
    realm: Realm,
}

fn intrinsic(agent: &Agent, realm: Realm, which: Intrinsic) -> Object {
    agent.heap.realm(realm).intrinsics.get(which)
}

impl Deserializer<'_> {
    fn value(&mut self, agent: &mut Agent, i: usize) -> JsResult<Value> {
        if let Some(v) = self.memory.get(i).copied().flatten() {
            return Ok(v);
        }
        let record = self.input.records[i].clone();
        let realm = self.realm;
        let v = match record {
            Record::Primitive(v) => v,
            Record::Transferred(t) => self.transferred.get(t).copied().unwrap_or(Value::Undefined),
            Record::BooleanObject(b) => {
                Value::Object(vjs::ops::to_object(agent, Value::Boolean(b))?)
            }
            Record::NumberObject(n) => Value::Object(vjs::ops::to_object(agent, Value::Number(n))?),
            Record::StringObject(s) | Record::BigIntObject(s) => {
                Value::Object(vjs::ops::to_object(agent, s)?)
            }
            Record::Date(t) => {
                let c = intrinsic(agent, realm, Intrinsic::Date);
                Value::Object(vjs::ops::construct(agent, c, &[Value::Number(t)], None)?)
            }
            Record::RegExp { source, flags } => {
                let c = intrinsic(agent, realm, Intrinsic::RegExp);
                let s = Value::String(agent.heap.alloc_str(&source));
                let f = Value::String(agent.heap.alloc_str(&flags));
                Value::Object(vjs::ops::construct(agent, c, &[s, f], None)?)
            }
            Record::ArrayBuffer(bytes) => Value::Object(new_array_buffer(agent, realm, bytes)?),
            Record::View {
                constructor,
                buffer,
                byte_offset,
                length,
            } => {
                let b = self.value(agent, buffer)?;
                let global = agent.heap.realm(realm).global_object;
                let Some(global) = global else {
                    return data_clone_error(agent);
                };
                let key = agent.heap.key_from_str(constructor);
                let Value::Object(c) = vjs::ops::get(agent, global, key)? else {
                    return data_clone_error(agent);
                };
                let mut args = vec![b, Value::Number(byte_offset as f64)];
                if let Some(l) = length {
                    args.push(Value::Number(l as f64));
                }
                Value::Object(vjs::ops::construct(agent, c, &args, None)?)
            }
            Record::Map(entries) => {
                let c = intrinsic(agent, realm, Intrinsic::Map);
                let m = vjs::ops::construct(agent, c, &[], None)?;
                self.memory[i] = Some(Value::Object(m));
                for (k, v) in entries {
                    let k = self.value(agent, k)?;
                    let v = self.value(agent, v)?;
                    vjs::builtins::collections::collection_append(agent, m, k, v);
                }
                Value::Object(m)
            }
            Record::Set(entries) => {
                let c = intrinsic(agent, realm, Intrinsic::Set);
                let m = vjs::ops::construct(agent, c, &[], None)?;
                self.memory[i] = Some(Value::Object(m));
                for k in entries {
                    let k = self.value(agent, k)?;
                    vjs::builtins::collections::collection_append(agent, m, k, Value::Undefined);
                }
                Value::Object(m)
            }
            Record::Error { name, message } => {
                let which = match name.as_str() {
                    "EvalError" => Intrinsic::EvalError,
                    "RangeError" => Intrinsic::RangeError,
                    "ReferenceError" => Intrinsic::ReferenceError,
                    "SyntaxError" => Intrinsic::SyntaxError,
                    "TypeError" => Intrinsic::TypeError,
                    "URIError" => Intrinsic::UriError,
                    _ => Intrinsic::Error,
                };
                let c = intrinsic(agent, realm, which);
                let args: Vec<Value> = match message {
                    Some(m) => vec![Value::String(agent.heap.alloc_str(&m))],
                    None => Vec::new(),
                };
                Value::Object(vjs::ops::construct(agent, c, &args, None)?)
            }
            Record::Array { length, properties } => {
                let a = vjs::ops::create_array_from_list(agent, &[]);
                let key = agent.heap.key_from_str("length");
                vjs::ops::set(agent, a, key, Value::Number(length), true)?;
                self.memory[i] = Some(Value::Object(a));
                self.fill(agent, a, properties)?;
                Value::Object(a)
            }
            Record::Object(properties) => {
                let o = vjs::ops::object_create(agent);
                self.memory[i] = Some(Value::Object(o));
                self.fill(agent, o, properties)?;
                Value::Object(o)
            }
        };
        self.memory[i] = Some(v);
        Ok(v)
    }

    fn fill(
        &mut self,
        agent: &mut Agent,
        o: Object,
        properties: Vec<(String, usize)>,
    ) -> JsResult<()> {
        for (name, index) in properties {
            let v = self.value(agent, index)?;
            let key = agent.heap.key_from_str(&name);
            vjs::ops::create_data_property_or_throw(agent, o, key, v)?;
        }
        Ok(())
    }
}

/// A new `ArrayBuffer` of `realm` holding `bytes`.
fn new_array_buffer(agent: &mut Agent, realm: Realm, bytes: Vec<u8>) -> JsResult<Object> {
    let c = intrinsic(agent, realm, Intrinsic::ArrayBuffer);
    let b = vjs::ops::construct(agent, c, &[Value::Number(bytes.len() as f64)], None)?;
    if let ObjectKind::ArrayBuffer(d) = &mut agent.heap.object_mut(b).kind {
        d.write(0, &bytes);
    }
    Ok(b)
}

/// StructuredDeserializeWithTransfer(serialized, realm): the value and the
/// `MessagePort` objects made for the transferred ports.
pub fn deserialize_with_transfer(
    agent: &mut Agent,
    realm: Realm,
    input: &Serialized,
) -> JsResult<(Value, Vec<Object>)> {
    in_realm(agent, realm, |agent| {
        let mut transferred = Vec::with_capacity(input.transfers.len());
        let mut ports = Vec::new();
        for t in &input.transfers {
            match t {
                Transferred::ArrayBuffer(bytes) => {
                    let b = new_array_buffer(agent, realm, bytes.clone())?;
                    transferred.push(Value::Object(b));
                }
                Transferred::Port(id) => {
                    let Some(state) = crate::bindings::state_of(agent, realm) else {
                        return data_clone_error(agent);
                    };
                    let p = super::messaging::port_object(agent, &state, *id);
                    ports.push(p);
                    transferred.push(Value::Object(p));
                }
            }
        }
        let keep = std::rc::Rc::new(super::window::Roots(transferred.clone()));
        let root = agent
            .heap
            .add_root_set(keep.clone() as std::rc::Rc<dyn Trace>);
        let mut d = Deserializer {
            input,
            memory: vec![None; input.records.len()],
            transferred,
            realm,
        };
        let result = if input.records.is_empty() {
            Ok(Value::Undefined)
        } else {
            d.value(agent, 0)
        };
        agent.heap.remove_root_set(root);
        result.map(|v| (v, ports))
    })
}

/// StructuredDeserialize without transferred objects.
pub fn deserialize(agent: &mut Agent, realm: Realm, input: &Serialized) -> JsResult<Value> {
    deserialize_with_transfer(agent, realm, input).map(|(v, _)| v)
}

/// StructuredSerialize(value) (no transfer list).
pub fn serialize(agent: &mut Agent, value: Value) -> JsResult<Serialized> {
    serialize_with_transfer(agent, value, &[])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent_with_realm() -> (Agent, Realm) {
        let mut agent = Agent::new();
        let realm = agent.create_realm();
        agent.set_realm_global_object(realm, None, None);
        agent
            .set_default_global_bindings(realm)
            .expect("global bindings");
        (agent, realm)
    }

    #[test]
    fn objects_round_trip_with_cycles() {
        let (mut agent, realm) = agent_with_realm();
        let v = vjs::evaluate_script_in(
            &mut agent,
            realm,
            "var o = {a: 1, s: 'x', d: new Date(5), m: new Map([[1, 'one']]), e: new RangeError('r')}; o.self = o; o.list = [o.a, , 3]; o",
        )
        .expect("value");
        let s = serialize(&mut agent, v).expect("serializes");
        let copy = deserialize(&mut agent, realm, &s).expect("deserializes");
        let global = agent.heap.realm(realm).global_object.expect("global");
        let key = agent.heap.key_from_str("copy");
        vjs::ops::set(&mut agent, global, key, copy, true).expect("set");
        let check = vjs::evaluate_script_in(
            &mut agent,
            realm,
            "copy !== o && copy.self === copy && copy.a === 1 && copy.s === 'x' && copy.d.getTime() === 5 && copy.m.get(1) === 'one' && copy.e instanceof RangeError && copy.e.message === 'r' && copy.list.length === 3 && !(1 in copy.list)",
        )
        .expect("check");
        assert_eq!(check, Value::Boolean(true));
    }

    #[test]
    fn functions_and_symbols_do_not_clone() {
        let (mut agent, realm) = agent_with_realm();
        let f = vjs::evaluate_script_in(&mut agent, realm, "(function () {})").expect("f");
        assert!(serialize(&mut agent, f).is_err());
        let s = vjs::evaluate_script_in(&mut agent, realm, "Symbol()").expect("s");
        assert!(serialize(&mut agent, s).is_err());
    }

    #[test]
    fn array_buffers_transfer() {
        let (mut agent, realm) = agent_with_realm();
        let b = vjs::evaluate_script_in(
            &mut agent,
            realm,
            "var b = new Uint8Array([1, 2, 3]).buffer; b",
        )
        .expect("b");
        let s = serialize_with_transfer(&mut agent, b, &[b]).expect("transfer");
        let copy = deserialize(&mut agent, realm, &s).expect("copy");
        let global = agent.heap.realm(realm).global_object.expect("global");
        let key = agent.heap.key_from_str("copy");
        vjs::ops::set(&mut agent, global, key, copy, true).expect("set");
        let check = vjs::evaluate_script_in(
            &mut agent,
            realm,
            "b.byteLength === 0 && copy.byteLength === 3 && new Uint8Array(copy)[2] === 3",
        )
        .expect("check");
        assert_eq!(check, Value::Boolean(true));
    }
}
