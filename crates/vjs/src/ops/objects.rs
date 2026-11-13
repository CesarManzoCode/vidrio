//! Operations on objects (ECMA-262 §7.3) and the property descriptor
//! conversions of §6.2.6.

use crate::agent::Agent;
use crate::completion::JsResult;
use crate::function::{self, call_object, construct_object};
use crate::gc::{Object, Realm, Symbol};
use crate::internal;
use crate::object::{
    Attributes, ObjectKind, PrivateElement, PrivateElementKind, PropertyDescriptor, Slot,
};
use crate::ops::conversion::{to_boolean, to_length, to_object};
use crate::realm::Intrinsic;
use crate::value::{MAX_ARRAY_INDEX, PropertyKey, Value};

pub use crate::function::{is_callable, is_constructor};

/// OrdinaryObjectCreate(proto).
pub fn ordinary_object_create(agent: &mut Agent, proto: Option<Object>) -> Object {
    agent.heap.create_object(proto, ObjectKind::Ordinary)
}

/// An ordinary object whose prototype is `%Object.prototype%`.
pub fn object_create(agent: &mut Agent) -> Object {
    let proto = agent.intrinsic(Intrinsic::ObjectPrototype);
    ordinary_object_create(agent, Some(proto))
}

/// OrdinaryCreateFromConstructor(constructor, intrinsicDefaultProto,
/// internalSlotsList): the slots come as the object kind.
pub fn ordinary_create_from_constructor(
    agent: &mut Agent,
    constructor: Object,
    default: Intrinsic,
    kind: ObjectKind,
) -> JsResult<Object> {
    let proto = get_prototype_from_constructor(agent, constructor, default)?;
    Ok(agent.heap.create_object(Some(proto), kind))
}

/// GetPrototypeFromConstructor(constructor, intrinsicDefaultProto).
pub fn get_prototype_from_constructor(
    agent: &mut Agent,
    constructor: Object,
    default: Intrinsic,
) -> JsResult<Object> {
    let key = PropertyKey::String(agent.heap.names.prototype);
    if let Value::Object(p) = get(agent, constructor, key)? {
        return Ok(p);
    }
    let realm = get_function_realm(agent, constructor)?;
    Ok(agent.heap.realm(realm).intrinsics.get(default))
}

/// GetFunctionRealm(obj).
pub fn get_function_realm(agent: &mut Agent, obj: Object) -> JsResult<Realm> {
    let mut o = obj;
    loop {
        let next = match &agent.heap.object(o).kind {
            ObjectKind::BytecodeFunction(f) => return Ok(f.realm),
            ObjectKind::NativeFunction(f) => return Ok(f.realm),
            ObjectKind::BoundFunction(b) => Some(b.target),
            ObjectKind::Proxy(p) => match p.target {
                Some(t) if p.handler.is_some() => Some(t),
                _ => None,
            },
            _ => return Ok(agent.current_realm()),
        };
        match next {
            Some(t) => o = t,
            None => return agent.type_error("GetFunctionRealm on a revoked Proxy"),
        }
    }
}

/// Get(O, P).
pub fn get(agent: &mut Agent, o: Object, key: PropertyKey) -> JsResult<Value> {
    internal::get(agent, o, key, Value::Object(o))
}

/// GetV(V, P).
pub fn get_v(agent: &mut Agent, v: Value, key: PropertyKey) -> JsResult<Value> {
    if let Value::Object(o) = v {
        return get(agent, o, key);
    }
    let o = to_object(agent, v)?;
    internal::get(agent, o, key, v)
}

/// Set(O, P, V, Throw).
pub fn set(
    agent: &mut Agent,
    o: Object,
    key: PropertyKey,
    value: Value,
    throw: bool,
) -> JsResult<()> {
    let ok = internal::set(agent, o, key, value, Value::Object(o))?;
    if !ok && throw {
        let name = agent.heap.key_to_rust(key);
        return agent.type_error(&format!("cannot assign to read-only property '{name}'"));
    }
    Ok(())
}

/// CreateDataProperty(O, P, V).
pub fn create_data_property(
    agent: &mut Agent,
    o: Object,
    key: PropertyKey,
    value: Value,
) -> JsResult<bool> {
    internal::define_own_property(
        agent,
        o,
        key,
        PropertyDescriptor::data(value, true, true, true),
    )
}

