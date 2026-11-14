//! Heap of the engine: typed arenas addressed by index and a precise,
//! non-moving mark-sweep collector (vault/CONTRACT.md §3.4, D-03).
//!
//! The collector only runs when the embedder calls [`Heap::collect`] (through
//! `Agent::collect_garbage` or `Agent::safepoint`) between interpreter
//! instructions or between event-loop tasks. Native code may therefore keep
//! handles in Rust locals without rooting them; whatever must outlive a safe
//! point has to be reachable from the roots given to the collection.
//!
//! After marking (ephemeron tables are marked to a fixed point), weak
//! references, `WeakMap`/`WeakSet` entries, `FinalizationRegistry` cells,
//! shape transitions and the atom table are cleared of dead entries; then
//! every arena is swept and the Rust finalizers of freed objects run.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::hash::{Hash, Hasher};
use std::marker::PhantomData;
use std::mem::size_of;
use std::rc::Rc;

use crate::bigint::BigInt;
use crate::env::EnvData;
use crate::object::{ObjectData, ObjectKind};
use crate::realm::RealmData;
use crate::shape::ShapeData;
use crate::string::{AtomData, StringData};
use crate::value::{PropertyKey, SymbolData, Value, WeakKey};

/// Index of a value of type `T` in its arena. Copyable and never moves; it
/// is only valid while the value is reachable from the roots of the next
/// collection.
pub struct Gc<T> {
    index: u32,
    marker: PhantomData<fn() -> T>,
}

impl<T> Gc<T> {
    pub(crate) const fn from_index(index: u32) -> Gc<T> {
        Gc {
            index,
            marker: PhantomData,
        }
    }

    /// Slot number in the arena (stable while the value is alive).
    pub fn index(self) -> u32 {
        self.index
    }
}

impl<T> Clone for Gc<T> {
    fn clone(&self) -> Gc<T> {
        *self
    }
}

impl<T> Copy for Gc<T> {}

impl<T> PartialEq for Gc<T> {
    fn eq(&self, other: &Gc<T>) -> bool {
        self.index == other.index
    }
}

impl<T> Eq for Gc<T> {}

impl<T> Hash for Gc<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.index.hash(state);
    }
}

impl<T> fmt::Debug for Gc<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = std::any::type_name::<T>();
        let short = name.rsplit("::").next().unwrap_or(name);
        write!(f, "{short}#{}", self.index)
    }
}

pub type Object = Gc<ObjectData>;
pub type JsString = Gc<StringData>;
pub type Symbol = Gc<SymbolData>;
pub type HeapBigInt = Gc<BigInt>;
pub type Shape = Gc<ShapeData>;
pub type Env = Gc<EnvData>;
pub type Atom = Gc<AtomData>;
pub type Realm = Gc<RealmData>;

/// A typed arena: slots are reused after they are swept, never moved.
pub(crate) struct Arena<T> {
    slots: Vec<Option<T>>,
    free: Vec<u32>,
    live: usize,
}

impl<T> Arena<T> {
    pub(crate) fn new() -> Arena<T> {
        Arena {
            slots: Vec::new(),
            free: Vec::new(),
            live: 0,
        }
    }

    pub(crate) fn alloc(&mut self, value: T) -> Gc<T> {
        self.live += 1;
        let index = match self.free.pop() {
            Some(index) => {
                self.slots[index as usize] = Some(value);
                index
            }
            None => {
                let index = u32::try_from(self.slots.len()).expect("arena exhausted");
                self.slots.push(Some(value));
                index
            }
        };
        Gc::from_index(index)
    }

    pub(crate) fn get(&self, handle: Gc<T>) -> &T {
        match self.slots.get(handle.index as usize) {
            Some(Some(value)) => value,
            _ => panic!("dangling handle {handle:?}"),
        }
    }

    pub(crate) fn get_mut(&mut self, handle: Gc<T>) -> &mut T {
        match self.slots.get_mut(handle.index as usize) {
            Some(Some(value)) => value,
            _ => panic!("dangling handle {handle:?}"),
        }
    }

    fn slot(&self, index: u32) -> &T {
        self.get(Gc::from_index(index))
    }

    pub(crate) fn contains(&self, handle: Gc<T>) -> bool {
        matches!(self.slots.get(handle.index as usize), Some(Some(_)))
    }

    pub(crate) fn live(&self) -> usize {
        self.live
    }

