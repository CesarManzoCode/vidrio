//! `ArrayBuffer` (ECMA-262 §25.1) with resizable buffers, `transfer`,
//! `transferToFixedLength` and the detached state, and
//! `SharedArrayBuffer` (§25.2) with growable buffers.

use crate::agent::Agent;
use crate::buffer::{
    self, ArrayBufferData, BufferStorage, MAX_BYTE_LENGTH, SharedBlock, allocate_bytes,
};
use crate::builtins::{
    accessor, constructor, intrinsic, method, plain_object, set_intrinsic, species_getter,
    to_string_tag,
};
use crate::completion::JsResult;
use crate::function::{NativeCall, NativeFn};
use crate::gc::{Object, Realm};
use crate::object::ObjectKind;
use crate::ops;
use crate::realm::Intrinsic;
use crate::value::Value;

pub(crate) fn install(agent: &mut Agent, realm: Realm) {
    install_array_buffer(agent, realm);
    install_shared_array_buffer(agent, realm);
}

fn install_array_buffer(agent: &mut Agent, realm: Realm) {
    let object_proto = intrinsic(agent, realm, Intrinsic::ObjectPrototype);
    let proto = plain_object(agent, object_proto);
    set_intrinsic(agent, realm, Intrinsic::ArrayBufferPrototype, proto);
    let ctor = constructor(
        agent,
        realm,
        "ArrayBuffer",
        array_buffer_constructor,
        1,
        proto,
        None,
    );
    set_intrinsic(agent, realm, Intrinsic::ArrayBuffer, ctor);
    method(agent, realm, ctor, "isView", is_view, 1);
    species_getter(agent, realm, ctor);

    let getters: &[(&str, NativeFn)] = &[
        ("byteLength", ab_byte_length),
        ("detached", ab_detached),
        ("maxByteLength", ab_max_byte_length),
        ("resizable", ab_resizable),
    ];
    for &(name, f) in getters {
        let key = agent.heap.key_from_str(name);
        accessor(agent, realm, proto, key, Some(f), None);
    }
    let methods: &[(&str, NativeFn, u32)] = &[
        ("resize", ab_resize, 1),
        ("slice", ab_slice, 2),
        ("transfer", ab_transfer, 0),
        ("transferToFixedLength", ab_transfer_to_fixed_length, 0),
    ];
    for &(name, f, length) in methods {
        method(agent, realm, proto, name, f, length);
    }
    to_string_tag(agent, proto, "ArrayBuffer");
}

fn install_shared_array_buffer(agent: &mut Agent, realm: Realm) {
    let object_proto = intrinsic(agent, realm, Intrinsic::ObjectPrototype);
    let proto = plain_object(agent, object_proto);
    set_intrinsic(agent, realm, Intrinsic::SharedArrayBufferPrototype, proto);
    let ctor = constructor(
        agent,
        realm,
        "SharedArrayBuffer",
        shared_array_buffer_constructor,
        1,
        proto,
        None,
    );
    set_intrinsic(agent, realm, Intrinsic::SharedArrayBuffer, ctor);
    species_getter(agent, realm, ctor);

    let getters: &[(&str, NativeFn)] = &[
        ("byteLength", sab_byte_length),
        ("growable", sab_growable),
        ("maxByteLength", sab_max_byte_length),
    ];
    for &(name, f) in getters {
        let key = agent.heap.key_from_str(name);
        accessor(agent, realm, proto, key, Some(f), None);
    }
    method(agent, realm, proto, "grow", sab_grow, 1);
    method(agent, realm, proto, "slice", sab_slice, 2);
    to_string_tag(agent, proto, "SharedArrayBuffer");
}

// ----- helpers -----

/// GetArrayBufferMaxByteLengthOption(options).
fn max_byte_length_option(agent: &mut Agent, options: Value) -> JsResult<Option<u64>> {
    let Value::Object(o) = options else {
        return Ok(None);
    };
    let key = agent.heap.key_from_str("maxByteLength");
    let max = ops::get(agent, o, key)?;
    if max.is_undefined() {
        return Ok(None);
    }
    Ok(Some(ops::to_index(agent, max)?))
}

