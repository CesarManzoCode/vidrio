//! Object storage: property attributes and descriptors, named properties
//! in shapes or a per-object dictionary, indexed elements (dense or
//! sparse), private elements, and the kind-specific internal slots.
//!
//! This is the storage layer: it does no validation. The internal methods
//! of ECMA-262 §10 that use it are in `internal.rs`.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::mem::size_of;
use std::rc::Rc;

use crate::buffer::{ArrayBufferData, TypedArrayData};
use crate::exotic::{ArgumentsData, HostObject, NamespaceData};
use crate::function::{BoundFunctionData, BytecodeFunctionData, NativeFunctionData};
use crate::gc::{Heap, HeapBigInt, JsString, Object, Realm, Shape, Symbol, Trace, Tracer};
use crate::interp::generator::GeneratorData;
use crate::interp::iter::{ArrayIteratorData, ForInData};
use crate::interp::promise::PromiseData;
use crate::ops::IteratorRecord;
use crate::proxy::ProxyData;
use crate::shape::{MAX_SHAPE_PROPERTIES, ShapeData, shape_with};
use crate::value::{PropertyKey, Value, WeakKey};

/// Property attributes: `[[Writable]]` (data only), `[[Enumerable]]`,
/// `[[Configurable]]` and whether the property is an accessor.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Attributes(u8);

impl Attributes {
    const WRITABLE: u8 = 1;
    const ENUMERABLE: u8 = 2;
    const CONFIGURABLE: u8 = 4;
    const ACCESSOR: u8 = 8;

    /// Writable, enumerable and configurable data property (what plain
    /// assignment creates).
    pub const ALL: Attributes = Attributes(Self::WRITABLE | Self::ENUMERABLE | Self::CONFIGURABLE);

    pub const fn data(writable: bool, enumerable: bool, configurable: bool) -> Attributes {
        Attributes((writable as u8) | ((enumerable as u8) << 1) | ((configurable as u8) << 2))
    }

    pub const fn accessor(enumerable: bool, configurable: bool) -> Attributes {
        Attributes(Self::ACCESSOR | ((enumerable as u8) << 1) | ((configurable as u8) << 2))
    }

    pub fn writable(self) -> bool {
        self.0 & Self::WRITABLE != 0
    }

    pub fn enumerable(self) -> bool {
        self.0 & Self::ENUMERABLE != 0
    }

    pub fn configurable(self) -> bool {
        self.0 & Self::CONFIGURABLE != 0
    }

    pub fn is_accessor(self) -> bool {
        self.0 & Self::ACCESSOR != 0
    }
}

/// The value part of a property: a data value or a getter/setter pair
/// (`undefined` or a callable object each).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Slot {
    Data(Value),
    Accessor { get: Value, set: Value },
}

impl Trace for Slot {
    fn trace(&self, tracer: &mut Tracer) {
        match *self {
            Slot::Data(v) => {
                tracer.value(v);
            }
            Slot::Accessor { get, set } => {
                tracer.value(get);
                tracer.value(set);
            }
        }
    }
}

/// Property Descriptor (ECMA-262 §6.2.6): every field may be absent.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PropertyDescriptor {
    pub value: Option<Value>,
    pub writable: Option<bool>,
    pub get: Option<Value>,
    pub set: Option<Value>,
    pub enumerable: Option<bool>,
    pub configurable: Option<bool>,
}

impl PropertyDescriptor {
    pub fn data(value: Value, writable: bool, enumerable: bool, configurable: bool) -> Self {
        PropertyDescriptor {
            value: Some(value),
            writable: Some(writable),
            get: None,
            set: None,
            enumerable: Some(enumerable),
            configurable: Some(configurable),
        }
    }

    pub fn accessor(get: Value, set: Value, enumerable: bool, configurable: bool) -> Self {
        PropertyDescriptor {
            value: None,
            writable: None,
            get: Some(get),
            set: Some(set),
            enumerable: Some(enumerable),
            configurable: Some(configurable),
        }
    }

    /// `{ [[Value]]: value }`.
    pub fn value_only(value: Value) -> Self {
        PropertyDescriptor {
            value: Some(value),
            ..PropertyDescriptor::default()
        }
    }

