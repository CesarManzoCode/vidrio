//! Proxy exotic objects (ECMA-262 §10.5): every internal method calls its
//! trap and checks the invariants against the target.

use crate::agent::Agent;
use crate::completion::JsResult;
use crate::exotic::InternalMethods;
use crate::function::{call_object, construct_object};
use crate::gc::{Atom, Object, Trace, Tracer};
use crate::internal;
use crate::object::{ObjectKind, PropertyDescriptor};
use crate::ops;
use crate::value::{PropertyKey, Value};

/// `[[ProxyTarget]]` and `[[ProxyHandler]]` (`None` once revoked), plus
/// whether the proxy has `[[Call]]` and `[[Construct]]`.
pub struct ProxyData {
    pub(crate) target: Option<Object>,
    pub(crate) handler: Option<Object>,
    pub(crate) callable: bool,
    pub(crate) constructor: bool,
}

impl Trace for ProxyData {
    fn trace(&self, tracer: &mut Tracer) {
        for o in [self.target, self.handler].into_iter().flatten() {
            tracer.object(o);
        }
    }
}

/// ProxyCreate(target, handler).
pub fn proxy_create(agent: &mut Agent, target: Value, handler: Value) -> JsResult<Object> {
    let (Value::Object(target), Value::Object(handler)) = (target, handler) else {
        return agent.type_error("Proxy target and handler must be objects");
    };
    let callable = agent.heap.object(target).is_callable();
    let constructor = agent.heap.object(target).is_constructor();
    Ok(agent.heap.create_object(
        None,
        ObjectKind::Proxy(ProxyData {
            target: Some(target),
            handler: Some(handler),
            callable,
            constructor,
        }),
    ))
}

/// Revokes a proxy (what the function from `Proxy.revocable` does).
pub fn proxy_revoke(agent: &mut Agent, p: Object) {
    if let ObjectKind::Proxy(data) = &mut agent.heap.object_mut(p).kind {
        data.target = None;
        data.handler = None;
    }
}

/// The target of a proxy, or `None` if revoked or not a proxy.
pub fn proxy_target(agent: &Agent, p: Object) -> Option<Object> {
    match &agent.heap.object(p).kind {
        ObjectKind::Proxy(data) => data.target,
        _ => None,
    }
}

/// ValidateNonRevokedProxy, returning target and handler.
fn validate(agent: &mut Agent, p: Object) -> JsResult<(Object, Object)> {
    let pair = match &agent.heap.object(p).kind {
        ObjectKind::Proxy(ProxyData {
            target: Some(t),
            handler: Some(h),
            ..
        }) => Some((*t, *h)),
        _ => None,
    };
    match pair {
        Some(pair) => Ok(pair),
        None => agent.type_error("operation on a revoked Proxy"),
    }
}

/// GetMethod(handler, name).
fn trap(agent: &mut Agent, handler: Object, name: Atom) -> JsResult<Option<Value>> {
    ops::get_method(agent, Value::Object(handler), PropertyKey::String(name))
}

fn call_trap(agent: &mut Agent, trap: Value, handler: Object, args: &[Value]) -> JsResult<Value> {
    ops::call(agent, trap, Value::Object(handler), args)
}

fn trap_bool(agent: &mut Agent, trap: Value, handler: Object, args: &[Value]) -> JsResult<bool> {
    let result = call_trap(agent, trap, handler, args)?;
    Ok(ops::to_boolean(agent, result))
}

pub(crate) struct ProxyMethods;

impl InternalMethods for ProxyMethods {
    fn has_ordinary_get_prototype_of(&self) -> bool {
        false
    }

    fn get_prototype_of(&self, agent: &mut Agent, o: Object) -> JsResult<Option<Object>> {
        let (target, handler) = validate(agent, o)?;
        let name = agent.heap.names.get_prototype_of;
        let Some(t) = trap(agent, handler, name)? else {
            return internal::get_prototype_of(agent, target);
        };
        let handler_proto = match call_trap(agent, t, handler, &[Value::Object(target)])? {
            Value::Object(p) => Some(p),
            Value::Null => None,
            _ => return agent.type_error("getPrototypeOf trap returned neither object nor null"),
        };
        if internal::is_extensible(agent, target)? {
            return Ok(handler_proto);
        }
        let target_proto = internal::get_prototype_of(agent, target)?;
        if handler_proto != target_proto {
            return agent.type_error(
                "getPrototypeOf trap result differs from the non-extensible target's prototype",
            );
        }
        Ok(handler_proto)
    }