/// AllocateArrayBuffer(constructor, byteLength[, maxByteLength]).
pub(crate) fn allocate_array_buffer(
    agent: &mut Agent,
    ctor: Object,
    byte_length: u64,
    max_byte_length: Option<u64>,
) -> JsResult<Object> {
    if max_byte_length.is_some_and(|max| byte_length > max) {
        return agent.range_error("ArrayBuffer byte length exceeds maxByteLength");
    }
    let proto = ops::get_prototype_from_constructor(agent, ctor, Intrinsic::ArrayBufferPrototype)?;
    let Some(bytes) = usize::try_from(byte_length).ok().and_then(allocate_bytes) else {
        return agent.range_error("cannot allocate an ArrayBuffer of that length");
    };
    let max = match max_byte_length {
        Some(max) if max > MAX_BYTE_LENGTH as u64 => {
            return agent.range_error("ArrayBuffer maxByteLength is too large");
        }
        Some(max) => Some(max as usize),
        None => None,
    };
    Ok(buffer::array_buffer_from_bytes(
        agent,
        Some(proto),
        bytes,
        max,
    ))
}

/// AllocateArrayBuffer(%ArrayBuffer%, byteLength) for a size computed by
/// the engine (a TypedArray's buffer).
pub(crate) fn allocate_plain_buffer(agent: &mut Agent, byte_length: f64) -> JsResult<Object> {
    if !(0.0..=MAX_BYTE_LENGTH as f64).contains(&byte_length) {
        return agent.range_error("cannot allocate an ArrayBuffer of that length");
    }
    let ctor = agent.intrinsic(Intrinsic::ArrayBuffer);
    allocate_array_buffer(agent, ctor, byte_length as u64, None)
}

/// RequireInternalSlot(this, [[ArrayBufferData]]) for a non-shared buffer.
fn this_array_buffer(agent: &mut Agent, this: Value, what: &str) -> JsResult<Object> {
    if let Value::Object(o) = this {
        if let ObjectKind::ArrayBuffer(data) = &agent.heap.object(o).kind {
            if !data.is_shared() {
                return Ok(o);
            }
        }
    }
    agent.type_error(&format!("ArrayBuffer.prototype.{what}: not an ArrayBuffer"))
}

fn this_shared_buffer(agent: &mut Agent, this: Value, what: &str) -> JsResult<Object> {
    if let Value::Object(o) = this {
        if let ObjectKind::ArrayBuffer(data) = &agent.heap.object(o).kind {
            if data.is_shared() {
                return Ok(o);
            }
        }
    }
    agent.type_error(&format!(
        "SharedArrayBuffer.prototype.{what}: not a SharedArrayBuffer"
    ))
}

fn data(agent: &Agent, o: Object) -> &ArrayBufferData {
    buffer::buffer_data(agent, o).expect("an ArrayBuffer")
}

