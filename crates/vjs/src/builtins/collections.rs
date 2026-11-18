//! `Map` and `Set` (ECMA-262 §24.1, §24.2): the constructors,
//! `Map.groupBy`, the prototypes with the set methods (`union`,
//! `intersection`, …) and `getOrInsert`/`getOrInsertComputed` (feature
//! `upsert`), and `%MapIteratorPrototype%` and `%SetIteratorPrototype%`.
//!
//! The entries of a collection live in insertion order in a vector; a
//! deleted entry leaves a hole that iteration skips, so entries added
//! during an iteration are visited and deleted ones are not. When holes
//! pile up the vector is compacted; each compaction (and each `clear`)
//! advances an [`Epoch`] that records how indices moved, and a [`Cursor`]
//! that was created before replays those moves before reading.

use std::cell::RefCell;
use std::collections::HashMap;
use std::mem::size_of;
use std::rc::Rc;

use crate::agent::Agent;
use crate::bigint::BigInt;
use crate::builtins::{
    accessor, constructor, intrinsic, method, plain_object, set_intrinsic, species_getter,
    to_string_tag,
};
use crate::completion::JsResult;
use crate::function::{NativeCall, NativeFn};
use crate::gc::{Object, Realm, Symbol, Trace, Tracer};
use crate::interp::iter::ArrayIterKind;
use crate::object::ObjectKind;
use crate::ops::{self, IteratorRecord};
use crate::realm::Intrinsic;
use crate::value::{PropertyKey, Value};

// ----- storage -----

/// A key compared with SameValueZero: `-0` and `+0` are one key, every NaN
/// is one key, strings and BigInts compare by value.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum MapKey {
    Undefined,
    Null,
    Boolean(bool),
    Number(u64),
    String(Rc<[u16]>),
    Symbol(Symbol),
    BigInt(BigInt),
    Object(Object),
}

/// The SameValueZero key of `v`.
pub(crate) fn map_key(agent: &mut Agent, v: Value) -> MapKey {
    match v {
        Value::Undefined => MapKey::Undefined,
        Value::Null => MapKey::Null,
        Value::Boolean(b) => MapKey::Boolean(b),
        Value::Number(n) => MapKey::Number(if n == 0.0 {
            0
        } else if n.is_nan() {
            f64::NAN.to_bits()
        } else {
            n.to_bits()
        }),
        Value::String(s) => MapKey::String(Rc::from(agent.heap.string_units(s))),
        Value::Symbol(s) => MapKey::Symbol(s),
        Value::BigInt(b) => MapKey::BigInt(agent.heap.bigint(b).clone()),
        Value::Object(o) => MapKey::Object(o),
    }
}

/// CanonicalizeKeyedCollectionKey(key).
pub(crate) fn canonicalize(v: Value) -> Value {
    match v {
        Value::Number(n) if n == 0.0 => Value::Number(0.0),
        v => v,
    }
}

/// How the indices of an entry vector moved.
enum Shift {
    /// The entries at these (ascending) indices were removed.
    Removed(Vec<usize>),
    /// Every entry was removed.
    Cleared,
}

/// A version of the index space of an entry vector. `next` is set when
/// the vector is compacted or cleared; cursors created in this version
/// follow it to the current one.
pub(crate) struct Epoch {
    next: RefCell<Option<(Shift, Rc<Epoch>)>>,
}

impl Epoch {
    fn new() -> Rc<Epoch> {
        Rc::new(Epoch {
            next: RefCell::new(None),
        })
    }
}

/// `[[MapData]]` or `[[SetData]]`.
pub struct MapData {
    entries: Vec<Option<(Value, Value)>>,
    index: HashMap<MapKey, usize>,
    holes: usize,
    epoch: Rc<Epoch>,
}

impl Default for MapData {
    fn default() -> MapData {
        MapData::new()
    }
}

impl MapData {
    pub fn new() -> MapData {
        MapData {
            entries: Vec::new(),
            index: HashMap::new(),
            holes: 0,
            epoch: Epoch::new(),
        }
    }