/// CreateDataPropertyOrThrow(O, P, V).
pub fn create_data_property_or_throw(
    agent: &mut Agent,
    o: Object,
    key: PropertyKey,
    value: Value,
) -> JsResult<()> {
    if !create_data_property(agent, o, key, value)? {
        let name = agent.heap.key_to_rust(key);
        return agent.type_error(&format!("cannot define property '{name}'"));
    }
    Ok(())
}

/// CreateNonEnumerableDataPropertyOrThrow(O, P, V).
pub fn create_non_enumerable_data_property_or_throw(
    agent: &mut Agent,
    o: Object,
    key: PropertyKey,
    value: Value,
) -> JsResult<()> {
    define_property_or_throw(
        agent,
        o,
        key,
        PropertyDescriptor::data(value, true, false, true),
    )
}

/// DefinePropertyOrThrow(O, P, desc).
pub fn define_property_or_throw(
    agent: &mut Agent,
    o: Object,
    key: PropertyKey,
    desc: PropertyDescriptor,
) -> JsResult<()> {
    if !internal::define_own_property(agent, o, key, desc)? {
        let name = agent.heap.key_to_rust(key);
        return agent.type_error(&format!("cannot redefine property '{name}'"));
    }
    Ok(())
}

/// DeletePropertyOrThrow(O, P).
pub fn delete_property_or_throw(agent: &mut Agent, o: Object, key: PropertyKey) -> JsResult<()> {
    if !internal::delete(agent, o, key)? {
        let name = agent.heap.key_to_rust(key);
        return agent.type_error(&format!("cannot delete property '{name}'"));
    }
    Ok(())
}

/// GetMethod(V, P): `None` for undefined or null.
pub fn get_method(agent: &mut Agent, v: Value, key: PropertyKey) -> JsResult<Option<Value>> {
    let func = get_v(agent, v, key)?;
    if func.is_nullish() {
        return Ok(None);
    }
    if !is_callable(agent, func) {
        let name = agent.heap.key_to_rust(key);
        return agent.type_error(&format!("'{name}' is not a function"));
    }
    Ok(Some(func))
}

/// HasProperty(O, P).
pub fn has_property(agent: &mut Agent, o: Object, key: PropertyKey) -> JsResult<bool> {
    internal::has_property(agent, o, key)
}

/// HasOwnProperty(O, P).
pub fn has_own_property(agent: &mut Agent, o: Object, key: PropertyKey) -> JsResult<bool> {
    Ok(internal::get_own_property(agent, o, key)?.is_some())
}

/// Call(F, V, argumentsList).
pub fn call(agent: &mut Agent, f: Value, this: Value, args: &[Value]) -> JsResult<Value> {
    match f {
        Value::Object(o) if agent.heap.object(o).is_callable() => call_object(agent, o, this, args),
        _ => agent.type_error("value is not a function"),
    }
}

