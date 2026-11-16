//! `Object` (ECMA-262 §20.1) with the Annex B methods of
//! `Object.prototype` (`__proto__`, `__defineGetter__` and friends).

use crate::agent::Agent;
use crate::builtins::{accessor, constructor, intrinsic, method, set_intrinsic, str_value};
use crate::completion::{JsResult, Throw};
use crate::function::NativeCall;
use crate::gc::{Object, Realm};
use crate::internal;
use crate::object::{ObjectKind, PropertyDescriptor};
use crate::ops::{self, EnumerableKind, IntegrityLevel, IteratorRecord};
use crate::realm::Intrinsic;
use crate::value::{PropertyKey, Value};

pub(crate) fn install(agent: &mut Agent, realm: Realm) {
    let proto = intrinsic(agent, realm, Intrinsic::ObjectPrototype);
    let ctor = constructor(agent, realm, "Object", object_constructor, 1, proto, None);
    set_intrinsic(agent, realm, Intrinsic::Object, ctor);

    let statics: [(&str, crate::function::NativeFn, u32); 23] = [
        ("assign", assign, 2),
        ("create", create, 2),
        ("defineProperties", define_properties, 2),
        ("defineProperty", define_property, 3),
        ("entries", entries, 1),
        ("freeze", freeze, 1),
        ("fromEntries", from_entries, 1),
        ("getOwnPropertyDescriptor", get_own_property_descriptor, 2),
        ("getOwnPropertyDescriptors", get_own_property_descriptors, 1),
        ("getOwnPropertyNames", get_own_property_names, 1),
        ("getOwnPropertySymbols", get_own_property_symbols, 1),
        ("getPrototypeOf", get_prototype_of, 1),
        ("groupBy", group_by, 2),
        ("hasOwn", has_own, 2),
        ("is", is, 2),
        ("isExtensible", is_extensible, 1),
        ("isFrozen", is_frozen, 1),
        ("isSealed", is_sealed, 1),
        ("keys", keys, 1),
        ("preventExtensions", prevent_extensions, 1),
        ("seal", seal, 1),
        ("setPrototypeOf", set_prototype_of, 2),
        ("values", values, 1),
    ];
    for (name, f, length) in statics {
        method(agent, realm, ctor, name, f, length);
    }

    let methods: [(&str, crate::function::NativeFn, u32); 10] = [
        ("__defineGetter__", define_getter, 2),
        ("__defineSetter__", define_setter, 2),
        ("hasOwnProperty", has_own_property, 1),
        ("isPrototypeOf", is_prototype_of, 1),
        ("__lookupGetter__", lookup_getter, 1),
        ("__lookupSetter__", lookup_setter, 1),
        ("propertyIsEnumerable", property_is_enumerable, 1),
        ("toLocaleString", to_locale_string, 0),
        ("toString", to_string, 0),
        ("valueOf", value_of, 0),
    ];
    for (name, f, length) in methods {
        method(agent, realm, proto, name, f, length);
    }
    let key = agent.heap.key_from_str("__proto__");
    accessor(
        agent,
        realm,
        proto,
        key,
        Some(proto_getter),
        Some(proto_setter),
    );
}

/// The object an argument must be, or a TypeError.
fn require_object(agent: &mut Agent, v: Value, what: &str) -> JsResult<Object> {
    match v {
        Value::Object(o) => Ok(o),
        _ => agent.type_error(&format!("{what} is not an object")),
    }
}

/// `Object(value)` (§20.1.1.1).
fn object_constructor(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    if let Some(nt) = call.new_target {
        if nt != call.callee {
            let o = ops::ordinary_create_from_constructor(
                agent,
                nt,
                Intrinsic::ObjectPrototype,
                ObjectKind::Ordinary,
            )?;
            return Ok(Value::Object(o));
        }
    }
    let v = call.arg(0);
    if v.is_nullish() {
        return Ok(Value::Object(ops::object_create(agent)));
    }
    Ok(Value::Object(ops::to_object(agent, v)?))
}

fn assign(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let to = ops::to_object(agent, call.arg(0))?;
    for &source in call.args.iter().skip(1) {
        if source.is_nullish() {
            continue;
        }
        let from = ops::to_object(agent, source)?;
        for key in internal::own_property_keys(agent, from)? {
            let Some(desc) = internal::get_own_property(agent, from, key)? else {
                continue;
            };
            if desc.enumerable == Some(true) {
                let v = ops::get(agent, from, key)?;
                ops::set(agent, to, key, v, true)?;
            }
        }
    }
    Ok(Value::Object(to))
}