    fn slot_count(&self) -> usize {
        self.slots.len()
    }

    pub(crate) fn values(&self) -> impl Iterator<Item = &T> {
        self.slots.iter().flatten()
    }

    fn values_mut(&mut self) -> impl Iterator<Item = &mut T> {
        self.slots.iter_mut().flatten()
    }

    /// Frees every allocated slot whose mark bit is clear and hands the
    /// value to `freed` before dropping it.
    fn sweep(&mut self, marks: &[bool], mut freed: impl FnMut(u32, T)) -> usize {
        let mut count = 0;
        for (index, slot) in self.slots.iter_mut().enumerate() {
            if slot.is_some() && !marks[index] {
                let value = slot.take().expect("slot checked above");
                self.free.push(index as u32);
                self.live -= 1;
                count += 1;
                freed(index as u32, value);
            }
        }
        count
    }
}

/// Mark bits of one arena plus the gray stack of marked, untraced slots.
pub(crate) struct Marks {
    bits: Vec<bool>,
    stack: Vec<u32>,
}

impl Marks {
    fn new(len: usize) -> Marks {
        Marks {
            bits: vec![false; len],
            stack: Vec::new(),
        }
    }

    fn mark(&mut self, index: u32) -> bool {
        let bit = &mut self.bits[index as usize];
        if *bit {
            false
        } else {
            *bit = true;
            self.stack.push(index);
            true
        }
    }

    pub(crate) fn is_marked(&self, index: u32) -> bool {
        self.bits.get(index as usize).copied().unwrap_or(false)
    }
}

/// Marking state handed to [`Trace`] implementations.
pub struct Tracer {
    pub(crate) objects: Marks,
    pub(crate) strings: Marks,
    pub(crate) symbols: Marks,
    pub(crate) bigints: Marks,
    pub(crate) shapes: Marks,
    pub(crate) envs: Marks,
    pub(crate) atoms: Marks,
    pub(crate) realms: Marks,
    /// Live `WeakMap`/`WeakSet` objects, whose entries are ephemerons.
    pub(crate) ephemeron_tables: Vec<Object>,
    /// Live `WeakRef` objects.
    pub(crate) weak_refs: Vec<Object>,
    /// Live `FinalizationRegistry` objects.
    pub(crate) registries: Vec<Object>,
}

impl Tracer {
    fn new(heap: &Heap) -> Tracer {
        Tracer {
            objects: Marks::new(heap.objects.slot_count()),
            strings: Marks::new(heap.strings.slot_count()),
            symbols: Marks::new(heap.symbols.slot_count()),
            bigints: Marks::new(heap.bigints.slot_count()),
            shapes: Marks::new(heap.shapes.slot_count()),
            envs: Marks::new(heap.envs.slot_count()),
            atoms: Marks::new(heap.atoms.slot_count()),
            realms: Marks::new(heap.realms.slot_count()),
            ephemeron_tables: Vec::new(),
            weak_refs: Vec::new(),
            registries: Vec::new(),
        }
    }

    /// Marks the value; returns whether it was newly marked.
    pub fn value(&mut self, value: Value) -> bool {
        match value {
            Value::String(s) => self.string(s),
            Value::Symbol(s) => self.symbol(s),
            Value::BigInt(b) => self.bigint(b),
            Value::Object(o) => self.object(o),
            Value::Undefined | Value::Null | Value::Boolean(_) | Value::Number(_) => false,
        }
    }

    pub fn object(&mut self, o: Object) -> bool {
        self.objects.mark(o.index)
    }

    pub fn string(&mut self, s: JsString) -> bool {
        self.strings.mark(s.index)
    }

    pub fn symbol(&mut self, s: Symbol) -> bool {
        self.symbols.mark(s.index)
    }

    pub fn bigint(&mut self, b: HeapBigInt) -> bool {
        self.bigints.mark(b.index)
    }

    pub fn shape(&mut self, s: Shape) -> bool {
        self.shapes.mark(s.index)
    }

    pub fn env(&mut self, e: Env) -> bool {
        self.envs.mark(e.index)
    }

    pub fn atom(&mut self, a: Atom) -> bool {
        self.atoms.mark(a.index)
    }

    pub fn realm(&mut self, r: Realm) -> bool {
        self.realms.mark(r.index)
    }