    fn set_prototype_of(
        &self,
        agent: &mut Agent,
        o: Object,
        proto: Option<Object>,
    ) -> JsResult<bool> {
        let (target, handler) = validate(agent, o)?;
        let name = agent.heap.names.set_prototype_of;
        let Some(t) = trap(agent, handler, name)? else {
            return internal::set_prototype_of(agent, target, proto);
        };
        let args = [Value::Object(target), Value::object_or_null(proto)];
        if !trap_bool(agent, t, handler, &args)? {
            return Ok(false);
        }
        if internal::is_extensible(agent, target)? {
            return Ok(true);
        }
        let target_proto = internal::get_prototype_of(agent, target)?;
        if proto != target_proto {
            return agent.type_error("setPrototypeOf trap returned true for a non-extensible target with another prototype");
        }
        Ok(true)
    }

    fn is_extensible(&self, agent: &mut Agent, o: Object) -> JsResult<bool> {
        let (target, handler) = validate(agent, o)?;
        let name = agent.heap.names.is_extensible;
        let Some(t) = trap(agent, handler, name)? else {
            return internal::is_extensible(agent, target);
        };
        let result = trap_bool(agent, t, handler, &[Value::Object(target)])?;
        if result != internal::is_extensible(agent, target)? {
            return agent.type_error("isExtensible trap result differs from the target");
        }
        Ok(result)
    }

    fn prevent_extensions(&self, agent: &mut Agent, o: Object) -> JsResult<bool> {
        let (target, handler) = validate(agent, o)?;
        let name = agent.heap.names.prevent_extensions;
        let Some(t) = trap(agent, handler, name)? else {
            return internal::prevent_extensions(agent, target);
        };
        let result = trap_bool(agent, t, handler, &[Value::Object(target)])?;
        if result && internal::is_extensible(agent, target)? {
            return agent
                .type_error("preventExtensions trap returned true for an extensible target");
        }
        Ok(result)
    }

    fn get_own_property(
        &self,
        agent: &mut Agent,
        o: Object,
        key: PropertyKey,
    ) -> JsResult<Option<PropertyDescriptor>> {
        let (target, handler) = validate(agent, o)?;
        let name = agent.heap.names.get_own_property_descriptor;
        let Some(t) = trap(agent, handler, name)? else {
            return internal::get_own_property(agent, target, key);
        };
        let key_value = agent.heap.key_to_value(key);
        let result = call_trap(agent, t, handler, &[Value::Object(target), key_value])?;
        if !result.is_object() && !result.is_undefined() {
            return agent
                .type_error("getOwnPropertyDescriptor trap returned neither object nor undefined");
        }
        let target_desc = internal::get_own_property(agent, target, key)?;
        if result.is_undefined() {
            let Some(target_desc) = target_desc else {
                return Ok(None);
            };
            if target_desc.configurable == Some(false) {
                return agent
                    .type_error("getOwnPropertyDescriptor trap hid a non-configurable property");
            }
            if !internal::is_extensible(agent, target)? {
                return agent.type_error(
                    "getOwnPropertyDescriptor trap hid a property of a non-extensible target",
                );
            }
            return Ok(None);
        }
        let extensible = internal::is_extensible(agent, target)?;
        let mut result_desc = ops::to_property_descriptor(agent, result)?;
        result_desc.complete();
        if !internal::is_compatible_property_descriptor(agent, extensible, result_desc, target_desc)
        {
            return agent
                .type_error("getOwnPropertyDescriptor trap returned an incompatible descriptor");
        }
        if result_desc.configurable == Some(false) {
            match target_desc {
                None
                | Some(PropertyDescriptor {
                    configurable: Some(true),
                    ..
                }) => {
                    return agent.type_error("getOwnPropertyDescriptor trap reported a configurable property as non-configurable");
                }
                Some(td) => {
                    if result_desc.writable == Some(false) && td.writable == Some(true) {
                        return agent.type_error("getOwnPropertyDescriptor trap reported a writable property as non-writable");
                    }
                }
            }
        }
        Ok(Some(result_desc))
    }