/// ObjectDefineProperties(O, Properties).
fn object_define_properties(agent: &mut Agent, o: Object, properties: Value) -> JsResult<()> {
    let props = ops::to_object(agent, properties)?;
    let mut descriptors = Vec::new();
    for key in internal::own_property_keys(agent, props)? {
        let Some(prop_desc) = internal::get_own_property(agent, props, key)? else {
            continue;
        };
        if prop_desc.enumerable == Some(true) {
            let desc_obj = ops::get(agent, props, key)?;
            let desc = ops::to_property_descriptor(agent, desc_obj)?;
            descriptors.push((key, desc));
        }
    }
    for (key, desc) in descriptors {
        ops::define_property_or_throw(agent, o, key, desc)?;
    }
    Ok(())
}

fn create(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let proto = match call.arg(0) {
        Value::Object(p) => Some(p),
        Value::Null => None,
        _ => return agent.type_error("Object prototype may only be an Object or null"),
    };
    let o = ops::ordinary_object_create(agent, proto);
    let properties = call.arg(1);
    if !properties.is_undefined() {
        object_define_properties(agent, o, properties)?;
    }
    Ok(Value::Object(o))
}

fn define_properties(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let o = require_object(agent, call.arg(0), "Object.defineProperties target")?;
    object_define_properties(agent, o, call.arg(1))?;
    Ok(Value::Object(o))
}

fn define_property(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let o = require_object(agent, call.arg(0), "Object.defineProperty target")?;
    let key = ops::to_property_key(agent, call.arg(1))?;
    let desc = ops::to_property_descriptor(agent, call.arg(2))?;
    ops::define_property_or_throw(agent, o, key, desc)?;
    Ok(Value::Object(o))
}

fn enumerable(agent: &mut Agent, v: Value, kind: EnumerableKind) -> JsResult<Value> {
    let o = ops::to_object(agent, v)?;
    let list = ops::enumerable_own_properties(agent, o, kind)?;
    Ok(Value::Object(ops::create_array_from_list(agent, &list)))
}

fn entries(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    enumerable(agent, call.arg(0), EnumerableKind::KeyValue)
}

fn keys(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    enumerable(agent, call.arg(0), EnumerableKind::Key)
}

fn values(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    enumerable(agent, call.arg(0), EnumerableKind::Value)
}

fn integrity(agent: &mut Agent, v: Value, level: IntegrityLevel) -> JsResult<Value> {
    let Value::Object(o) = v else {
        return Ok(v);
    };
    if !ops::set_integrity_level(agent, o, level)? {
        return agent.type_error("cannot change the integrity level of the object");
    }
    Ok(v)
}

fn freeze(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    integrity(agent, call.arg(0), IntegrityLevel::Frozen)
}

fn seal(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    integrity(agent, call.arg(0), IntegrityLevel::Sealed)
}

fn prevent_extensions(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let v = call.arg(0);
    let Value::Object(o) = v else {
        return Ok(v);
    };
    if !internal::prevent_extensions(agent, o)? {
        return agent.type_error("cannot prevent extensions");
    }
    Ok(v)
}

fn test_integrity(agent: &mut Agent, v: Value, level: IntegrityLevel) -> JsResult<Value> {
    let Value::Object(o) = v else {
        return Ok(Value::Boolean(true));
    };
    Ok(Value::Boolean(ops::test_integrity_level(agent, o, level)?))
}

fn is_frozen(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    test_integrity(agent, call.arg(0), IntegrityLevel::Frozen)
}

fn is_sealed(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    test_integrity(agent, call.arg(0), IntegrityLevel::Sealed)
}

fn is_extensible(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let Value::Object(o) = call.arg(0) else {
        return Ok(Value::Boolean(false));
    };
    Ok(Value::Boolean(internal::is_extensible(agent, o)?))
}

/// Runs `step` and closes the iterator if it throws.
fn or_close<T>(
    agent: &mut Agent,
    record: &IteratorRecord,
    result: JsResult<T>,
) -> Result<T, Throw> {
    match result {
        Ok(v) => Ok(v),
        Err(e) => ops::iterator_close(agent, record, Err(e)),
    }
}

/// `Object.fromEntries(iterable)` with AddEntriesFromIterable.
fn from_entries(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let iterable = ops::require_object_coercible(agent, call.arg(0))?;
    let obj = ops::object_create(agent);
    let mut record = ops::get_iterator(agent, iterable)?;
    while let Some(next) = ops::iterator_step_value(agent, &mut record)? {
        let Value::Object(entry) = next else {
            let error = agent.type_error("iterator value is not an entry object");
            return ops::iterator_close(agent, &record, error);
        };
        let k = ops::get(agent, entry, PropertyKey::Index(0));
        let k = or_close(agent, &record, k)?;
        let v = ops::get(agent, entry, PropertyKey::Index(1));
        let v = or_close(agent, &record, v)?;
        let key = ops::to_property_key(agent, k);
        let key = or_close(agent, &record, key)?;
        let defined = ops::create_data_property_or_throw(agent, obj, key, v);
        or_close(agent, &record, defined)?;
    }
    Ok(Value::Object(obj))
}

