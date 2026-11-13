//! Storage of `ArrayBuffer`/`SharedArrayBuffer` and the element access of
//! TypedArrays (ECMA-262 §10.4.5, §25.1). Elements are little-endian.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};

use crate::agent::Agent;
use crate::bigint::BigInt;
use crate::completion::JsResult;
use crate::gc::{Object, Trace, Tracer};
use crate::number::{
    f16_bits_to_f64, f64_to_f16_bits, f64_to_int8, f64_to_int16, f64_to_int32, f64_to_uint8,
    f64_to_uint8_clamp, f64_to_uint16, f64_to_uint32,
};
use crate::object::ObjectKind;
use crate::ops;
use crate::value::Value;

/// Bytes of a `SharedArrayBuffer`, shared between agents (threads).
pub struct SharedBlock {
    bytes: Box<[AtomicU8]>,
    len: AtomicUsize,
}

impl SharedBlock {
    /// A block of `len` zero bytes that can grow up to `capacity`.
    pub fn new(len: usize, capacity: usize) -> Arc<SharedBlock> {
        let bytes = (0..capacity.max(len)).map(|_| AtomicU8::new(0)).collect();
        Arc::new(SharedBlock {
            bytes,
            len: AtomicUsize::new(len),
        })
    }

    pub fn len(&self) -> usize {
        self.len.load(Ordering::SeqCst)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Grows to `new_len` bytes; `false` if it would shrink or exceed the
    /// capacity.
    pub fn grow(&self, new_len: usize) -> bool {
        if new_len > self.bytes.len() {
            return false;
        }
        self.len
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |cur| {
                (new_len >= cur).then_some(new_len)
            })
            .is_ok()
    }

    pub fn bytes(&self) -> &[AtomicU8] {
        &self.bytes
    }
}

pub enum BufferStorage {
    Owned(Vec<u8>),
    Detached,
    Shared(Arc<SharedBlock>),
}

/// Slots of an `ArrayBuffer` or `SharedArrayBuffer`.
pub struct ArrayBufferData {
    pub storage: BufferStorage,
    /// `[[ArrayBufferMaxByteLength]]` of resizable buffers.
    pub max_byte_length: Option<usize>,
    pub detach_key: Value,
}

impl ArrayBufferData {
    pub fn byte_length(&self) -> usize {
        match &self.storage {
            BufferStorage::Owned(v) => v.len(),
            BufferStorage::Detached => 0,
            BufferStorage::Shared(b) => b.len(),
        }
    }

    pub fn is_detached(&self) -> bool {
        matches!(self.storage, BufferStorage::Detached)
    }

    pub fn is_shared(&self) -> bool {
        matches!(self.storage, BufferStorage::Shared(_))
    }

    fn read(&self, offset: usize, out: &mut [u8]) {
        match &self.storage {
            BufferStorage::Owned(v) => out.copy_from_slice(&v[offset..offset + out.len()]),
            BufferStorage::Detached => out.fill(0),
            BufferStorage::Shared(b) => {
                for (i, byte) in out.iter_mut().enumerate() {
                    *byte = b.bytes()[offset + i].load(Ordering::SeqCst);
                }
            }
        }
    }

    fn write(&mut self, offset: usize, bytes: &[u8]) {
        match &mut self.storage {
            BufferStorage::Owned(v) => v[offset..offset + bytes.len()].copy_from_slice(bytes),
            BufferStorage::Detached => {}
            BufferStorage::Shared(b) => {
                for (i, byte) in bytes.iter().enumerate() {
                    b.bytes()[offset + i].store(*byte, Ordering::SeqCst);
                }
            }
        }
    }
}

