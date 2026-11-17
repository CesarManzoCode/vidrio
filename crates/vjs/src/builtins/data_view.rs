//! `DataView` (ECMA-262 §25.3): views over fixed-length, resizable and
//! shared buffers with the get and set methods of every element type,
//! `getFloat16` and `setFloat16` included.

use crate::agent::Agent;
use crate::buffer::{self, DataViewData, TypedArrayKind};
use crate::builtins::{
    accessor, constructor, intrinsic, method, plain_object, set_intrinsic, to_string_tag,
};
use crate::completion::JsResult;
use crate::function::{NativeCall, NativeFn};
use crate::gc::{Object, Realm};
use crate::object::ObjectKind;
use crate::ops;
use crate::realm::Intrinsic;
use crate::value::Value;

pub(crate) fn install(agent: &mut Agent, realm: Realm) {
    let object_proto = intrinsic(agent, realm, Intrinsic::ObjectPrototype);
    let proto = plain_object(agent, object_proto);
    set_intrinsic(agent, realm, Intrinsic::DataViewPrototype, proto);
    let ctor = constructor(
        agent,
        realm,
        "DataView",
        data_view_constructor,
        1,
        proto,
        None,
    );
    set_intrinsic(agent, realm, Intrinsic::DataView, ctor);

    let getters: &[(&str, NativeFn)] = &[
        ("buffer", get_buffer),
        ("byteLength", get_byte_length),
        ("byteOffset", get_byte_offset),
    ];
    for &(name, f) in getters {
        let key = agent.heap.key_from_str(name);
        accessor(agent, realm, proto, key, Some(f), None);
    }
    let methods: &[(&str, NativeFn, u32)] = &[
        ("getBigInt64", get_big_int64, 1),
        ("getBigUint64", get_big_uint64, 1),
        ("getFloat16", get_float16, 1),
        ("getFloat32", get_float32, 1),
        ("getFloat64", get_float64, 1),
        ("getInt8", get_int8, 1),
        ("getInt16", get_int16, 1),
        ("getInt32", get_int32, 1),
        ("getUint8", get_uint8, 1),
        ("getUint16", get_uint16, 1),
        ("getUint32", get_uint32, 1),
        ("setBigInt64", set_big_int64, 2),
        ("setBigUint64", set_big_uint64, 2),
        ("setFloat16", set_float16, 2),
        ("setFloat32", set_float32, 2),
        ("setFloat64", set_float64, 2),
        ("setInt8", set_int8, 2),
        ("setInt16", set_int16, 2),
        ("setInt32", set_int32, 2),
        ("setUint8", set_uint8, 2),
        ("setUint16", set_uint16, 2),
        ("setUint32", set_uint32, 2),
    ];
    for &(name, f, length) in methods {
        method(agent, realm, proto, name, f, length);
    }
    to_string_tag(agent, proto, "DataView");
}

fn data_view_constructor(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let Some(new_target) = call.new_target else {
        return agent.type_error("DataView constructor requires 'new'");
    };
    let buffer = match call.arg(0) {
        Value::Object(o) if matches!(agent.heap.object(o).kind, ObjectKind::ArrayBuffer(_)) => o,
        _ => return agent.type_error("DataView: the first argument is not an ArrayBuffer"),
    };
    let offset = ops::to_index(agent, call.arg(1))?;
    if buffer::is_detached(agent, buffer) {
        return agent.type_error("DataView: the buffer is detached");
    }
    let buffer_len = buffer::buffer_byte_length(agent, buffer) as u64;
    if offset > buffer_len {
        return agent.range_error("DataView: the offset is past the end of the buffer");
    }
    let fixed = buffer::buffer_data(agent, buffer).is_some_and(|d| d.is_fixed_length());
    let byte_length = call.arg(2);
    let view_length = if byte_length.is_undefined() {
        fixed.then_some(buffer_len - offset)
    } else {
        let len = ops::to_index(agent, byte_length)?;
        if offset + len > buffer_len {
            return agent.range_error("DataView: the view does not fit in the buffer");
        }
        Some(len)
    };
    let proto =
        ops::get_prototype_from_constructor(agent, new_target, Intrinsic::DataViewPrototype)?;
    // Getting the prototype may have run code that changed the buffer.
    if buffer::is_detached(agent, buffer) {
        return agent.type_error("DataView: the buffer is detached");
    }
    let buffer_len = buffer::buffer_byte_length(agent, buffer) as u64;
    if offset > buffer_len {
        return agent.range_error("DataView: the offset is past the end of the buffer");
    }
    if !byte_length.is_undefined() && offset + view_length.unwrap_or(0) > buffer_len {
        return agent.range_error("DataView: the view does not fit in the buffer");
    }
    let o = agent.heap.create_object(
        Some(proto),
        ObjectKind::DataView(Box::new(DataViewData {
            buffer,
            byte_offset: offset as usize,
            byte_length: view_length.map(|l| l as usize),
        })),
    );
    Ok(Value::Object(o))
}