    pub fn key(&mut self, key: PropertyKey) {
        match key {
            PropertyKey::Index(_) => {}
            PropertyKey::String(a) => {
                self.atom(a);
            }
            PropertyKey::Symbol(s) => {
                self.symbol(s);
            }
        }
    }

    pub fn weak_key(&mut self, key: WeakKey) -> bool {
        match key {
            WeakKey::Object(o) => self.object(o),
            WeakKey::Symbol(s) => self.symbol(s),
        }
    }

    pub(crate) fn is_weak_key_marked(&self, key: WeakKey) -> bool {
        match key {
            WeakKey::Object(o) => self.objects.is_marked(o.index),
            WeakKey::Symbol(s) => self.symbols.is_marked(s.index),
        }
    }

    fn has_gray(&self) -> bool {
        !(self.objects.stack.is_empty()
            && self.strings.stack.is_empty()
            && self.symbols.stack.is_empty()
            && self.shapes.stack.is_empty()
            && self.envs.stack.is_empty()
            && self.realms.stack.is_empty())
    }
}

/// Anything that holds heap handles and can report them to the collector.
pub trait Trace {
    fn trace(&self, tracer: &mut Tracer);
}

impl Trace for Value {
    fn trace(&self, tracer: &mut Tracer) {
        tracer.value(*self);
    }
}

impl Trace for PropertyKey {
    fn trace(&self, tracer: &mut Tracer) {
        tracer.key(*self);
    }
}

impl Trace for Gc<ObjectData> {
    fn trace(&self, tracer: &mut Tracer) {
        tracer.object(*self);
    }
}

impl Trace for Gc<StringData> {
    fn trace(&self, tracer: &mut Tracer) {
        tracer.string(*self);
    }
}

impl Trace for Gc<SymbolData> {
    fn trace(&self, tracer: &mut Tracer) {
        tracer.symbol(*self);
    }
}

impl Trace for Gc<EnvData> {
    fn trace(&self, tracer: &mut Tracer) {
        tracer.env(*self);
    }
}

impl Trace for Gc<RealmData> {
    fn trace(&self, tracer: &mut Tracer) {
        tracer.realm(*self);
    }
}

impl<T: Trace> Trace for Option<T> {
    fn trace(&self, tracer: &mut Tracer) {
        if let Some(x) = self {
            x.trace(tracer);
        }
    }
}

impl<T: Trace> Trace for [T] {
    fn trace(&self, tracer: &mut Tracer) {
        for x in self {
            x.trace(tracer);
        }
    }
}

impl<T: Trace> Trace for Vec<T> {
    fn trace(&self, tracer: &mut Tracer) {
        self.as_slice().trace(tracer);
    }
}

impl<T: Trace> Trace for VecDeque<T> {
    fn trace(&self, tracer: &mut Tracer) {
        for x in self {
            x.trace(tracer);
        }
    }
}

impl<K, V: Trace> Trace for HashMap<K, V> {
    fn trace(&self, tracer: &mut Tracer) {
        for v in self.values() {
            v.trace(tracer);
        }
    }
}

impl<A: Trace, B: Trace> Trace for (A, B) {
    fn trace(&self, tracer: &mut Tracer) {
        self.0.trace(tracer);
        self.1.trace(tracer);
    }
}

impl<T: Trace + ?Sized> Trace for &T {
    fn trace(&self, tracer: &mut Tracer) {
        (**self).trace(tracer);
    }
}

impl<T: Trace + ?Sized> Trace for Box<T> {
    fn trace(&self, tracer: &mut Tracer) {
        (**self).trace(tracer);
    }
}

impl<T: Trace + ?Sized> Trace for Rc<T> {
    fn trace(&self, tracer: &mut Tracer) {
        (**self).trace(tracer);
    }
}

impl<T: Trace + ?Sized> Trace for RefCell<T> {
    fn trace(&self, tracer: &mut Tracer) {
        self.borrow().trace(tracer);
    }
}

/// Handle of a single persistent root (see [`Heap::add_root`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RootId(usize);

/// Handle of a host root set (see [`Heap::add_root_set`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RootSetId(usize);

/// Smallest allocation budget between two collections.
pub const MIN_GC_THRESHOLD: usize = 4 << 20;

