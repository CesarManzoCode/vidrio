//! The core built-ins (ECMA-262 §19–§20, §28): `Object`, `Function` and the
//! dynamic function constructors, the errors, `Symbol`, `Boolean`,
//! `Reflect`, `Proxy`, the function properties of the global object, and
//! the numeric built-ins `Number`, `BigInt` and `Math` (§21).
//!
//! Every property gets the attributes, `length` and `name` the
//! specification gives it: methods are `{ writable, !enumerable,
//! configurable }` and constructors link to their prototypes with
//! non-writable, non-configurable `prototype` properties.

pub mod error;
pub mod function;
pub mod global;
pub mod math;
pub mod number;
pub mod object;
pub mod reflect;
pub mod symbol;

use crate::agent::Agent;
use crate::function::{
    NativeFn, create_builtin_function, make_constructor, set_native_constructor,
};
use crate::gc::{Object, Realm};
use crate::object::{Attributes, ObjectKind, Slot};
use crate::ops;
use crate::realm::Intrinsic;
use crate::value::{PropertyKey, Value};

/// Installs the core built-ins in `realm` (an [`crate::realm::IntrinsicInstaller`]).
pub fn install(agent: &mut Agent, realm: Realm) {
    object::install(agent, realm);
    function::install(agent, realm);
    error::install(agent, realm);
    symbol::install(agent, realm);
    reflect::install(agent, realm);
    global::install(agent, realm);
    // After `global`: `Number.parseFloat` and `Number.parseInt` are the
    // global functions.
    number::install(agent, realm);
    math::install(agent, realm);
}

pub(crate) fn intrinsic(agent: &Agent, realm: Realm, which: Intrinsic) -> Object {
    agent.heap.realm(realm).intrinsics.get(which)
}

pub(crate) fn set_intrinsic(agent: &mut Agent, realm: Realm, which: Intrinsic, o: Object) {
    agent.heap.realm_mut(realm).intrinsics.set(which, o);
}

/// A built-in function of `realm` named `name`.
pub(crate) fn builtin_function(
    agent: &mut Agent,
    realm: Realm,
    name: &str,
    f: NativeFn,
    length: u32,
) -> Object {
    let key = agent.heap.key_from_str(name);
    create_builtin_function(agent, f, length, key, Some(realm), None, None)
}

/// A method `{ writable, !enumerable, configurable }` of `target`.
pub(crate) fn method(
    agent: &mut Agent,
    realm: Realm,
    target: Object,
    name: &str,
    f: NativeFn,
    length: u32,
) -> Object {
    let key = agent.heap.key_from_str(name);
    let func = create_builtin_function(agent, f, length, key, Some(realm), None, None);
    ops::define_builtin_method(agent, target, key, func);
    func
}

/// An accessor `{ !enumerable, configurable }` of `target`; the getter is
/// named `get <name>` and the setter `set <name>`.
pub(crate) fn accessor(
    agent: &mut Agent,
    realm: Realm,
    target: Object,
    key: PropertyKey,
    get: Option<NativeFn>,
    set: Option<NativeFn>,
) {
    let getter = get.map_or(Value::Undefined, |f| {
        Value::Object(create_builtin_function(
            agent,
            f,
            0,
            key,
            Some(realm),
            None,
            Some("get"),
        ))
    });
    let setter = set.map_or(Value::Undefined, |f| {
        Value::Object(create_builtin_function(
            agent,
            f,
            1,
            key,
            Some(realm),
            None,
            Some("set"),
        ))
    });
    agent.heap.set_own(
        target,
        key,
        Attributes::accessor(false, true),
        Slot::Accessor {
            get: getter,
            set: setter,
        },
    );
}

/// A data property with the given attributes.
pub(crate) fn value(
    agent: &mut Agent,
    target: Object,
    key: PropertyKey,
    v: Value,
    writable: bool,
    enumerable: bool,
    configurable: bool,
) {
    agent.heap.set_own(
        target,
        key,
        Attributes::data(writable, enumerable, configurable),
        Slot::Data(v),
    );
}

/// `@@toStringTag`: `{ !writable, !enumerable, configurable }`.
pub(crate) fn to_string_tag(agent: &mut Agent, target: Object, tag: &str) {
    let key = PropertyKey::Symbol(agent.symbols.to_string_tag);
    let v = Value::String(agent.heap.alloc_str(tag));
    value(agent, target, key, v, false, false, true);
}

/// A built-in constructor linked to `prototype` (`F.prototype` is
/// `{ !writable, !enumerable, !configurable }`, `prototype.constructor` is
/// `{ writable, !enumerable, configurable }`). `parent` is the
/// constructor's `[[Prototype]]` (`%Function.prototype%` if `None`).
pub(crate) fn constructor(
    agent: &mut Agent,
    realm: Realm,
    name: &str,
    f: NativeFn,
    length: u32,
    prototype: Object,
    parent: Option<Object>,
) -> Object {
    let key = agent.heap.key_from_str(name);
    let ctor = create_builtin_function(agent, f, length, key, Some(realm), parent, None);
    set_native_constructor(agent, ctor);
    make_constructor(agent, ctor, false, Some(prototype));
    let key = PropertyKey::String(agent.heap.names.constructor);
    value(
        agent,
        prototype,
        key,
        Value::Object(ctor),
        true,
        false,
        true,
    );
    ctor
}

/// An ordinary object whose prototype is `proto`.
pub(crate) fn plain_object(agent: &mut Agent, proto: Object) -> Object {
    agent.heap.create_object(Some(proto), ObjectKind::Ordinary)
}

/// A string value from Rust text.
pub(crate) fn str_value(agent: &mut Agent, s: &str) -> Value {
    Value::String(agent.heap.alloc_str(s))
}