/// The relative index `v` clamped to `[0, len]`.
fn relative_index(agent: &mut Agent, v: Value, len: f64, default: f64) -> JsResult<f64> {
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

// ----- ArrayBuffer -----

fn array_buffer_constructor(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let Some(new_target) = call.new_target else {
        return agent.type_error("ArrayBuffer constructor requires 'new'");
    };
    let byte_length = ops::to_index(agent, call.arg(0))?;
    let max = max_byte_length_option(agent, call.arg(1))?;
    let o = allocate_array_buffer(agent, new_target, byte_length, max)?;
    Ok(Value::Object(o))
}

/// `ArrayBuffer.isView(arg)`.
fn is_view(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    Ok(Value::Boolean(match call.arg(0) {
        Value::Object(o) => matches!(
            agent.heap.object(o).kind,
            ObjectKind::TypedArray(_) | ObjectKind::DataView(_)
        ),
        _ => false,
    }))
}

fn ab_byte_length(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let o = this_array_buffer(agent, call.this, "byteLength")?;
    Ok(Value::Number(data(agent, o).byte_length() as f64))
}

fn ab_detached(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let o = this_array_buffer(agent, call.this, "detached")?;
    Ok(Value::Boolean(data(agent, o).is_detached()))
}

fn ab_max_byte_length(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let o = this_array_buffer(agent, call.this, "maxByteLength")?;
    let d = data(agent, o);
    let n = if d.is_detached() {
        0
    } else {
        d.max_byte_length.unwrap_or_else(|| d.byte_length())
    };
    Ok(Value::Number(n as f64))
}

fn ab_resizable(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let o = this_array_buffer(agent, call.this, "resizable")?;
    Ok(Value::Boolean(!data(agent, o).is_fixed_length()))
}

fn ab_resize(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let o = this_array_buffer(agent, call.this, "resize")?;
    let Some(max) = data(agent, o).max_byte_length else {
        return agent.type_error("ArrayBuffer.prototype.resize: the buffer is not resizable");
    };
    let new_length = ops::to_index(agent, call.arg(0))?;
    if data(agent, o).is_detached() {
        return agent.type_error("ArrayBuffer.prototype.resize: the buffer is detached");
    }
    if new_length > max as u64 {
        return agent.range_error("ArrayBuffer.prototype.resize: length exceeds maxByteLength");
    }
    let new_length = new_length as usize;
    let mut grown = 0;
    let mut failed = false;
    if let Some(d) = buffer::buffer_data_mut(agent, o) {
        if let BufferStorage::Owned(v) = &mut d.storage {
            if new_length > v.len() {
                grown = new_length - v.len();
                failed = v.try_reserve_exact(grown).is_err();
            }
            if !failed {
                v.resize(new_length, 0);
            }
        }
    }
    if failed {
        return agent.range_error("ArrayBuffer.prototype.resize: out of memory");
    }
    agent.heap.note_alloc(grown);
    Ok(Value::Undefined)
}

fn ab_slice(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let o = this_array_buffer(agent, call.this, "slice")?;
    if data(agent, o).is_detached() {
        return agent.type_error("ArrayBuffer.prototype.slice: the buffer is detached");
    }
    let len = data(agent, o).byte_length() as f64;
    let first = relative_index(agent, call.arg(0), len, 0.0)?;
    let fin = relative_index(agent, call.arg(1), len, len)?;
    let new_len = (fin - first).max(0.0);
    let default = agent.intrinsic(Intrinsic::ArrayBuffer);
    let ctor = ops::species_constructor(agent, o, default)?;
    let new = ops::construct(agent, ctor, &[Value::Number(new_len)], None)?;
    let ok = match &agent.heap.object(new).kind {
        ObjectKind::ArrayBuffer(d) => !d.is_shared(),
        _ => false,
    };
    if !ok {
        return agent.type_error("ArrayBuffer.prototype.slice: the species is not an ArrayBuffer");
    }
    if data(agent, new).is_detached() {
        return agent.type_error("ArrayBuffer.prototype.slice: the new buffer is detached");
    }
    if new == o {
        return agent.type_error("ArrayBuffer.prototype.slice: the species returned this buffer");
    }
    if (data(agent, new).byte_length() as f64) < new_len {
        return agent.type_error("ArrayBuffer.prototype.slice: the new buffer is too small");
    }
    if data(agent, o).is_detached() {
        return agent.type_error("ArrayBuffer.prototype.slice: the buffer is detached");
    }
    let first = first as usize;
    let current = data(agent, o).byte_length();
    if first < current {
        let count = (new_len as usize).min(current - first);
        let bytes = buffer::read_bytes(agent, o, first, count);
        buffer::write_bytes(agent, new, 0, &bytes);
    }
    Ok(Value::Object(new))
}

/// ArrayBufferCopyAndDetach(O, newLength, preserveResizability).
fn copy_and_detach(
    agent: &mut Agent,
    call: &NativeCall<'_>,
    preserve: bool,
    what: &str,
) -> JsResult<Value> {
    let o = this_array_buffer(agent, call.this, what)?;
    let new_length = if call.arg(0).is_undefined() {
        data(agent, o).byte_length() as u64
    } else {
        ops::to_index(agent, call.arg(0))?
    };
    if data(agent, o).is_detached() {
        return agent.type_error(&format!(
            "ArrayBuffer.prototype.{what}: the buffer is detached"
        ));
    }
    let new_max = if preserve {
        data(agent, o).max_byte_length
    } else {
        None
    };
    if !data(agent, o).detach_key.is_undefined() {
        return agent.type_error(&format!(
            "ArrayBuffer.prototype.{what}: the buffer cannot be detached"
        ));
    }
    if new_max.is_some_and(|max| new_length > max as u64) {
        return agent.range_error(&format!(
            "ArrayBuffer.prototype.{what}: length exceeds maxByteLength"
        ));
    }
    if new_length > MAX_BYTE_LENGTH as u64 {
        return agent.range_error(&format!(
            "ArrayBuffer.prototype.{what}: cannot allocate a buffer of that length"
        ));
    }
    let new_length = new_length as usize;
    let mut bytes = match buffer::buffer_data_mut(agent, o)
        .map(|d| std::mem::replace(&mut d.storage, BufferStorage::Detached))
    {
        Some(BufferStorage::Owned(v)) => v,
        _ => Vec::new(),
    };
    if new_length > bytes.len() {
        if bytes.try_reserve_exact(new_length - bytes.len()).is_err() {
            // Put the bytes back: the buffer is not detached after all.
            if let Some(d) = buffer::buffer_data_mut(agent, o) {
                d.storage = BufferStorage::Owned(bytes);
            }
            return agent.range_error(&format!("ArrayBuffer.prototype.{what}: out of memory"));
        }
        agent.heap.note_alloc(new_length - bytes.len());
    }
    bytes.resize(new_length, 0);
    let proto = agent.intrinsic(Intrinsic::ArrayBufferPrototype);
    let new = agent.heap.create_object(
        Some(proto),
        ObjectKind::ArrayBuffer(Box::new(ArrayBufferData {
            storage: BufferStorage::Owned(bytes),
            max_byte_length: new_max,
            detach_key: Value::Undefined,
        })),
    );
    Ok(Value::Object(new))
}

fn ab_transfer(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    copy_and_detach(agent, &call, true, "transfer")
}

fn ab_transfer_to_fixed_length(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    copy_and_detach(agent, &call, false, "transferToFixedLength")
}

// ----- SharedArrayBuffer -----

/// AllocateSharedArrayBuffer(constructor, byteLength[, maxByteLength]).
fn allocate_shared_array_buffer(
    agent: &mut Agent,
    ctor: Object,
    byte_length: u64,
    max_byte_length: Option<u64>,
) -> JsResult<Object> {
    if max_byte_length.is_some_and(|max| byte_length > max) {
        return agent.range_error("SharedArrayBuffer byte length exceeds maxByteLength");
    }
    let proto =
        ops::get_prototype_from_constructor(agent, ctor, Intrinsic::SharedArrayBufferPrototype)?;
    let capacity = max_byte_length.unwrap_or(byte_length);
    let block = match (usize::try_from(byte_length), usize::try_from(capacity)) {
        (Ok(len), Ok(cap)) => SharedBlock::try_new(len, cap),
        _ => None,
    };
    let Some(block) = block else {
        return agent.range_error("cannot allocate a SharedArrayBuffer of that length");
    };
    agent.heap.note_alloc(byte_length as usize);
    Ok(agent.heap.create_object(
        Some(proto),
        ObjectKind::ArrayBuffer(Box::new(ArrayBufferData {
            storage: BufferStorage::Shared(block),
            max_byte_length: max_byte_length.map(|m| m as usize),
            detach_key: Value::Undefined,
        })),
    ))
}

fn shared_array_buffer_constructor(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let Some(new_target) = call.new_target else {
        return agent.type_error("SharedArrayBuffer constructor requires 'new'");
    };
    let byte_length = ops::to_index(agent, call.arg(0))?;
    let max = max_byte_length_option(agent, call.arg(1))?;
    let o = allocate_shared_array_buffer(agent, new_target, byte_length, max)?;
    Ok(Value::Object(o))
}

fn sab_byte_length(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let o = this_shared_buffer(agent, call.this, "byteLength")?;
    Ok(Value::Number(data(agent, o).byte_length() as f64))
}

fn sab_growable(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let o = this_shared_buffer(agent, call.this, "growable")?;
    Ok(Value::Boolean(!data(agent, o).is_fixed_length()))
}

fn sab_max_byte_length(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let o = this_shared_buffer(agent, call.this, "maxByteLength")?;
    let d = data(agent, o);
    let n = d.max_byte_length.unwrap_or_else(|| d.byte_length());
    Ok(Value::Number(n as f64))
}

fn sab_grow(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let growable = match call.this {
        Value::Object(o) => match &agent.heap.object(o).kind {
            ObjectKind::ArrayBuffer(d) => d.max_byte_length.is_some().then_some((o, d.is_shared())),
            _ => None,
        },
        _ => None,
    };
    let Some((o, true)) = growable else {
        return agent
            .type_error("SharedArrayBuffer.prototype.grow: not a growable SharedArrayBuffer");
    };
    let new_length = ops::to_index(agent, call.arg(0))?;
    let Some(block) = data(agent, o).shared_block().cloned() else {
        return agent.type_error("SharedArrayBuffer.prototype.grow: not a SharedArrayBuffer");
    };
    let max = data(agent, o).max_byte_length.unwrap_or(0);
    if new_length > max as u64 {
        return agent.range_error("SharedArrayBuffer.prototype.grow: length exceeds maxByteLength");
    }
    let old = block.len();
    if !block.grow(new_length as usize) {
        return agent.range_error("SharedArrayBuffer.prototype.grow: cannot shrink");
    }
    agent
        .heap
        .note_alloc((new_length as usize).saturating_sub(old));
    Ok(Value::Undefined)
}

fn sab_slice(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let o = this_shared_buffer(agent, call.this, "slice")?;
    let len = data(agent, o).byte_length() as f64;
    let first = relative_index(agent, call.arg(0), len, 0.0)?;
    let fin = relative_index(agent, call.arg(1), len, len)?;
    let new_len = (fin - first).max(0.0);
    let default = agent.intrinsic(Intrinsic::SharedArrayBuffer);
    let ctor = ops::species_constructor(agent, o, default)?;
    let new = ops::construct(agent, ctor, &[Value::Number(new_len)], None)?;
    let ok = match &agent.heap.object(new).kind {
        ObjectKind::ArrayBuffer(d) => d.is_shared(),
        _ => false,
    };
    if !ok {
        return agent.type_error(
            "SharedArrayBuffer.prototype.slice: the species is not a SharedArrayBuffer",
        );
    }
    let same_block = match (
        data(agent, new).shared_block(),
        data(agent, o).shared_block(),
    ) {
        (Some(a), Some(b)) => a.id() == b.id(),
        _ => false,
    };
    if new == o || same_block {
        return agent
            .type_error("SharedArrayBuffer.prototype.slice: the species returned this buffer");
    }
    if (data(agent, new).byte_length() as f64) < new_len {
        return agent.type_error("SharedArrayBuffer.prototype.slice: the new buffer is too small");
    }
    let bytes = buffer::read_bytes(agent, o, first as usize, new_len as usize);
    buffer::write_bytes(agent, new, 0, &bytes);
    Ok(Value::Object(new))
}

#[cfg(test)]
mod tests {
    use crate::agent::Agent;
    use crate::evaluate_script;
    use crate::value::Value;

    fn eval_bool(src: &str) -> bool {
        let mut agent = Agent::new();
        agent.initialize_host_defined_realm();
        matches!(evaluate_script(&mut agent, src), Ok(Value::Boolean(true)))
    }

    #[test]
    fn resize_and_transfer() {
        assert!(eval_bool(
            "var b = new ArrayBuffer(2, { maxByteLength: 8 });
             b.resize(6);
             var t = b.transfer();
             b.detached && b.byteLength === 0 && t.byteLength === 6 && t.resizable
               && t.transferToFixedLength(3).byteLength === 3 && t.detached"
        ));
    }

    #[test]
    fn shared_buffers_grow_but_never_shrink() {
        assert!(eval_bool(
            "var s = new SharedArrayBuffer(2, { maxByteLength: 8 });
             s.grow(4);
             var threw = false;
             try { s.grow(3); } catch (e) { threw = e instanceof RangeError; }
             threw && s.byteLength === 4 && s.growable && s.maxByteLength === 8"
        ));
    }
}
