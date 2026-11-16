//! `%TypedArray%` (ECMA-262 §23.2) with `from`, `of`, `@@species` and
//! every method of `%TypedArray.prototype%`, the twelve concrete
//! constructors (`Float16Array` included), and the base64 and hex methods
//! of `Uint8Array` (feature `uint8array-base64`).
//!
//! A TypedArray may view a resizable buffer, either with a fixed length or
//! tracking the buffer's length; every method revalidates the view after
//! running user code, as the specification does, and reads past the
//! current end give `undefined`.

use crate::agent::Agent;
use crate::base64::{self, LastChunk};
use crate::buffer::{self, ElementValue, TypedArrayKind};
use crate::builtins::array_buffer::allocate_plain_buffer;
use crate::builtins::{
    accessor, constructor, intrinsic, method, plain_object, set_intrinsic, species_getter, value,
};
use crate::completion::JsResult;
use crate::function::{NativeCall, NativeFn, native_slots, native_slots_mut};
use crate::gc::{JsString, Object, Realm};
use crate::interp::iter::{ArrayIterKind, create_array_iterator};
use crate::object::{ObjectKind, Slot};
use crate::ops;
use crate::realm::Intrinsic;
use crate::string::MAX_STRING_LENGTH;
use crate::value::{PropertyKey, Value};

/// The constructor and prototype intrinsics of a kind.
pub(crate) fn kind_intrinsics(kind: TypedArrayKind) -> (Intrinsic, Intrinsic) {
    match kind {
        TypedArrayKind::Int8 => (Intrinsic::Int8Array, Intrinsic::Int8ArrayPrototype),
        TypedArrayKind::Uint8 => (Intrinsic::Uint8Array, Intrinsic::Uint8ArrayPrototype),
        TypedArrayKind::Uint8Clamped => (
            Intrinsic::Uint8ClampedArray,
            Intrinsic::Uint8ClampedArrayPrototype,
        ),
        TypedArrayKind::Int16 => (Intrinsic::Int16Array, Intrinsic::Int16ArrayPrototype),
        TypedArrayKind::Uint16 => (Intrinsic::Uint16Array, Intrinsic::Uint16ArrayPrototype),
        TypedArrayKind::Int32 => (Intrinsic::Int32Array, Intrinsic::Int32ArrayPrototype),
        TypedArrayKind::Uint32 => (Intrinsic::Uint32Array, Intrinsic::Uint32ArrayPrototype),
        TypedArrayKind::Float16 => (Intrinsic::Float16Array, Intrinsic::Float16ArrayPrototype),
        TypedArrayKind::Float32 => (Intrinsic::Float32Array, Intrinsic::Float32ArrayPrototype),
        TypedArrayKind::Float64 => (Intrinsic::Float64Array, Intrinsic::Float64ArrayPrototype),
        TypedArrayKind::BigInt64 => (Intrinsic::BigInt64Array, Intrinsic::BigInt64ArrayPrototype),
        TypedArrayKind::BigUint64 => (
            Intrinsic::BigUint64Array,
            Intrinsic::BigUint64ArrayPrototype,
        ),
    }
}

pub(crate) fn install(agent: &mut Agent, realm: Realm) {
    let object_proto = intrinsic(agent, realm, Intrinsic::ObjectPrototype);
    let proto = plain_object(agent, object_proto);
    set_intrinsic(agent, realm, Intrinsic::TypedArrayPrototype, proto);
    let ctor = constructor(
        agent,
        realm,
        "TypedArray",
        typed_array_intrinsic,
        0,
        proto,
        None,
    );
    set_intrinsic(agent, realm, Intrinsic::TypedArray, ctor);
    method(agent, realm, ctor, "from", from, 1);
    method(agent, realm, ctor, "of", of, 0);
    species_getter(agent, realm, ctor);

    let getters: &[(&str, NativeFn)] = &[
        ("buffer", get_buffer),
        ("byteLength", get_byte_length),
        ("byteOffset", get_byte_offset),
        ("length", get_length),
    ];
    for &(name, f) in getters {
        let key = agent.heap.key_from_str(name);
        accessor(agent, realm, proto, key, Some(f), None);
    }
    let methods: &[(&str, NativeFn, u32)] = &[
        ("at", at, 1),
        ("copyWithin", copy_within, 2),
        ("entries", entries, 0),
        ("every", every, 1),
        ("fill", fill, 1),
        ("filter", filter, 1),
        ("find", find, 1),
        ("findIndex", find_index, 1),
        ("findLast", find_last, 1),
        ("findLastIndex", find_last_index, 1),
        ("forEach", for_each, 1),
        ("includes", includes, 1),
        ("indexOf", index_of, 1),
        ("join", join, 1),
        ("keys", keys, 0),
        ("lastIndexOf", last_index_of, 1),
        ("map", map, 1),
        ("reduce", reduce, 1),
        ("reduceRight", reduce_right, 1),
        ("reverse", reverse, 0),
        ("set", set, 1),
        ("slice", slice, 2),
        ("some", some, 1),
        ("sort", sort, 1),
        ("subarray", subarray, 2),
        ("toLocaleString", to_locale_string, 0),
        ("toReversed", to_reversed, 0),
        ("toSorted", to_sorted, 1),
        ("with", with, 2),
    ];
    for &(name, f, length) in methods {
        method(agent, realm, proto, name, f, length);
    }
    let values = method(agent, realm, proto, "values", values, 0);
    let key = PropertyKey::Symbol(agent.symbols.iterator);
    ops::define_builtin_method(agent, proto, key, values);
    // `toString` is the same function object as Array.prototype.toString.
    let array_proto = intrinsic(agent, realm, Intrinsic::ArrayPrototype);
    let key = PropertyKey::String(agent.heap.names.to_string);
    if let Some((_, Slot::Data(Value::Object(f)))) = agent.heap.get_own(array_proto, key) {
        ops::define_builtin_method(agent, proto, key, f);
    }
    let key = PropertyKey::Symbol(agent.symbols.to_string_tag);
    accessor(agent, realm, proto, key, Some(get_to_string_tag), None);

    for kind in TypedArrayKind::ALL {
        install_concrete(agent, realm, kind, ctor, proto);
    }
    install_uint8_base64(agent, realm);
}

fn install_concrete(
    agent: &mut Agent,
    realm: Realm,
    kind: TypedArrayKind,
    parent: Object,
    parent_proto: Object,
) {
    let (ctor_id, proto_id) = kind_intrinsics(kind);
    let proto = plain_object(agent, parent_proto);
    set_intrinsic(agent, realm, proto_id, proto);
    let ctor = constructor(
        agent,
        realm,
        kind.name(),
        concrete_constructor,
        3,
        proto,
        Some(parent),
    );
    if let Some(slots) = native_slots_mut(agent, ctor) {
        let index = TypedArrayKind::ALL
            .iter()
            .position(|k| *k == kind)
            .unwrap_or(0);
        slots.push(Value::Number(index as f64));
    }
    set_intrinsic(agent, realm, ctor_id, ctor);
    let key = agent.heap.key_from_str("BYTES_PER_ELEMENT");
    let size = Value::Number(kind.element_size() as f64);
    value(agent, ctor, key, size, false, false, false);
    value(agent, proto, key, size, false, false, false);
}