    /// The complete descriptor of a stored property.
    pub fn from_property(attrs: Attributes, slot: Slot) -> Self {
        match slot {
            Slot::Data(v) => PropertyDescriptor::data(
                v,
                attrs.writable(),
                attrs.enumerable(),
                attrs.configurable(),
            ),
            Slot::Accessor { get, set } => {
                PropertyDescriptor::accessor(get, set, attrs.enumerable(), attrs.configurable())
            }
        }
    }

    pub fn is_accessor(&self) -> bool {
        self.get.is_some() || self.set.is_some()
    }

    pub fn is_data(&self) -> bool {
        self.value.is_some() || self.writable.is_some()
    }

    pub fn is_generic(&self) -> bool {
        !self.is_accessor() && !self.is_data()
    }

    pub fn has_no_fields(&self) -> bool {
        *self == PropertyDescriptor::default()
    }

    /// CompletePropertyDescriptor.
    pub fn complete(&mut self) {
        if self.is_generic() || self.is_data() {
            self.value.get_or_insert(Value::Undefined);
            self.writable.get_or_insert(false);
        } else {
            self.get.get_or_insert(Value::Undefined);
            self.set.get_or_insert(Value::Undefined);
        }
        self.enumerable.get_or_insert(false);
        self.configurable.get_or_insert(false);
    }

    /// Storage form of a complete descriptor.
    pub fn to_property(&self) -> (Attributes, Slot) {
        let enumerable = self.enumerable.unwrap_or(false);
        let configurable = self.configurable.unwrap_or(false);
        if self.is_accessor() {
            (
                Attributes::accessor(enumerable, configurable),
                Slot::Accessor {
                    get: self.get.unwrap_or(Value::Undefined),
                    set: self.set.unwrap_or(Value::Undefined),
                },
            )
        } else {
            (
                Attributes::data(self.writable.unwrap_or(false), enumerable, configurable),
                Slot::Data(self.value.unwrap_or(Value::Undefined)),
            )
        }
    }
}

impl Trace for PropertyDescriptor {
    fn trace(&self, tracer: &mut Tracer) {
        for v in [self.value, self.get, self.set].into_iter().flatten() {
            tracer.value(v);
        }
    }
}

/// Insertion-ordered named properties of an object in dictionary mode.
pub(crate) struct PropertyMap {
    entries: Vec<Option<(PropertyKey, Attributes, Slot)>>,
    index: HashMap<PropertyKey, usize>,
    removed: usize,
}

impl PropertyMap {
    fn new() -> PropertyMap {
        PropertyMap {
            entries: Vec::new(),
            index: HashMap::new(),
            removed: 0,
        }
    }

    fn get(&self, key: PropertyKey) -> Option<(Attributes, Slot)> {
        let &i = self.index.get(&key)?;
        self.entries[i].map(|(_, a, s)| (a, s))
    }

    fn insert(&mut self, key: PropertyKey, attrs: Attributes, slot: Slot) {
        match self.index.get(&key) {
            Some(&i) => self.entries[i] = Some((key, attrs, slot)),
            None => {
                self.index.insert(key, self.entries.len());
                self.entries.push(Some((key, attrs, slot)));
            }
        }
    }

    fn remove(&mut self, key: PropertyKey) -> bool {
        let Some(i) = self.index.remove(&key) else {
            return false;
        };
        self.entries[i] = None;
        self.removed += 1;
        if self.removed > 16 && self.removed * 2 > self.entries.len() {
            self.entries.retain(|e| e.is_some());
            self.index.clear();
            for (i, e) in self.entries.iter().enumerate() {
                if let Some((k, _, _)) = e {
                    self.index.insert(*k, i);
                }
            }
            self.removed = 0;
        }
        true
    }

    fn iter(&self) -> impl Iterator<Item = &(PropertyKey, Attributes, Slot)> {
        self.entries.iter().flatten()
    }

    fn len(&self) -> usize {
        self.entries.len() - self.removed
    }
}

/// Named (non-index) properties.
pub(crate) enum Properties {
    Shaped { shape: Shape, slots: Vec<Slot> },
    Dictionary(PropertyMap),
}

