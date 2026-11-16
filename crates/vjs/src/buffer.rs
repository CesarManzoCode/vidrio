//! Storage of `ArrayBuffer`/`SharedArrayBuffer` and the element access of
//! TypedArrays and DataViews (ECMA-262 §10.4.5, §25.1–§25.3). Elements are
//! stored little-endian.
//!
//! The bytes of a `SharedArrayBuffer` live in a [`SharedBlock`] of 64-bit
//! atomic words that every agent (thread) holding the buffer shares. An
//! element of 1, 2, 4 or 8 bytes at an offset that is a multiple of its
//! size lies inside one word, so the atomic operations of `Atomics` are a
//! compare-and-swap loop on that word; unaligned accesses (DataView) go
//! byte by byte.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

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

/// The largest data block vjs allocates for an `ArrayBuffer` (and the
/// largest `maxByteLength` it accepts); larger requests throw a
/// `RangeError`, as CreateByteDataBlock allows.
pub const MAX_BYTE_LENGTH: usize = 1 << 32;

/// The largest `maxByteLength` of a growable `SharedArrayBuffer`, whose
/// whole capacity is reserved up front (its bytes cannot move while other
/// agents use them).
pub const MAX_SHARED_CAPACITY: usize = 1 << 30;

/// A zero-filled byte vector of `len` bytes, or `None` if it cannot be
/// allocated.
pub fn allocate_bytes(len: usize) -> Option<Vec<u8>> {
    if len > MAX_BYTE_LENGTH {
        return None;
    }
    let mut v = Vec::new();
    v.try_reserve_exact(len).ok()?;
    v.resize(len, 0);
    Some(v)
}

/// The mask of the low `size` bytes of a word.
fn low_mask(size: usize) -> u64 {
    if size >= 8 {
        u64::MAX
    } else {
        (1u64 << (size * 8)) - 1
    }
}

/// Bytes of a `SharedArrayBuffer`, shared between agents (threads).
pub struct SharedBlock {
    words: Box<[AtomicU64]>,
    capacity: usize,
    len: AtomicUsize,
}

impl SharedBlock {
    /// A block of `len` zero bytes that can grow up to `capacity`, or
    /// `None` if it cannot be allocated.
    pub fn try_new(len: usize, capacity: usize) -> Option<Arc<SharedBlock>> {
        let capacity = capacity.max(len);
        if capacity > MAX_SHARED_CAPACITY {
            return None;
        }
        let count = capacity.div_ceil(8);
        let mut words = Vec::new();
        words.try_reserve_exact(count).ok()?;
        words.extend((0..count).map(|_| AtomicU64::new(0)));
        Some(Arc::new(SharedBlock {
            words: words.into_boxed_slice(),
            capacity,
            len: AtomicUsize::new(len),
        }))
    }

    /// A block of `len` zero bytes that can grow up to `capacity`; panics
    /// if it cannot be allocated.
    pub fn new(len: usize, capacity: usize) -> Arc<SharedBlock> {
        SharedBlock::try_new(len, capacity).expect("cannot allocate a shared block")
    }

    pub fn len(&self) -> usize {
        self.len.load(Ordering::SeqCst)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Grows to `new_len` bytes; `false` if it would shrink or exceed the
    /// capacity.
    pub fn grow(&self, new_len: usize) -> bool {
        if new_len > self.capacity {
            return false;
        }
        self.len
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |cur| {
                (new_len >= cur).then_some(new_len)
            })
            .is_ok()
    }

    /// The identity of the block (for the waiter lists of `Atomics`).
    pub fn id(self: &Arc<Self>) -> usize {
        Arc::as_ptr(self) as usize
    }

    pub fn load_byte(&self, i: usize) -> u8 {
        (self.words[i / 8].load(Ordering::SeqCst) >> ((i % 8) * 8)) as u8
    }

    pub fn store_byte(&self, i: usize, b: u8) {
        let shift = (i % 8) * 8;
        let mask = 0xffu64 << shift;
        let _ = self.words[i / 8].fetch_update(Ordering::SeqCst, Ordering::SeqCst, |w| {
            Some((w & !mask) | (u64::from(b) << shift))
        });
    }

    pub fn read(&self, offset: usize, out: &mut [u8]) {
        for (i, byte) in out.iter_mut().enumerate() {
            *byte = self.load_byte(offset + i);
        }
    }

    pub fn write(&self, offset: usize, bytes: &[u8]) {
        for (i, byte) in bytes.iter().enumerate() {
            self.store_byte(offset + i, *byte);
        }
    }