/// What one collection freed, plus the `FinalizationRegistry` objects that
/// now have cells to clean up (the agent turns them into host jobs).
#[derive(Debug, Default)]
pub struct GcReport {
    pub freed_objects: usize,
    pub freed_strings: usize,
    pub freed_symbols: usize,
    pub freed_bigints: usize,
    pub freed_shapes: usize,
    pub freed_envs: usize,
    pub freed_atoms: usize,
    pub freed_realms: usize,
    pub finalizers_run: usize,
    pub cleanup_registries: Vec<Object>,
}

/// Live values per arena and the estimated live bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HeapStats {
    pub objects: usize,
    pub strings: usize,
    pub symbols: usize,
    pub bigints: usize,
    pub shapes: usize,
    pub envs: usize,
    pub atoms: usize,
    pub realms: usize,
    pub live_bytes: usize,
}

macro_rules! names {
    ($($field:ident = $text:literal),* $(,)?) => {
        /// Atoms interned when the heap is created; they are permanent roots.
        #[derive(Clone, Copy, Debug)]
        pub struct Names {
            $(pub $field: Atom,)*
        }

        impl Names {
            fn intern_all(heap: &mut Heap) -> Names {
                Names { $($field: heap.intern_str($text),)* }
            }

            fn placeholder() -> Names {
                Names { $($field: Gc::from_index(0),)* }
            }

            fn trace(&self, tracer: &mut Tracer) {
                $(tracer.atom(self.$field);)*
            }
        }
    };
}

names! {
    empty = "",
    length = "length",
    prototype = "prototype",
    constructor = "constructor",
    name = "name",
    message = "message",
    cause = "cause",
    errors = "errors",
    error = "error",
    suppressed = "suppressed",
    value = "value",
    writable = "writable",
    get = "get",
    set = "set",
    enumerable = "enumerable",
    configurable = "configurable",
    to_string = "toString",
    value_of = "valueOf",
    done = "done",
    next = "next",
    return_ = "return",
    throw = "throw",
    callee = "callee",
    caller = "caller",
    arguments = "arguments",
    then = "then",
    default = "default",
    global_this = "globalThis",
    undefined = "undefined",
    nan = "NaN",
    infinity = "Infinity",
    apply = "apply",
    construct = "construct",
    define_property = "defineProperty",
    delete_property = "deleteProperty",
    get_own_property_descriptor = "getOwnPropertyDescriptor",
    get_prototype_of = "getPrototypeOf",
    set_prototype_of = "setPrototypeOf",
    has = "has",
    is_extensible = "isExtensible",
    own_keys = "ownKeys",
    prevent_extensions = "preventExtensions",
    values = "values",
    index = "index",
    input = "input",
    last_index = "lastIndex",
}

/// The process heap: one arena per kind of value plus the collector state.
pub struct Heap {
    pub(crate) objects: Arena<ObjectData>,
    pub(crate) strings: Arena<StringData>,
    pub(crate) symbols: Arena<SymbolData>,
    pub(crate) bigints: Arena<BigInt>,
    pub(crate) shapes: Arena<ShapeData>,
    pub(crate) envs: Arena<EnvData>,
    pub(crate) atoms: Arena<AtomData>,
    pub(crate) realms: Arena<RealmData>,
    pub(crate) atom_table: HashMap<Rc<[u16]>, Atom>,
    roots: Vec<Option<Value>>,
    free_roots: Vec<usize>,
    root_sets: Vec<Option<Rc<dyn Trace>>>,
    finalizers: HashMap<u32, Vec<Box<dyn FnOnce()>>>,
    /// Frequently used property names.
    pub names: Names,
    pub(crate) empty_shape: Shape,
    pub(crate) empty_string: JsString,
    allocated: usize,
    threshold: usize,
    min_threshold: usize,
    live_bytes: usize,
    collections: u64,
}

impl Default for Heap {
    fn default() -> Heap {
        Heap::new()
    }
}

impl Heap {
    pub fn new() -> Heap {
        let mut heap = Heap {
            objects: Arena::new(),
            strings: Arena::new(),
            symbols: Arena::new(),
            bigints: Arena::new(),
            shapes: Arena::new(),
            envs: Arena::new(),
            atoms: Arena::new(),
            realms: Arena::new(),
            atom_table: HashMap::new(),
            roots: Vec::new(),
            free_roots: Vec::new(),
            root_sets: Vec::new(),
            finalizers: HashMap::new(),
            names: Names::placeholder(),
            empty_shape: Gc::from_index(0),
            empty_string: Gc::from_index(0),
            allocated: 0,
            threshold: MIN_GC_THRESHOLD,
            min_threshold: MIN_GC_THRESHOLD,
            live_bytes: 0,
            collections: 0,
        };
        heap.empty_shape = heap.shapes.alloc(ShapeData::root());
        heap.empty_string = heap.strings.alloc(StringData::Flat(Box::new([])));
        let names = Names::intern_all(&mut heap);
        heap.names = names;
        heap
    }

