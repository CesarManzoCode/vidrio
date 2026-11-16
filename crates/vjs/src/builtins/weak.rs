//! `WeakMap` and `WeakSet` (ECMA-262 §24.3, §24.4) with
//! `getOrInsert`/`getOrInsertComputed` (feature `upsert`), `WeakRef`
//! (§26.1) and `FinalizationRegistry` (§26.2). The storage and its
//! integration with the collector are in [`crate::weak`].

use crate::agent::Agent;
use crate::builtins::collections::{add_entries_from_iterable, add_values_from_iterable};
use crate::builtins::{constructor, intrinsic, method, plain_object, set_intrinsic, to_string_tag};
use crate::completion::JsResult;
use crate::function::{NativeCall, NativeFn};
use crate::gc::{Object, Realm};
use crate::object::ObjectKind;
use crate::ops;
use crate::realm::Intrinsic;
use crate::value::{PropertyKey, Value, WeakKey};
use crate::weak;

pub(crate) fn install(agent: &mut Agent, realm: Realm) {
    let object_proto = intrinsic(agent, realm, Intrinsic::ObjectPrototype);

    let proto = plain_object(agent, object_proto);
    set_intrinsic(agent, realm, Intrinsic::WeakMapPrototype, proto);
    let ctor = constructor(
        agent,
        realm,
        "WeakMap",
        weak_map_constructor,
        0,
        proto,
        None,
    );
    set_intrinsic(agent, realm, Intrinsic::WeakMap, ctor);
    let methods: &[(&str, NativeFn, u32)] = &[
        ("delete", weak_map_delete, 1),
        ("get", weak_map_get, 1),
        ("getOrInsert", weak_map_get_or_insert, 2),
        ("getOrInsertComputed", weak_map_get_or_insert_computed, 2),
        ("has", weak_map_has, 1),
        ("set", weak_map_set, 2),
    ];
    for &(name, f, length) in methods {
        method(agent, realm, proto, name, f, length);
    }
    to_string_tag(agent, proto, "WeakMap");

    let proto = plain_object(agent, object_proto);
    set_intrinsic(agent, realm, Intrinsic::WeakSetPrototype, proto);
    let ctor = constructor(
        agent,
        realm,
        "WeakSet",
        weak_set_constructor,
        0,
        proto,
        None,
    );
    set_intrinsic(agent, realm, Intrinsic::WeakSet, ctor);
    let methods: &[(&str, NativeFn, u32)] = &[
        ("add", weak_set_add, 1),
        ("delete", weak_set_delete, 1),
        ("has", weak_set_has, 1),
    ];
    for &(name, f, length) in methods {
        method(agent, realm, proto, name, f, length);
    }
    to_string_tag(agent, proto, "WeakSet");

    let proto = plain_object(agent, object_proto);
    set_intrinsic(agent, realm, Intrinsic::WeakRefPrototype, proto);
    let ctor = constructor(
        agent,
        realm,
        "WeakRef",
        weak_ref_constructor,
        1,
        proto,
        None,
    );
    set_intrinsic(agent, realm, Intrinsic::WeakRef, ctor);
    method(agent, realm, proto, "deref", weak_ref_deref, 0);
    to_string_tag(agent, proto, "WeakRef");

    let proto = plain_object(agent, object_proto);
    set_intrinsic(
        agent,
        realm,
        Intrinsic::FinalizationRegistryPrototype,
        proto,
    );
    let ctor = constructor(
        agent,
        realm,
        "FinalizationRegistry",
        registry_constructor,
        1,
        proto,
        None,
    );
    set_intrinsic(agent, realm, Intrinsic::FinalizationRegistry, ctor);
    method(agent, realm, proto, "register", registry_register, 2);
    method(agent, realm, proto, "unregister", registry_unregister, 1);
    to_string_tag(agent, proto, "FinalizationRegistry");
}

/// RequireInternalSlot for the weak collections.
fn require(agent: &mut Agent, this: Value, set: bool, what: &str) -> JsResult<Object> {
    if let Value::Object(o) = this {
        let ok = match &agent.heap.object(o).kind {
            ObjectKind::WeakMap(_) => !set,
            ObjectKind::WeakSet(_) => set,
            _ => false,
        };
        if ok {
            return Ok(o);
        }
    }
    let kind = if set { "WeakSet" } else { "WeakMap" };
    agent.type_error(&format!(
        "{kind}.prototype.{what} called on an incompatible receiver"
    ))
}