    fn define_own_property(
        &self,
        agent: &mut Agent,
        o: Object,
        key: PropertyKey,
        desc: PropertyDescriptor,
    ) -> JsResult<bool> {
        let (target, handler) = validate(agent, o)?;
        let name = agent.heap.names.define_property;
        let Some(t) = trap(agent, handler, name)? else {
            return internal::define_own_property(agent, target, key, desc);
        };
        let desc_obj = ops::from_property_descriptor(agent, Some(desc));
        let key_value = agent.heap.key_to_value(key);
        let args = [Value::Object(target), key_value, desc_obj];
        if !trap_bool(agent, t, handler, &args)? {
            return Ok(false);
        }
        let target_desc = internal::get_own_property(agent, target, key)?;
        let extensible = internal::is_extensible(agent, target)?;
        let setting_config_false = desc.configurable == Some(false);
        match target_desc {
            None => {
                if !extensible {
                    return agent.type_error(
                        "defineProperty trap added a property to a non-extensible target",
                    );
                }
                if setting_config_false {
                    return agent.type_error(
                        "defineProperty trap reported a missing property as non-configurable",
                    );
                }
            }
            Some(td) => {
                if !internal::is_compatible_property_descriptor(agent, extensible, desc, Some(td)) {
                    return agent
                        .type_error("defineProperty trap accepted an incompatible descriptor");
                }
                if setting_config_false && td.configurable == Some(true) {
                    return agent.type_error(
                        "defineProperty trap reported a configurable property as non-configurable",
                    );
                }
                if td.is_data()
                    && td.configurable == Some(false)
                    && td.writable == Some(true)
                    && desc.writable == Some(false)
                {
                    return agent.type_error(
                        "defineProperty trap reported a writable property as non-writable",
                    );
                }
            }
        }
        Ok(true)
    }

    fn has_property(&self, agent: &mut Agent, o: Object, key: PropertyKey) -> JsResult<bool> {
        let (target, handler) = validate(agent, o)?;
        let name = agent.heap.names.has;
        let Some(t) = trap(agent, handler, name)? else {
            return internal::has_property(agent, target, key);
        };
        let key_value = agent.heap.key_to_value(key);
        let result = trap_bool(agent, t, handler, &[Value::Object(target), key_value])?;
        if !result {
            if let Some(td) = internal::get_own_property(agent, target, key)? {
                if td.configurable == Some(false) {
                    return agent.type_error("has trap hid a non-configurable property");
                }
                if !internal::is_extensible(agent, target)? {
                    return agent.type_error("has trap hid a property of a non-extensible target");
                }
            }
        }
        Ok(result)
    }

    fn get(
        &self,
        agent: &mut Agent,
        o: Object,
        key: PropertyKey,
        receiver: Value,
    ) -> JsResult<Value> {
        let (target, handler) = validate(agent, o)?;
        let name = agent.heap.names.get;
        let Some(t) = trap(agent, handler, name)? else {
            return internal::get(agent, target, key, receiver);
        };
        let key_value = agent.heap.key_to_value(key);
        let result = call_trap(
            agent,
            t,
            handler,
            &[Value::Object(target), key_value, receiver],
        )?;
        if let Some(td) = internal::get_own_property(agent, target, key)? {
            if td.configurable == Some(false) {
                if td.is_data() && td.writable == Some(false) {
                    let expected = td.value.unwrap_or(Value::Undefined);
                    if !ops::same_value(agent, result, expected) {
                        return agent.type_error("get trap result differs from a non-writable, non-configurable property");
                    }
                }
                if td.is_accessor() && td.get == Some(Value::Undefined) && !result.is_undefined() {
                    return agent
                        .type_error("get trap returned a value for an accessor without getter");
                }
            }
        }
        Ok(result)
    }

    fn set(
        &self,
        agent: &mut Agent,
        o: Object,
        key: PropertyKey,
        value: Value,
        receiver: Value,
    ) -> JsResult<bool> {
        let (target, handler) = validate(agent, o)?;
        let name = agent.heap.names.set;
        let Some(t) = trap(agent, handler, name)? else {
            return internal::set(agent, target, key, value, receiver);
        };
        let key_value = agent.heap.key_to_value(key);
        let args = [Value::Object(target), key_value, value, receiver];
        if !trap_bool(agent, t, handler, &args)? {
            return Ok(false);
        }
        if let Some(td) = internal::get_own_property(agent, target, key)? {
            if td.configurable == Some(false) {
                if td.is_data() && td.writable == Some(false) {
                    let expected = td.value.unwrap_or(Value::Undefined);
                    if !ops::same_value(agent, value, expected) {
                        return agent.type_error(
                            "set trap changed a non-writable, non-configurable property",
                        );
                    }
                }
                if td.is_accessor() && td.set == Some(Value::Undefined) {
                    return agent.type_error("set trap succeeded for an accessor without setter");
                }
            }
        }
        Ok(true)
    }

