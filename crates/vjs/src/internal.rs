//! The essential internal methods (ECMA-262 §6.1.7.2), dispatched to the
//! object's [`InternalMethods`], and the ordinary object internal methods
//! of §10.1 with the Array exotic `[[DefineOwnProperty]]` helpers.
//!
//! `internal::get(agent, o, key, receiver)` is `O.[[Get]](P, Receiver)`;
//! the abstract operation `Get(O, P)` is `ops::get`.

use crate::agent::Agent;
use crate::completion::JsResult;
use crate::exotic::methods_of;
use crate::gc::{JsString, Object};
use crate::object::{Attributes, ObjectKind, PropertyDescriptor, Slot};
use crate::ops;
use crate::realm::Intrinsic;
use crate::value::{PropertyKey, Value};

pub use crate::exotic::InternalMethods;

/// `O.[[GetPrototypeOf]]()`.
pub fn get_prototype_of(agent: &mut Agent, o: Object) -> JsResult<Option<Object>> {
    if is_plain(agent, o) {
        return Ok(agent.heap.object(o).proto);
    }
    methods_of(agent, o).get().get_prototype_of(agent, o)
}

/// `O.[[SetPrototypeOf]](V)`.
pub fn set_prototype_of(agent: &mut Agent, o: Object, proto: Option<Object>) -> JsResult<bool> {
    methods_of(agent, o).get().set_prototype_of(agent, o, proto)
}

/// `O.[[IsExtensible]]()`.
pub fn is_extensible(agent: &mut Agent, o: Object) -> JsResult<bool> {
    if is_plain(agent, o) {
        return Ok(agent.heap.object(o).extensible);
    }
    methods_of(agent, o).get().is_extensible(agent, o)
}

/// `O.[[PreventExtensions]]()`.
pub fn prevent_extensions(agent: &mut Agent, o: Object) -> JsResult<bool> {
    methods_of(agent, o).get().prevent_extensions(agent, o)
}

/// `O.[[GetOwnProperty]](P)`.
pub fn get_own_property(
    agent: &mut Agent,
    o: Object,
    key: PropertyKey,
) -> JsResult<Option<PropertyDescriptor>> {
    if is_plain(agent, o) {
        return Ok(ordinary_get_own_property(agent, o, key));
    }
    methods_of(agent, o).get().get_own_property(agent, o, key)
}

/// `O.[[DefineOwnProperty]](P, Desc)`.
pub fn define_own_property(
    agent: &mut Agent,
    o: Object,
    key: PropertyKey,
    desc: PropertyDescriptor,
) -> JsResult<bool> {
    methods_of(agent, o)
        .get()
        .define_own_property(agent, o, key, desc)
}

/// `O.[[HasProperty]](P)`.
pub fn has_property(agent: &mut Agent, o: Object, key: PropertyKey) -> JsResult<bool> {
    if is_plain(agent, o) {
        return ordinary_has_property(agent, o, key);
    }
    methods_of(agent, o).get().has_property(agent, o, key)
}

/// `O.[[Get]](P, Receiver)`.
pub fn get(agent: &mut Agent, o: Object, key: PropertyKey, receiver: Value) -> JsResult<Value> {
    if is_plain(agent, o) {
        return ordinary_get(agent, o, key, receiver);
    }
    methods_of(agent, o).get().get(agent, o, key, receiver)
}

/// `O.[[Set]](P, V, Receiver)`.
pub fn set(
    agent: &mut Agent,
    o: Object,
    key: PropertyKey,
    value: Value,
    receiver: Value,
) -> JsResult<bool> {
    if is_plain(agent, o) {
        return ordinary_set(agent, o, key, value, receiver);
    }
    methods_of(agent, o)
        .get()
        .set(agent, o, key, value, receiver)
}

/// `O.[[Delete]](P)`.
pub fn delete(agent: &mut Agent, o: Object, key: PropertyKey) -> JsResult<bool> {
    methods_of(agent, o).get().delete(agent, o, key)
}