    /// Counts `bytes` against the allocation budget of the next collection.
    pub(crate) fn note_alloc(&mut self, bytes: usize) {
        self.allocated = self.allocated.saturating_add(bytes);
    }

    pub(crate) fn alloc_object(&mut self, data: ObjectData) -> Object {
        self.note_alloc(size_of::<ObjectData>());
        self.objects.alloc(data)
    }

    pub fn object(&self, o: Object) -> &ObjectData {
        self.objects.get(o)
    }

    pub fn object_mut(&mut self, o: Object) -> &mut ObjectData {
        self.objects.get_mut(o)
    }

    pub fn object_exists(&self, o: Object) -> bool {
        self.objects.contains(o)
    }

    pub fn string_exists(&self, s: JsString) -> bool {
        self.strings.contains(s)
    }

    pub fn symbol_exists(&self, s: Symbol) -> bool {
        self.symbols.contains(s)
    }

    pub fn atom_exists(&self, a: Atom) -> bool {
        self.atoms.contains(a)
    }

    pub fn shape_exists(&self, s: Shape) -> bool {
        self.shapes.contains(s)
    }

    pub fn alloc_symbol(&mut self, description: Option<JsString>) -> Symbol {
        self.note_alloc(size_of::<SymbolData>());
        self.symbols.alloc(SymbolData {
            description,
            registered: false,
            private: false,
        })
    }

    /// A private name (`#x`): a symbol that is never a property key.
    pub fn alloc_private_name(&mut self, description: JsString) -> Symbol {
        self.note_alloc(size_of::<SymbolData>());
        self.symbols.alloc(SymbolData {
            description: Some(description),
            registered: false,
            private: true,
        })
    }

    pub fn symbol(&self, s: Symbol) -> &SymbolData {
        self.symbols.get(s)
    }

    pub(crate) fn symbol_mut(&mut self, s: Symbol) -> &mut SymbolData {
        self.symbols.get_mut(s)
    }

    pub fn alloc_bigint(&mut self, value: BigInt) -> HeapBigInt {
        self.note_alloc(size_of::<BigInt>() + value.word_count() * 4);
        self.bigints.alloc(value)
    }

    pub fn bigint(&self, b: HeapBigInt) -> &BigInt {
        self.bigints.get(b)
    }

    pub fn alloc_env(&mut self, outer: Option<Env>, slots: usize) -> Env {
        self.note_alloc(size_of::<EnvData>() + slots * size_of::<Option<Value>>());
        self.envs.alloc(EnvData::declarative(outer, slots))
    }

    /// Allocates a prepared environment record.
    pub fn alloc_env_data(&mut self, data: EnvData) -> Env {
        self.note_alloc(size_of::<EnvData>() + data.slots.len() * size_of::<Option<Value>>());
        self.envs.alloc(data)
    }

    pub fn env(&self, e: Env) -> &EnvData {
        self.envs.get(e)
    }

    pub fn env_mut(&mut self, e: Env) -> &mut EnvData {
        self.envs.get_mut(e)
    }

    pub(crate) fn alloc_realm(&mut self, data: RealmData) -> Realm {
        self.note_alloc(size_of::<RealmData>());
        self.realms.alloc(data)
    }

    pub fn realm(&self, r: Realm) -> &RealmData {
        self.realms.get(r)
    }

    pub fn realm_mut(&mut self, r: Realm) -> &mut RealmData {
        self.realms.get_mut(r)
    }

    /// Keeps `value` alive until [`Heap::remove_root`].
    pub fn add_root(&mut self, value: Value) -> RootId {
        match self.free_roots.pop() {
            Some(i) => {
                self.roots[i] = Some(value);
                RootId(i)
            }
            None => {
                self.roots.push(Some(value));
                RootId(self.roots.len() - 1)
            }
        }
    }