impl Trace for ArrayBufferData {
    fn trace(&self, tracer: &mut Tracer) {
        tracer.value(self.detach_key);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TypedArrayKind {
    Int8,
    Uint8,
    Uint8Clamped,
    Int16,
    Uint16,
    Int32,
    Uint32,
    Float16,
    Float32,
    Float64,
    BigInt64,
    BigUint64,
}

impl TypedArrayKind {
    pub fn element_size(self) -> usize {
        match self {
            TypedArrayKind::Int8 | TypedArrayKind::Uint8 | TypedArrayKind::Uint8Clamped => 1,
            TypedArrayKind::Int16 | TypedArrayKind::Uint16 | TypedArrayKind::Float16 => 2,
            TypedArrayKind::Int32 | TypedArrayKind::Uint32 | TypedArrayKind::Float32 => 4,
            TypedArrayKind::Float64 | TypedArrayKind::BigInt64 | TypedArrayKind::BigUint64 => 8,
        }
    }

    /// `[[ContentType]]` is BigInt.
    pub fn is_bigint(self) -> bool {
        matches!(self, TypedArrayKind::BigInt64 | TypedArrayKind::BigUint64)
    }

    pub fn name(self) -> &'static str {
        match self {
            TypedArrayKind::Int8 => "Int8Array",
            TypedArrayKind::Uint8 => "Uint8Array",
            TypedArrayKind::Uint8Clamped => "Uint8ClampedArray",
            TypedArrayKind::Int16 => "Int16Array",
            TypedArrayKind::Uint16 => "Uint16Array",
            TypedArrayKind::Int32 => "Int32Array",
            TypedArrayKind::Uint32 => "Uint32Array",
            TypedArrayKind::Float16 => "Float16Array",
            TypedArrayKind::Float32 => "Float32Array",
            TypedArrayKind::Float64 => "Float64Array",
            TypedArrayKind::BigInt64 => "BigInt64Array",
            TypedArrayKind::BigUint64 => "BigUint64Array",
        }
    }
}

/// Slots of a TypedArray. `length` and `byte_length` are `None` for
/// length-tracking views of resizable buffers (AUTO).
pub struct TypedArrayData {
    pub kind: TypedArrayKind,
    pub buffer: Object,
    pub byte_offset: usize,
    pub length: Option<usize>,
}

impl Trace for TypedArrayData {
    fn trace(&self, tracer: &mut Tracer) {
        tracer.object(self.buffer);
    }
}

/// A numeric value already converted for a TypedArray element.
#[derive(Clone, Debug)]
pub enum ElementValue {
    Number(f64),
    BigInt(BigInt),
}

/// NumericToRawBytes (little-endian).
pub fn numeric_to_raw_bytes(kind: TypedArrayKind, value: &ElementValue) -> Vec<u8> {
    match (kind, value) {
        (TypedArrayKind::BigInt64, ElementValue::BigInt(b)) => {
            b.to_i64_wrapping().to_le_bytes().to_vec()
        }
        (TypedArrayKind::BigUint64, ElementValue::BigInt(b)) => {
            b.to_u64_wrapping().to_le_bytes().to_vec()
        }
        (_, ElementValue::Number(n)) => {
            let n = *n;
            match kind {
                TypedArrayKind::Int8 => f64_to_int8(n).to_le_bytes().to_vec(),
                TypedArrayKind::Uint8 => f64_to_uint8(n).to_le_bytes().to_vec(),
                TypedArrayKind::Uint8Clamped => f64_to_uint8_clamp(n).to_le_bytes().to_vec(),
                TypedArrayKind::Int16 => f64_to_int16(n).to_le_bytes().to_vec(),
                TypedArrayKind::Uint16 => f64_to_uint16(n).to_le_bytes().to_vec(),
                TypedArrayKind::Int32 => f64_to_int32(n).to_le_bytes().to_vec(),
                TypedArrayKind::Uint32 => f64_to_uint32(n).to_le_bytes().to_vec(),
                TypedArrayKind::Float16 => f64_to_f16_bits(n).to_le_bytes().to_vec(),
                TypedArrayKind::Float32 => (n as f32).to_le_bytes().to_vec(),
                TypedArrayKind::Float64 => n.to_le_bytes().to_vec(),
                TypedArrayKind::BigInt64 | TypedArrayKind::BigUint64 => vec![0; 8],
            }
        }
        _ => vec![0; kind.element_size()],
    }
}

/// RawBytesToNumeric (little-endian).
pub fn raw_bytes_to_numeric(kind: TypedArrayKind, bytes: &[u8]) -> ElementValue {
    let b2 = || [bytes[0], bytes[1]];
    let b4 = || [bytes[0], bytes[1], bytes[2], bytes[3]];
    let b8 = || {
        let mut a = [0u8; 8];
        a.copy_from_slice(&bytes[..8]);
        a
    };
    match kind {
        TypedArrayKind::Int8 => ElementValue::Number(f64::from(bytes[0] as i8)),
        TypedArrayKind::Uint8 | TypedArrayKind::Uint8Clamped => {
            ElementValue::Number(f64::from(bytes[0]))
        }
        TypedArrayKind::Int16 => ElementValue::Number(f64::from(i16::from_le_bytes(b2()))),
        TypedArrayKind::Uint16 => ElementValue::Number(f64::from(u16::from_le_bytes(b2()))),
        TypedArrayKind::Int32 => ElementValue::Number(f64::from(i32::from_le_bytes(b4()))),
        TypedArrayKind::Uint32 => ElementValue::Number(f64::from(u32::from_le_bytes(b4()))),
        TypedArrayKind::Float16 => ElementValue::Number(f16_bits_to_f64(u16::from_le_bytes(b2()))),
        TypedArrayKind::Float32 => ElementValue::Number(f64::from(f32::from_le_bytes(b4()))),
        TypedArrayKind::Float64 => ElementValue::Number(f64::from_le_bytes(b8())),
        TypedArrayKind::BigInt64 => {
            ElementValue::BigInt(BigInt::from_i64(i64::from_le_bytes(b8())))
        }
        TypedArrayKind::BigUint64 => {
            ElementValue::BigInt(BigInt::from_u64(u64::from_le_bytes(b8())))
        }
    }
}

fn view(agent: &Agent, o: Object) -> Option<(TypedArrayKind, Object, usize, Option<usize>)> {
    match &agent.heap.object(o).kind {
        ObjectKind::TypedArray(t) => Some((t.kind, t.buffer, t.byte_offset, t.length)),
        _ => None,
    }
}

fn buffer_data(agent: &Agent, buffer: Object) -> Option<&ArrayBufferData> {
    match &agent.heap.object(buffer).kind {
        ObjectKind::ArrayBuffer(b) => Some(b),
        _ => None,
    }
}

/// TypedArrayLength of a TypedArray With Buffer Witness Record, or `None`
/// if the view is out of bounds (or its buffer detached).
pub fn typed_array_length(agent: &Agent, o: Object) -> Option<usize> {
    let (kind, buffer, offset, length) = view(agent, o)?;
    let data = buffer_data(agent, buffer)?;
    if data.is_detached() {
        return None;
    }
    let buffer_len = data.byte_length();
    let size = kind.element_size();
    match length {
        Some(len) => (offset + len * size <= buffer_len).then_some(len),
        None => (offset <= buffer_len).then(|| (buffer_len - offset) / size),
    }
}

/// IsTypedArrayFixedLength.
pub fn is_typed_array_fixed_length(agent: &Agent, o: Object) -> bool {
    let Some((_, buffer, _, length)) = view(agent, o) else {
        return true;
    };
    if length.is_none() {
        return false;
    }
    match buffer_data(agent, buffer) {
        Some(data) => !(data.max_byte_length.is_some() && !data.is_shared()),
        None => true,
    }
}

/// IsValidIntegerIndex(O, index).
pub fn is_valid_integer_index(agent: &Agent, o: Object, index: f64) -> bool {
    if index.trunc() != index || (index == 0.0 && index.is_sign_negative()) {
        return false;
    }
    match typed_array_length(agent, o) {
        Some(len) => index >= 0.0 && index < len as f64,
        None => false,
    }
}

/// TypedArrayGetElement(O, index): `None` for an invalid index.
pub fn typed_array_get_element(agent: &mut Agent, o: Object, index: f64) -> Option<Value> {
    if !is_valid_integer_index(agent, o, index) {
        return None;
    }
    let (kind, buffer, offset, _) = view(agent, o)?;
    let size = kind.element_size();
    let mut bytes = [0u8; 8];
    buffer_data(agent, buffer)?.read(offset + index as usize * size, &mut bytes[..size]);
    Some(match raw_bytes_to_numeric(kind, &bytes[..size]) {
        ElementValue::Number(n) => Value::Number(n),
        ElementValue::BigInt(b) => Value::BigInt(agent.heap.alloc_bigint(b)),
    })
}

/// TypedArraySetElement(O, index, value): converts first, then writes if
/// the index is (still) valid.
pub fn typed_array_set_element(
    agent: &mut Agent,
    o: Object,
    index: f64,
    value: Value,
) -> JsResult<()> {
    let Some((kind, _, _, _)) = view(agent, o) else {
        return Ok(());
    };
    let num = if kind.is_bigint() {
        let b = ops::to_bigint(agent, value)?;
        ElementValue::BigInt(agent.heap.bigint(b).clone())
    } else {
        ElementValue::Number(ops::to_number(agent, value)?)
    };
    if !is_valid_integer_index(agent, o, index) {
        return Ok(());
    }
    let Some((kind, buffer, offset, _)) = view(agent, o) else {
        return Ok(());
    };
    let bytes = numeric_to_raw_bytes(kind, &num);
    if let ObjectKind::ArrayBuffer(data) = &mut agent.heap.object_mut(buffer).kind {
        data.write(offset + index as usize * kind.element_size(), &bytes);
    }
    Ok(())
}

/// AllocateArrayBuffer storage helper: a new `ArrayBuffer` object with
/// `len` zero bytes and prototype `proto`.
pub fn array_buffer_create(
    agent: &mut Agent,
    proto: Option<Object>,
    len: usize,
    max_byte_length: Option<usize>,
) -> Object {
    agent.heap.note_alloc(len);
    agent.heap.create_object(
        proto,
        ObjectKind::ArrayBuffer(Box::new(ArrayBufferData {
            storage: BufferStorage::Owned(vec![0; len]),
            max_byte_length,
            detach_key: Value::Undefined,
        })),
    )
}

/// DetachArrayBuffer(buffer, key).
pub fn detach_array_buffer(agent: &mut Agent, buffer: Object, key: Value) -> JsResult<()> {
    let ok = match &mut agent.heap.object_mut(buffer).kind {
        ObjectKind::ArrayBuffer(data) if !data.is_shared() => {
            if data.detach_key == key {
                data.storage = BufferStorage::Detached;
                true
            } else {
                false
            }
        }
        _ => true,
    };
    if ok {
        Ok(())
    } else {
        agent.type_error("ArrayBuffer detach key mismatch")
    }
}

/// A TypedArray view object of `kind` over `buffer`.
pub fn typed_array_create(
    agent: &mut Agent,
    proto: Option<Object>,
    kind: TypedArrayKind,
    buffer: Object,
    byte_offset: usize,
    length: Option<usize>,
) -> Object {
    agent.heap.create_object(
        proto,
        ObjectKind::TypedArray(Box::new(TypedArrayData {
            kind,
            buffer,
            byte_offset,
            length,
        })),
    )
}