/// Construct(F, argumentsList, newTarget).
pub fn construct(
    agent: &mut Agent,
    f: Object,
    args: &[Value],
    new_target: Option<Object>,
) -> JsResult<Object> {
    if !agent.heap.object(f).is_constructor() {
        return agent.type_error("value is not a constructor");
    }
    construct_object(agent, f, args, new_target.unwrap_or(f))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IntegrityLevel {
    Sealed,
    Frozen,
}

/// SetIntegrityLevel(O, level).
pub fn set_integrity_level(agent: &mut Agent, o: Object, level: IntegrityLevel) -> JsResult<bool> {
    if !internal::prevent_extensions(agent, o)? {
        return Ok(false);
    }
    let keys = internal::own_property_keys(agent, o)?;
    for key in keys {
        let desc = match level {
            IntegrityLevel::Sealed => PropertyDescriptor {
                configurable: Some(false),
                ..PropertyDescriptor::default()
            },
            IntegrityLevel::Frozen => match internal::get_own_property(agent, o, key)? {
                None => continue,
                Some(current) if current.is_accessor() => PropertyDescriptor {
                    configurable: Some(false),
                    ..PropertyDescriptor::default()
                },
                Some(_) => PropertyDescriptor {
                    configurable: Some(false),
                    writable: Some(false),
                    ..PropertyDescriptor::default()
                },
            },
        };
        define_property_or_throw(agent, o, key, desc)?;
    }
    Ok(true)
}

/// TestIntegrityLevel(O, level).
pub fn test_integrity_level(agent: &mut Agent, o: Object, level: IntegrityLevel) -> JsResult<bool> {
    if internal::is_extensible(agent, o)? {
        return Ok(false);
    }
    let keys = internal::own_property_keys(agent, o)?;
    for key in keys {
        if let Some(d) = internal::get_own_property(agent, o, key)? {
            if d.configurable == Some(true) {
                return Ok(false);
            }
            if level == IntegrityLevel::Frozen && d.is_data() && d.writable == Some(true) {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

/// CreateArrayFromList(elements).
pub fn create_array_from_list(agent: &mut Agent, elements: &[Value]) -> Object {
    let a = internal::array_create(agent, 0.0, None).expect("length 0 is a valid array length");
    for (i, v) in elements.iter().enumerate() {
        agent.heap.set_own(
            a,
            PropertyKey::Index(i as u32),
            Attributes::ALL,
            Slot::Data(*v),
        );
    }
    let length = PropertyKey::String(agent.heap.names.length);
    agent
        .heap
        .set_own_value(a, length, Value::Number(elements.len() as f64));
    a
}

/// The property key of an integer index (array index or its string).
pub fn index_key(agent: &mut Agent, i: u64) -> PropertyKey {
    if i <= u64::from(MAX_ARRAY_INDEX) {
        PropertyKey::Index(i as u32)
    } else {
        agent.heap.key_from_str(&i.to_string())
    }
}

/// LengthOfArrayLike(obj).
pub fn length_of_array_like(agent: &mut Agent, o: Object) -> JsResult<f64> {
    let key = PropertyKey::String(agent.heap.names.length);
    let v = get(agent, o, key)?;
    to_length(agent, v)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListElementTypes {
    All,
    PropertyKey,
}

/// CreateListFromArrayLike(obj[, validElementTypes]).
pub fn create_list_from_array_like(
    agent: &mut Agent,
    v: Value,
    types: ListElementTypes,
) -> JsResult<Vec<Value>> {
    let Value::Object(o) = v else {
        return agent.type_error("CreateListFromArrayLike called on a non-object");
    };
    let len = length_of_array_like(agent, o)?;
    if len > f64::from(u32::MAX) {
        return agent.range_error("array-like object is too long");
    }
    let mut list = Vec::new();
    for i in 0..len as u64 {
        let key = index_key(agent, i);
        let next = get(agent, o, key)?;
        if types == ListElementTypes::PropertyKey
            && !matches!(next, Value::String(_) | Value::Symbol(_))
        {
            return agent.type_error("element is not a property key");
        }
        list.push(next);
    }
    Ok(list)
}

/// Invoke(V, P[, argumentsList]).
pub fn invoke(agent: &mut Agent, v: Value, key: PropertyKey, args: &[Value]) -> JsResult<Value> {
    let func = get_v(agent, v, key)?;
    call(agent, func, v, args)
}

/// OrdinaryHasInstance(C, O).
pub fn ordinary_has_instance(agent: &mut Agent, c: Value, o: Value) -> JsResult<bool> {
    if !is_callable(agent, c) {
        return Ok(false);
    }
    let Value::Object(c_obj) = c else {
        return Ok(false);
    };
    let bound_target = match &agent.heap.object(c_obj).kind {
        ObjectKind::BoundFunction(b) => Some(b.target),
        _ => None,
    };
    if let Some(target) = bound_target {
        return instanceof_operator(agent, o, Value::Object(target));
    }
    let Value::Object(mut obj) = o else {
        return Ok(false);
    };
    let key = PropertyKey::String(agent.heap.names.prototype);
    let Value::Object(p) = get(agent, c_obj, key)? else {
        return agent.type_error("function has a non-object prototype in instanceof check");
    };
    loop {
        match internal::get_prototype_of(agent, obj)? {
            None => return Ok(false),
            Some(x) if x == p => return Ok(true),
            Some(x) => obj = x,
        }
    }
}

/// InstanceofOperator(V, target).
pub fn instanceof_operator(agent: &mut Agent, v: Value, target: Value) -> JsResult<bool> {
    if !target.is_object() {
        return agent.type_error("right-hand side of instanceof is not an object");
    }
    let key = PropertyKey::Symbol(agent.symbols.has_instance);
    if let Some(handler) = get_method(agent, target, key)? {
        let result = call(agent, handler, target, &[v])?;
        return Ok(to_boolean(agent, result));
    }
    if !is_callable(agent, target) {
        return agent.type_error("right-hand side of instanceof is not callable");
    }
    ordinary_has_instance(agent, target, v)
}

/// SpeciesConstructor(O, defaultConstructor).
pub fn species_constructor(agent: &mut Agent, o: Object, default: Object) -> JsResult<Object> {
    let key = PropertyKey::String(agent.heap.names.constructor);
    let c = get(agent, o, key)?;
    let c = match c {
        Value::Undefined => return Ok(default),
        Value::Object(c) => c,
        _ => return agent.type_error("constructor property is not an object"),
    };
    let species = PropertyKey::Symbol(agent.symbols.species);
    let s = get(agent, c, species)?;
    if s.is_nullish() {
        return Ok(default);
    }
    match s {
        Value::Object(s) if agent.heap.object(s).is_constructor() => Ok(s),
        _ => agent.type_error("Symbol.species is not a constructor"),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnumerableKind {
    Key,
    Value,
    KeyValue,
}

/// EnumerableOwnProperties(O, kind).
pub fn enumerable_own_properties(
    agent: &mut Agent,
    o: Object,
    kind: EnumerableKind,
) -> JsResult<Vec<Value>> {
    let keys = internal::own_property_keys(agent, o)?;
    let mut results = Vec::new();
    for key in keys {
        if key.is_symbol() {
            continue;
        }
        let Some(desc) = internal::get_own_property(agent, o, key)? else {
            continue;
        };
        if desc.enumerable != Some(true) {
            continue;
        }
        let key_value = agent.heap.key_to_value(key);
        match kind {
            EnumerableKind::Key => results.push(key_value),
            EnumerableKind::Value => results.push(get(agent, o, key)?),
            EnumerableKind::KeyValue => {
                let value = get(agent, o, key)?;
                let entry = create_array_from_list(agent, &[key_value, value]);
                results.push(Value::Object(entry));
            }
        }
    }
    Ok(results)
}

/// CopyDataProperties(target, source, excludedItems).
pub fn copy_data_properties(
    agent: &mut Agent,
    target: Object,
    source: Value,
    excluded: &[PropertyKey],
) -> JsResult<()> {
    if source.is_nullish() {
        return Ok(());
    }
    let from = to_object(agent, source)?;
    let keys = internal::own_property_keys(agent, from)?;
    for key in keys {
        if excluded.contains(&key) {
            continue;
        }
        if let Some(desc) = internal::get_own_property(agent, from, key)? {
            if desc.enumerable == Some(true) {
                let value = get(agent, from, key)?;
                create_data_property_or_throw(agent, target, key, value)?;
            }
        }
    }
    Ok(())
}

/// PrivateElementFind(O, P).
pub fn private_element_find(agent: &Agent, o: Object, name: Symbol) -> Option<PrivateElement> {
    agent
        .heap
        .object(o)
        .private_elements()
        .iter()
        .find(|e| e.key == name)
        .copied()
}

/// PrivateFieldAdd(O, P, value).
pub fn private_field_add(agent: &mut Agent, o: Object, name: Symbol, value: Value) -> JsResult<()> {
    if private_element_find(agent, o, name).is_some() {
        return agent.type_error("private field already declared");
    }
    agent.heap.private_elements_mut(o).push(PrivateElement {
        key: name,
        kind: PrivateElementKind::Field(value),
    });
    Ok(())
}

/// PrivateMethodOrAccessorAdd(O, method).
pub fn private_method_or_accessor_add(
    agent: &mut Agent,
    o: Object,
    method: PrivateElement,
) -> JsResult<()> {
    if private_element_find(agent, o, method.key).is_some() {
        return agent.type_error("private method already declared");
    }
    agent.heap.private_elements_mut(o).push(method);
    Ok(())
}

/// PrivateGet(O, P).
pub fn private_get(agent: &mut Agent, o: Object, name: Symbol) -> JsResult<Value> {
    let Some(entry) = private_element_find(agent, o, name) else {
        return agent.type_error(
            "cannot read a private member from an object whose class did not declare it",
        );
    };
    match entry.kind {
        PrivateElementKind::Field(v) | PrivateElementKind::Method(v) => Ok(v),
        PrivateElementKind::Accessor { get, .. } => {
            if get.is_undefined() {
                return agent.type_error("private accessor has no getter");
            }
            call(agent, get, Value::Object(o), &[])
        }
    }
}

/// PrivateSet(O, P, value).
pub fn private_set(agent: &mut Agent, o: Object, name: Symbol, value: Value) -> JsResult<()> {
    let Some(entry) = private_element_find(agent, o, name) else {
        return agent.type_error(
            "cannot write a private member to an object whose class did not declare it",
        );
    };
    match entry.kind {
        PrivateElementKind::Field(_) => {
            if let Some(e) = agent
                .heap
                .private_elements_mut(o)
                .iter_mut()
                .find(|e| e.key == name)
            {
                e.kind = PrivateElementKind::Field(value);
            }
            Ok(())
        }
        PrivateElementKind::Method(_) => agent.type_error("private method is not writable"),
        PrivateElementKind::Accessor { set, .. } => {
            if set.is_undefined() {
                return agent.type_error("private accessor has no setter");
            }
            call(agent, set, Value::Object(o), &[value])?;
            Ok(())
        }
    }
}

/// ToPropertyDescriptor(Obj).
pub fn to_property_descriptor(agent: &mut Agent, v: Value) -> JsResult<PropertyDescriptor> {
    let Value::Object(o) = v else {
        return agent.type_error("property descriptor must be an object");
    };
    let names = agent.heap.names;
    let mut desc = PropertyDescriptor::default();
    let field = |agent: &mut Agent, name| -> JsResult<Option<Value>> {
        let key = PropertyKey::String(name);
        if has_property(agent, o, key)? {
            Ok(Some(get(agent, o, key)?))
        } else {
            Ok(None)
        }
    };
    if let Some(v) = field(agent, names.enumerable)? {
        desc.enumerable = Some(to_boolean(agent, v));
    }
    if let Some(v) = field(agent, names.configurable)? {
        desc.configurable = Some(to_boolean(agent, v));
    }
    if let Some(v) = field(agent, names.value)? {
        desc.value = Some(v);
    }
    if let Some(v) = field(agent, names.writable)? {
        desc.writable = Some(to_boolean(agent, v));
    }
    if let Some(g) = field(agent, names.get)? {
        if !g.is_undefined() && !is_callable(agent, g) {
            return agent.type_error("getter must be a function");
        }
        desc.get = Some(g);
    }
    if let Some(s) = field(agent, names.set)? {
        if !s.is_undefined() && !is_callable(agent, s) {
            return agent.type_error("setter must be a function");
        }
        desc.set = Some(s);
    }
    if desc.is_accessor() && desc.is_data() {
        return agent.type_error(
            "a property descriptor cannot have both accessors and a value or writable",
        );
    }
    Ok(desc)
}

/// FromPropertyDescriptor(Desc).
pub fn from_property_descriptor(agent: &mut Agent, desc: Option<PropertyDescriptor>) -> Value {
    let Some(desc) = desc else {
        return Value::Undefined;
    };
    let o = object_create(agent);
    let names = agent.heap.names;
    let fields = [
        (names.value, desc.value),
        (names.writable, desc.writable.map(Value::Boolean)),
        (names.get, desc.get),
        (names.set, desc.set),
        (names.enumerable, desc.enumerable.map(Value::Boolean)),
        (names.configurable, desc.configurable.map(Value::Boolean)),
    ];
    for (name, value) in fields {
        if let Some(v) = value {
            agent
                .heap
                .set_own(o, PropertyKey::String(name), Attributes::ALL, Slot::Data(v));
        }
    }
    Value::Object(o)
}

/// InstallErrorCause(O, options).
pub fn install_error_cause(agent: &mut Agent, o: Object, options: Value) -> JsResult<()> {
    let Value::Object(opts) = options else {
        return Ok(());
    };
    let key = PropertyKey::String(agent.heap.names.cause);
    if has_property(agent, opts, key)? {
        let cause = get(agent, opts, key)?;
        create_non_enumerable_data_property_or_throw(agent, o, key, cause)?;
    }
    Ok(())
}

/// Defines a method of a built-in object: `{ writable, !enumerable,
/// configurable }`.
pub fn define_builtin_method(agent: &mut Agent, o: Object, key: PropertyKey, f: Object) {
    let desc = function::method_descriptor(f);
    let (attrs, slot) = desc.to_property();
    agent.heap.set_own(o, key, attrs, slot);
}