// ----- helpers -----

fn kind_of(agent: &Agent, o: Object) -> Option<TypedArrayKind> {
    match &agent.heap.object(o).kind {
        ObjectKind::TypedArray(t) => Some(t.kind),
        _ => None,
    }
}

/// RequireInternalSlot(O, [[TypedArrayName]]).
fn require_typed_array(
    agent: &mut Agent,
    v: Value,
    what: &str,
) -> JsResult<(Object, TypedArrayKind)> {
    if let Value::Object(o) = v {
        if let Some(kind) = kind_of(agent, o) {
            return Ok((o, kind));
        }
    }
    agent.type_error(&format!("{what}: not a TypedArray"))
}

/// ValidateTypedArray(O, order): the array, its kind and its length.
fn validate(agent: &mut Agent, v: Value, what: &str) -> JsResult<(Object, TypedArrayKind, usize)> {
    let (o, kind) = require_typed_array(agent, v, what)?;
    match buffer::typed_array_length(agent, o) {
        Some(len) => Ok((o, kind, len)),
        None => agent.type_error(&format!(
            "{what}: the TypedArray is detached or out of bounds"
        )),
    }
}

fn this_array(
    agent: &mut Agent,
    call: &NativeCall<'_>,
    name: &str,
) -> JsResult<(Object, TypedArrayKind, usize)> {
    validate(agent, call.this, &format!("%TypedArray%.prototype.{name}"))
}

/// Get(O, k) on a TypedArray.
fn get(agent: &mut Agent, o: Object, k: usize) -> Value {
    buffer::typed_array_get_element(agent, o, k as f64).unwrap_or(Value::Undefined)
}

/// Set(O, k, v, true) on a TypedArray.
fn put(agent: &mut Agent, o: Object, k: usize, v: Value) -> JsResult<()> {
    buffer::typed_array_set_element(agent, o, k as f64, v)
}

fn callback(agent: &mut Agent, v: Value, name: &str) -> JsResult<Value> {
    if ops::is_callable(agent, v) {
        Ok(v)
    } else {
        agent.type_error(&format!(
            "%TypedArray%.prototype.{name}: the callback is not a function"
        ))
    }
}

/// A relative index `v` clamped to `[0, len]` (`default` if undefined).
fn relative(agent: &mut Agent, v: Value, len: f64, default: f64) -> JsResult<f64> {
    if v.is_undefined() {
        return Ok(default);
    }
    let rel = ops::to_integer_or_infinity(agent, v)?;
    Ok(if rel < 0.0 {
        (len + rel).max(0.0)
    } else {
        rel.min(len)
    })
}

/// AllocateTypedArray with a length: a new array of `kind` with prototype
/// `proto` over a new buffer of `len` elements.
fn allocate(agent: &mut Agent, kind: TypedArrayKind, proto: Object, len: f64) -> JsResult<Object> {
    let buf = allocate_plain_buffer(agent, len * kind.element_size() as f64)?;
    Ok(buffer::typed_array_create(
        agent,
        Some(proto),
        kind,
        buf,
        0,
        Some(len as usize),
    ))
}

/// TypedArrayCreateFromConstructor(constructor, argumentList).
fn create_from_constructor(agent: &mut Agent, ctor: Object, args: &[Value]) -> JsResult<Object> {
    let new = ops::construct(agent, ctor, args, None)?;
    let (_, _, len) = validate(agent, Value::Object(new), "TypedArray species")?;
    if let [Value::Number(n)] = args {
        if (len as f64) < *n {
            return agent.type_error("TypedArray species: the new array is too short");
        }
    }
    Ok(new)
}

/// TypedArraySpeciesCreate(exemplar, argumentList).
fn species_create(
    agent: &mut Agent,
    exemplar: Object,
    kind: TypedArrayKind,
    args: &[Value],
) -> JsResult<Object> {
    let default = agent.intrinsic(kind_intrinsics(kind).0);
    let ctor = ops::species_constructor(agent, exemplar, default)?;
    let result = create_from_constructor(agent, ctor, args)?;
    if kind_of(agent, result).is_some_and(|k| k.is_bigint() != kind.is_bigint()) {
        return agent.type_error("TypedArray species: the content types differ");
    }
    Ok(result)
}

/// TypedArrayCreateSameType(exemplar, «len»).
fn create_same_type(agent: &mut Agent, kind: TypedArrayKind, len: usize) -> JsResult<Object> {
    let ctor = agent.intrinsic(kind_intrinsics(kind).0);
    create_from_constructor(agent, ctor, &[Value::Number(len as f64)])
}

/// The element of `kind` at `byte_index` of `buf`, without conversion.
fn raw_element(
    agent: &Agent,
    buf: Object,
    byte_index: usize,
    kind: TypedArrayKind,
) -> ElementValue {
    let bytes = buffer::read_bytes(agent, buf, byte_index, kind.element_size());
    buffer::raw_bytes_to_numeric(kind, &bytes)
}

fn append(agent: &mut Agent, out: &mut Vec<u16>, s: JsString) -> JsResult<()> {
    let units = agent.heap.string_units(s);
    if out.len() + units.len() > MAX_STRING_LENGTH {
        return agent.range_error("invalid string length");
    }
    out.extend_from_slice(units);
    Ok(())
}

// ----- constructors -----

fn typed_array_intrinsic(agent: &mut Agent, _call: NativeCall<'_>) -> JsResult<Value> {
    agent.type_error("TypedArray is an abstract class and cannot be constructed")
}