/// Distance past the end within which a dense element vector grows
/// instead of turning sparse.
const DENSE_GAP: usize = 1024;

/// Indexed properties (array index keys).
pub(crate) enum Elements {
    /// Writable, enumerable, configurable data properties; `None` is a hole.
    Dense(Vec<Option<Value>>),
    Sparse(BTreeMap<u32, (Attributes, Slot)>),
}

impl Elements {
    fn get(&self, i: u32) -> Option<(Attributes, Slot)> {
        match self {
            Elements::Dense(v) => v
                .get(i as usize)
                .copied()
                .flatten()
                .map(|value| (Attributes::ALL, Slot::Data(value))),
            Elements::Sparse(m) => m.get(&i).copied(),
        }
    }

    /// Stores the element; returns the bytes it added.
    fn set(&mut self, i: u32, attrs: Attributes, slot: Slot) -> usize {
        if let Elements::Dense(v) = self {
            if let Slot::Data(value) = slot {
                if attrs == Attributes::ALL {
                    let idx = i as usize;
                    if idx < v.len() {
                        v[idx] = Some(value);
                        return 0;
                    }
                    if idx <= v.len() + DENSE_GAP || idx < v.len() * 2 {
                        let old = v.len();
                        v.resize(idx + 1, None);
                        v[idx] = Some(value);
                        return (idx + 1 - old) * size_of::<Option<Value>>();
                    }
                }
            }
            self.make_sparse();
        }
        if let Elements::Sparse(m) = self {
            m.insert(i, (attrs, slot));
        }
        size_of::<(u32, (Attributes, Slot))>() * 2
    }

    fn make_sparse(&mut self) {
        if let Elements::Dense(v) = self {
            let map: BTreeMap<u32, (Attributes, Slot)> = v
                .iter()
                .enumerate()
                .filter_map(|(i, x)| {
                    (*x).map(|value| (i as u32, (Attributes::ALL, Slot::Data(value))))
                })
                .collect();
            *self = Elements::Sparse(map);
        }
    }

    fn remove(&mut self, i: u32) -> bool {
        match self {
            Elements::Dense(v) => {
                let idx = i as usize;
                if idx >= v.len() || v[idx].is_none() {
                    return false;
                }
                v[idx] = None;
                while v.last() == Some(&None) {
                    v.pop();
                }
                true
            }
            Elements::Sparse(m) => m.remove(&i).is_some(),
        }
    }

    fn push_indices(&self, out: &mut Vec<PropertyKey>) {
        match self {
            Elements::Dense(v) => {
                for (i, x) in v.iter().enumerate() {
                    if x.is_some() {
                        out.push(PropertyKey::Index(i as u32));
                    }
                }
            }
            Elements::Sparse(m) => out.extend(m.keys().map(|i| PropertyKey::Index(*i))),
        }
    }

    /// Deletes the elements at `new_len` and above, from the highest down,
    /// stopping at the first non-configurable one, whose index it returns.
    fn truncate(&mut self, new_len: u32) -> Option<u32> {
        match self {
            Elements::Dense(v) => {
                if v.len() > new_len as usize {
                    v.truncate(new_len as usize);
                }
                while v.last() == Some(&None) {
                    v.pop();
                }
                None
            }
            Elements::Sparse(m) => {
                let keys: Vec<u32> = m.range(new_len..).map(|(k, _)| *k).collect();
                for k in keys.into_iter().rev() {
                    if !m[&k].0.configurable() {
                        return Some(k);
                    }
                    m.remove(&k);
                }
                None
            }
        }
    }

    fn trace(&self, tracer: &mut Tracer) {
        match self {
            Elements::Dense(v) => {
                for x in v.iter().flatten() {
                    tracer.value(*x);
                }
            }
            Elements::Sparse(m) => {
                for (_, slot) in m.values() {
                    slot.trace(tracer);
                }
            }
        }
    }

    fn bytes(&self) -> usize {
        match self {
            Elements::Dense(v) => v.capacity() * size_of::<Option<Value>>(),
            Elements::Sparse(m) => m.len() * 2 * size_of::<(u32, (Attributes, Slot))>(),
        }
    }
}