    /// The `size` bytes at `offset` as a little-endian integer, read
    /// atomically when the element is aligned.
    pub fn load(&self, offset: usize, size: usize) -> u64 {
        if offset % size == 0 {
            let shift = (offset % 8) * 8;
            (self.words[offset / 8].load(Ordering::SeqCst) >> shift) & low_mask(size)
        } else {
            let mut b = [0u8; 8];
            self.read(offset, &mut b[..size]);
            u64::from_le_bytes(b)
        }
    }

    /// Atomically replaces the aligned element of `size` bytes at `offset`
    /// with `f(old)` (no store if `f` returns `None`) and returns `old`.
    pub fn read_modify_write(
        &self,
        offset: usize,
        size: usize,
        mut f: impl FnMut(u64) -> Option<u64>,
    ) -> u64 {
        let shift = (offset % 8) * 8;
        let mask = low_mask(size) << shift;
        let word = &self.words[offset / 8];
        let mut current = word.load(Ordering::SeqCst);
        loop {
            let old = (current & mask) >> shift;
            let Some(new) = f(old) else {
                return old;
            };
            let next = (current & !mask) | ((new << shift) & mask);
            match word.compare_exchange_weak(current, next, Ordering::SeqCst, Ordering::SeqCst) {
                Ok(_) => return old,
                Err(actual) => current = actual,
            }
        }
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
    /// `[[ArrayBufferMaxByteLength]]` of resizable (or growable) buffers.
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

    /// IsFixedLengthArrayBuffer.
    pub fn is_fixed_length(&self) -> bool {
        self.max_byte_length.is_none()
    }

    pub fn shared_block(&self) -> Option<&Arc<SharedBlock>> {
        match &self.storage {
            BufferStorage::Shared(b) => Some(b),
            _ => None,
        }
    }

    /// Copies `out.len()` bytes from `offset` (zeros past the end).
    pub fn read(&self, offset: usize, out: &mut [u8]) {
        match &self.storage {
            BufferStorage::Owned(v) => {
                for (i, byte) in out.iter_mut().enumerate() {
                    *byte = v.get(offset + i).copied().unwrap_or(0);
                }
            }
            BufferStorage::Detached => out.fill(0),
            BufferStorage::Shared(b) => {
                let end = (offset + out.len()).min(b.capacity());
                let n = end.saturating_sub(offset);
                b.read(offset, &mut out[..n]);
                out[n..].fill(0);
            }
        }
    }

    /// Writes `bytes` at `offset` (bytes past the end are dropped).
    pub fn write(&mut self, offset: usize, bytes: &[u8]) {
        match &mut self.storage {
            BufferStorage::Owned(v) => {
                let end = (offset + bytes.len()).min(v.len());
                if offset < end {
                    v[offset..end].copy_from_slice(&bytes[..end - offset]);
                }
            }
            BufferStorage::Detached => {}
            BufferStorage::Shared(b) => {
                let end = (offset + bytes.len()).min(b.capacity());
                if offset < end {
                    b.write(offset, &bytes[..end - offset]);
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
    /// The kinds in the order of the table of ECMA-262 §23.2 (Float16
    /// after Uint32).
    pub const ALL: [TypedArrayKind; 12] = [
        TypedArrayKind::Int8,
        TypedArrayKind::Uint8,
        TypedArrayKind::Uint8Clamped,
        TypedArrayKind::Int16,
        TypedArrayKind::Uint16,
        TypedArrayKind::Int32,
        TypedArrayKind::Uint32,
        TypedArrayKind::Float16,
        TypedArrayKind::Float32,
        TypedArrayKind::Float64,
        TypedArrayKind::BigInt64,
        TypedArrayKind::BigUint64,
    ];

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

    /// IsUnclampedIntegerElementType.
    pub fn is_unclamped_integer(self) -> bool {
        matches!(
            self,
            TypedArrayKind::Int8
                | TypedArrayKind::Uint8
                | TypedArrayKind::Int16
                | TypedArrayKind::Uint16
                | TypedArrayKind::Int32
                | TypedArrayKind::Uint32
        )
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

/// Slots of a TypedArray. `length` is `None` for length-tracking views of
/// resizable buffers (`[[ArrayLength]]` is AUTO).
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

/// Slots of a DataView. `byte_length` is `None` for views that track the
/// length of a resizable buffer.
pub struct DataViewData {
    pub buffer: Object,
    pub byte_offset: usize,
    pub byte_length: Option<usize>,
}

impl Trace for DataViewData {
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

/// NumericToRawBytes as a little-endian integer of the element's size.
pub fn numeric_to_bits(kind: TypedArrayKind, value: &ElementValue) -> u64 {
    match (kind, value) {
        (TypedArrayKind::BigInt64, ElementValue::BigInt(b)) => b.to_i64_wrapping() as u64,
        (TypedArrayKind::BigUint64, ElementValue::BigInt(b)) => b.to_u64_wrapping(),
        (_, ElementValue::Number(n)) => {
            let n = *n;
            match kind {
                TypedArrayKind::Int8 => u64::from(f64_to_int8(n) as u8),
                TypedArrayKind::Uint8 => u64::from(f64_to_uint8(n)),
                TypedArrayKind::Uint8Clamped => u64::from(f64_to_uint8_clamp(n)),
                TypedArrayKind::Int16 => u64::from(f64_to_int16(n) as u16),
                TypedArrayKind::Uint16 => u64::from(f64_to_uint16(n)),
                TypedArrayKind::Int32 => u64::from(f64_to_int32(n) as u32),
                TypedArrayKind::Uint32 => u64::from(f64_to_uint32(n)),
                TypedArrayKind::Float16 => u64::from(f64_to_f16_bits(n)),
                TypedArrayKind::Float32 => u64::from((n as f32).to_bits()),
                TypedArrayKind::Float64 => n.to_bits(),
                TypedArrayKind::BigInt64 | TypedArrayKind::BigUint64 => 0,
            }
        }
        _ => 0,
    }
}

/// RawBytesToNumeric of a little-endian integer of the element's size.
pub fn bits_to_numeric(kind: TypedArrayKind, bits: u64) -> ElementValue {
    let n = match kind {
        TypedArrayKind::Int8 => f64::from(bits as u8 as i8),
        TypedArrayKind::Uint8 | TypedArrayKind::Uint8Clamped => f64::from(bits as u8),
        TypedArrayKind::Int16 => f64::from(bits as u16 as i16),
        TypedArrayKind::Uint16 => f64::from(bits as u16),
        TypedArrayKind::Int32 => f64::from(bits as u32 as i32),
        TypedArrayKind::Uint32 => f64::from(bits as u32),
        TypedArrayKind::Float16 => f16_bits_to_f64(bits as u16),
        TypedArrayKind::Float32 => f64::from(f32::from_bits(bits as u32)),
        TypedArrayKind::Float64 => f64::from_bits(bits),
        TypedArrayKind::BigInt64 => return ElementValue::BigInt(BigInt::from_i64(bits as i64)),
        TypedArrayKind::BigUint64 => return ElementValue::BigInt(BigInt::from_u64(bits)),
    };
    ElementValue::Number(n)
}

/// NumericToRawBytes (little-endian).
pub fn numeric_to_raw_bytes(kind: TypedArrayKind, value: &ElementValue) -> Vec<u8> {
    numeric_to_bits(kind, value).to_le_bytes()[..kind.element_size()].to_vec()
}

/// RawBytesToNumeric (little-endian).
pub fn raw_bytes_to_numeric(kind: TypedArrayKind, bytes: &[u8]) -> ElementValue {
    let mut b = [0u8; 8];
    b[..kind.element_size()].copy_from_slice(&bytes[..kind.element_size()]);
    bits_to_numeric(kind, u64::from_le_bytes(b))
}

/// The JS value of an element.
pub fn element_to_value(agent: &mut Agent, value: ElementValue) -> Value {
    match value {
        ElementValue::Number(n) => Value::Number(n),
        ElementValue::BigInt(b) => Value::BigInt(agent.heap.alloc_bigint(b)),
    }
}

/// ToBigInt or ToNumber, as the content type of `kind` asks.
pub fn to_element_value(
    agent: &mut Agent,
    kind: TypedArrayKind,
    value: Value,
) -> JsResult<ElementValue> {
    if kind.is_bigint() {
        let b = ops::to_bigint(agent, value)?;
        Ok(ElementValue::BigInt(agent.heap.bigint(b).clone()))
    } else {
        Ok(ElementValue::Number(ops::to_number(agent, value)?))
    }
}

// ----- buffers -----

pub fn buffer_data(agent: &Agent, buffer: Object) -> Option<&ArrayBufferData> {
    match &agent.heap.object(buffer).kind {
        ObjectKind::ArrayBuffer(b) => Some(b),
        _ => None,
    }
}

pub fn buffer_data_mut(agent: &mut Agent, buffer: Object) -> Option<&mut ArrayBufferData> {
    match &mut agent.heap.object_mut(buffer).kind {
        ObjectKind::ArrayBuffer(b) => Some(b),
        _ => None,
    }
}

/// ArrayBufferByteLength (0 for a detached buffer).
pub fn buffer_byte_length(agent: &Agent, buffer: Object) -> usize {
    buffer_data(agent, buffer).map_or(0, ArrayBufferData::byte_length)
}

pub fn is_detached(agent: &Agent, buffer: Object) -> bool {
    buffer_data(agent, buffer).is_none_or(ArrayBufferData::is_detached)
}

pub fn is_shared(agent: &Agent, buffer: Object) -> bool {
    buffer_data(agent, buffer).is_some_and(ArrayBufferData::is_shared)
}

/// `count` bytes of `buffer` from `offset`.
pub fn read_bytes(agent: &Agent, buffer: Object, offset: usize, count: usize) -> Vec<u8> {
    let mut out = vec![0; count];
    if let Some(data) = buffer_data(agent, buffer) {
        data.read(offset, &mut out);
    }
    out
}

pub fn write_bytes(agent: &mut Agent, buffer: Object, offset: usize, bytes: &[u8]) {
    if let Some(data) = buffer_data_mut(agent, buffer) {
        data.write(offset, bytes);
    }
}

/// Copies `count` bytes of `from_buffer` at `from` to `to_buffer` at `to`
/// one byte at a time in increasing order (as the byte loops of
/// `%TypedArray%.prototype.slice` and `set` do, even when the ranges of
/// one buffer overlap).
pub fn copy_bytes_forward(
    agent: &mut Agent,
    from_buffer: Object,
    from: usize,
    to_buffer: Object,
    to: usize,
    count: usize,
) {
    if from_buffer != to_buffer || to <= from || to >= from + count {
        let bytes = read_bytes(agent, from_buffer, from, count);
        write_bytes(agent, to_buffer, to, &bytes);
        return;
    }
    for i in 0..count {
        let b = read_bytes(agent, from_buffer, from + i, 1);
        write_bytes(agent, to_buffer, to + i, &b);
    }
}

/// GetValueFromBuffer(buffer, byteIndex, type, isTypedArray, order,
/// isLittleEndian).
pub fn get_value_from_buffer(
    agent: &mut Agent,
    buffer: Object,
    byte_index: usize,
    kind: TypedArrayKind,
    little_endian: bool,
) -> Value {
    let size = kind.element_size();
    let bits = match buffer_data(agent, buffer) {
        Some(ArrayBufferData {
            storage: BufferStorage::Shared(b),
            ..
        }) if byte_index + size <= b.capacity() => b.load(byte_index, size),
        Some(data) => {
            let mut bytes = [0u8; 8];
            data.read(byte_index, &mut bytes[..size]);
            u64::from_le_bytes(bytes)
        }
        None => 0,
    };
    let bits = if little_endian {
        bits
    } else {
        swap(bits, size)
    };
    let v = bits_to_numeric(kind, bits);
    element_to_value(agent, v)
}

/// SetValueInBuffer(buffer, byteIndex, type, value, isTypedArray, order,
/// isLittleEndian).
pub fn set_value_in_buffer(
    agent: &mut Agent,
    buffer: Object,
    byte_index: usize,
    kind: TypedArrayKind,
    value: &ElementValue,
    little_endian: bool,
) {
    let size = kind.element_size();
    let bits = numeric_to_bits(kind, value);
    let bits = if little_endian {
        bits
    } else {
        swap(bits, size)
    };
    if let Some(data) = buffer_data(agent, buffer) {
        if let BufferStorage::Shared(b) = &data.storage {
            if byte_index % size == 0 && byte_index + size <= b.capacity() {
                b.read_modify_write(byte_index, size, |_| Some(bits));
                return;
            }
        }
    }
    write_bytes(agent, buffer, byte_index, &bits.to_le_bytes()[..size]);
}

/// Reverses the byte order of the low `size` bytes of `bits`.
fn swap(bits: u64, size: usize) -> u64 {
    bits.swap_bytes() >> ((8 - size) * 8)
}

/// GetModifySetValueInBuffer: atomically replaces the element of `kind` at
/// `byte_index` by `f(old)` (no store if `None`) and returns the old raw
/// bits. The element must be aligned and in bounds.
pub fn modify_value_in_buffer(
    agent: &mut Agent,
    buffer: Object,
    byte_index: usize,
    kind: TypedArrayKind,
    mut f: impl FnMut(u64) -> Option<u64>,
) -> u64 {
    let size = kind.element_size();
    if let Some(b) = buffer_data(agent, buffer).and_then(ArrayBufferData::shared_block) {
        return b.read_modify_write(byte_index, size, f);
    }
    let mut bytes = [0u8; 8];
    if let Some(data) = buffer_data(agent, buffer) {
        data.read(byte_index, &mut bytes[..size]);
    }
    let old = u64::from_le_bytes(bytes);
    if let Some(new) = f(old) {
        write_bytes(agent, buffer, byte_index, &new.to_le_bytes()[..size]);
    }
    old
}

/// AllocateArrayBuffer storage helper: a new `ArrayBuffer` object with
/// `len` zero bytes and prototype `proto`. Panics if the bytes cannot be
/// allocated (callers check with [`allocate_bytes`] first when the size
/// comes from code).
pub fn array_buffer_create(
    agent: &mut Agent,
    proto: Option<Object>,
    len: usize,
    max_byte_length: Option<usize>,
) -> Object {
    let bytes = allocate_bytes(len).expect("cannot allocate an ArrayBuffer");
    array_buffer_from_bytes(agent, proto, bytes, max_byte_length)
}

/// A new `ArrayBuffer` that owns `bytes`.
pub fn array_buffer_from_bytes(
    agent: &mut Agent,
    proto: Option<Object>,
    bytes: Vec<u8>,
    max_byte_length: Option<usize>,
) -> Object {
    agent.heap.note_alloc(bytes.len());
    agent.heap.create_object(
        proto,
        ObjectKind::ArrayBuffer(Box::new(ArrayBufferData {
            storage: BufferStorage::Owned(bytes),
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

// ----- TypedArray views -----

fn view(agent: &Agent, o: Object) -> Option<(TypedArrayKind, Object, usize, Option<usize>)> {
    match &agent.heap.object(o).kind {
        ObjectKind::TypedArray(t) => Some((t.kind, t.buffer, t.byte_offset, t.length)),
        _ => None,
    }
}

/// The slots of a TypedArray: kind, buffer, byte offset and fixed length.
pub fn typed_array_slots(
    agent: &Agent,
    o: Object,
) -> Option<(TypedArrayKind, Object, usize, Option<usize>)> {
    view(agent, o)
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
    let byte_index = offset + index as usize * kind.element_size();
    Some(get_value_from_buffer(agent, buffer, byte_index, kind, true))
}

/// Writes an already converted value to element `index` if it is valid.
pub fn typed_array_store_element(agent: &mut Agent, o: Object, index: f64, num: &ElementValue) {
    if !is_valid_integer_index(agent, o, index) {
        return;
    }
    let Some((kind, buffer, offset, _)) = view(agent, o) else {
        return;
    };
    let byte_index = offset + index as usize * kind.element_size();
    set_value_in_buffer(agent, buffer, byte_index, kind, num, true);
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
    let num = to_element_value(agent, kind, value)?;
    typed_array_store_element(agent, o, index, &num);
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_block_elements_are_little_endian_fields_of_words() {
        let b = SharedBlock::new(16, 16);
        b.write(0, &[1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(b.load(0, 4), 0x0403_0201);
        assert_eq!(b.load(4, 2), 0x0605);
        assert_eq!(b.load(0, 8), 0x0807_0605_0403_0201);
        let old = b.read_modify_write(4, 4, |x| Some(x.wrapping_add(1)));
        assert_eq!(old, 0x0807_0605);
        assert_eq!(b.load(4, 4), 0x0807_0606);
        assert_eq!(b.load(0, 4), 0x0403_0201);
        // Unaligned reads go byte by byte.
        assert_eq!(b.load(1, 2), 0x0302);
        assert!(!b.grow(17));
        assert!(b.grow(16));
        assert!(SharedBlock::try_new(0, MAX_SHARED_CAPACITY + 1).is_none());
    }

    #[test]
    fn raw_bits_round_trip() {
        let v = ElementValue::Number(-2.0);
        assert_eq!(numeric_to_bits(TypedArrayKind::Int16, &v), 0xfffe);
        assert!(matches!(
            bits_to_numeric(TypedArrayKind::Int16, 0xfffe),
            ElementValue::Number(n) if n == -2.0
        ));
        assert_eq!(swap(0x0102, 2), 0x0201);
        assert_eq!(swap(0x0102_0304, 4), 0x0403_0201);
        assert_eq!(
            numeric_to_raw_bytes(TypedArrayKind::Float16, &ElementValue::Number(1.0)),
            vec![0x00, 0x3c]
        );
    }
}