fn concrete_constructor(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let index = match native_slots(agent, call.callee).first() {
        Some(Value::Number(n)) => *n as usize,
        _ => 0,
    };
    let kind = TypedArrayKind::ALL[index];
    let Some(new_target) = call.new_target else {
        return agent.type_error(&format!("{} constructor requires 'new'", kind.name()));
    };
    let proto_id = kind_intrinsics(kind).1;
    let first = call.arg(0);
    let Value::Object(source) = first else {
        let len = if call.args.is_empty() {
            0
        } else {
            ops::to_index(agent, first)?
        };
        let proto = ops::get_prototype_from_constructor(agent, new_target, proto_id)?;
        return Ok(Value::Object(allocate(agent, kind, proto, len as f64)?));
    };
    let proto = ops::get_prototype_from_constructor(agent, new_target, proto_id)?;
    let (is_view, is_buffer) = match &agent.heap.object(source).kind {
        ObjectKind::TypedArray(_) => (true, false),
        ObjectKind::ArrayBuffer(_) => (false, true),
        _ => (false, false),
    };
    let o = match (is_view, is_buffer) {
        (true, _) => from_typed_array(agent, kind, proto, source)?,
        (_, true) => from_array_buffer(agent, kind, proto, source, call.arg(1), call.arg(2))?,
        _ => {
            let key = PropertyKey::Symbol(agent.symbols.iterator);
            match ops::get_method(agent, first, key)? {
                Some(m) => {
                    let mut record = ops::get_iterator_from_method(agent, first, m)?;
                    let values = ops::iterator_to_list(agent, &mut record)?;
                    let o = allocate(agent, kind, proto, values.len() as f64)?;
                    for (k, v) in values.into_iter().enumerate() {
                        put(agent, o, k, v)?;
                    }
                    o
                }
                None => {
                    let len = ops::length_of_array_like(agent, source)?;
                    let o = allocate(agent, kind, proto, len)?;
                    for k in 0..len as usize {
                        let key = ops::index_key(agent, k as u64);
                        let v = ops::get(agent, source, key)?;
                        put(agent, o, k, v)?;
                    }
                    o
                }
            }
        }
    };
    Ok(Value::Object(o))
}

/// InitializeTypedArrayFromTypedArray.
fn from_typed_array(
    agent: &mut Agent,
    kind: TypedArrayKind,
    proto: Object,
    src: Object,
) -> JsResult<Object> {
    let Some((src_kind, src_buf, src_offset, _)) = buffer::typed_array_slots(agent, src) else {
        return agent.type_error("not a TypedArray");
    };
    let Some(len) = buffer::typed_array_length(agent, src) else {
        return agent.type_error("the source TypedArray is detached or out of bounds");
    };
    if src_kind.is_bigint() != kind.is_bigint() {
        return agent.type_error("cannot mix BigInt and Number TypedArrays");
    }
    let o = allocate(agent, kind, proto, len as f64)?;
    let Some((_, buf, _, _)) = buffer::typed_array_slots(agent, o) else {
        return Ok(o);
    };
    if src_kind == kind {
        let bytes = buffer::read_bytes(agent, src_buf, src_offset, len * kind.element_size());
        buffer::write_bytes(agent, buf, 0, &bytes);
    } else {
        for k in 0..len {
            let v = raw_element(
                agent,
                src_buf,
                src_offset + k * src_kind.element_size(),
                src_kind,
            );
            buffer::set_value_in_buffer(agent, buf, k * kind.element_size(), kind, &v, true);
        }
    }
    Ok(o)
}

/// InitializeTypedArrayFromArrayBuffer.
fn from_array_buffer(
    agent: &mut Agent,
    kind: TypedArrayKind,
    proto: Object,
    buf: Object,
    byte_offset: Value,
    length: Value,
) -> JsResult<Object> {
    let size = kind.element_size() as u64;
    let offset = ops::to_index(agent, byte_offset)?;
    if offset % size != 0 {
        return agent.range_error(&format!(
            "start offset of {} should be a multiple of {size}",
            kind.name()
        ));
    }
    let fixed = buffer::buffer_data(agent, buf).is_some_and(|d| d.is_fixed_length());
    let new_length = if length.is_undefined() {
        None
    } else {
        Some(ops::to_index(agent, length)?)
    };
    if buffer::is_detached(agent, buf) {
        return agent.type_error("the ArrayBuffer is detached");
    }
    let buffer_len = buffer::buffer_byte_length(agent, buf) as u64;
    let length = match new_length {
        None if !fixed => {
            if offset > buffer_len {
                return agent.range_error("the offset is past the end of the buffer");
            }
            None
        }
        None => {
            if buffer_len % size != 0 {
                return agent.range_error(&format!(
                    "byte length of {} should be a multiple of {size}",
                    kind.name()
                ));
            }
            if offset > buffer_len {
                return agent.range_error("the offset is past the end of the buffer");
            }
            Some((buffer_len - offset) / size)
        }
        Some(n) => {
            if offset as f64 + n as f64 * size as f64 > buffer_len as f64 {
                return agent.range_error("the view does not fit in the buffer");
            }
            Some(n)
        }
    };
    Ok(buffer::typed_array_create(
        agent,
        Some(proto),
        kind,
        buf,
        offset as usize,
        length.map(|l| l as usize),
    ))
}

/// `%TypedArray%.from(source[, mapfn[, thisArg]])`.
fn from(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let c = call.this;
    let Value::Object(ctor) = c else {
        return agent.type_error("%TypedArray%.from: this is not a constructor");
    };
    if !ops::is_constructor(agent, c) {
        return agent.type_error("%TypedArray%.from: this is not a constructor");
    }
    let mapfn = call.arg(1);
    let this_arg = call.arg(2);
    let mapping = !mapfn.is_undefined();
    if mapping && !ops::is_callable(agent, mapfn) {
        return agent.type_error("%TypedArray%.from: the map function is not a function");
    }
    let source = call.arg(0);
    let key = PropertyKey::Symbol(agent.symbols.iterator);
    if let Some(m) = ops::get_method(agent, source, key)? {
        let mut record = ops::get_iterator_from_method(agent, source, m)?;
        let values = ops::iterator_to_list(agent, &mut record)?;
        let target = create_from_constructor(agent, ctor, &[Value::Number(values.len() as f64)])?;
        for (k, v) in values.into_iter().enumerate() {
            let mapped = if mapping {
                ops::call(agent, mapfn, this_arg, &[v, Value::Number(k as f64)])?
            } else {
                v
            };
            let key = ops::index_key(agent, k as u64);
            ops::set(agent, target, key, mapped, true)?;
        }
        return Ok(Value::Object(target));
    }
    let array_like = ops::to_object(agent, source)?;
    let len = ops::length_of_array_like(agent, array_like)?;
    let target = create_from_constructor(agent, ctor, &[Value::Number(len)])?;
    for k in 0..len as u64 {
        let key = ops::index_key(agent, k);
        let v = ops::get(agent, array_like, key)?;
        let mapped = if mapping {
            ops::call(agent, mapfn, this_arg, &[v, Value::Number(k as f64)])?
        } else {
            v
        };
        ops::set(agent, target, key, mapped, true)?;
    }
    Ok(Value::Object(target))
}

/// `%TypedArray%.of(...items)`.
fn of(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let c = call.this;
    let Value::Object(ctor) = c else {
        return agent.type_error("%TypedArray%.of: this is not a constructor");
    };
    if !ops::is_constructor(agent, c) {
        return agent.type_error("%TypedArray%.of: this is not a constructor");
    }
    let len = call.args.len();
    let target = create_from_constructor(agent, ctor, &[Value::Number(len as f64)])?;
    for (k, &v) in call.args.iter().enumerate() {
        let key = ops::index_key(agent, k as u64);
        ops::set(agent, target, key, v, true)?;
    }
    Ok(Value::Object(target))
}