/// A private element (ECMA-262 §6.2.10) keyed by a Private Name.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PrivateElement {
    pub key: Symbol,
    pub kind: PrivateElementKind,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PrivateElementKind {
    Field(Value),
    Method(Value),
    Accessor { get: Value, set: Value },
}

impl Trace for PrivateElement {
    fn trace(&self, tracer: &mut Tracer) {
        tracer.symbol(self.key);
        match self.kind {
            PrivateElementKind::Field(v) | PrivateElementKind::Method(v) => {
                tracer.value(v);
            }
            PrivateElementKind::Accessor { get, set } => {
                tracer.value(get);
                tracer.value(set);
            }
        }
    }
}

/// Entries of a `WeakMap` or `WeakSet` (value `undefined`): ephemerons
/// whose values are marked only once their keys are.
pub type EphemeronTable = HashMap<WeakKey, Value>;

/// A cell of a `FinalizationRegistry`.
#[derive(Clone, Copy, Debug)]
pub struct RegistryCell {
    pub target: WeakKey,
    pub held: Value,
    pub token: Option<WeakKey>,
}

/// Internal slots of a `FinalizationRegistry`. `pending` holds the held
/// values of collected targets until the cleanup job calls the callback.
pub struct RegistryData {
    pub cleanup: Value,
    pub realm: Realm,
    pub cells: Vec<RegistryCell>,
    pub pending: VecDeque<Value>,
    pub job_queued: bool,
}

/// What kind of object this is, with its kind-specific internal slots.
pub enum ObjectKind {
    Ordinary,
    /// `%Object.prototype%`, an immutable prototype exotic object.
    ImmutablePrototype,
    /// Array exotic object; `length` is an ordinary own data property.
    Array,
    BytecodeFunction(Box<BytecodeFunctionData>),
    NativeFunction(Box<NativeFunctionData>),
    BoundFunction(Box<BoundFunctionData>),
    Boolean(bool),
    Number(f64),
    /// String exotic object.
    String(JsString),
    Symbol(Symbol),
    BigInt(HeapBigInt),
    Error,
    /// Mapped `arguments` exotic object.
    Arguments(Box<ArgumentsData>),
    UnmappedArguments,
    ArrayBuffer(Box<ArrayBufferData>),
    /// TypedArray (integer-indexed exotic object).
    TypedArray(Box<TypedArrayData>),
    ModuleNamespace(Box<NamespaceData>),
    Proxy(ProxyData),
    WeakMap(EphemeronTable),
    WeakSet(EphemeronTable),
    WeakRef(Option<WeakKey>),
    FinalizationRegistry(Box<RegistryData>),
    /// An object of the host (the DOM) with its own internal methods.
    Host(Rc<dyn HostObject>),
    Promise(Box<PromiseData>),
    /// Generator, async generator, or the suspended body of an async
    /// function (an object that is never exposed to code).
    Generator(Box<GeneratorData>),
    /// `%ForInIteratorPrototype%` instance (CreateForInIterator).
    ForInIterator(Box<ForInData>),
    ArrayIterator(Box<ArrayIteratorData>),
    /// `%AsyncFromSyncIteratorPrototype%` instance: `[[SyncIteratorRecord]]`.
    AsyncFromSyncIterator(Box<IteratorRecord>),
}

impl ObjectKind {
    pub(crate) fn ephemerons(&self) -> Option<&EphemeronTable> {
        match self {
            ObjectKind::WeakMap(t) | ObjectKind::WeakSet(t) => Some(t),
            _ => None,
        }
    }

    pub(crate) fn ephemerons_mut(&mut self) -> Option<&mut EphemeronTable> {
        match self {
            ObjectKind::WeakMap(t) | ObjectKind::WeakSet(t) => Some(t),
            _ => None,
        }
    }