    pub fn root_value(&self, id: RootId) -> Value {
        self.roots[id.0].expect("removed root")
    }

    pub fn set_root_value(&mut self, id: RootId, value: Value) {
        self.roots[id.0] = Some(value);
    }

    pub fn remove_root(&mut self, id: RootId) {
        if self.roots[id.0].take().is_some() {
            self.free_roots.push(id.0);
        }
    }

    /// Registers a host structure (for example the node→wrapper table of an
    /// active document) whose handles are roots until it is removed.
    pub fn add_root_set(&mut self, set: Rc<dyn Trace>) -> RootSetId {
        self.root_sets.push(Some(set));
        RootSetId(self.root_sets.len() - 1)
    }

    pub fn remove_root_set(&mut self, id: RootSetId) {
        self.root_sets[id.0] = None;
    }

    /// Runs `finalizer` after the collection that frees `o`. It must not
    /// touch the heap (it gets no access to it).
    pub fn add_finalizer(&mut self, o: Object, finalizer: Box<dyn FnOnce()>) {
        self.finalizers
            .entry(o.index())
            .or_default()
            .push(finalizer);
    }

    /// Whether the bytes allocated since the last collection reached the
    /// threshold; the embedder checks this at safe points.
    pub fn should_collect(&self) -> bool {
        self.allocated >= self.threshold
    }

    /// Sets the minimum allocation budget between collections.
    pub fn set_gc_threshold(&mut self, bytes: usize) {
        self.min_threshold = bytes;
        self.threshold = bytes.max(self.live_bytes);
    }

    pub fn allocated_bytes(&self) -> usize {
        self.allocated
    }

    pub fn gc_threshold(&self) -> usize {
        self.threshold
    }

    pub fn collections(&self) -> u64 {
        self.collections
    }

    pub fn stats(&self) -> HeapStats {
        HeapStats {
            objects: self.objects.live(),
            strings: self.strings.live(),
            symbols: self.symbols.live(),
            bigints: self.bigints.live(),
            shapes: self.shapes.live(),
            envs: self.envs.live(),
            atoms: self.atoms.live(),
            realms: self.realms.live(),
            live_bytes: self.estimate_live_bytes(),
        }
    }

    /// Collects everything not reachable from the heap's own roots
    /// (persistent roots, root sets, interned names) and from `roots`.
    /// Must only be called at a safe point.
    pub fn collect(&mut self, roots: &[&dyn Trace]) -> GcReport {
        let mut t = Tracer::new(self);
        t.shape(self.empty_shape);
        t.string(self.empty_string);
        self.names.trace(&mut t);
        for v in self.roots.iter().flatten() {
            t.value(*v);
        }
        for set in self.root_sets.iter().flatten() {
            set.trace(&mut t);
        }
        for r in roots {
            r.trace(&mut t);
        }
        self.mark(&mut t);
        let mut report = GcReport {
            cleanup_registries: self.process_weak(&t),
            ..GcReport::default()
        };
        let finalizers = self.sweep(&t, &mut report);
        report.finalizers_run = finalizers.len();
        self.collections += 1;
        self.allocated = 0;
        self.live_bytes = self.estimate_live_bytes();
        self.threshold = self.live_bytes.max(self.min_threshold);
        for f in finalizers {
            f();
        }
        report
    }

    /// Traces gray values until none is left.
    fn drain(&self, t: &mut Tracer) {
        loop {
            if let Some(i) = t.objects.stack.pop() {
                self.objects.slot(i).trace_object(Gc::from_index(i), t);
            } else if let Some(i) = t.strings.stack.pop() {
                self.strings.slot(i).trace(t);
            } else if let Some(i) = t.symbols.stack.pop() {
                self.symbols.slot(i).trace(t);
            } else if let Some(i) = t.shapes.stack.pop() {
                self.shapes.slot(i).trace(t);
            } else if let Some(i) = t.envs.stack.pop() {
                self.envs.slot(i).trace(t);
            } else if let Some(i) = t.realms.stack.pop() {
                self.realms.slot(i).trace(t);
            } else {
                // Atoms and BigInts hold no handles.
                t.atoms.stack.clear();
                t.bigints.stack.clear();
                break;
            }
        }
    }

