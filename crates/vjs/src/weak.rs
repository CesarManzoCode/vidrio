//! The heap side of `WeakRef`, `WeakMap`, `WeakSet` and
//! `FinalizationRegistry` (ECMA-262 §26.1, §26.2, §24.3, §24.4): creating
//! the objects and the operations on their slots. The constructors and
//! prototype methods are built on top of these.

use std::collections::VecDeque;

use crate::agent::Agent;
use crate::completion::JsResult;
use crate::gc::Object;
use crate::object::{EphemeronTable, ObjectKind, RegistryCell, RegistryData};
use crate::ops;
use crate::realm::Intrinsic;
use crate::value::{Value, WeakKey};

/// CanBeHeldWeakly(v).
pub fn can_be_held_weakly(agent: &Agent, v: Value) -> Option<WeakKey> {
    match v {
        Value::Object(o) => Some(WeakKey::Object(o)),
        Value::Symbol(s) if !agent.heap.symbol(s).registered => Some(WeakKey::Symbol(s)),
        _ => None,
    }
}

fn proto_or(agent: &Agent, proto: Option<Object>, default: Intrinsic) -> Option<Object> {
    proto.or_else(|| {
        agent
            .heap
            .realm(agent.current_realm())
            .intrinsics
            .try_get(default)
    })
}

/// A `WeakRef` to `target`; the target is kept alive until the end of the
/// current job (AddToKeptObjects).
pub fn weak_ref_create(agent: &mut Agent, target: WeakKey, proto: Option<Object>) -> Object {
    let proto = proto_or(agent, proto, Intrinsic::WeakRefPrototype);
    agent.add_to_kept_objects(target);
    agent
        .heap
        .create_object(proto, ObjectKind::WeakRef(Some(target)))
}

/// WeakRefDeref: the target, or `undefined` once collected.
pub fn weak_ref_deref(agent: &mut Agent, weak_ref: Object) -> Value {
    let target = match &agent.heap.object(weak_ref).kind {
        ObjectKind::WeakRef(t) => *t,
        _ => None,
    };
    match target {
        Some(key) => {
            agent.add_to_kept_objects(key);
            key.to_value()
        }
        None => Value::Undefined,
    }
}

pub fn weak_map_create(agent: &mut Agent, proto: Option<Object>) -> Object {
    let proto = proto_or(agent, proto, Intrinsic::WeakMapPrototype);
    agent
        .heap
        .create_object(proto, ObjectKind::WeakMap(EphemeronTable::new()))
}

pub fn weak_set_create(agent: &mut Agent, proto: Option<Object>) -> Object {
    let proto = proto_or(agent, proto, Intrinsic::WeakSetPrototype);
    agent
        .heap
        .create_object(proto, ObjectKind::WeakSet(EphemeronTable::new()))
}

fn table(agent: &Agent, o: Object) -> Option<&EphemeronTable> {
    agent.heap.object(o).kind.ephemerons()
}

fn table_mut(agent: &mut Agent, o: Object) -> Option<&mut EphemeronTable> {
    agent.heap.object_mut(o).kind.ephemerons_mut()
}

/// `WeakMap.prototype.get` on the slots.
pub fn weak_map_get(agent: &Agent, map: Object, key: WeakKey) -> Value {
    table(agent, map)
        .and_then(|t| t.get(&key).copied())
        .unwrap_or(Value::Undefined)
}

pub fn weak_map_has(agent: &Agent, map: Object, key: WeakKey) -> bool {
    table(agent, map).is_some_and(|t| t.contains_key(&key))
}

pub fn weak_map_set(agent: &mut Agent, map: Object, key: WeakKey, value: Value) {
    let added = match table_mut(agent, map) {
        Some(t) => t.insert(key, value).is_none(),
        None => false,
    };
    if added {
        agent
            .heap
            .note_alloc(2 * std::mem::size_of::<(WeakKey, Value)>());
    }
}

pub fn weak_map_delete(agent: &mut Agent, map: Object, key: WeakKey) -> bool {
    table_mut(agent, map).is_some_and(|t| t.remove(&key).is_some())
}

pub fn weak_set_add(agent: &mut Agent, set: Object, key: WeakKey) {
    weak_map_set(agent, set, key, Value::Undefined);
}

pub fn weak_set_has(agent: &Agent, set: Object, key: WeakKey) -> bool {
    weak_map_has(agent, set, key)
}

pub fn weak_set_delete(agent: &mut Agent, set: Object, key: WeakKey) -> bool {
    weak_map_delete(agent, set, key)
}

/// A `FinalizationRegistry` whose cleanup callback is `cleanup` (callable)
/// and whose cleanup jobs run in the current realm.
pub fn finalization_registry_create(
    agent: &mut Agent,
    cleanup: Value,
    proto: Option<Object>,
) -> Object {
    let proto = proto_or(agent, proto, Intrinsic::FinalizationRegistryPrototype);
    let realm = agent.current_realm();
    agent.heap.create_object(
        proto,
        ObjectKind::FinalizationRegistry(Box::new(RegistryData {
            cleanup,
            realm,
            cells: Vec::new(),
            pending: VecDeque::new(),
            job_queued: false,
        })),
    )
}

/// `FinalizationRegistry.prototype.register` on the slots (the caller
/// checks that target and held differ).
pub fn finalization_registry_register(
    agent: &mut Agent,
    registry: Object,
    target: WeakKey,
    held: Value,
    token: Option<WeakKey>,
) {
    if let ObjectKind::FinalizationRegistry(data) = &mut agent.heap.object_mut(registry).kind {
        data.cells.push(RegistryCell {
            target,
            held,
            token,
        });
    }
    agent.heap.note_alloc(std::mem::size_of::<RegistryCell>());
}

/// `FinalizationRegistry.prototype.unregister` on the slots: whether a
/// cell was removed.
pub fn finalization_registry_unregister(
    agent: &mut Agent,
    registry: Object,
    token: WeakKey,
) -> bool {
    match &mut agent.heap.object_mut(registry).kind {
        ObjectKind::FinalizationRegistry(data) => {
            let before = data.cells.len();
            data.cells.retain(|c| c.token != Some(token));
            data.cells.len() != before
        }
        _ => false,
    }
}

/// CleanupFinalizationRegistry(registry[, callback]): calls the callback
/// with the held value of every collected target. Stops at the first
/// abrupt completion, leaving the rest queued.
pub fn cleanup_finalization_registry(
    agent: &mut Agent,
    registry: Object,
    callback: Option<Value>,
) -> JsResult<()> {
    let cleanup = match &mut agent.heap.object_mut(registry).kind {
        ObjectKind::FinalizationRegistry(data) => {
            data.job_queued = false;
            data.cleanup
        }
        _ => return Ok(()),
    };
    let callback = callback.unwrap_or(cleanup);
    loop {
        let held = match &mut agent.heap.object_mut(registry).kind {
            ObjectKind::FinalizationRegistry(data) => data.pending.pop_front(),
            _ => None,
        };
        let Some(held) = held else {
            return Ok(());
        };
        ops::call(agent, callback, Value::Undefined, &[held])?;
    }
}