fn get_own_property_descriptor(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let o = ops::to_object(agent, call.arg(0))?;
    let key = ops::to_property_key(agent, call.arg(1))?;
    let desc = internal::get_own_property(agent, o, key)?;
    Ok(ops::from_property_descriptor(agent, desc))
}

fn get_own_property_descriptors(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let o = ops::to_object(agent, call.arg(0))?;
    let keys = internal::own_property_keys(agent, o)?;
    let descriptors = ops::object_create(agent);
    for key in keys {
        let desc = internal::get_own_property(agent, o, key)?;
        let d = ops::from_property_descriptor(agent, desc);
        if !d.is_undefined() {
            ops::create_data_property_or_throw(agent, descriptors, key, d)?;
        }
    }
    Ok(Value::Object(descriptors))
}

/// GetOwnPropertyKeys(O, type).
fn own_keys_of_type(agent: &mut Agent, v: Value, symbols: bool) -> JsResult<Value> {
    let o = ops::to_object(agent, v)?;
    let keys = internal::own_property_keys(agent, o)?;
    let list: Vec<Value> = keys
        .into_iter()
        .filter(|k| k.is_symbol() == symbols)
        .map(|k| agent.heap.key_to_value(k))
        .collect();
    Ok(Value::Object(ops::create_array_from_list(agent, &list)))
}

fn get_own_property_names(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    own_keys_of_type(agent, call.arg(0), false)
}

fn get_own_property_symbols(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    own_keys_of_type(agent, call.arg(0), true)
}

fn get_prototype_of(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let o = ops::to_object(agent, call.arg(0))?;
    let proto = internal::get_prototype_of(agent, o)?;
    Ok(Value::object_or_null(proto))
}

/// GroupBy(items, callback, property): the groups in insertion order.
pub(crate) fn group_by_property(
    agent: &mut Agent,
    items: Value,
    callback: Value,
) -> JsResult<Vec<(PropertyKey, Vec<Value>)>> {
    ops::require_object_coercible(agent, items)?;
    if !ops::is_callable(agent, callback) {
        return agent.type_error("groupBy callback is not a function");
    }
    let mut groups: Vec<(PropertyKey, Vec<Value>)> = Vec::new();
    let mut record = ops::get_iterator(agent, items)?;
    let mut k: f64 = 0.0;
    loop {
        if k >= 9007199254740991.0 {
            let error = agent.type_error("too many elements to group");
            return ops::iterator_close(agent, &record, error);
        }
        let Some(value) = ops::iterator_step_value(agent, &mut record)? else {
            return Ok(groups);
        };
        let key = ops::call(
            agent,
            callback,
            Value::Undefined,
            &[value, Value::Number(k)],
        );
        let key = or_close(agent, &record, key)?;
        let key = ops::to_property_key(agent, key);
        let key = or_close(agent, &record, key)?;
        match groups.iter_mut().find(|(g, _)| *g == key) {
            Some((_, elements)) => elements.push(value),
            None => groups.push((key, vec![value])),
        }
        k += 1.0;
    }
}

fn group_by(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let groups = group_by_property(agent, call.arg(0), call.arg(1))?;
    let obj = ops::ordinary_object_create(agent, None);
    for (key, elements) in groups {
        let array = ops::create_array_from_list(agent, &elements);
        ops::create_data_property_or_throw(agent, obj, key, Value::Object(array))?;
    }
    Ok(Value::Object(obj))
}

fn has_own(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let o = ops::to_object(agent, call.arg(0))?;
    let key = ops::to_property_key(agent, call.arg(1))?;
    Ok(Value::Boolean(ops::has_own_property(agent, o, key)?))
}

fn is(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    Ok(Value::Boolean(ops::same_value(
        agent,
        call.arg(0),
        call.arg(1),
    )))
}

/// The prototype argument of `setPrototypeOf` and `__proto__`: an object or
/// null.
fn proto_argument(v: Value) -> Option<Option<Object>> {
    match v {
        Value::Object(p) => Some(Some(p)),
        Value::Null => Some(None),
        _ => None,
    }
}

fn set_prototype_of(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let v = ops::require_object_coercible(agent, call.arg(0))?;
    let Some(proto) = proto_argument(call.arg(1)) else {
        return agent.type_error("Object prototype may only be an Object or null");
    };
    let Value::Object(o) = v else {
        return Ok(v);
    };
    if !internal::set_prototype_of(agent, o, proto)? {
        return agent.type_error("cannot set the prototype of the object");
    }
    Ok(v)
}

// ----- Object.prototype -----

fn has_own_property(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let key = ops::to_property_key(agent, call.arg(0))?;
    let o = ops::to_object(agent, call.this)?;
    Ok(Value::Boolean(ops::has_own_property(agent, o, key)?))
}