    /// Marks from the roots, then marks ephemeron values whose keys are
    /// marked until a fixed point.
    fn mark(&self, t: &mut Tracer) {
        loop {
            self.drain(t);
            let mut k = 0;
            while k < t.ephemeron_tables.len() {
                let table = t.ephemeron_tables[k];
                k += 1;
                if let Some(entries) = self.objects.get(table).kind.ephemerons() {
                    for (key, value) in entries.iter() {
                        if t.is_weak_key_marked(*key) {
                            t.value(*value);
                        }
                    }
                }
            }
            if !t.has_gray() {
                break;
            }
        }
    }

    /// Clears dead weak entries; returns the registries that need a
    /// cleanup job.
    fn process_weak(&mut self, t: &Tracer) -> Vec<Object> {
        for &table in &t.ephemeron_tables {
            if let Some(entries) = self.objects.get_mut(table).kind.ephemerons_mut() {
                entries.retain(|key, _| t.is_weak_key_marked(*key));
            }
        }
        for &w in &t.weak_refs {
            if let ObjectKind::WeakRef(target) = &mut self.objects.get_mut(w).kind {
                if let Some(key) = *target {
                    if !t.is_weak_key_marked(key) {
                        *target = None;
                    }
                }
            }
        }
        let mut cleanup = Vec::new();
        for &r in &t.registries {
            if let ObjectKind::FinalizationRegistry(reg) = &mut self.objects.get_mut(r).kind {
                let cells = std::mem::take(&mut reg.cells);
                for mut cell in cells {
                    if !t.is_weak_key_marked(cell.target) {
                        reg.pending.push_back(cell.held);
                        continue;
                    }
                    if let Some(token) = cell.token {
                        if !t.is_weak_key_marked(token) {
                            cell.token = None;
                        }
                    }
                    reg.cells.push(cell);
                }
                if !reg.pending.is_empty() && !reg.job_queued {
                    reg.job_queued = true;
                    cleanup.push(r);
                }
            }
        }
        for shape in self.shapes.values_mut() {
            shape
                .transitions
                .retain(|_, child| t.shapes.is_marked(child.index()));
        }
        self.atom_table
            .retain(|_, atom| t.atoms.is_marked(atom.index()));
        cleanup
    }

    fn sweep(&mut self, t: &Tracer, report: &mut GcReport) -> Vec<Box<dyn FnOnce()>> {
        let mut pending: Vec<Box<dyn FnOnce()>> = Vec::new();
        let finalizers = &mut self.finalizers;
        report.freed_objects = self.objects.sweep(&t.objects.bits, |index, data| {
            if let Some(list) = finalizers.remove(&index) {
                pending.extend(list);
            }
            drop(data);
        });
        report.freed_strings = self.strings.sweep(&t.strings.bits, |_, _| {});
        report.freed_symbols = self.symbols.sweep(&t.symbols.bits, |_, _| {});
        report.freed_bigints = self.bigints.sweep(&t.bigints.bits, |_, _| {});
        report.freed_shapes = self.shapes.sweep(&t.shapes.bits, |_, _| {});
        report.freed_envs = self.envs.sweep(&t.envs.bits, |_, _| {});
        report.freed_atoms = self.atoms.sweep(&t.atoms.bits, |_, _| {});
        report.freed_realms = self.realms.sweep(&t.realms.bits, |_, _| {});
        pending
    }

    fn estimate_live_bytes(&self) -> usize {
        let mut bytes = self.objects.live() * size_of::<Option<ObjectData>>()
            + self.strings.live() * size_of::<Option<StringData>>()
            + self.symbols.live() * size_of::<Option<SymbolData>>()
            + self.bigints.live() * size_of::<Option<BigInt>>()
            + self.shapes.live() * size_of::<Option<ShapeData>>()
            + self.envs.live() * size_of::<Option<EnvData>>()
            + self.atoms.live() * size_of::<Option<AtomData>>()
            + self.realms.live() * size_of::<Option<RealmData>>();
        for o in self.objects.values() {
            bytes += o.extra_bytes();
        }
        for s in self.strings.values() {
            bytes += s.extra_bytes();
        }
        for a in self.atoms.values() {
            bytes += a.0.len() * 2;
        }
        for b in self.bigints.values() {
            bytes += b.word_count() * 4;
        }
        for s in self.shapes.values() {
            bytes += s.extra_bytes();
        }
        for e in self.envs.values() {
            bytes += e.slots.len() * size_of::<Option<Value>>();
        }
        bytes
    }
}