    fn delete(&self, agent: &mut Agent, o: Object, key: PropertyKey) -> JsResult<bool> {
        let (target, handler) = validate(agent, o)?;
        let name = agent.heap.names.delete_property;
        let Some(t) = trap(agent, handler, name)? else {
            return internal::delete(agent, target, key);
        };
        let key_value = agent.heap.key_to_value(key);
        if !trap_bool(agent, t, handler, &[Value::Object(target), key_value])? {
            return Ok(false);
        }
        let Some(td) = internal::get_own_property(agent, target, key)? else {
            return Ok(true);
        };
        if td.configurable == Some(false) {
            return agent.type_error("deleteProperty trap deleted a non-configurable property");
        }
        if !internal::is_extensible(agent, target)? {
            return agent
                .type_error("deleteProperty trap deleted a property of a non-extensible target");
        }
        Ok(true)
    }

    fn own_property_keys(&self, agent: &mut Agent, o: Object) -> JsResult<Vec<PropertyKey>> {
        let (target, handler) = validate(agent, o)?;
        let name = agent.heap.names.own_keys;
        let Some(t) = trap(agent, handler, name)? else {
            return internal::own_property_keys(agent, target);
        };
        let array = call_trap(agent, t, handler, &[Value::Object(target)])?;
        let values =
            ops::create_list_from_array_like(agent, array, ops::ListElementTypes::PropertyKey)?;
        let mut trap_result = Vec::with_capacity(values.len());
        for v in values {
            let key = match v {
                Value::String(s) => agent.heap.key_from_string(s),
                Value::Symbol(s) => PropertyKey::Symbol(s),
                _ => unreachable!("CreateListFromArrayLike checked the element types"),
            };
            if trap_result.contains(&key) {
                return agent.type_error("ownKeys trap returned a duplicate key");
            }
            trap_result.push(key);
        }
        let extensible = internal::is_extensible(agent, target)?;
        let target_keys = internal::own_property_keys(agent, target)?;
        let mut configurable = Vec::new();
        let mut nonconfigurable = Vec::new();
        for key in target_keys {
            match internal::get_own_property(agent, target, key)? {
                Some(d) if d.configurable == Some(false) => nonconfigurable.push(key),
                _ => configurable.push(key),
            }
        }
        if extensible && nonconfigurable.is_empty() {
            return Ok(trap_result);
        }
        let mut unchecked: Vec<Option<PropertyKey>> =
            trap_result.iter().map(|k| Some(*k)).collect();
        for key in nonconfigurable {
            match unchecked.iter_mut().find(|k| **k == Some(key)) {
                Some(slot) => *slot = None,
                None => {
                    return agent.type_error("ownKeys trap omitted a non-configurable key");
                }
            }
        }
        if extensible {
            return Ok(trap_result);
        }
        for key in configurable {
            match unchecked.iter_mut().find(|k| **k == Some(key)) {
                Some(slot) => *slot = None,
                None => {
                    return agent
                        .type_error("ownKeys trap omitted a key of a non-extensible target");
                }
            }
        }
        if unchecked.iter().any(|k| k.is_some()) {
            return agent.type_error("ownKeys trap added a key to a non-extensible target");
        }
        Ok(trap_result)
    }
}

/// The Proxy `[[Call]]`.
pub(crate) fn proxy_call(
    agent: &mut Agent,
    p: Object,
    this: Value,
    args: &[Value],
) -> JsResult<Value> {
    let (target, handler) = validate(agent, p)?;
    let name = agent.heap.names.apply;
    let Some(t) = trap(agent, handler, name)? else {
        return call_object(agent, target, this, args);
    };
    let array = ops::create_array_from_list(agent, args);
    call_trap(
        agent,
        t,
        handler,
        &[Value::Object(target), this, Value::Object(array)],
    )
}

/// The Proxy `[[Construct]]`.
pub(crate) fn proxy_construct(
    agent: &mut Agent,
    p: Object,
    args: &[Value],
    new_target: Object,
) -> JsResult<Object> {
    let (target, handler) = validate(agent, p)?;
    let name = agent.heap.names.construct;
    let Some(t) = trap(agent, handler, name)? else {
        return construct_object(agent, target, args, new_target);
    };
    let array = ops::create_array_from_list(agent, args);
    let result = call_trap(
        agent,
        t,
        handler,
        &[
            Value::Object(target),
            Value::Object(array),
            Value::Object(new_target),
        ],
    )?;
    match result {
        Value::Object(o) => Ok(o),
        _ => agent.type_error("construct trap returned a non-object"),
    }
}