/// RequireInternalSlot(view, [[DataView]]): buffer, offset and length.
fn this_view(
    agent: &mut Agent,
    this: Value,
    what: &str,
) -> JsResult<(Object, usize, Option<usize>)> {
    if let Value::Object(o) = this {
        if let ObjectKind::DataView(d) = &agent.heap.object(o).kind {
            return Ok((d.buffer, d.byte_offset, d.byte_length));
        }
    }
    agent.type_error(&format!("DataView.prototype.{what}: not a DataView"))
}

/// GetViewByteLength, or `None` if IsViewOutOfBounds.
fn view_byte_length(
    agent: &Agent,
    buffer: Object,
    offset: usize,
    length: Option<usize>,
) -> Option<usize> {
    if buffer::is_detached(agent, buffer) {
        return None;
    }
    let buffer_len = buffer::buffer_byte_length(agent, buffer);
    match length {
        Some(len) => (offset + len <= buffer_len).then_some(len),
        None => (offset <= buffer_len).then(|| buffer_len - offset),
    }
}

fn get_buffer(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let (buffer, _, _) = this_view(agent, call.this, "buffer")?;
    Ok(Value::Object(buffer))
}

fn get_byte_length(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let (buffer, offset, length) = this_view(agent, call.this, "byteLength")?;
    match view_byte_length(agent, buffer, offset, length) {
        Some(len) => Ok(Value::Number(len as f64)),
        // An out-of-bounds or detached view reports +0 (no TypeError).
        None => Ok(Value::Number(0.0)),
    }
}

fn get_byte_offset(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let (buffer, offset, length) = this_view(agent, call.this, "byteOffset")?;
    match view_byte_length(agent, buffer, offset, length) {
        Some(_) => Ok(Value::Number(offset as f64)),
        None => Ok(Value::Number(0.0)),
    }
}

/// GetViewValue(view, requestIndex, isLittleEndian, type).
fn get_view_value(
    agent: &mut Agent,
    call: &NativeCall<'_>,
    kind: TypedArrayKind,
) -> JsResult<Value> {
    let (buffer, offset, length) = this_view(agent, call.this, "get")?;
    let index = ops::to_index(agent, call.arg(0))?;
    let little_endian = ops::to_boolean(agent, call.arg(1));
    let Some(view_len) = view_byte_length(agent, buffer, offset, length) else {
        return agent.type_error("DataView: the view is out of bounds");
    };
    if index + kind.element_size() as u64 > view_len as u64 {
        return agent.range_error("DataView: the index is out of range");
    }
    Ok(buffer::get_value_from_buffer(
        agent,
        buffer,
        index as usize + offset,
        kind,
        little_endian,
    ))
}

/// SetViewValue(view, requestIndex, isLittleEndian, type, value).
fn set_view_value(
    agent: &mut Agent,
    call: &NativeCall<'_>,
    kind: TypedArrayKind,
) -> JsResult<Value> {
    let (buffer, offset, length) = this_view(agent, call.this, "set")?;
    let index = ops::to_index(agent, call.arg(0))?;
    let value = buffer::to_element_value(agent, kind, call.arg(1))?;
    let little_endian = ops::to_boolean(agent, call.arg(2));
    let Some(view_len) = view_byte_length(agent, buffer, offset, length) else {
        return agent.type_error("DataView: the view is out of bounds");
    };
    if index + kind.element_size() as u64 > view_len as u64 {
        return agent.range_error("DataView: the index is out of range");
    }
    buffer::set_value_in_buffer(
        agent,
        buffer,
        index as usize + offset,
        kind,
        &value,
        little_endian,
    );
    Ok(Value::Undefined)
}

macro_rules! view_methods {
    ($($get:ident, $set:ident => $kind:ident;)*) => {
        $(
            fn $get(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
                get_view_value(agent, &call, TypedArrayKind::$kind)
            }

            fn $set(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
                set_view_value(agent, &call, TypedArrayKind::$kind)
            }
        )*
    };
}

view_methods! {
    get_big_int64, set_big_int64 => BigInt64;
    get_big_uint64, set_big_uint64 => BigUint64;
    get_float16, set_float16 => Float16;
    get_float32, set_float32 => Float32;
    get_float64, set_float64 => Float64;
    get_int8, set_int8 => Int8;
    get_int16, set_int16 => Int16;
    get_int32, set_int32 => Int32;
    get_uint8, set_uint8 => Uint8;
    get_uint16, set_uint16 => Uint16;
    get_uint32, set_uint32 => Uint32;
}