/// `O.[[OwnPropertyKeys]]()`.
pub fn own_property_keys(agent: &mut Agent, o: Object) -> JsResult<Vec<PropertyKey>> {
    methods_of(agent, o).get().own_property_keys(agent, o)
}

/// Whether every internal method of `o` except possibly
/// `[[DefineOwnProperty]]` and `[[SetPrototypeOf]]` is the ordinary one,
/// so lookups can walk its storage directly.
pub(crate) fn is_plain(agent: &Agent, o: Object) -> bool {
    !matches!(
        agent.heap.object(o).kind,
        ObjectKind::String(_)
            | ObjectKind::Arguments(_)
            | ObjectKind::TypedArray(_)
            | ObjectKind::ModuleNamespace(_)
            | ObjectKind::Proxy(_)
            | ObjectKind::Host(_)
    )
}

/// OrdinarySetPrototypeOf(O, V).
pub fn ordinary_set_prototype_of(agent: &mut Agent, o: Object, proto: Option<Object>) -> bool {
    let current = agent.heap.object(o).proto;
    if proto == current {
        return true;
    }
    if !agent.heap.object(o).extensible {
        return false;
    }
    let mut p = proto;
    while let Some(po) = p {
        if po == o {
            return false;
        }
        if !methods_of(agent, po).get().has_ordinary_get_prototype_of() {
            break;
        }
        p = agent.heap.object(po).proto;
    }
    agent.heap.set_proto(o, proto);
    true
}

/// OrdinaryPreventExtensions(O).
pub fn ordinary_prevent_extensions(agent: &mut Agent, o: Object) -> bool {
    agent.heap.set_extensible(o, false);
    true
}

/// OrdinaryGetOwnProperty(O, P).
pub fn ordinary_get_own_property(
    agent: &Agent,
    o: Object,
    key: PropertyKey,
) -> Option<PropertyDescriptor> {
    agent
        .heap
        .get_own(o, key)
        .map(|(attrs, slot)| PropertyDescriptor::from_property(attrs, slot))
}

/// OrdinaryDefineOwnProperty(O, P, Desc).
pub fn ordinary_define_own_property(
    agent: &mut Agent,
    o: Object,
    key: PropertyKey,
    desc: PropertyDescriptor,
) -> JsResult<bool> {
    let current = get_own_property(agent, o, key)?;
    let extensible = is_extensible(agent, o)?;
    Ok(validate_and_apply_property_descriptor(
        agent,
        Some(o),
        key,
        extensible,
        desc,
        current,
    ))
}

/// IsCompatiblePropertyDescriptor(Extensible, Desc, Current).
pub fn is_compatible_property_descriptor(
    agent: &mut Agent,
    extensible: bool,
    desc: PropertyDescriptor,
    current: Option<PropertyDescriptor>,
) -> bool {
    validate_and_apply_property_descriptor(
        agent,
        None,
        PropertyKey::Index(0),
        extensible,
        desc,
        current,
    )
}