// ----- accessors -----

fn get_buffer(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let (o, _) = require_typed_array(agent, call.this, "%TypedArray%.prototype.buffer")?;
    let buf = buffer::typed_array_slots(agent, o).map(|s| s.1);
    Ok(buf.map_or(Value::Undefined, Value::Object))
}

fn get_byte_length(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let (o, kind) = require_typed_array(agent, call.this, "%TypedArray%.prototype.byteLength")?;
    let len = buffer::typed_array_length(agent, o).unwrap_or(0);
    Ok(Value::Number((len * kind.element_size()) as f64))
}

fn get_byte_offset(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let (o, _) = require_typed_array(agent, call.this, "%TypedArray%.prototype.byteOffset")?;
    if buffer::typed_array_length(agent, o).is_none() {
        return Ok(Value::Number(0.0));
    }
    let offset = buffer::typed_array_slots(agent, o).map_or(0, |s| s.2);
    Ok(Value::Number(offset as f64))
}

fn get_length(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let (o, _) = require_typed_array(agent, call.this, "%TypedArray%.prototype.length")?;
    let len = buffer::typed_array_length(agent, o).unwrap_or(0);
    Ok(Value::Number(len as f64))
}

fn get_to_string_tag(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let Value::Object(o) = call.this else {
        return Ok(Value::Undefined);
    };
    Ok(match kind_of(agent, o) {
        Some(kind) => Value::String(agent.heap.alloc_str(kind.name())),
        None => Value::Undefined,
    })
}

// ----- iteration -----

fn iterator(
    agent: &mut Agent,
    call: &NativeCall<'_>,
    kind: ArrayIterKind,
    name: &str,
) -> JsResult<Value> {
    let (o, _, _) = this_array(agent, call, name)?;
    Ok(Value::Object(create_array_iterator(agent, o, kind)))
}

fn entries(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    iterator(agent, &call, ArrayIterKind::Entries, "entries")
}

fn keys(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    iterator(agent, &call, ArrayIterKind::Keys, "keys")
}

fn values(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    iterator(agent, &call, ArrayIterKind::Values, "values")
}

// ----- methods -----

fn at(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let (o, _, len) = this_array(agent, &call, "at")?;
    let rel = ops::to_integer_or_infinity(agent, call.arg(0))?;
    let k = if rel >= 0.0 { rel } else { len as f64 + rel };
    if k < 0.0 || k >= len as f64 {
        return Ok(Value::Undefined);
    }
    Ok(get(agent, o, k as usize))
}

fn copy_within(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let (o, kind, len) = this_array(agent, &call, "copyWithin")?;
    let lenf = len as f64;
    let target = relative(agent, call.arg(0), lenf, 0.0)?;
    let start = relative(agent, call.arg(1), lenf, 0.0)?;
    let end = relative(agent, call.arg(2), lenf, lenf)?;
    let count = (end - start).min(lenf - target);
    if count > 0.0 {
        let Some(len) = buffer::typed_array_length(agent, o) else {
            return agent.type_error(
                "%TypedArray%.prototype.copyWithin: the TypedArray is detached or out of bounds",
            );
        };
        let Some((_, buf, offset, _)) = buffer::typed_array_slots(agent, o) else {
            return Ok(Value::Object(o));
        };
        let size = kind.element_size();
        let limit = len * size + offset;
        let to = target as usize * size + offset;
        let from = start as usize * size + offset;
        let count_bytes = count as usize * size;
        if from < to && to < from + count_bytes {
            // Backwards: nothing is copied if the last byte is past the
            // limit.
            if to + count_bytes <= limit {
                let bytes = buffer::read_bytes(agent, buf, from, count_bytes);
                buffer::write_bytes(agent, buf, to, &bytes);
            }
        } else {
            let n = count_bytes
                .min(limit.saturating_sub(from))
                .min(limit.saturating_sub(to));
            buffer::copy_bytes_forward(agent, buf, from, buf, to, n);
        }
    }
    Ok(Value::Object(o))
}

/// Calls `f(value, index, O)` for each element in order; stops when `stop`
/// says so and returns that index and value.
fn scan(
    agent: &mut Agent,
    call: &NativeCall<'_>,
    name: &str,
    reverse: bool,
    stop: fn(bool) -> bool,
) -> JsResult<Option<(usize, Value)>> {
    let (o, _, len) = this_array(agent, call, name)?;
    let f = callback(agent, call.arg(0), name)?;
    let this_arg = call.arg(1);
    for i in 0..len {
        let k = if reverse { len - 1 - i } else { i };
        let v = get(agent, o, k);
        let r = ops::call(
            agent,
            f,
            this_arg,
            &[v, Value::Number(k as f64), Value::Object(o)],
        )?;
        if stop(ops::to_boolean(agent, r)) {
            return Ok(Some((k, v)));
        }
    }
    Ok(None)
}

fn every(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let r = scan(agent, &call, "every", false, |b| !b)?;
    Ok(Value::Boolean(r.is_none()))
}

fn some(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let r = scan(agent, &call, "some", false, |b| b)?;
    Ok(Value::Boolean(r.is_some()))
}

fn find(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let r = scan(agent, &call, "find", false, |b| b)?;
    Ok(r.map_or(Value::Undefined, |(_, v)| v))
}

fn find_index(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let r = scan(agent, &call, "findIndex", false, |b| b)?;
    Ok(Value::Number(r.map_or(-1.0, |(k, _)| k as f64)))
}

fn find_last(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let r = scan(agent, &call, "findLast", true, |b| b)?;
    Ok(r.map_or(Value::Undefined, |(_, v)| v))
}

fn find_last_index(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let r = scan(agent, &call, "findLastIndex", true, |b| b)?;
    Ok(Value::Number(r.map_or(-1.0, |(k, _)| k as f64)))
}

fn for_each(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    scan(agent, &call, "forEach", false, |_| false)?;
    Ok(Value::Undefined)
}