fn new_target(agent: &mut Agent, call: &NativeCall<'_>, name: &str) -> JsResult<Object> {
    match call.new_target {
        Some(nt) => Ok(nt),
        None => agent.type_error(&format!("Constructor {name} requires 'new'")),
    }
}

// ----- WeakMap -----

/// `WeakMap([iterable])` (§24.3.1.1).
fn weak_map_constructor(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let nt = new_target(agent, &call, "WeakMap")?;
    let proto = ops::get_prototype_from_constructor(agent, nt, Intrinsic::WeakMapPrototype)?;
    let map = weak::weak_map_create(agent, Some(proto));
    let iterable = call.arg(0);
    if iterable.is_nullish() {
        return Ok(Value::Object(map));
    }
    let key = PropertyKey::String(agent.heap.names.set);
    let adder = ops::get(agent, map, key)?;
    if !ops::is_callable(agent, adder) {
        return agent.type_error("WeakMap.prototype.set is not a function");
    }
    add_entries_from_iterable(agent, map, iterable, adder)?;
    Ok(Value::Object(map))
}

fn weak_map_delete(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let m = require(agent, call.this, false, "delete")?;
    Ok(Value::Boolean(
        match weak::can_be_held_weakly(agent, call.arg(0)) {
            Some(k) => weak::weak_map_delete(agent, m, k),
            None => false,
        },
    ))
}

fn weak_map_get(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let m = require(agent, call.this, false, "get")?;
    Ok(match weak::can_be_held_weakly(agent, call.arg(0)) {
        Some(k) => weak::weak_map_get(agent, m, k),
        None => Value::Undefined,
    })
}

fn weak_map_has(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let m = require(agent, call.this, false, "has")?;
    Ok(Value::Boolean(
        match weak::can_be_held_weakly(agent, call.arg(0)) {
            Some(k) => weak::weak_map_has(agent, m, k),
            None => false,
        },
    ))
}

fn weak_key(agent: &mut Agent, v: Value) -> JsResult<WeakKey> {
    match weak::can_be_held_weakly(agent, v) {
        Some(k) => Ok(k),
        None => agent.type_error("invalid value used as a weak key"),
    }
}

fn weak_map_set(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let m = require(agent, call.this, false, "set")?;
    let k = weak_key(agent, call.arg(0))?;
    weak::weak_map_set(agent, m, k, call.arg(1));
    Ok(Value::Object(m))
}

/// `WeakMap.prototype.getOrInsert(key, value)`.
fn weak_map_get_or_insert(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let m = require(agent, call.this, false, "getOrInsert")?;
    let k = weak_key(agent, call.arg(0))?;
    if weak::weak_map_has(agent, m, k) {
        return Ok(weak::weak_map_get(agent, m, k));
    }
    let value = call.arg(1);
    weak::weak_map_set(agent, m, k, value);
    Ok(value)
}

/// `WeakMap.prototype.getOrInsertComputed(key, callbackfn)`.
fn weak_map_get_or_insert_computed(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let m = require(agent, call.this, false, "getOrInsertComputed")?;
    let k = weak_key(agent, call.arg(0))?;
    let callback = call.arg(1);
    if !ops::is_callable(agent, callback) {
        return agent.type_error("getOrInsertComputed callback is not a function");
    }
    if weak::weak_map_has(agent, m, k) {
        return Ok(weak::weak_map_get(agent, m, k));
    }
    let value = ops::call(agent, callback, Value::Undefined, &[k.to_value()])?;
    weak::weak_map_set(agent, m, k, value);
    Ok(value)
}

// ----- WeakSet -----

/// `WeakSet([iterable])` (§24.4.1.1).
fn weak_set_constructor(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let nt = new_target(agent, &call, "WeakSet")?;
    let proto = ops::get_prototype_from_constructor(agent, nt, Intrinsic::WeakSetPrototype)?;
    let set = weak::weak_set_create(agent, Some(proto));
    let iterable = call.arg(0);
    if iterable.is_nullish() {
        return Ok(Value::Object(set));
    }
    let key = agent.heap.key_from_str("add");
    let adder = ops::get(agent, set, key)?;
    if !ops::is_callable(agent, adder) {
        return agent.type_error("WeakSet.prototype.add is not a function");
    }
    add_values_from_iterable(agent, set, iterable, adder)?;
    Ok(Value::Object(set))
}