    /// The number of live entries.
    pub fn len(&self) -> usize {
        self.entries.len() - self.holes
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub(crate) fn get(&self, key: &MapKey) -> Option<Value> {
        let &i = self.index.get(key)?;
        self.entries[i].map(|(_, v)| v)
    }

    pub(crate) fn has(&self, key: &MapKey) -> bool {
        self.index.contains_key(key)
    }

    /// Sets the value of the entry for `key`, appending one if there is
    /// none; whether it was appended.
    pub(crate) fn set(&mut self, k: MapKey, key: Value, value: Value) -> bool {
        if let Some(&i) = self.index.get(&k) {
            if let Some(entry) = &mut self.entries[i] {
                entry.1 = value;
            }
            return false;
        }
        self.index.insert(k, self.entries.len());
        self.entries.push(Some((key, value)));
        true
    }

    pub(crate) fn delete(&mut self, k: &MapKey) -> bool {
        let Some(i) = self.index.remove(k) else {
            return false;
        };
        self.entries[i] = None;
        self.holes += 1;
        if self.holes > 16 && self.holes * 2 > self.entries.len() {
            self.compact();
        }
        true
    }

    pub(crate) fn clear(&mut self) {
        self.entries.clear();
        self.index.clear();
        self.holes = 0;
        self.advance(Shift::Cleared);
    }

    fn compact(&mut self) {
        if self.holes == 0 {
            return;
        }
        let mut removed = Vec::with_capacity(self.holes);
        let mut kept = Vec::with_capacity(self.len());
        for (i, e) in self.entries.drain(..).enumerate() {
            match e {
                Some(e) => kept.push(Some(e)),
                None => removed.push(i),
            }
        }
        self.entries = kept;
        for pos in self.index.values_mut() {
            *pos -= removed.partition_point(|&r| r < *pos);
        }
        self.holes = 0;
        self.advance(Shift::Removed(removed));
    }

    fn advance(&mut self, shift: Shift) {
        // Only cursors (and the previous epoch, which a cursor may hold)
        // share the current epoch; with no sharer nobody needs the record.
        if Rc::strong_count(&self.epoch) == 1 {
            return;
        }
        let next = Epoch::new();
        *self.epoch.next.borrow_mut() = Some((shift, next.clone()));
        self.epoch = next;
    }

    /// A compacted copy that shares no iteration state.
    pub(crate) fn copy(&self) -> MapData {
        let mut c = MapData {
            entries: self.entries.clone(),
            index: self.index.clone(),
            holes: self.holes,
            epoch: Epoch::new(),
        };
        c.compact();
        c
    }

    /// The live entries, in order.
    pub(crate) fn live(&self) -> Vec<(Value, Value)> {
        self.entries.iter().flatten().copied().collect()
    }

    pub(crate) fn bytes(&self) -> usize {
        self.entries.capacity() * size_of::<Option<(Value, Value)>>()
            + self.index.capacity() * (size_of::<MapKey>() + size_of::<usize>())
    }
}

/// The live entries of a `Map` or `Set` (`[[MapData]]`/`[[SetData]]`, in
/// order), for hosts that walk them (structured clone); `None` for other
/// objects.
pub fn collection_entries(agent: &Agent, o: Object) -> Option<Vec<(Value, Value)>> {
    match &agent.heap.object(o).kind {
        ObjectKind::Map(d) | ObjectKind::Set(d) => Some(d.live()),
        _ => None,
    }
}

/// Appends `key` and `value` to the data of a `Map` or `Set` (for a `Set`
/// the value is ignored) without calling any method of the object, as a
/// host deserializing a collection does.
pub fn collection_append(agent: &mut Agent, o: Object, key: Value, value: Value) {
    let key = canonicalize(key);
    let k = map_key(agent, key);
    match &mut agent.heap.object_mut(o).kind {
        ObjectKind::Map(d) => {
            d.set(k, key, value);
        }
        ObjectKind::Set(d) => {
            d.set(k, key, Value::Undefined);
        }
        _ => {}
    }
}

impl Trace for MapData {
    fn trace(&self, tracer: &mut Tracer) {
        for (k, v) in self.entries.iter().flatten() {
            tracer.value(*k);
            tracer.value(*v);
        }
    }
}

/// A position in an entry vector that survives compactions.
#[derive(Clone)]
pub(crate) struct Cursor {
    index: usize,
    epoch: Rc<Epoch>,
}

impl Cursor {
    pub(crate) fn new(data: &MapData) -> Cursor {
        Cursor {
            index: 0,
            epoch: data.epoch.clone(),
        }
    }