fn fill(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let (o, kind, len) = this_array(agent, &call, "fill")?;
    let v = buffer::to_element_value(agent, kind, call.arg(0))?;
    let lenf = len as f64;
    let start = relative(agent, call.arg(1), lenf, 0.0)?;
    let end = relative(agent, call.arg(2), lenf, lenf)?;
    let Some(len) = buffer::typed_array_length(agent, o) else {
        return agent.type_error(
            "%TypedArray%.prototype.fill: the TypedArray is detached or out of bounds",
        );
    };
    let end = end.min(len as f64) as usize;
    let start = start as usize;
    if start < end {
        let Some((_, buf, offset, _)) = buffer::typed_array_slots(agent, o) else {
            return Ok(Value::Object(o));
        };
        let size = kind.element_size();
        if buffer::is_shared(agent, buf) {
            for k in start..end {
                buffer::set_value_in_buffer(agent, buf, offset + k * size, kind, &v, true);
            }
        } else {
            let raw = buffer::numeric_to_raw_bytes(kind, &v);
            let bytes: Vec<u8> = raw
                .iter()
                .copied()
                .cycle()
                .take((end - start) * size)
                .collect();
            buffer::write_bytes(agent, buf, offset + start * size, &bytes);
        }
    }
    Ok(Value::Object(o))
}

fn filter(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let (o, kind, len) = this_array(agent, &call, "filter")?;
    let f = callback(agent, call.arg(0), "filter")?;
    let this_arg = call.arg(1);
    let mut kept = Vec::new();
    for k in 0..len {
        let v = get(agent, o, k);
        let r = ops::call(
            agent,
            f,
            this_arg,
            &[v, Value::Number(k as f64), Value::Object(o)],
        )?;
        if ops::to_boolean(agent, r) {
            kept.push(v);
        }
    }
    let a = species_create(agent, o, kind, &[Value::Number(kept.len() as f64)])?;
    for (n, v) in kept.into_iter().enumerate() {
        put(agent, a, n, v)?;
    }
    Ok(Value::Object(a))
}

/// The start index of `includes` and `indexOf`, or `None` if the search
/// cannot find anything.
fn from_index(agent: &mut Agent, v: Value, len: usize) -> JsResult<Option<usize>> {
    let n = ops::to_integer_or_infinity(agent, v)?;
    if n == f64::INFINITY {
        return Ok(None);
    }
    let k = if n >= 0.0 {
        n
    } else {
        (len as f64 + n).max(0.0)
    };
    Ok((k < len as f64).then_some(k as usize))
}

fn includes(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let (o, _, len) = this_array(agent, &call, "includes")?;
    if len == 0 {
        return Ok(Value::Boolean(false));
    }
    let Some(start) = from_index(agent, call.arg(1), len)? else {
        return Ok(Value::Boolean(false));
    };
    let search = call.arg(0);
    for k in start..len {
        let v = get(agent, o, k);
        if ops::same_value_zero(agent, search, v) {
            return Ok(Value::Boolean(true));
        }
    }
    Ok(Value::Boolean(false))
}

fn index_of(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let (o, _, len) = this_array(agent, &call, "indexOf")?;
    if len == 0 {
        return Ok(Value::Number(-1.0));
    }
    let Some(start) = from_index(agent, call.arg(1), len)? else {
        return Ok(Value::Number(-1.0));
    };
    let search = call.arg(0);
    for k in start..len {
        if let Some(v) = buffer::typed_array_get_element(agent, o, k as f64) {
            if ops::is_strictly_equal(agent, search, v) {
                return Ok(Value::Number(k as f64));
            }
        }
    }
    Ok(Value::Number(-1.0))
}

fn last_index_of(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let (o, _, len) = this_array(agent, &call, "lastIndexOf")?;
    if len == 0 {
        return Ok(Value::Number(-1.0));
    }
    let n = if call.args.len() > 1 {
        ops::to_integer_or_infinity(agent, call.arg(1))?
    } else {
        len as f64 - 1.0
    };
    if n == f64::NEG_INFINITY {
        return Ok(Value::Number(-1.0));
    }
    let mut k = if n >= 0.0 {
        n.min(len as f64 - 1.0)
    } else {
        len as f64 + n
    };
    let search = call.arg(0);
    while k >= 0.0 {
        if let Some(v) = buffer::typed_array_get_element(agent, o, k) {
            if ops::is_strictly_equal(agent, search, v) {
                return Ok(Value::Number(k));
            }
        }
        k -= 1.0;
    }
    Ok(Value::Number(-1.0))
}

fn join(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let (o, _, len) = this_array(agent, &call, "join")?;
    let sep = match call.arg(0) {
        Value::Undefined => agent.heap.alloc_str(","),
        s => ops::to_string(agent, s)?,
    };
    let mut out: Vec<u16> = Vec::new();
    for k in 0..len {
        if k > 0 {
            append(agent, &mut out, sep)?;
        }
        let v = get(agent, o, k);
        if !v.is_undefined() {
            let s = ops::to_string(agent, v)?;
            append(agent, &mut out, s)?;
        }
    }
    Ok(Value::String(agent.heap.alloc_string_owned(out)))
}

fn map(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let (o, kind, len) = this_array(agent, &call, "map")?;
    let f = callback(agent, call.arg(0), "map")?;
    let this_arg = call.arg(1);
    let a = species_create(agent, o, kind, &[Value::Number(len as f64)])?;
    for k in 0..len {
        let v = get(agent, o, k);
        let mapped = ops::call(
            agent,
            f,
            this_arg,
            &[v, Value::Number(k as f64), Value::Object(o)],
        )?;
        let key = ops::index_key(agent, k as u64);
        ops::set(agent, a, key, mapped, true)?;
    }
    Ok(Value::Object(a))
}

fn reduce_impl(
    agent: &mut Agent,
    call: &NativeCall<'_>,
    name: &str,
    right: bool,
) -> JsResult<Value> {
    let (o, _, len) = this_array(agent, call, name)?;
    let f = callback(agent, call.arg(0), name)?;
    let index = |i: usize| if right { len - 1 - i } else { i };
    let mut start = 0;
    let mut acc = if call.args.len() > 1 {
        call.arg(1)
    } else {
        if len == 0 {
            return agent.type_error(&format!(
                "%TypedArray%.prototype.{name}: empty array with no initial value"
            ));
        }
        start = 1;
        get(agent, o, index(0))
    };
    for i in start..len {
        let k = index(i);
        let v = get(agent, o, k);
        acc = ops::call(
            agent,
            f,
            Value::Undefined,
            &[acc, v, Value::Number(k as f64), Value::Object(o)],
        )?;
    }
    Ok(acc)
}

fn reduce(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    reduce_impl(agent, &call, "reduce", false)
}

fn reduce_right(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    reduce_impl(agent, &call, "reduceRight", true)
}

fn reverse(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let (o, kind, len) = this_array(agent, &call, "reverse")?;
    let Some((_, buf, offset, _)) = buffer::typed_array_slots(agent, o) else {
        return Ok(Value::Object(o));
    };
    let size = kind.element_size();
    let mut lower = 0;
    while lower < len / 2 {
        let upper = len - 1 - lower;
        let a = buffer::read_bytes(agent, buf, offset + lower * size, size);
        let b = buffer::read_bytes(agent, buf, offset + upper * size, size);
        buffer::write_bytes(agent, buf, offset + lower * size, &b);
        buffer::write_bytes(agent, buf, offset + upper * size, &a);
        lower += 1;
    }
    Ok(Value::Object(o))
}