/// ValidateAndApplyPropertyDescriptor(O, P, extensible, Desc, current).
pub fn validate_and_apply_property_descriptor(
    agent: &mut Agent,
    o: Option<Object>,
    key: PropertyKey,
    extensible: bool,
    desc: PropertyDescriptor,
    current: Option<PropertyDescriptor>,
) -> bool {
    let Some(current) = current else {
        if !extensible {
            return false;
        }
        if let Some(o) = o {
            let (attrs, slot) = desc.to_property();
            agent.heap.set_own(o, key, attrs, slot);
        }
        return true;
    };
    if desc.has_no_fields() {
        return true;
    }
    if current.configurable != Some(true) {
        if desc.configurable == Some(true) {
            return false;
        }
        if desc.enumerable.is_some() && desc.enumerable != current.enumerable {
            return false;
        }
        if !desc.is_generic() && desc.is_accessor() != current.is_accessor() {
            return false;
        }
        if current.is_accessor() {
            if let Some(g) = desc.get {
                if !ops::same_value(agent, g, current.get.unwrap_or(Value::Undefined)) {
                    return false;
                }
            }
            if let Some(s) = desc.set {
                if !ops::same_value(agent, s, current.set.unwrap_or(Value::Undefined)) {
                    return false;
                }
            }
        } else if current.writable != Some(true) {
            if desc.writable == Some(true) {
                return false;
            }
            if let Some(v) = desc.value {
                if !ops::same_value(agent, v, current.value.unwrap_or(Value::Undefined)) {
                    return false;
                }
            }
        }
    }
    if let Some(o) = o {
        let enumerable = desc.enumerable.or(current.enumerable).unwrap_or(false);
        let configurable = desc.configurable.or(current.configurable).unwrap_or(false);
        let (attrs, slot) = if current.is_data() && desc.is_accessor() {
            (
                Attributes::accessor(enumerable, configurable),
                Slot::Accessor {
                    get: desc.get.unwrap_or(Value::Undefined),
                    set: desc.set.unwrap_or(Value::Undefined),
                },
            )
        } else if current.is_accessor() && desc.is_data() {
            (
                Attributes::data(desc.writable.unwrap_or(false), enumerable, configurable),
                Slot::Data(desc.value.unwrap_or(Value::Undefined)),
            )
        } else if current.is_accessor() {
            (
                Attributes::accessor(enumerable, configurable),
                Slot::Accessor {
                    get: desc.get.or(current.get).unwrap_or(Value::Undefined),
                    set: desc.set.or(current.set).unwrap_or(Value::Undefined),
                },
            )
        } else {
            (
                Attributes::data(
                    desc.writable.or(current.writable).unwrap_or(false),
                    enumerable,
                    configurable,
                ),
                Slot::Data(desc.value.or(current.value).unwrap_or(Value::Undefined)),
            )
        };
        agent.heap.set_own(o, key, attrs, slot);
    }
    true
}

/// OrdinaryHasProperty(O, P).
pub fn ordinary_has_property(agent: &mut Agent, o: Object, key: PropertyKey) -> JsResult<bool> {
    if get_own_property(agent, o, key)?.is_some() {
        return Ok(true);
    }
    let mut parent = get_prototype_of(agent, o)?;
    while let Some(p) = parent {
        if !is_plain(agent, p) {
            return has_property(agent, p, key);
        }
        if agent.heap.get_own(p, key).is_some() {
            return Ok(true);
        }
        parent = agent.heap.object(p).proto;
    }
    Ok(false)
}

/// OrdinaryGet(O, P, Receiver), walking ordinary prototypes in a loop.
pub fn ordinary_get(
    agent: &mut Agent,
    o: Object,
    key: PropertyKey,
    receiver: Value,
) -> JsResult<Value> {
    let mut desc = get_own_property(agent, o, key)?;
    let mut obj = o;
    loop {
        if let Some(d) = desc {
            if d.is_data() {
                return Ok(d.value.unwrap_or(Value::Undefined));
            }
            let getter = d.get.unwrap_or(Value::Undefined);
            if getter.is_undefined() {
                return Ok(Value::Undefined);
            }
            return ops::call(agent, getter, receiver, &[]);
        }
        let parent = if is_plain(agent, obj) {
            agent.heap.object(obj).proto
        } else {
            get_prototype_of(agent, obj)?
        };
        let Some(p) = parent else {
            return Ok(Value::Undefined);
        };
        if !is_plain(agent, p) {
            return get(agent, p, key, receiver);
        }
        obj = p;
        desc = ordinary_get_own_property(agent, p, key);
    }
}

