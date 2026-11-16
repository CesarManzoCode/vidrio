//! `Reflect` (ECMA-262 §28.1) and the `Proxy` constructor with
//! `Proxy.revocable` (§28.2). The proxy internal methods and their
//! invariants are in `proxy.rs`.

use crate::agent::Agent;
use crate::builtins::{intrinsic, method, plain_object, set_intrinsic, to_string_tag};
use crate::completion::JsResult;
use crate::function::{
    NativeCall, create_builtin_function, native_slots, native_slots_mut, set_native_constructor,
};
use crate::gc::{Object, Realm};
use crate::internal;
use crate::ops::{self, ListElementTypes};
use crate::proxy::{proxy_create, proxy_revoke};
use crate::realm::Intrinsic;
use crate::value::{PropertyKey, Value};

pub(crate) fn install(agent: &mut Agent, realm: Realm) {
    let object_proto = intrinsic(agent, realm, Intrinsic::ObjectPrototype);
    let reflect = plain_object(agent, object_proto);
    let methods: [(&str, crate::function::NativeFn, u32); 13] = [
        ("apply", apply, 3),
        ("construct", construct, 2),
        ("defineProperty", define_property, 3),
        ("deleteProperty", delete_property, 2),
        ("get", get, 2),
        ("getOwnPropertyDescriptor", get_own_property_descriptor, 2),
        ("getPrototypeOf", get_prototype_of, 1),
        ("has", has, 2),
        ("isExtensible", is_extensible, 1),
        ("ownKeys", own_keys, 1),
        ("preventExtensions", prevent_extensions, 1),
        ("set", set, 3),
        ("setPrototypeOf", set_prototype_of, 2),
    ];
    for (name, f, length) in methods {
        method(agent, realm, reflect, name, f, length);
    }
    to_string_tag(agent, reflect, "Reflect");
    set_intrinsic(agent, realm, Intrinsic::Reflect, reflect);

    // Proxy has no `prototype` property.
    let key = agent.heap.key_from_str("Proxy");
    let proxy = create_builtin_function(agent, proxy_constructor, 2, key, Some(realm), None, None);
    set_native_constructor(agent, proxy);
    method(agent, realm, proxy, "revocable", revocable, 2);
    set_intrinsic(agent, realm, Intrinsic::Proxy, proxy);
}

fn target(agent: &mut Agent, v: Value, method: &str) -> JsResult<Object> {
    match v {
        Value::Object(o) => Ok(o),
        _ => agent.type_error(&format!("Reflect.{method} target is not an object")),
    }
}

fn apply(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let f = call.arg(0);
    if !ops::is_callable(agent, f) {
        return agent.type_error("Reflect.apply target is not a function");
    }
    let args = ops::create_list_from_array_like(agent, call.arg(2), ListElementTypes::All)?;
    ops::call(agent, f, call.arg(1), &args)
}

fn construct(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let Value::Object(f) = call.arg(0) else {
        return agent.type_error("Reflect.construct target is not a constructor");
    };
    if !agent.heap.object(f).is_constructor() {
        return agent.type_error("Reflect.construct target is not a constructor");
    }
    let new_target = match call.args.get(2) {
        None => f,
        Some(&Value::Object(nt)) if agent.heap.object(nt).is_constructor() => nt,
        Some(_) => return agent.type_error("Reflect.construct newTarget is not a constructor"),
    };
    let args = ops::create_list_from_array_like(agent, call.arg(1), ListElementTypes::All)?;
    Ok(Value::Object(ops::construct(
        agent,
        f,
        &args,
        Some(new_target),
    )?))
}

fn define_property(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let o = target(agent, call.arg(0), "defineProperty")?;
    let key = ops::to_property_key(agent, call.arg(1))?;
    let desc = ops::to_property_descriptor(agent, call.arg(2))?;
    Ok(Value::Boolean(internal::define_own_property(
        agent, o, key, desc,
    )?))
}