fn weak_set_add(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let s = require(agent, call.this, true, "add")?;
    let k = weak_key(agent, call.arg(0))?;
    weak::weak_set_add(agent, s, k);
    Ok(Value::Object(s))
}

fn weak_set_delete(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let s = require(agent, call.this, true, "delete")?;
    Ok(Value::Boolean(
        match weak::can_be_held_weakly(agent, call.arg(0)) {
            Some(k) => weak::weak_set_delete(agent, s, k),
            None => false,
        },
    ))
}

fn weak_set_has(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let s = require(agent, call.this, true, "has")?;
    Ok(Value::Boolean(
        match weak::can_be_held_weakly(agent, call.arg(0)) {
            Some(k) => weak::weak_set_has(agent, s, k),
            None => false,
        },
    ))
}

// ----- WeakRef -----

/// `WeakRef(target)` (§26.1.1.1).
fn weak_ref_constructor(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let nt = new_target(agent, &call, "WeakRef")?;
    let Some(target) = weak::can_be_held_weakly(agent, call.arg(0)) else {
        return agent.type_error("WeakRef target cannot be held weakly");
    };
    let proto = ops::get_prototype_from_constructor(agent, nt, Intrinsic::WeakRefPrototype)?;
    Ok(Value::Object(weak::weak_ref_create(
        agent,
        target,
        Some(proto),
    )))
}

fn weak_ref_deref(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    match call.this {
        Value::Object(o) if matches!(agent.heap.object(o).kind, ObjectKind::WeakRef(_)) => {
            Ok(weak::weak_ref_deref(agent, o))
        }
        _ => agent.type_error("WeakRef.prototype.deref called on an incompatible receiver"),
    }
}

// ----- FinalizationRegistry -----

/// `FinalizationRegistry(cleanupCallback)` (§26.2.1.1).
fn registry_constructor(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let nt = new_target(agent, &call, "FinalizationRegistry")?;
    let cleanup = call.arg(0);
    if !ops::is_callable(agent, cleanup) {
        return agent.type_error("FinalizationRegistry cleanup callback is not a function");
    }
    let proto =
        ops::get_prototype_from_constructor(agent, nt, Intrinsic::FinalizationRegistryPrototype)?;
    Ok(Value::Object(weak::finalization_registry_create(
        agent,
        cleanup,
        Some(proto),
    )))
}

fn require_registry(agent: &mut Agent, this: Value, what: &str) -> JsResult<Object> {
    match this {
        Value::Object(o)
            if matches!(
                agent.heap.object(o).kind,
                ObjectKind::FinalizationRegistry(_)
            ) =>
        {
            Ok(o)
        }
        _ => agent.type_error(&format!(
            "FinalizationRegistry.prototype.{what} called on an incompatible receiver"
        )),
    }
}

/// `FinalizationRegistry.prototype.register(target, heldValue[, unregisterToken])`.
fn registry_register(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let registry = require_registry(agent, call.this, "register")?;
    let (target, held, token) = (call.arg(0), call.arg(1), call.arg(2));
    let Some(target_key) = weak::can_be_held_weakly(agent, target) else {
        return agent.type_error("FinalizationRegistry target cannot be held weakly");
    };
    if ops::same_value(agent, target, held) {
        return agent.type_error("the target and the held value are the same");
    }
    let token_key = match weak::can_be_held_weakly(agent, token) {
        Some(k) => Some(k),
        None if token.is_undefined() => None,
        None => return agent.type_error("unregister token cannot be held weakly"),
    };
    weak::finalization_registry_register(agent, registry, target_key, held, token_key);
    Ok(Value::Undefined)
}

/// `FinalizationRegistry.prototype.unregister(unregisterToken)`.
fn registry_unregister(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let registry = require_registry(agent, call.this, "unregister")?;
    let Some(token) = weak::can_be_held_weakly(agent, call.arg(0)) else {
        return agent.type_error("unregister token cannot be held weakly");
    };
    Ok(Value::Boolean(weak::finalization_registry_unregister(
        agent, registry, token,
    )))
}