    /// Strong handles of the kind (weak collections and registries are
    /// handled by `ObjectData::trace_object`).
    fn trace(&self, tracer: &mut Tracer) {
        match self {
            ObjectKind::Ordinary
            | ObjectKind::ImmutablePrototype
            | ObjectKind::Array
            | ObjectKind::Boolean(_)
            | ObjectKind::Number(_)
            | ObjectKind::Error
            | ObjectKind::UnmappedArguments
            | ObjectKind::WeakMap(_)
            | ObjectKind::WeakSet(_)
            | ObjectKind::WeakRef(_)
            | ObjectKind::FinalizationRegistry(_) => {}
            ObjectKind::BytecodeFunction(f) => f.trace(tracer),
            ObjectKind::NativeFunction(f) => f.trace(tracer),
            ObjectKind::BoundFunction(f) => f.trace(tracer),
            ObjectKind::String(s) => {
                tracer.string(*s);
            }
            ObjectKind::Symbol(s) => {
                tracer.symbol(*s);
            }
            ObjectKind::BigInt(b) => {
                tracer.bigint(*b);
            }
            ObjectKind::Arguments(a) => a.trace(tracer),
            ObjectKind::ArrayBuffer(b) => b.trace(tracer),
            ObjectKind::TypedArray(t) => t.trace(tracer),
            ObjectKind::ModuleNamespace(n) => n.trace(tracer),
            ObjectKind::Proxy(p) => p.trace(tracer),
            ObjectKind::Host(h) => h.trace(tracer),
            ObjectKind::Promise(p) => p.trace(tracer),
            ObjectKind::Generator(g) => g.trace(tracer),
            ObjectKind::ForInIterator(f) => f.trace(tracer),
            ObjectKind::ArrayIterator(a) => a.trace(tracer),
            ObjectKind::AsyncFromSyncIterator(r) => r.trace(tracer),
        }
    }

    fn extra_bytes(&self) -> usize {
        match self {
            ObjectKind::ArrayBuffer(b) => b.byte_length(),
            ObjectKind::WeakMap(t) | ObjectKind::WeakSet(t) => {
                t.capacity() * size_of::<(WeakKey, Value)>()
            }
            ObjectKind::FinalizationRegistry(r) => {
                r.cells.capacity() * size_of::<RegistryCell>()
                    + r.pending.capacity() * size_of::<Value>()
            }
            _ => 0,
        }
    }
}

/// An object: prototype, extensibility, properties and its kind.
pub struct ObjectData {
    pub(crate) proto: Option<Object>,
    pub(crate) extensible: bool,
    pub(crate) props: Properties,
    pub(crate) elements: Elements,
    pub(crate) private: Vec<PrivateElement>,
    pub kind: ObjectKind,
}

impl ObjectData {
    pub fn proto(&self) -> Option<Object> {
        self.proto
    }

    pub fn is_extensible(&self) -> bool {
        self.extensible
    }

    pub fn private_elements(&self) -> &[PrivateElement] {
        &self.private
    }

    pub fn is_callable(&self) -> bool {
        match &self.kind {
            ObjectKind::BytecodeFunction(_)
            | ObjectKind::NativeFunction(_)
            | ObjectKind::BoundFunction(_) => true,
            ObjectKind::Proxy(p) => p.callable,
            _ => false,
        }
    }

    pub fn is_constructor(&self) -> bool {
        match &self.kind {
            ObjectKind::BytecodeFunction(f) => f.is_constructor,
            ObjectKind::NativeFunction(f) => f.constructor,
            ObjectKind::BoundFunction(f) => f.constructor,
            ObjectKind::Proxy(p) => p.constructor,
            _ => false,
        }
    }

    pub(crate) fn trace_object(&self, me: Object, tracer: &mut Tracer) {
        if let Some(p) = self.proto {
            tracer.object(p);
        }
        match &self.props {
            Properties::Shaped { shape, slots } => {
                tracer.shape(*shape);
                for s in slots {
                    s.trace(tracer);
                }
            }
            Properties::Dictionary(map) => {
                for (key, _, slot) in map.iter() {
                    tracer.key(*key);
                    slot.trace(tracer);
                }
            }
        }
        self.elements.trace(tracer);
        for p in &self.private {
            p.trace(tracer);
        }
        match &self.kind {
            ObjectKind::WeakMap(_) | ObjectKind::WeakSet(_) => tracer.ephemeron_tables.push(me),
            ObjectKind::WeakRef(_) => tracer.weak_refs.push(me),
            ObjectKind::FinalizationRegistry(reg) => {
                tracer.registries.push(me);
                tracer.value(reg.cleanup);
                tracer.realm(reg.realm);
                for cell in &reg.cells {
                    tracer.value(cell.held);
                }
                for v in &reg.pending {
                    tracer.value(*v);
                }
            }
            kind => kind.trace(tracer),
        }
    }