    /// The next live entry, or `None` at the end.
    pub(crate) fn next(&mut self, data: &MapData) -> Option<(Value, Value)> {
        loop {
            let next = match &*self.epoch.next.borrow() {
                None => None,
                Some((shift, next)) => {
                    self.index = match shift {
                        Shift::Cleared => 0,
                        Shift::Removed(r) => self.index - r.partition_point(|&x| x < self.index),
                    };
                    Some(next.clone())
                }
            };
            match next {
                Some(e) => self.epoch = e,
                None => break,
            }
        }
        while self.index < data.entries.len() {
            let e = data.entries[self.index];
            self.index += 1;
            if e.is_some() {
                return e;
            }
        }
        None
    }
}

/// The state of a Map or Set iterator; `target` is `None` once done.
pub struct CollectionIteratorData {
    target: Option<Object>,
    cursor: Cursor,
    kind: ArrayIterKind,
}

impl Trace for CollectionIteratorData {
    fn trace(&self, tracer: &mut Tracer) {
        if let Some(o) = self.target {
            tracer.object(o);
        }
    }
}

fn data(agent: &Agent, o: Object) -> Option<&MapData> {
    match &agent.heap.object(o).kind {
        ObjectKind::Map(d) | ObjectKind::Set(d) => Some(d),
        _ => None,
    }
}

fn data_mut(agent: &mut Agent, o: Object) -> Option<&mut MapData> {
    match &mut agent.heap.object_mut(o).kind {
        ObjectKind::Map(d) | ObjectKind::Set(d) => Some(d),
        _ => None,
    }
}

fn size_of_data(agent: &Agent, o: Object) -> usize {
    data(agent, o).map_or(0, MapData::len)
}

fn has_key(agent: &Agent, o: Object, k: &MapKey) -> bool {
    data(agent, o).is_some_and(|d| d.has(k))
}

/// The next entry of `o` after `cursor`.
fn next_entry(agent: &Agent, o: Object, cursor: &mut Cursor) -> Option<(Value, Value)> {
    data(agent, o).and_then(|d| cursor.next(d))
}

fn note_entry(agent: &mut Agent) {
    agent
        .heap
        .note_alloc(size_of::<Option<(Value, Value)>>() + size_of::<(MapKey, usize)>());
}

// ----- installation -----

pub(crate) fn install(agent: &mut Agent, realm: Realm) {
    install_map(agent, realm);
    install_set(agent, realm);
}

fn install_map(agent: &mut Agent, realm: Realm) {
    let object_proto = intrinsic(agent, realm, Intrinsic::ObjectPrototype);
    let iterator_proto = intrinsic(agent, realm, Intrinsic::IteratorPrototype);
    let proto = plain_object(agent, object_proto);
    set_intrinsic(agent, realm, Intrinsic::MapPrototype, proto);
    let ctor = constructor(agent, realm, "Map", map_constructor, 0, proto, None);
    set_intrinsic(agent, realm, Intrinsic::Map, ctor);
    method(agent, realm, ctor, "groupBy", map_group_by, 2);
    species_getter(agent, realm, ctor);

    let methods: &[(&str, NativeFn, u32)] = &[
        ("clear", map_clear, 0),
        ("delete", map_delete, 1),
        ("forEach", map_for_each, 1),
        ("get", map_get, 1),
        ("getOrInsert", map_get_or_insert, 2),
        ("getOrInsertComputed", map_get_or_insert_computed, 2),
        ("has", map_has, 1),
        ("keys", map_keys, 0),
        ("set", map_set, 2),
        ("values", map_values, 0),
    ];
    for &(name, f, length) in methods {
        method(agent, realm, proto, name, f, length);
    }
    let entries = method(agent, realm, proto, "entries", map_entries, 0);
    let key = PropertyKey::Symbol(agent.symbols.iterator);
    ops::define_builtin_method(agent, proto, key, entries);
    let key = agent.heap.key_from_str("size");
    accessor(agent, realm, proto, key, Some(map_size), None);
    to_string_tag(agent, proto, "Map");

    let iter_proto = plain_object(agent, iterator_proto);
    method(agent, realm, iter_proto, "next", map_iterator_next, 0);
    to_string_tag(agent, iter_proto, "Map Iterator");
    set_intrinsic(agent, realm, Intrinsic::MapIteratorPrototype, iter_proto);
}

fn install_set(agent: &mut Agent, realm: Realm) {
    let object_proto = intrinsic(agent, realm, Intrinsic::ObjectPrototype);
    let iterator_proto = intrinsic(agent, realm, Intrinsic::IteratorPrototype);
    let proto = plain_object(agent, object_proto);
    set_intrinsic(agent, realm, Intrinsic::SetPrototype, proto);
    let ctor = constructor(agent, realm, "Set", set_constructor, 0, proto, None);
    set_intrinsic(agent, realm, Intrinsic::Set, ctor);
    species_getter(agent, realm, ctor);

    let methods: &[(&str, NativeFn, u32)] = &[
        ("add", set_add, 1),
        ("clear", set_clear, 0),
        ("delete", set_delete, 1),
        ("difference", set_difference, 1),
        ("entries", set_entries, 0),
        ("forEach", set_for_each, 1),
        ("has", set_has, 1),
        ("intersection", set_intersection, 1),
        ("isDisjointFrom", set_is_disjoint_from, 1),
        ("isSubsetOf", set_is_subset_of, 1),
        ("isSupersetOf", set_is_superset_of, 1),
        ("symmetricDifference", set_symmetric_difference, 1),
        ("union", set_union, 1),
    ];
    for &(name, f, length) in methods {
        method(agent, realm, proto, name, f, length);
    }
    let values = method(agent, realm, proto, "values", set_values, 0);
    let key = agent.heap.key_from_str("keys");
    ops::define_builtin_method(agent, proto, key, values);
    let key = PropertyKey::Symbol(agent.symbols.iterator);
    ops::define_builtin_method(agent, proto, key, values);
    let key = agent.heap.key_from_str("size");
    accessor(agent, realm, proto, key, Some(set_size), None);
    to_string_tag(agent, proto, "Set");

    let iter_proto = plain_object(agent, iterator_proto);
    method(agent, realm, iter_proto, "next", set_iterator_next, 0);
    to_string_tag(agent, iter_proto, "Set Iterator");
    set_intrinsic(agent, realm, Intrinsic::SetIteratorPrototype, iter_proto);
}

// ----- shared helpers -----

/// RequireInternalSlot(this, [[MapData]]) (or `[[SetData]]` if `set`).
fn require(agent: &mut Agent, this: Value, set: bool, what: &str) -> JsResult<Object> {
    if let Value::Object(o) = this {
        let ok = match &agent.heap.object(o).kind {
            ObjectKind::Map(_) => !set,
            ObjectKind::Set(_) => set,
            _ => false,
        };
        if ok {
            return Ok(o);
        }
    }
    let kind = if set { "Set" } else { "Map" };
    agent.type_error(&format!(
        "{kind}.prototype.{what} called on an incompatible receiver"
    ))
}

/// AddEntriesFromIterable(target, iterable, adder).
pub(crate) fn add_entries_from_iterable(
    agent: &mut Agent,
    target: Object,
    iterable: Value,
    adder: Value,
) -> JsResult<()> {
    let mut record = ops::get_iterator(agent, iterable)?;
    while let Some(next) = ops::iterator_step_value(agent, &mut record)? {
        let Value::Object(entry) = next else {
            let error = agent.type_error("iterator value is not an entry object");
            return ops::iterator_close(agent, &record, error);
        };
        let k = ops::get(agent, entry, PropertyKey::Index(0));
        let k = ops::if_abrupt_close_iterator(agent, &record, k)?;
        let v = ops::get(agent, entry, PropertyKey::Index(1));
        let v = ops::if_abrupt_close_iterator(agent, &record, v)?;
        let status = ops::call(agent, adder, Value::Object(target), &[k, v]);
        ops::if_abrupt_close_iterator(agent, &record, status)?;
    }
    Ok(())
}

/// Adds every value of `iterable` to `target` with `adder` (the loop of
/// the `Set`, `WeakSet` constructors).
pub(crate) fn add_values_from_iterable(
    agent: &mut Agent,
    target: Object,
    iterable: Value,
    adder: Value,
) -> JsResult<()> {
    let mut record = ops::get_iterator(agent, iterable)?;
    while let Some(next) = ops::iterator_step_value(agent, &mut record)? {
        let status = ops::call(agent, adder, Value::Object(target), &[next]);
        ops::if_abrupt_close_iterator(agent, &record, status)?;
    }
    Ok(())
}

fn new_collection(agent: &mut Agent, data: MapData, set: bool) -> Object {
    let (which, kind) = if set {
        (Intrinsic::SetPrototype, ObjectKind::Set(Box::new(data)))
    } else {
        (Intrinsic::MapPrototype, ObjectKind::Map(Box::new(data)))
    };
    let proto = agent.intrinsic(which);
    let bytes = match &kind {
        ObjectKind::Map(d) | ObjectKind::Set(d) => d.bytes(),
        _ => 0,
    };
    let o = agent.heap.create_object(Some(proto), kind);
    agent.heap.note_alloc(bytes);
    o
}

fn create_iterator(agent: &mut Agent, target: Object, kind: ArrayIterKind, set: bool) -> Value {
    let cursor = match data(agent, target) {
        Some(d) => Cursor::new(d),
        None => Cursor {
            index: 0,
            epoch: Epoch::new(),
        },
    };
    let d = Box::new(CollectionIteratorData {
        target: Some(target),
        cursor,
        kind,
    });
    let (which, kind) = if set {
        (Intrinsic::SetIteratorPrototype, ObjectKind::SetIterator(d))
    } else {
        (Intrinsic::MapIteratorPrototype, ObjectKind::MapIterator(d))
    };
    let proto = agent.intrinsic(which);
    Value::Object(agent.heap.create_object(Some(proto), kind))
}

/// `%MapIteratorPrototype%.next` and `%SetIteratorPrototype%.next`.
fn collection_iterator_next(agent: &mut Agent, this: Value, set: bool) -> JsResult<Value> {
    let state = match this {
        Value::Object(it) => match &agent.heap.object(it).kind {
            ObjectKind::MapIterator(d) if !set => Some((it, d.target, d.cursor.clone(), d.kind)),
            ObjectKind::SetIterator(d) if set => Some((it, d.target, d.cursor.clone(), d.kind)),
            _ => None,
        },
        _ => None,
    };
    let Some((it, target, mut cursor, kind)) = state else {
        let what = if set { "Set Iterator" } else { "Map Iterator" };
        return agent.type_error(&format!("next called on something that is not a {what}"));
    };
    let entry = target.and_then(|t| next_entry(agent, t, &mut cursor));
    if let ObjectKind::MapIterator(d) | ObjectKind::SetIterator(d) =
        &mut agent.heap.object_mut(it).kind
    {
        d.cursor = cursor;
        if entry.is_none() {
            d.target = None;
        }
    }
    let Some((k, v)) = entry else {
        return Ok(Value::Object(ops::create_iter_result_object(
            agent,
            Value::Undefined,
            true,
        )));
    };
    let v = if set { k } else { v };
    let result = match kind {
        ArrayIterKind::Keys => k,
        ArrayIterKind::Values => v,
        ArrayIterKind::Entries => Value::Object(ops::create_array_from_list(agent, &[k, v])),
    };
    Ok(Value::Object(ops::create_iter_result_object(
        agent, result, false,
    )))
}

// ----- Map -----

/// `Map([iterable])` (§24.1.1.1).
fn map_constructor(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let Some(new_target) = call.new_target else {
        return agent.type_error("Constructor Map requires 'new'");
    };
    let map = ops::ordinary_create_from_constructor(
        agent,
        new_target,
        Intrinsic::MapPrototype,
        ObjectKind::Map(Box::default()),
    )?;
    let iterable = call.arg(0);
    if iterable.is_nullish() {
        return Ok(Value::Object(map));
    }
    let key = PropertyKey::String(agent.heap.names.set);
    let adder = ops::get(agent, map, key)?;
    if !ops::is_callable(agent, adder) {
        return agent.type_error("Map.prototype.set is not a function");
    }
    add_entries_from_iterable(agent, map, iterable, adder)?;
    Ok(Value::Object(map))
}

/// `Map.groupBy(items, callback)` (§24.1.2.1): GroupBy with the `zero` key
/// coercion.
fn map_group_by(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let (items, callback) = (call.arg(0), call.arg(1));
    ops::require_object_coercible(agent, items)?;
    if !ops::is_callable(agent, callback) {
        return agent.type_error("groupBy callback is not a function");
    }
    let mut keys: Vec<Value> = Vec::new();
    let mut groups: Vec<Vec<Value>> = Vec::new();
    let mut index: HashMap<MapKey, usize> = HashMap::new();
    let mut record = ops::get_iterator(agent, items)?;
    let mut k: f64 = 0.0;
    loop {
        if k >= 9007199254740991.0 {
            let error = agent.type_error("too many elements to group");
            return ops::iterator_close(agent, &record, error);
        }
        let Some(value) = ops::iterator_step_value(agent, &mut record)? else {
            break;
        };
        let key = ops::call(
            agent,
            callback,
            Value::Undefined,
            &[value, Value::Number(k)],
        );
        let key = canonicalize(ops::if_abrupt_close_iterator(agent, &record, key)?);
        let mk = map_key(agent, key);
        match index.get(&mk) {
            Some(&i) => groups[i].push(value),
            None => {
                index.insert(mk, keys.len());
                keys.push(key);
                groups.push(vec![value]);
            }
        }
        k += 1.0;
    }
    let mut data = MapData::new();
    for (key, elements) in keys.into_iter().zip(groups) {
        let array = ops::create_array_from_list(agent, &elements);
        let mk = map_key(agent, key);
        data.set(mk, key, Value::Object(array));
    }
    Ok(Value::Object(new_collection(agent, data, false)))
}

fn map_clear(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let m = require(agent, call.this, false, "clear")?;
    if let Some(d) = data_mut(agent, m) {
        d.clear();
    }
    Ok(Value::Undefined)
}

fn map_delete(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let m = require(agent, call.this, false, "delete")?;
    let k = map_key(agent, call.arg(0));
    Ok(Value::Boolean(
        data_mut(agent, m).is_some_and(|d| d.delete(&k)),
    ))
}

fn map_entries(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let m = require(agent, call.this, false, "entries")?;
    Ok(create_iterator(agent, m, ArrayIterKind::Entries, false))
}

fn map_keys(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let m = require(agent, call.this, false, "keys")?;
    Ok(create_iterator(agent, m, ArrayIterKind::Keys, false))
}

fn map_values(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let m = require(agent, call.this, false, "values")?;
    Ok(create_iterator(agent, m, ArrayIterKind::Values, false))
}

fn map_for_each(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let m = require(agent, call.this, false, "forEach")?;
    let (callback, this_arg) = (call.arg(0), call.arg(1));
    if !ops::is_callable(agent, callback) {
        return agent.type_error("Map.prototype.forEach callback is not a function");
    }
    let Some(d) = data(agent, m) else {
        return Ok(Value::Undefined);
    };
    let mut cursor = Cursor::new(d);
    while let Some((k, v)) = next_entry(agent, m, &mut cursor) {
        ops::call(agent, callback, this_arg, &[v, k, Value::Object(m)])?;
    }
    Ok(Value::Undefined)
}

fn map_get(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let m = require(agent, call.this, false, "get")?;
    let k = map_key(agent, call.arg(0));
    Ok(data(agent, m)
        .and_then(|d| d.get(&k))
        .unwrap_or(Value::Undefined))
}

fn map_has(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let m = require(agent, call.this, false, "has")?;
    let k = map_key(agent, call.arg(0));
    Ok(Value::Boolean(has_key(agent, m, &k)))
}

fn map_set(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let m = require(agent, call.this, false, "set")?;
    let key = canonicalize(call.arg(0));
    let k = map_key(agent, key);
    let value = call.arg(1);
    if data_mut(agent, m).is_some_and(|d| d.set(k, key, value)) {
        note_entry(agent);
    }
    Ok(Value::Object(m))
}

/// `Map.prototype.getOrInsert(key, value)`.
fn map_get_or_insert(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let m = require(agent, call.this, false, "getOrInsert")?;
    let key = canonicalize(call.arg(0));
    let k = map_key(agent, key);
    if let Some(v) = data(agent, m).and_then(|d| d.get(&k)) {
        return Ok(v);
    }
    let value = call.arg(1);
    if data_mut(agent, m).is_some_and(|d| d.set(k, key, value)) {
        note_entry(agent);
    }
    Ok(value)
}

/// `Map.prototype.getOrInsertComputed(key, callbackfn)`.
fn map_get_or_insert_computed(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let m = require(agent, call.this, false, "getOrInsertComputed")?;
    let callback = call.arg(1);
    if !ops::is_callable(agent, callback) {
        return agent.type_error("getOrInsertComputed callback is not a function");
    }
    let key = canonicalize(call.arg(0));
    let k = map_key(agent, key);
    if let Some(v) = data(agent, m).and_then(|d| d.get(&k)) {
        return Ok(v);
    }
    let value = ops::call(agent, callback, Value::Undefined, &[key])?;
    // The callback may have changed the map: overwrite or append.
    if data_mut(agent, m).is_some_and(|d| d.set(k, key, value)) {
        note_entry(agent);
    }
    Ok(value)
}

fn map_size(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let m = require(agent, call.this, false, "size")?;
    Ok(Value::Number(size_of_data(agent, m) as f64))
}

fn map_iterator_next(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    collection_iterator_next(agent, call.this, false)
}

// ----- Set -----

/// `Set([iterable])` (§24.2.2.1).
fn set_constructor(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let Some(new_target) = call.new_target else {
        return agent.type_error("Constructor Set requires 'new'");
    };
    let set = ops::ordinary_create_from_constructor(
        agent,
        new_target,
        Intrinsic::SetPrototype,
        ObjectKind::Set(Box::default()),
    )?;
    let iterable = call.arg(0);
    if iterable.is_nullish() {
        return Ok(Value::Object(set));
    }
    let key = agent.heap.key_from_str("add");
    let adder = ops::get(agent, set, key)?;
    if !ops::is_callable(agent, adder) {
        return agent.type_error("Set.prototype.add is not a function");
    }
    add_values_from_iterable(agent, set, iterable, adder)?;
    Ok(Value::Object(set))
}

fn set_add(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let s = require(agent, call.this, true, "add")?;
    let value = canonicalize(call.arg(0));
    let k = map_key(agent, value);
    if data_mut(agent, s).is_some_and(|d| d.set(k, value, Value::Undefined)) {
        note_entry(agent);
    }
    Ok(Value::Object(s))
}

fn set_clear(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let s = require(agent, call.this, true, "clear")?;
    if let Some(d) = data_mut(agent, s) {
        d.clear();
    }
    Ok(Value::Undefined)
}

fn set_delete(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let s = require(agent, call.this, true, "delete")?;
    let k = map_key(agent, call.arg(0));
    Ok(Value::Boolean(
        data_mut(agent, s).is_some_and(|d| d.delete(&k)),
    ))
}

fn set_entries(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let s = require(agent, call.this, true, "entries")?;
    Ok(create_iterator(agent, s, ArrayIterKind::Entries, true))
}

fn set_values(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let s = require(agent, call.this, true, "values")?;
    Ok(create_iterator(agent, s, ArrayIterKind::Values, true))
}

fn set_for_each(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let s = require(agent, call.this, true, "forEach")?;
    let (callback, this_arg) = (call.arg(0), call.arg(1));
    if !ops::is_callable(agent, callback) {
        return agent.type_error("Set.prototype.forEach callback is not a function");
    }
    let Some(d) = data(agent, s) else {
        return Ok(Value::Undefined);
    };
    let mut cursor = Cursor::new(d);
    while let Some((k, _)) = next_entry(agent, s, &mut cursor) {
        ops::call(agent, callback, this_arg, &[k, k, Value::Object(s)])?;
    }
    Ok(Value::Undefined)
}

fn set_has(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let s = require(agent, call.this, true, "has")?;
    let k = map_key(agent, call.arg(0));
    Ok(Value::Boolean(has_key(agent, s, &k)))
}

fn set_size(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let s = require(agent, call.this, true, "size")?;
    Ok(Value::Number(size_of_data(agent, s) as f64))
}

fn set_iterator_next(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    collection_iterator_next(agent, call.this, true)
}

// ----- set methods -----

/// Set Record.
struct SetRecord {
    object: Object,
    size: f64,
    has: Value,
    keys: Value,
}

/// GetSetRecord(obj).
fn get_set_record(agent: &mut Agent, v: Value) -> JsResult<SetRecord> {
    let Value::Object(object) = v else {
        return agent.type_error("set method argument is not an object");
    };
    let key = agent.heap.key_from_str("size");
    let raw_size = ops::get(agent, object, key)?;
    let num_size = ops::to_number(agent, raw_size)?;
    if num_size.is_nan() {
        return agent.type_error("the size of the set-like object is not a number");
    }
    let size = ops::f64_to_integer_or_infinity(num_size);
    if size < 0.0 {
        return agent.range_error("the size of the set-like object is negative");
    }
    let key = PropertyKey::String(agent.heap.names.has);
    let has = ops::get(agent, object, key)?;
    if !ops::is_callable(agent, has) {
        return agent.type_error("the 'has' of the set-like object is not a function");
    }
    let key = agent.heap.key_from_str("keys");
    let keys = ops::get(agent, object, key)?;
    if !ops::is_callable(agent, keys) {
        return agent.type_error("the 'keys' of the set-like object is not a function");
    }
    Ok(SetRecord {
        object,
        size,
        has,
        keys,
    })
}

fn other_has(agent: &mut Agent, other: &SetRecord, e: Value) -> JsResult<bool> {
    let r = ops::call(agent, other.has, Value::Object(other.object), &[e])?;
    Ok(ops::to_boolean(agent, r))
}

fn other_keys(agent: &mut Agent, other: &SetRecord) -> JsResult<IteratorRecord> {
    ops::get_iterator_from_method(agent, Value::Object(other.object), other.keys)
}

fn copy_of(agent: &Agent, s: Object) -> MapData {
    data(agent, s).map_or_else(MapData::new, MapData::copy)
}

/// `Set.prototype.union(other)`.
fn set_union(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let s = require(agent, call.this, true, "union")?;
    let other = get_set_record(agent, call.arg(0))?;
    let mut keys = other_keys(agent, &other)?;
    let mut result = copy_of(agent, s);
    while let Some(next) = ops::iterator_step_value(agent, &mut keys)? {
        let next = canonicalize(next);
        let k = map_key(agent, next);
        result.set(k, next, Value::Undefined);
    }
    Ok(Value::Object(new_collection(agent, result, true)))
}

/// `Set.prototype.intersection(other)`.
fn set_intersection(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let s = require(agent, call.this, true, "intersection")?;
    let other = get_set_record(agent, call.arg(0))?;
    let mut result = MapData::new();
    if size_of_data(agent, s) as f64 <= other.size {
        let mut cursor = match data(agent, s) {
            Some(d) => Cursor::new(d),
            None => return Ok(Value::Object(new_collection(agent, result, true))),
        };
        while let Some((e, _)) = next_entry(agent, s, &mut cursor) {
            if other_has(agent, &other, e)? {
                let k = map_key(agent, e);
                result.set(k, e, Value::Undefined);
            }
        }
    } else {
        let mut keys = other_keys(agent, &other)?;
        while let Some(next) = ops::iterator_step_value(agent, &mut keys)? {
            let next = canonicalize(next);
            let k = map_key(agent, next);
            if has_key(agent, s, &k) {
                result.set(k, next, Value::Undefined);
            }
        }
    }
    Ok(Value::Object(new_collection(agent, result, true)))
}

/// `Set.prototype.difference(other)`.
fn set_difference(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let s = require(agent, call.this, true, "difference")?;
    let other = get_set_record(agent, call.arg(0))?;
    let mut result = copy_of(agent, s);
    if size_of_data(agent, s) as f64 <= other.size {
        for (e, _) in result.live() {
            if other_has(agent, &other, e)? {
                let k = map_key(agent, e);
                result.delete(&k);
            }
        }
    } else {
        let mut keys = other_keys(agent, &other)?;
        while let Some(next) = ops::iterator_step_value(agent, &mut keys)? {
            let next = canonicalize(next);
            let k = map_key(agent, next);
            result.delete(&k);
        }
    }
    Ok(Value::Object(new_collection(agent, result, true)))
}

/// `Set.prototype.symmetricDifference(other)`.
fn set_symmetric_difference(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let s = require(agent, call.this, true, "symmetricDifference")?;
    let other = get_set_record(agent, call.arg(0))?;
    let mut keys = other_keys(agent, &other)?;
    let mut result = copy_of(agent, s);
    while let Some(next) = ops::iterator_step_value(agent, &mut keys)? {
        let next = canonicalize(next);
        let k = map_key(agent, next);
        let already = result.has(&k);
        if has_key(agent, s, &k) {
            if already {
                result.delete(&k);
            }
        } else if !already {
            result.set(k, next, Value::Undefined);
        }
    }
    Ok(Value::Object(new_collection(agent, result, true)))
}

/// `Set.prototype.isSubsetOf(other)`.
fn set_is_subset_of(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let s = require(agent, call.this, true, "isSubsetOf")?;
    let other = get_set_record(agent, call.arg(0))?;
    if size_of_data(agent, s) as f64 > other.size {
        return Ok(Value::Boolean(false));
    }
    let Some(d) = data(agent, s) else {
        return Ok(Value::Boolean(true));
    };
    let mut cursor = Cursor::new(d);
    while let Some((e, _)) = next_entry(agent, s, &mut cursor) {
        if !other_has(agent, &other, e)? {
            return Ok(Value::Boolean(false));
        }
    }
    Ok(Value::Boolean(true))
}

/// `Set.prototype.isSupersetOf(other)`.
fn set_is_superset_of(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let s = require(agent, call.this, true, "isSupersetOf")?;
    let other = get_set_record(agent, call.arg(0))?;
    if (size_of_data(agent, s) as f64) < other.size {
        return Ok(Value::Boolean(false));
    }
    let mut keys = other_keys(agent, &other)?;
    while let Some(next) = ops::iterator_step_value(agent, &mut keys)? {
        let k = map_key(agent, next);
        if !has_key(agent, s, &k) {
            ops::iterator_close(agent, &keys, Ok(()))?;
            return Ok(Value::Boolean(false));
        }
    }
    Ok(Value::Boolean(true))
}

/// `Set.prototype.isDisjointFrom(other)`.
fn set_is_disjoint_from(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let s = require(agent, call.this, true, "isDisjointFrom")?;
    let other = get_set_record(agent, call.arg(0))?;
    if size_of_data(agent, s) as f64 <= other.size {
        let Some(d) = data(agent, s) else {
            return Ok(Value::Boolean(true));
        };
        let mut cursor = Cursor::new(d);
        while let Some((e, _)) = next_entry(agent, s, &mut cursor) {
            if other_has(agent, &other, e)? {
                return Ok(Value::Boolean(false));
            }
        }
    } else {
        let mut keys = other_keys(agent, &other)?;
        while let Some(next) = ops::iterator_step_value(agent, &mut keys)? {
            let k = map_key(agent, next);
            if has_key(agent, s, &k) {
                ops::iterator_close(agent, &keys, Ok(()))?;
                return Ok(Value::Boolean(false));
            }
        }
    }
    Ok(Value::Boolean(true))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn num(n: f64) -> (MapKey, Value) {
        let k = MapKey::Number(if n == 0.0 { 0 } else { n.to_bits() });
        (k, Value::Number(n))
    }

    #[test]
    fn cursors_survive_compaction_and_clear() {
        let mut d = MapData::new();
        for i in 0..40 {
            let (k, v) = num(f64::from(i));
            d.set(k, v, Value::Undefined);
        }
        let mut c = Cursor::new(&d);
        assert_eq!(c.next(&d).map(|e| e.0), Some(Value::Number(0.0)));
        assert_eq!(c.next(&d).map(|e| e.0), Some(Value::Number(1.0)));
        // Delete 0..30: compacts at least once.
        for i in 0..30 {
            assert!(d.delete(&num(f64::from(i)).0));
        }
        assert!(d.entries.len() < 40);
        assert_eq!(c.next(&d).map(|e| e.0), Some(Value::Number(30.0)));
        d.clear();
        let (k, v) = num(100.0);
        d.set(k, v, Value::Undefined);
        assert_eq!(c.next(&d).map(|e| e.0), Some(Value::Number(100.0)));
        assert_eq!(c.next(&d), None);
    }

    #[test]
    fn hosts_append_and_read_entries() {
        let mut agent = Agent::new();
        let realm = agent.create_realm();
        agent.set_realm_global_object(realm, None, None);
        agent
            .set_default_global_bindings(realm)
            .expect("global bindings");
        let m = crate::evaluate_script_in(&mut agent, realm, "new Map([[1, 2]])").expect("map");
        let Value::Object(m) = m else {
            panic!("a map");
        };
        collection_append(&mut agent, m, Value::Number(-0.0), Value::Boolean(true));
        collection_append(&mut agent, m, Value::Number(1.0), Value::Null);
        assert_eq!(
            collection_entries(&agent, m),
            Some(vec![
                (Value::Number(1.0), Value::Null),
                (Value::Number(0.0), Value::Boolean(true)),
            ])
        );
    }

    #[test]
    fn zero_keys_are_one_key() {
        assert_eq!(canonicalize(Value::Number(-0.0)), Value::Number(0.0));
        let mut d = MapData::new();
        d.set(num(0.0).0, Value::Number(0.0), Value::Boolean(true));
        assert!(d.has(&num(0.0).0));
        assert_eq!(d.len(), 1);
    }
}