fn set(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let (target, kind) = require_typed_array(agent, call.this, "%TypedArray%.prototype.set")?;
    let target_offset = ops::to_integer_or_infinity(agent, call.arg(1))?;
    if target_offset < 0.0 {
        return agent.range_error("%TypedArray%.prototype.set: the offset is negative");
    }
    let source = call.arg(0);
    let source_ta = match source {
        Value::Object(s) => kind_of(agent, s).map(|k| (s, k)),
        _ => None,
    };
    if let Some((src, src_kind)) = source_ta {
        set_from_typed_array(agent, target, kind, target_offset, src, src_kind)?;
    } else {
        set_from_array_like(agent, target, target_offset, source)?;
    }
    Ok(Value::Undefined)
}

/// SetTypedArrayFromTypedArray.
fn set_from_typed_array(
    agent: &mut Agent,
    target: Object,
    kind: TypedArrayKind,
    target_offset: f64,
    src: Object,
    src_kind: TypedArrayKind,
) -> JsResult<()> {
    let Some(target_len) = buffer::typed_array_length(agent, target) else {
        return agent
            .type_error("%TypedArray%.prototype.set: the target is detached or out of bounds");
    };
    let Some(src_len) = buffer::typed_array_length(agent, src) else {
        return agent
            .type_error("%TypedArray%.prototype.set: the source is detached or out of bounds");
    };
    if target_offset == f64::INFINITY || src_len as f64 + target_offset > target_len as f64 {
        return agent.range_error("%TypedArray%.prototype.set: the source does not fit");
    }
    if kind.is_bigint() != src_kind.is_bigint() {
        return agent.type_error("%TypedArray%.prototype.set: cannot mix BigInt and Number arrays");
    }
    let (Some((_, src_buf, src_offset, _)), Some((_, target_buf, target_byte_offset, _))) = (
        buffer::typed_array_slots(agent, src),
        buffer::typed_array_slots(agent, target),
    ) else {
        return Ok(());
    };
    // Reading all the source bytes first is CloneArrayBuffer when both
    // views share a buffer.
    let src_size = src_kind.element_size();
    let bytes = buffer::read_bytes(agent, src_buf, src_offset, src_len * src_size);
    let target_index = target_offset as usize * kind.element_size() + target_byte_offset;
    if src_kind == kind {
        buffer::write_bytes(agent, target_buf, target_index, &bytes);
    } else {
        for k in 0..src_len {
            let v = buffer::raw_bytes_to_numeric(src_kind, &bytes[k * src_size..]);
            buffer::set_value_in_buffer(
                agent,
                target_buf,
                target_index + k * kind.element_size(),
                kind,
                &v,
                true,
            );
        }
    }
    Ok(())
}

/// SetTypedArrayFromArrayLike.
fn set_from_array_like(
    agent: &mut Agent,
    target: Object,
    target_offset: f64,
    source: Value,
) -> JsResult<()> {
    let Some(target_len) = buffer::typed_array_length(agent, target) else {
        return agent
            .type_error("%TypedArray%.prototype.set: the target is detached or out of bounds");
    };
    let src = ops::to_object(agent, source)?;
    let src_len = ops::length_of_array_like(agent, src)?;
    if target_offset == f64::INFINITY || src_len + target_offset > target_len as f64 {
        return agent.range_error("%TypedArray%.prototype.set: the source does not fit");
    }
    for k in 0..src_len as u64 {
        let key = ops::index_key(agent, k);
        let v = ops::get(agent, src, key)?;
        buffer::typed_array_set_element(agent, target, target_offset + k as f64, v)?;
    }
    Ok(())
}

fn slice(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let (o, kind, len) = this_array(agent, &call, "slice")?;
    let lenf = len as f64;
    let start = relative(agent, call.arg(0), lenf, 0.0)?;
    let end = relative(agent, call.arg(1), lenf, lenf)?;
    let count = (end - start).max(0.0);
    let a = species_create(agent, o, kind, &[Value::Number(count)])?;
    if count > 0.0 {
        let Some(len) = buffer::typed_array_length(agent, o) else {
            return agent.type_error(
                "%TypedArray%.prototype.slice: the TypedArray is detached or out of bounds",
            );
        };
        let end = end.min(len as f64);
        let count = (end - start).max(0.0) as usize;
        let start = start as usize;
        let target_kind = kind_of(agent, a).unwrap_or(kind);
        if target_kind == kind {
            let (Some((_, src_buf, src_offset, _)), Some((_, target_buf, target_offset, _))) = (
                buffer::typed_array_slots(agent, o),
                buffer::typed_array_slots(agent, a),
            ) else {
                return Ok(Value::Object(a));
            };
            let size = kind.element_size();
            buffer::copy_bytes_forward(
                agent,
                src_buf,
                src_offset + start * size,
                target_buf,
                target_offset,
                count * size,
            );
        } else {
            for n in 0..count {
                let v = get(agent, o, start + n);
                put(agent, a, n, v)?;
            }
        }
    }
    Ok(Value::Object(a))
}

/// The comparison of SortCompare without a comparator: numeric order,
/// `-0` before `+0`, NaN last.
fn compare_default(agent: &Agent, x: Value, y: Value) -> f64 {
    match (x, y) {
        (Value::BigInt(a), Value::BigInt(b)) => {
            match agent.heap.bigint(a).cmp_bigint(agent.heap.bigint(b)) {
                std::cmp::Ordering::Less => -1.0,
                std::cmp::Ordering::Equal => 0.0,
                std::cmp::Ordering::Greater => 1.0,
            }
        }
        (Value::Number(a), Value::Number(b)) => {
            if a.is_nan() && b.is_nan() {
                0.0
            } else if a.is_nan() {
                1.0
            } else if b.is_nan() {
                -1.0
            } else if a < b {
                -1.0
            } else if a > b {
                1.0
            } else if a == 0.0 && b == 0.0 {
                match (a.is_sign_negative(), b.is_sign_negative()) {
                    (true, false) => -1.0,
                    (false, true) => 1.0,
                    _ => 0.0,
                }
            } else {
                0.0
            }
        }
        _ => 0.0,
    }
}

fn compare(agent: &mut Agent, x: Value, y: Value, comparefn: Value) -> JsResult<f64> {
    if comparefn.is_undefined() {
        return Ok(compare_default(agent, x, y));
    }
    let r = ops::call(agent, comparefn, Value::Undefined, &[x, y])?;
    let v = ops::to_number(agent, r)?;
    Ok(if v.is_nan() { 0.0 } else { v })
}