fn is_prototype_of(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let Value::Object(mut v) = call.arg(0) else {
        return Ok(Value::Boolean(false));
    };
    let o = ops::to_object(agent, call.this)?;
    loop {
        match internal::get_prototype_of(agent, v)? {
            None => return Ok(Value::Boolean(false)),
            Some(p) if p == o => return Ok(Value::Boolean(true)),
            Some(p) => v = p,
        }
    }
}

fn property_is_enumerable(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let key = ops::to_property_key(agent, call.arg(0))?;
    let o = ops::to_object(agent, call.this)?;
    let desc = internal::get_own_property(agent, o, key)?;
    Ok(Value::Boolean(
        desc.is_some_and(|d| d.enumerable == Some(true)),
    ))
}

fn to_locale_string(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let key = PropertyKey::String(agent.heap.names.to_string);
    ops::invoke(agent, call.this, key, &[])
}

/// `Object.prototype.toString` (§20.1.3.6).
fn to_string(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    match call.this {
        Value::Undefined => return Ok(str_value(agent, "[object Undefined]")),
        Value::Null => return Ok(str_value(agent, "[object Null]")),
        _ => {}
    }
    let o = ops::to_object(agent, call.this)?;
    let builtin_tag = if ops::is_array(agent, Value::Object(o))? {
        "Array"
    } else {
        let data = agent.heap.object(o);
        match &data.kind {
            ObjectKind::Arguments(_) | ObjectKind::UnmappedArguments => "Arguments",
            _ if data.is_callable() => "Function",
            ObjectKind::Error => "Error",
            ObjectKind::Boolean(_) => "Boolean",
            ObjectKind::Number(_) => "Number",
            ObjectKind::String(_) => "String",
            _ => "Object",
        }
    };
    let key = PropertyKey::Symbol(agent.symbols.to_string_tag);
    let tag = match ops::get(agent, o, key)? {
        Value::String(s) => agent.heap.string_units(s).to_vec(),
        _ => crate::string::utf16(builtin_tag),
    };
    let mut units = crate::string::utf16("[object ");
    units.extend_from_slice(&tag);
    units.push(u16::from(b']'));
    Ok(Value::String(agent.heap.alloc_string_owned(units)))
}

fn value_of(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    Ok(Value::Object(ops::to_object(agent, call.this)?))
}

fn proto_getter(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let o = ops::to_object(agent, call.this)?;
    let proto = internal::get_prototype_of(agent, o)?;
    Ok(Value::object_or_null(proto))
}

fn proto_setter(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let this = ops::require_object_coercible(agent, call.this)?;
    let Some(proto) = proto_argument(call.arg(0)) else {
        return Ok(Value::Undefined);
    };
    let Value::Object(o) = this else {
        return Ok(Value::Undefined);
    };
    if !internal::set_prototype_of(agent, o, proto)? {
        return agent.type_error("cannot set the prototype of the object");
    }
    Ok(Value::Undefined)
}

/// `__defineGetter__` and `__defineSetter__` (§B.2.2.2–3).
fn define_accessor(agent: &mut Agent, call: &NativeCall<'_>, getter: bool) -> JsResult<Value> {
    let o = ops::to_object(agent, call.this)?;
    let f = call.arg(1);
    if !ops::is_callable(agent, f) {
        return agent.type_error("accessor is not a function");
    }
    let desc = PropertyDescriptor {
        get: getter.then_some(f),
        set: (!getter).then_some(f),
        enumerable: Some(true),
        configurable: Some(true),
        ..PropertyDescriptor::default()
    };
    let key = ops::to_property_key(agent, call.arg(0))?;
    ops::define_property_or_throw(agent, o, key, desc)?;
    Ok(Value::Undefined)
}

fn define_getter(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    define_accessor(agent, &call, true)
}

fn define_setter(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    define_accessor(agent, &call, false)
}

/// `__lookupGetter__` and `__lookupSetter__` (§B.2.2.4–5).
fn lookup_accessor(agent: &mut Agent, call: &NativeCall<'_>, getter: bool) -> JsResult<Value> {
    let mut o = ops::to_object(agent, call.this)?;
    let key = ops::to_property_key(agent, call.arg(0))?;
    loop {
        if let Some(desc) = internal::get_own_property(agent, o, key)? {
            if desc.is_accessor() {
                let f = if getter { desc.get } else { desc.set };
                return Ok(f.unwrap_or(Value::Undefined));
            }
            return Ok(Value::Undefined);
        }
        match internal::get_prototype_of(agent, o)? {
            Some(p) => o = p,
            None => return Ok(Value::Undefined),
        }
    }
}

fn lookup_getter(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    lookup_accessor(agent, &call, true)
}

fn lookup_setter(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    lookup_accessor(agent, &call, false)
}