    pub(crate) fn extra_bytes(&self) -> usize {
        let props = match &self.props {
            Properties::Shaped { slots, .. } => slots.capacity() * size_of::<Slot>(),
            Properties::Dictionary(map) => {
                map.len() * 2 * size_of::<(PropertyKey, Attributes, Slot)>()
            }
        };
        props
            + self.elements.bytes()
            + self.private.capacity() * size_of::<PrivateElement>()
            + self.kind.extra_bytes()
    }
}

impl Heap {
    /// MakeBasicObject: an extensible object with no properties.
    pub fn create_object(&mut self, proto: Option<Object>, kind: ObjectKind) -> Object {
        let shape = self.empty_shape;
        self.alloc_object(ObjectData {
            proto,
            extensible: true,
            props: Properties::Shaped {
                shape,
                slots: Vec::new(),
            },
            elements: Elements::Dense(Vec::new()),
            private: Vec::new(),
            kind,
        })
    }

    pub fn shape(&self, s: Shape) -> &ShapeData {
        self.shapes.get(s)
    }

    /// The shape of `o`, or `None` in dictionary mode.
    pub fn object_shape(&self, o: Object) -> Option<Shape> {
        match &self.objects.get(o).props {
            Properties::Shaped { shape, .. } => Some(*shape),
            Properties::Dictionary(_) => None,
        }
    }

    pub fn is_dictionary(&self, o: Object) -> bool {
        self.object_shape(o).is_none()
    }

    /// The stored own property `key` of `o` (no exotic behaviour).
    pub fn get_own(&self, o: Object, key: PropertyKey) -> Option<(Attributes, Slot)> {
        let obj = self.objects.get(o);
        match key {
            PropertyKey::Index(i) => obj.elements.get(i),
            _ => match &obj.props {
                Properties::Shaped { shape, slots } => self
                    .shapes
                    .get(*shape)
                    .find(key)
                    .map(|(i, attrs)| (attrs, slots[i])),
                Properties::Dictionary(map) => map.get(key),
            },
        }
    }

    /// Adds or replaces the stored own property `key` of `o`.
    pub fn set_own(&mut self, o: Object, key: PropertyKey, attrs: Attributes, slot: Slot) {
        match key {
            PropertyKey::Index(i) => {
                let bytes = self.objects.get_mut(o).elements.set(i, attrs, slot);
                self.note_alloc(bytes);
            }
            _ => self.set_named(o, key, attrs, slot),
        }
    }

    /// Replaces the value of an existing data property, keeping its
    /// attributes.
    pub fn set_own_value(&mut self, o: Object, key: PropertyKey, value: Value) {
        if let Some((attrs, _)) = self.get_own(o, key) {
            self.set_own(o, key, attrs, Slot::Data(value));
        }
    }

    fn set_named(&mut self, o: Object, key: PropertyKey, attrs: Attributes, slot: Slot) {
        let shape = self.object_shape(o);
        let Some(shape) = shape else {
            if let Properties::Dictionary(map) = &mut self.objects.get_mut(o).props {
                map.insert(key, attrs, slot);
            }
            self.note_alloc(2 * size_of::<(PropertyKey, Attributes, Slot)>());
            return;
        };
        let found = self.shapes.get(shape).find(key);
        match found {
            Some((index, old)) if old == attrs => {
                if let Properties::Shaped { slots, .. } = &mut self.objects.get_mut(o).props {
                    slots[index] = slot;
                }
            }
            Some(_) => self.insert_in_dictionary(o, key, attrs, slot),
            None if self.shapes.get(shape).len() >= MAX_SHAPE_PROPERTIES => {
                self.insert_in_dictionary(o, key, attrs, slot)
            }
            None => {
                let (child, bytes) = shape_with(&mut self.shapes, shape, key, attrs);
                if let Properties::Shaped { shape, slots } = &mut self.objects.get_mut(o).props {
                    *shape = child;
                    slots.push(slot);
                }
                self.note_alloc(bytes + size_of::<Slot>());
            }
        }
    }