/// OrdinarySet(O, P, V, Receiver).
pub fn ordinary_set(
    agent: &mut Agent,
    o: Object,
    key: PropertyKey,
    value: Value,
    receiver: Value,
) -> JsResult<bool> {
    let own = get_own_property(agent, o, key)?;
    ordinary_set_with_own_descriptor(agent, o, key, value, receiver, own)
}

/// OrdinarySetWithOwnDescriptor(O, P, V, Receiver, ownDesc).
pub fn ordinary_set_with_own_descriptor(
    agent: &mut Agent,
    o: Object,
    key: PropertyKey,
    value: Value,
    receiver: Value,
    own: Option<PropertyDescriptor>,
) -> JsResult<bool> {
    let mut obj = o;
    let mut own = own;
    let own = loop {
        if let Some(d) = own {
            break d;
        }
        let parent = if is_plain(agent, obj) {
            agent.heap.object(obj).proto
        } else {
            get_prototype_of(agent, obj)?
        };
        match parent {
            Some(p) if !is_plain(agent, p) => return set(agent, p, key, value, receiver),
            Some(p) => {
                obj = p;
                own = ordinary_get_own_property(agent, p, key);
            }
            None => break PropertyDescriptor::data(Value::Undefined, true, true, true),
        }
    };
    if own.is_data() {
        if own.writable != Some(true) {
            return Ok(false);
        }
        let Value::Object(r) = receiver else {
            return Ok(false);
        };
        if let Some(existing) = get_own_property(agent, r, key)? {
            if existing.is_accessor() || existing.writable != Some(true) {
                return Ok(false);
            }
            if is_plain(agent, r) && !matches!(agent.heap.object(r).kind, ObjectKind::Array) {
                agent.heap.set_own_value(r, key, value);
                return Ok(true);
            }
            return define_own_property(agent, r, key, PropertyDescriptor::value_only(value));
        }
        return define_own_property(
            agent,
            r,
            key,
            PropertyDescriptor::data(value, true, true, true),
        );
    }
    let setter = own.set.unwrap_or(Value::Undefined);
    if setter.is_undefined() {
        return Ok(false);
    }
    ops::call(agent, setter, receiver, &[value])?;
    Ok(true)
}

/// OrdinaryDelete(O, P).
pub fn ordinary_delete(agent: &mut Agent, o: Object, key: PropertyKey) -> JsResult<bool> {
    match get_own_property(agent, o, key)? {
        None => Ok(true),
        Some(d) if d.configurable == Some(true) => {
            agent.heap.delete_own(o, key);
            Ok(true)
        }
        Some(_) => Ok(false),
    }
}

/// OrdinaryOwnPropertyKeys(O).
pub fn ordinary_own_property_keys(agent: &Agent, o: Object) -> Vec<PropertyKey> {
    agent.heap.own_keys(o)
}

/// ArrayCreate(length, proto).
pub fn array_create(agent: &mut Agent, length: f64, proto: Option<Object>) -> JsResult<Object> {
    if length > 4294967295.0 {
        return agent.range_error("invalid array length");
    }
    let proto = match proto {
        Some(p) => p,
        None => agent.intrinsic(Intrinsic::ArrayPrototype),
    };
    let a = agent.heap.create_object(Some(proto), ObjectKind::Array);
    let key = PropertyKey::String(agent.heap.names.length);
    agent.heap.set_own(
        a,
        key,
        Attributes::data(true, false, false),
        Slot::Data(Value::Number(length)),
    );
    Ok(a)
}

/// The `length` of an Array exotic object.
pub fn array_length(agent: &Agent, a: Object) -> u32 {
    let key = PropertyKey::String(agent.heap.names.length);
    match agent.heap.get_own(a, key) {
        Some((_, Slot::Data(Value::Number(n)))) => n as u32,
        _ => 0,
    }
}