/// A stable merge sort whose comparisons may throw.
fn sort_values(agent: &mut Agent, items: Vec<Value>, comparefn: Value) -> JsResult<Vec<Value>> {
    let n = items.len();
    let mut src = items;
    let mut dst = src.clone();
    let mut width = 1;
    while width < n {
        let mut lo = 0;
        while lo < n {
            let mid = (lo + width).min(n);
            let hi = (lo + 2 * width).min(n);
            let (mut l, mut r, mut out) = (lo, mid, lo);
            while l < mid && r < hi {
                if compare(agent, src[l], src[r], comparefn)? > 0.0 {
                    dst[out] = src[r];
                    r += 1;
                } else {
                    dst[out] = src[l];
                    l += 1;
                }
                out += 1;
            }
            dst[out..out + (mid - l)].copy_from_slice(&src[l..mid]);
            out += mid - l;
            dst[out..out + (hi - r)].copy_from_slice(&src[r..hi]);
            lo = hi;
        }
        std::mem::swap(&mut src, &mut dst);
        width *= 2;
    }
    Ok(src)
}

fn comparefn_argument(agent: &mut Agent, v: Value, name: &str) -> JsResult<Value> {
    if v.is_undefined() || ops::is_callable(agent, v) {
        Ok(v)
    } else {
        agent.type_error(&format!(
            "%TypedArray%.prototype.{name}: the comparison function is not a function"
        ))
    }
}

fn sort(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let comparefn = comparefn_argument(agent, call.arg(0), "sort")?;
    let (o, _, len) = this_array(agent, &call, "sort")?;
    let items: Vec<Value> = (0..len).map(|k| get(agent, o, k)).collect();
    let sorted = sort_values(agent, items, comparefn)?;
    for (k, v) in sorted.into_iter().enumerate() {
        put(agent, o, k, v)?;
    }
    Ok(Value::Object(o))
}

fn subarray(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let (o, kind) = require_typed_array(agent, call.this, "%TypedArray%.prototype.subarray")?;
    let Some((_, buf, offset, length)) = buffer::typed_array_slots(agent, o) else {
        return agent.type_error("not a TypedArray");
    };
    let src_len = buffer::typed_array_length(agent, o).unwrap_or(0) as f64;
    let start = relative(agent, call.arg(0), src_len, 0.0)?;
    let size = kind.element_size() as f64;
    let begin = offset as f64 + start * size;
    let args = if length.is_none() && call.arg(1).is_undefined() {
        vec![Value::Object(buf), Value::Number(begin)]
    } else {
        let end = relative(agent, call.arg(1), src_len, src_len)?;
        let new_len = (end - start).max(0.0);
        vec![
            Value::Object(buf),
            Value::Number(begin),
            Value::Number(new_len),
        ]
    };
    Ok(Value::Object(species_create(agent, o, kind, &args)?))
}

fn to_locale_string(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let (o, _, len) = this_array(agent, &call, "toLocaleString")?;
    let sep = agent.heap.alloc_str(",");
    let method = agent.heap.key_from_str("toLocaleString");
    let mut out: Vec<u16> = Vec::new();
    for k in 0..len {
        if k > 0 {
            append(agent, &mut out, sep)?;
        }
        let v = get(agent, o, k);
        if !v.is_nullish() {
            let r = ops::invoke(agent, v, method, &[])?;
            let s = ops::to_string(agent, r)?;
            append(agent, &mut out, s)?;
        }
    }
    Ok(Value::String(agent.heap.alloc_string_owned(out)))
}

fn to_reversed(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let (o, kind, len) = this_array(agent, &call, "toReversed")?;
    let a = create_same_type(agent, kind, len)?;
    for k in 0..len {
        let v = get(agent, o, len - 1 - k);
        put(agent, a, k, v)?;
    }
    Ok(Value::Object(a))
}

fn to_sorted(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let comparefn = comparefn_argument(agent, call.arg(0), "toSorted")?;
    let (o, kind, len) = this_array(agent, &call, "toSorted")?;
    let a = create_same_type(agent, kind, len)?;
    let items: Vec<Value> = (0..len).map(|k| get(agent, o, k)).collect();
    let sorted = sort_values(agent, items, comparefn)?;
    for (k, v) in sorted.into_iter().enumerate() {
        put(agent, a, k, v)?;
    }
    Ok(Value::Object(a))
}

fn with(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let (o, kind, len) = this_array(agent, &call, "with")?;
    let rel = ops::to_integer_or_infinity(agent, call.arg(0))?;
    let actual = if rel >= 0.0 { rel } else { len as f64 + rel };
    let v = buffer::to_element_value(agent, kind, call.arg(1))?;
    if !buffer::is_valid_integer_index(agent, o, actual) {
        return agent.range_error("%TypedArray%.prototype.with: index out of range");
    }
    let a = create_same_type(agent, kind, len)?;
    let replacement = buffer::element_to_value(agent, v);
    for k in 0..len {
        let from = if k as f64 == actual {
            replacement
        } else {
            get(agent, o, k)
        };
        put(agent, a, k, from)?;
    }
    Ok(Value::Object(a))
}

// ----- Uint8Array base64 and hex -----

fn install_uint8_base64(agent: &mut Agent, realm: Realm) {
    let ctor = intrinsic(agent, realm, Intrinsic::Uint8Array);
    let proto = intrinsic(agent, realm, Intrinsic::Uint8ArrayPrototype);
    method(agent, realm, ctor, "fromBase64", from_base64, 1);
    method(agent, realm, ctor, "fromHex", from_hex, 1);
    let methods: &[(&str, NativeFn, u32)] = &[
        ("setFromBase64", set_from_base64, 1),
        ("setFromHex", set_from_hex, 1),
        ("toBase64", to_base64, 0),
        ("toHex", to_hex, 0),
    ];
    for &(name, f, length) in methods {
        method(agent, realm, proto, name, f, length);
    }
}

/// ValidateUint8Array(ta).
fn validate_uint8(agent: &mut Agent, v: Value, what: &str) -> JsResult<Object> {
    if let Value::Object(o) = v {
        if kind_of(agent, o) == Some(TypedArrayKind::Uint8) {
            return Ok(o);
        }
    }
    agent.type_error(&format!("{what}: not a Uint8Array"))
}

/// GetOptionsObject(options).
fn options_object(agent: &mut Agent, v: Value, what: &str) -> JsResult<Option<Object>> {
    match v {
        Value::Undefined => Ok(None),
        Value::Object(o) => Ok(Some(o)),
        _ => agent.type_error(&format!("{what}: options must be an object")),
    }
}

fn option(agent: &mut Agent, opts: Option<Object>, name: &str) -> JsResult<Value> {
    match opts {
        Some(o) => {
            let key = agent.heap.key_from_str(name);
            ops::get(agent, o, key)
        }
        None => Ok(Value::Undefined),
    }
}