fn delete_property(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let o = target(agent, call.arg(0), "deleteProperty")?;
    let key = ops::to_property_key(agent, call.arg(1))?;
    Ok(Value::Boolean(internal::delete(agent, o, key)?))
}

fn get(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let o = target(agent, call.arg(0), "get")?;
    let key = ops::to_property_key(agent, call.arg(1))?;
    let receiver = call.args.get(2).copied().unwrap_or(Value::Object(o));
    internal::get(agent, o, key, receiver)
}

fn get_own_property_descriptor(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let o = target(agent, call.arg(0), "getOwnPropertyDescriptor")?;
    let key = ops::to_property_key(agent, call.arg(1))?;
    let desc = internal::get_own_property(agent, o, key)?;
    Ok(ops::from_property_descriptor(agent, desc))
}

fn get_prototype_of(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let o = target(agent, call.arg(0), "getPrototypeOf")?;
    Ok(Value::object_or_null(internal::get_prototype_of(agent, o)?))
}

fn has(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let o = target(agent, call.arg(0), "has")?;
    let key = ops::to_property_key(agent, call.arg(1))?;
    Ok(Value::Boolean(internal::has_property(agent, o, key)?))
}

fn is_extensible(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let o = target(agent, call.arg(0), "isExtensible")?;
    Ok(Value::Boolean(internal::is_extensible(agent, o)?))
}

fn own_keys(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let o = target(agent, call.arg(0), "ownKeys")?;
    let keys = internal::own_property_keys(agent, o)?;
    let list: Vec<Value> = keys
        .into_iter()
        .map(|k| agent.heap.key_to_value(k))
        .collect();
    Ok(Value::Object(ops::create_array_from_list(agent, &list)))
}

fn prevent_extensions(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let o = target(agent, call.arg(0), "preventExtensions")?;
    Ok(Value::Boolean(internal::prevent_extensions(agent, o)?))
}

fn set(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let o = target(agent, call.arg(0), "set")?;
    let key = ops::to_property_key(agent, call.arg(1))?;
    let receiver = call.args.get(3).copied().unwrap_or(Value::Object(o));
    Ok(Value::Boolean(internal::set(
        agent,
        o,
        key,
        call.arg(2),
        receiver,
    )?))
}

fn set_prototype_of(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let o = target(agent, call.arg(0), "setPrototypeOf")?;
    let proto = match call.arg(1) {
        Value::Object(p) => Some(p),
        Value::Null => None,
        _ => return agent.type_error("Object prototype may only be an Object or null"),
    };
    Ok(Value::Boolean(internal::set_prototype_of(agent, o, proto)?))
}

/// `Proxy(target, handler)` (§28.2.1.1).
fn proxy_constructor(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    if call.new_target.is_none() {
        return agent.type_error("Constructor Proxy requires 'new'");
    }
    Ok(Value::Object(proxy_create(
        agent,
        call.arg(0),
        call.arg(1),
    )?))
}

/// `Proxy.revocable(target, handler)` (§28.2.2.1).
fn revocable(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let p = proxy_create(agent, call.arg(0), call.arg(1))?;
    let empty = PropertyKey::String(agent.heap.names.empty);
    let revoker = create_builtin_function(agent, revoke, 0, empty, None, None, None);
    if let Some(slots) = native_slots_mut(agent, revoker) {
        slots.push(Value::Object(p));
    }
    let result = ops::object_create(agent);
    let key = agent.heap.key_from_str("proxy");
    ops::create_data_property_or_throw(agent, result, key, Value::Object(p))?;
    let key = agent.heap.key_from_str("revoke");
    ops::create_data_property_or_throw(agent, result, key, Value::Object(revoker))?;
    Ok(Value::Object(result))
}

/// A Proxy revocation function: slot 0 is `[[RevocableProxy]]`.
fn revoke(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let Some(&Value::Object(p)) = native_slots(agent, call.callee).first() else {
        return Ok(Value::Undefined);
    };
    if let Some(slots) = native_slots_mut(agent, call.callee) {
        slots[0] = Value::Null;
    }
    proxy_revoke(agent, p);
    Ok(Value::Undefined)
}