/// The Array exotic `[[DefineOwnProperty]]` (ECMA-262 §10.4.2.1).
pub(crate) fn array_define_own_property(
    agent: &mut Agent,
    a: Object,
    key: PropertyKey,
    desc: PropertyDescriptor,
) -> JsResult<bool> {
    let length_key = PropertyKey::String(agent.heap.names.length);
    if key == length_key {
        return array_set_length(agent, a, desc);
    }
    if let PropertyKey::Index(index) = key {
        let (length_attrs, _) = agent
            .heap
            .get_own(a, length_key)
            .expect("arrays have a length property");
        let length = array_length(agent, a);
        if index >= length && !length_attrs.writable() {
            return Ok(false);
        }
        if !ordinary_define_own_property(agent, a, key, desc)? {
            return Ok(false);
        }
        if index >= length {
            agent
                .heap
                .set_own_value(a, length_key, Value::Number(f64::from(index) + 1.0));
        }
        return Ok(true);
    }
    ordinary_define_own_property(agent, a, key, desc)
}

/// ArraySetLength(A, Desc).
pub fn array_set_length(agent: &mut Agent, a: Object, desc: PropertyDescriptor) -> JsResult<bool> {
    let length_key = PropertyKey::String(agent.heap.names.length);
    let Some(value) = desc.value else {
        return ordinary_define_own_property(agent, a, length_key, desc);
    };
    let mut new_len_desc = desc;
    let new_len = ops::to_uint32(agent, value)?;
    let number_len = ops::to_number(agent, value)?;
    if f64::from(new_len) != number_len {
        return agent.range_error("invalid array length");
    }
    new_len_desc.value = Some(Value::Number(f64::from(new_len)));
    let (old_attrs, _) = agent
        .heap
        .get_own(a, length_key)
        .expect("arrays have a length property");
    let old_len = array_length(agent, a);
    if new_len >= old_len {
        return ordinary_define_own_property(agent, a, length_key, new_len_desc);
    }
    if !old_attrs.writable() {
        return Ok(false);
    }
    let new_writable = new_len_desc.writable != Some(false);
    if !new_writable {
        new_len_desc.writable = Some(true);
    }
    if !ordinary_define_own_property(agent, a, length_key, new_len_desc)? {
        return Ok(false);
    }
    if let Some(stuck) = agent.heap.truncate_elements(a, new_len) {
        new_len_desc.value = Some(Value::Number(f64::from(stuck) + 1.0));
        if !new_writable {
            new_len_desc.writable = Some(false);
        }
        ordinary_define_own_property(agent, a, length_key, new_len_desc)?;
        return Ok(false);
    }
    if !new_writable {
        let only_writable = PropertyDescriptor {
            writable: Some(false),
            ..PropertyDescriptor::default()
        };
        ordinary_define_own_property(agent, a, length_key, only_writable)?;
    }
    Ok(true)
}

/// StringCreate(value, prototype).
pub fn string_create(agent: &mut Agent, s: JsString, proto: Option<Object>) -> Object {
    let proto = match proto {
        Some(p) => p,
        None => agent.intrinsic(Intrinsic::StringPrototype),
    };
    let o = agent.heap.create_object(Some(proto), ObjectKind::String(s));
    let len = agent.heap.string_len(s);
    let key = PropertyKey::String(agent.heap.names.length);
    agent.heap.set_own(
        o,
        key,
        Attributes::data(false, false, false),
        Slot::Data(Value::Number(len as f64)),
    );
    o
}

/// StringGetOwnProperty(S, P).
pub fn string_get_own_property(
    agent: &mut Agent,
    o: Object,
    key: PropertyKey,
) -> Option<PropertyDescriptor> {
    let PropertyKey::Index(i) = key else {
        return None;
    };
    let ObjectKind::String(s) = agent.heap.object(o).kind else {
        return None;
    };
    let unit = agent.heap.code_unit_at(s, i as usize)?;
    let ch = Value::String(agent.heap.alloc_string(&[unit]));
    Some(PropertyDescriptor::data(ch, false, true, false))
}