/// A string option that must be one of `allowed` (the first if undefined);
/// its index.
fn choice(
    agent: &mut Agent,
    opts: Option<Object>,
    name: &str,
    allowed: &[&str],
    what: &str,
) -> JsResult<usize> {
    let v = option(agent, opts, name)?;
    let Value::String(s) = v else {
        if v.is_undefined() {
            return Ok(0);
        }
        return agent.type_error(&format!("{what}: '{name}' must be a string"));
    };
    let s = agent.heap.string_to_rust(s);
    match allowed.iter().position(|a| *a == s) {
        Some(i) => Ok(i),
        None => agent.type_error(&format!("{what}: invalid '{name}' option")),
    }
}

/// The `alphabet` option: whether it is base64url.
fn alphabet(agent: &mut Agent, opts: Option<Object>, what: &str) -> JsResult<bool> {
    Ok(choice(agent, opts, "alphabet", &["base64", "base64url"], what)? == 1)
}

fn last_chunk_handling(agent: &mut Agent, opts: Option<Object>, what: &str) -> JsResult<LastChunk> {
    let i = choice(
        agent,
        opts,
        "lastChunkHandling",
        &["loose", "strict", "stop-before-partial"],
        what,
    )?;
    Ok([
        LastChunk::Loose,
        LastChunk::Strict,
        LastChunk::StopBeforePartial,
    ][i])
}

/// GetUint8ArrayBytes(ta).
fn uint8_bytes(agent: &mut Agent, o: Object, what: &str) -> JsResult<Vec<u8>> {
    let Some(len) = buffer::typed_array_length(agent, o) else {
        return agent.type_error(&format!("{what}: the array is detached or out of bounds"));
    };
    let Some((_, buf, offset, _)) = buffer::typed_array_slots(agent, o) else {
        return Ok(Vec::new());
    };
    Ok(buffer::read_bytes(agent, buf, offset, len))
}

fn string_argument(agent: &mut Agent, v: Value, what: &str) -> JsResult<Vec<u16>> {
    match v {
        Value::String(s) => Ok(agent.heap.string_units(s).to_vec()),
        _ => agent.type_error(&format!("{what}: the argument is not a string")),
    }
}

/// A new Uint8Array holding `bytes`.
fn uint8_array_of(agent: &mut Agent, bytes: &[u8]) -> JsResult<Object> {
    let proto = agent.intrinsic(Intrinsic::Uint8ArrayPrototype);
    let o = allocate(agent, TypedArrayKind::Uint8, proto, bytes.len() as f64)?;
    if let Some((_, buf, _, _)) = buffer::typed_array_slots(agent, o) {
        buffer::write_bytes(agent, buf, 0, bytes);
    }
    Ok(o)
}

/// SetUint8ArrayBytes(into, bytes) plus the `{ read, written }` result.
fn store_decoded(
    agent: &mut Agent,
    into: Object,
    decoded: base64::Decoded,
    what: &str,
) -> JsResult<Value> {
    if let Some((_, buf, offset, _)) = buffer::typed_array_slots(agent, into) {
        buffer::write_bytes(agent, buf, offset, &decoded.bytes);
    }
    if decoded.error {
        return agent.syntax_error(&format!("{what}: invalid input"));
    }
    let proto = agent.intrinsic(Intrinsic::ObjectPrototype);
    let result = plain_object(agent, proto);
    let key = agent.heap.key_from_str("read");
    ops::create_data_property_or_throw(agent, result, key, Value::Number(decoded.read as f64))?;
    let key = agent.heap.key_from_str("written");
    let written = Value::Number(decoded.bytes.len() as f64);
    ops::create_data_property_or_throw(agent, result, key, written)?;
    Ok(Value::Object(result))
}

fn from_base64(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    const WHAT: &str = "Uint8Array.fromBase64";
    let units = string_argument(agent, call.arg(0), WHAT)?;
    let opts = options_object(agent, call.arg(1), WHAT)?;
    let url = alphabet(agent, opts, WHAT)?;
    let last = last_chunk_handling(agent, opts, WHAT)?;
    let decoded = base64::from_base64(&units, url, last, usize::MAX);
    if decoded.error {
        return agent.syntax_error(&format!("{WHAT}: invalid base64"));
    }
    Ok(Value::Object(uint8_array_of(agent, &decoded.bytes)?))
}

fn from_hex(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    const WHAT: &str = "Uint8Array.fromHex";
    let units = string_argument(agent, call.arg(0), WHAT)?;
    let decoded = base64::from_hex(&units, usize::MAX);
    if decoded.error {
        return agent.syntax_error(&format!("{WHAT}: invalid hex"));
    }
    Ok(Value::Object(uint8_array_of(agent, &decoded.bytes)?))
}

fn set_from_base64(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    const WHAT: &str = "Uint8Array.prototype.setFromBase64";
    let into = validate_uint8(agent, call.this, WHAT)?;
    let units = string_argument(agent, call.arg(0), WHAT)?;
    let opts = options_object(agent, call.arg(1), WHAT)?;
    let url = alphabet(agent, opts, WHAT)?;
    let last = last_chunk_handling(agent, opts, WHAT)?;
    let Some(len) = buffer::typed_array_length(agent, into) else {
        return agent.type_error(&format!("{WHAT}: the array is detached or out of bounds"));
    };
    let decoded = base64::from_base64(&units, url, last, len);
    store_decoded(agent, into, decoded, WHAT)
}

fn set_from_hex(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    const WHAT: &str = "Uint8Array.prototype.setFromHex";
    let into = validate_uint8(agent, call.this, WHAT)?;
    let units = string_argument(agent, call.arg(0), WHAT)?;
    let Some(len) = buffer::typed_array_length(agent, into) else {
        return agent.type_error(&format!("{WHAT}: the array is detached or out of bounds"));
    };
    let decoded = base64::from_hex(&units, len);
    store_decoded(agent, into, decoded, WHAT)
}

fn to_base64(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    const WHAT: &str = "Uint8Array.prototype.toBase64";
    let o = validate_uint8(agent, call.this, WHAT)?;
    let opts = options_object(agent, call.arg(0), WHAT)?;
    let url = alphabet(agent, opts, WHAT)?;
    let omit = option(agent, opts, "omitPadding")?;
    let omit = ops::to_boolean(agent, omit);
    let bytes = uint8_bytes(agent, o, WHAT)?;
    let s = base64::to_base64(&bytes, url, omit);
    Ok(Value::String(agent.heap.alloc_str(&s)))
}

fn to_hex(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    const WHAT: &str = "Uint8Array.prototype.toHex";
    let o = validate_uint8(agent, call.this, WHAT)?;
    let bytes = uint8_bytes(agent, o, WHAT)?;
    let s = base64::to_hex(&bytes);
    Ok(Value::String(agent.heap.alloc_str(&s)))
}