    fn insert_in_dictionary(&mut self, o: Object, key: PropertyKey, attrs: Attributes, slot: Slot) {
        self.make_dictionary(o);
        if let Properties::Dictionary(map) = &mut self.objects.get_mut(o).props {
            map.insert(key, attrs, slot);
        }
    }

    /// Moves the named properties of `o` out of its shape into a
    /// dictionary, keeping their order.
    pub fn make_dictionary(&mut self, o: Object) {
        let obj = self.objects.get_mut(o);
        let map = match &obj.props {
            Properties::Dictionary(_) => return,
            Properties::Shaped { shape, slots } => {
                let mut map = PropertyMap::new();
                for ((key, attrs), slot) in self.shapes.get(*shape).keys.iter().zip(slots) {
                    map.insert(*key, *attrs, *slot);
                }
                map
            }
        };
        obj.props = Properties::Dictionary(map);
        let bytes = self.objects.get(o).extra_bytes();
        self.note_alloc(bytes);
    }

    /// Removes the stored own property `key`; whether it existed.
    pub fn delete_own(&mut self, o: Object, key: PropertyKey) -> bool {
        if let PropertyKey::Index(i) = key {
            return self.objects.get_mut(o).elements.remove(i);
        }
        let Some(shape) = self.object_shape(o) else {
            if let Properties::Dictionary(map) = &mut self.objects.get_mut(o).props {
                return map.remove(key);
            }
            return false;
        };
        let shape_data = self.shapes.get(shape);
        let Some((index, _)) = shape_data.find(key) else {
            return false;
        };
        if index + 1 == shape_data.len() {
            // Deleting the newest property goes back to the parent shape,
            // whose keys are exactly the remaining prefix.
            let parent = shape_data.parent.expect("non-empty shape has a parent");
            if let Properties::Shaped { shape, slots } = &mut self.objects.get_mut(o).props {
                *shape = parent;
                slots.pop();
            }
            return true;
        }
        self.make_dictionary(o);
        if let Properties::Dictionary(map) = &mut self.objects.get_mut(o).props {
            return map.remove(key);
        }
        false
    }

    /// The stored own keys in the order of OrdinaryOwnPropertyKeys: array
    /// indices ascending, then strings, then symbols, each in creation
    /// order.
    pub fn own_keys(&self, o: Object) -> Vec<PropertyKey> {
        let obj = self.objects.get(o);
        let mut keys = Vec::new();
        obj.elements.push_indices(&mut keys);
        let named: Vec<PropertyKey> = match &obj.props {
            Properties::Shaped { shape, .. } => self
                .shapes
                .get(*shape)
                .keys
                .iter()
                .map(|(k, _)| *k)
                .collect(),
            Properties::Dictionary(map) => map.iter().map(|(k, _, _)| *k).collect(),
        };
        keys.extend(named.iter().filter(|k| !k.is_symbol()));
        keys.extend(named.iter().filter(|k| k.is_symbol()));
        keys
    }

    /// Deletes stored elements at `new_len` and above from the highest
    /// down; returns the first non-configurable index that stopped it.
    pub(crate) fn truncate_elements(&mut self, o: Object, new_len: u32) -> Option<u32> {
        self.objects.get_mut(o).elements.truncate(new_len)
    }

    pub fn set_proto(&mut self, o: Object, proto: Option<Object>) {
        self.objects.get_mut(o).proto = proto;
    }

    pub fn set_extensible(&mut self, o: Object, extensible: bool) {
        self.objects.get_mut(o).extensible = extensible;
    }

    pub(crate) fn private_elements_mut(&mut self, o: Object) -> &mut Vec<PrivateElement> {
        &mut self.objects.get_mut(o).private
    }
}
