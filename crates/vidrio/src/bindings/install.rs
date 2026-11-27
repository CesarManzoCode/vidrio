//! Interface objects, interface prototype objects, namespaces and their
//! members (Web IDL §3.7): the property layout every idlharness test
//! checks, and the native functions behind attributes, operations,
//! constructors, iterators and `maplike`/`setlike` members.

use std::rc::Rc;

use vjs::agent::ExecutionContext;
use vjs::function::{
    NativeCall, NativeFn, create_builtin_function, native_slots, native_slots_mut,
    set_native_constructor,
};
use vjs::gc::Realm;
use vjs::object::{Attributes, ObjectKind, Slot};
use vjs::ops;
use vjs::realm::Intrinsic;
use vjs::{Agent, JsResult, Object, PropertyKey, Throw, Value};

use super::convert::{to_idl, to_js};
use super::generated::{INTERFACES, interface_by_name};
use super::imp;
use super::meta::{Attribute, Id, Iterable, Kind, Operation, Stringifier, Ty};
use super::overload;
use super::platform::{self, IterKind, Native, PlatformObject, PlatformRef};
use super::value::{IdlValue, Invocation};
use super::{RealmState, state_of};

// ----- helpers -------------------------------------------------------------------

fn num(x: usize) -> Value {
    Value::Number(x as f64)
}

/// Slot `i` of a native function, as an index.
fn slot(agent: &Agent, f: Object, i: usize) -> usize {
    native_slots(agent, f)
        .get(i)
        .and_then(|v| v.as_number())
        .unwrap_or(0.0) as usize
}

fn intrinsic(agent: &Agent, realm: Realm, which: Intrinsic) -> Object {
    agent.heap.realm(realm).intrinsics.get(which)
}

/// A native function of `realm` with traced `slots`.
pub fn make_fn(
    agent: &mut Agent,
    realm: Realm,
    behavior: NativeFn,
    length: u32,
    name: PropertyKey,
    prefix: Option<&str>,
    slots: Vec<Value>,
) -> Object {
    let f = create_builtin_function(agent, behavior, length, name, Some(realm), None, prefix);
    if let Some(s) = native_slots_mut(agent, f) {
        *s = slots;
    }
    f
}

fn define(agent: &mut Agent, o: Object, key: PropertyKey, value: Value, w: bool, e: bool, c: bool) {
    agent
        .heap
        .set_own(o, key, Attributes::data(w, e, c), Slot::Data(value));
}

fn define_str(agent: &mut Agent, o: Object, name: &str, value: Value, w: bool, e: bool, c: bool) {
    let key = agent.heap.key_from_str(name);
    define(agent, o, key, value, w, e, c);
}

fn define_accessor(
    agent: &mut Agent,
    o: Object,
    key: PropertyKey,
    get: Value,
    set: Value,
    e: bool,
    c: bool,
) {
    agent.heap.set_own(
        o,
        key,
        Attributes::accessor(e, c),
        Slot::Accessor { get, set },
    );
}

fn str_value(agent: &mut Agent, s: &str) -> Value {
    Value::String(agent.heap.alloc_str(s))
}

/// True if a member restricted to secure contexts is left out of the
/// realm.
fn hidden(state: &RealmState, secure_context: bool) -> bool {
    secure_context && !state.options.secure_context
}

fn is_promise_type(ty: Ty) -> bool {
    matches!(ty.inner(), Ty::Promise(_))
}

/// A promise rejected with the thrown value (operations that return
/// promises never throw, Web IDL §3.7.4).
fn rejected(agent: &mut Agent, e: Throw) -> JsResult<Value> {
    let realm = agent.current_realm();
    let cap = vjs::interp::promise::new_intrinsic_capability(agent, realm);
    ops::call(agent, cap.reject, Value::Undefined, &[e.0])?;
    Ok(cap.promise)
}

// ----- the window ---------------------------------------------------------------

/// The `Window` interface, or `EventTarget` if html.idl is missing.
pub fn window_interface() -> Id {
    interface_by_name("Window").unwrap_or(super::generated::ids::EventTarget)
}

/// Creates the realm's global object and installs every interface,
/// namespace and global member.
pub fn install_window(agent: &mut Agent, state: &Rc<RealmState>) {
    let realm = state.realm;
    agent.push_context(ExecutionContext {
        realm,
        function: None,
        script_or_module: None,
    });
    let window = window_interface();
    let object_proto = intrinsic(agent, realm, Intrinsic::ObjectPrototype);
    let global = agent.heap.create_object(
        Some(object_proto),
        ObjectKind::Host(Rc::new(PlatformObject::new(window, realm, Native::Window))),
    );
    state.global.set(Some(global));
    agent.set_realm_global_object(realm, Some(global), state.html.proxy.get());
    if agent.set_default_global_bindings(realm).is_err() {
        panic!("the global object refused the default bindings");
    }
    for id in 0..INTERFACES.len() {
        ensure_interface(agent, state, id as Id);
    }
    if let Some(p) = state.proto(window) {
        agent.heap.set_proto(global, Some(p));
    }
    define_instance_members(agent, state, global, window, true);
    agent.pop_context();
}

/// Creates the interface object and interface prototype object of `id`
/// (and of its ancestors) if they do not exist yet; returns the
/// prototype.
fn ensure_interface(agent: &mut Agent, state: &RealmState, id: Id) -> Option<Object> {
    if let Some(p) = state.proto(id) {
        return Some(p);
    }
    let def = &INTERFACES[id as usize];
    let realm = state.realm;
    let global = state.global.get()?;
    if def.secure_context && !state.options.secure_context {
        return None;
    }
    match def.kind {
        Kind::Namespace => {
            if def.exposed && state.ctor(id).is_none() {
                install_namespace(agent, state, id, global);
            }
            return None;
        }
        Kind::CallbackInterface => {
            if def.exposed
                && !def.constants.is_empty()
                && !def.no_interface_object
                && state.ctor(id).is_none()
            {
                let key = agent.heap.key_from_str(def.name);
                let f = make_fn(
                    agent,
                    realm,
                    callback_interface_object,
                    0,
                    key,
                    None,
                    Vec::new(),
                );
                define_constants(agent, f, id);
                state.ctors.borrow_mut()[id as usize] = Some(f);
                define_str(agent, global, def.name, Value::Object(f), true, false, true);
            }
            return None;
        }
        Kind::Interface => {}
    }
    let parent_proto = match def.parent {
        Some(p) => ensure_interface(agent, state, p),
        None if def.name == "DOMException" => {
            Some(intrinsic(agent, realm, Intrinsic::ErrorPrototype))
        }
        None => None,
    }
    .unwrap_or_else(|| intrinsic(agent, realm, Intrinsic::ObjectPrototype));
    let proto = agent
        .heap
        .create_object(Some(parent_proto), ObjectKind::Ordinary);
    state.protos.borrow_mut()[id as usize] = Some(proto);

    let parent_ctor = def
        .parent
        .and_then(|p| state.ctor(p))
        .unwrap_or_else(|| intrinsic(agent, realm, Intrinsic::FunctionPrototype));
    let length = def.constructor.map_or(0, |c| c.length);
    let key = agent.heap.key_from_str(def.name);
    let ctor = make_fn(
        agent,
        realm,
        interface_object,
        length,
        key,
        None,
        vec![num(id as usize)],
    );
    set_native_constructor(agent, ctor);
    agent.heap.set_proto(ctor, Some(parent_ctor));
    let prototype_key = PropertyKey::String(agent.heap.names.prototype);
    define(
        agent,
        ctor,
        prototype_key,
        Value::Object(proto),
        false,
        false,
        false,
    );
    let constructor_key = PropertyKey::String(agent.heap.names.constructor);
    define(
        agent,
        proto,
        constructor_key,
        Value::Object(ctor),
        true,
        false,
        true,
    );
    state.ctors.borrow_mut()[id as usize] = Some(ctor);

    define_constants(agent, ctor, id);
    define_constants(agent, proto, id);

    // Static members on the interface object; regular members on the
    // prototype (on the global object itself for [Global] interfaces).
    for (i, a) in def.attributes.iter().enumerate() {
        if hidden(state, a.secure_context) {
            continue;
        }
        if a.is_static {
            define_attribute(agent, realm, ctor, id, i, a);
        } else if !a.unforgeable && !def.global {
            define_attribute(agent, realm, proto, id, i, a);
        }
    }
    for (i, op) in def.operations.iter().enumerate() {
        if hidden(state, op.secure_context) {
            continue;
        }
        if op.is_static {
            define_operation(agent, realm, ctor, id, i, op);
        } else if !op.unforgeable && !def.global {
            define_operation(agent, realm, proto, id, i, op);
        }
    }
    if !def.global {
        define_stringifier(agent, realm, proto, id);
    }
    define_iterable(agent, state, proto, id);

    let tag = agent.symbols.to_string_tag;
    let name = str_value(agent, def.name);
    define(
        agent,
        proto,
        PropertyKey::Symbol(tag),
        name,
        false,
        false,
        true,
    );
    define_unscopables(agent, proto, id);

    for (i, f) in def.legacy_factories.iter().enumerate() {
        let key = agent.heap.key_from_str(f.name);
        let factory = make_fn(
            agent,
            realm,
            legacy_factory,
            f.length,
            key,
            None,
            vec![num(id as usize), num(i)],
        );
        set_native_constructor(agent, factory);
        define(
            agent,
            factory,
            prototype_key,
            Value::Object(proto),
            false,
            false,
            false,
        );
        define_str(
            agent,
            global,
            f.name,
            Value::Object(factory),
            true,
            false,
            true,
        );
    }
    if def.exposed && !def.no_interface_object {
        define_str(
            agent,
            global,
            def.name,
            Value::Object(ctor),
            true,
            false,
            true,
        );
        for alias in def.window_aliases {
            define_str(agent, global, alias, Value::Object(ctor), true, false, true);
        }
    }
    Some(proto)
}

fn define_constants(agent: &mut Agent, o: Object, id: Id) {
    for c in INTERFACES[id as usize].constants {
        let v = match c.value {
            super::meta::DefaultValue::Bool(b) => Value::Boolean(b),
            super::meta::DefaultValue::Number(n) => Value::Number(n),
            _ => Value::Null,
        };
        define_str(agent, o, c.name, v, false, true, false);
    }
}

fn install_namespace(agent: &mut Agent, state: &RealmState, id: Id, global: Object) {
    let def = &INTERFACES[id as usize];
    let realm = state.realm;
    let object_proto = intrinsic(agent, realm, Intrinsic::ObjectPrototype);
    let ns = agent
        .heap
        .create_object(Some(object_proto), ObjectKind::Ordinary);
    define_constants(agent, ns, id);
    for (i, a) in def.attributes.iter().enumerate() {
        if !hidden(state, a.secure_context) {
            define_attribute(agent, realm, ns, id, i, a);
        }
    }
    for (i, op) in def.operations.iter().enumerate() {
        if !hidden(state, op.secure_context) {
            define_operation(agent, realm, ns, id, i, op);
        }
    }
    let tag = agent.symbols.to_string_tag;
    let name = str_value(agent, def.name);
    define(
        agent,
        ns,
        PropertyKey::Symbol(tag),
        name,
        false,
        false,
        true,
    );
    state.ctors.borrow_mut()[id as usize] = Some(ns);
    define_str(
        agent,
        global,
        def.name,
        Value::Object(ns),
        true,
        false,
        true,
    );
}

fn define_attribute(agent: &mut Agent, realm: Realm, o: Object, id: Id, i: usize, a: &Attribute) {
    let key = agent.heap.key_from_str(a.name);
    let slots = vec![num(id as usize), num(i)];
    let getter = make_fn(
        agent,
        realm,
        attribute_getter,
        0,
        key,
        Some("get"),
        slots.clone(),
    );
    let has_setter =
        a.setter.is_some() || a.put_forwards.is_some() || a.replaceable || a.lenient_setter;
    let setter = if has_setter {
        Value::Object(make_fn(
            agent,
            realm,
            attribute_setter,
            1,
            key,
            Some("set"),
            slots,
        ))
    } else {
        Value::Undefined
    };
    define_accessor(
        agent,
        o,
        key,
        Value::Object(getter),
        setter,
        true,
        !a.unforgeable,
    );
}

fn define_operation(agent: &mut Agent, realm: Realm, o: Object, id: Id, i: usize, op: &Operation) {
    let key = agent.heap.key_from_str(op.name);
    let f = make_fn(
        agent,
        realm,
        operation,
        op.length,
        key,
        None,
        vec![num(id as usize), num(i)],
    );
    define(
        agent,
        o,
        key,
        Value::Object(f),
        !op.unforgeable,
        true,
        !op.unforgeable,
    );
}

fn define_stringifier(agent: &mut Agent, realm: Realm, proto: Object, id: Id) {
    let def = &INTERFACES[id as usize];
    if matches!(def.stringifier, Stringifier::None) {
        return;
    }
    if def
        .operations
        .iter()
        .any(|o| o.name == "toString" && !o.is_static)
    {
        return;
    }
    let key = agent.heap.key_from_str("toString");
    let f = make_fn(
        agent,
        realm,
        stringifier,
        0,
        key,
        None,
        vec![num(id as usize)],
    );
    let unforgeable = def.attributes.iter().any(|a| {
        a.unforgeable && matches!(def.stringifier, Stringifier::Attribute(op) if op == a.getter)
    });
    if unforgeable {
        return;
    }
    define(agent, proto, key, Value::Object(f), true, true, true);
}

fn define_unscopables(agent: &mut Agent, proto: Object, id: Id) {
    let def = &INTERFACES[id as usize];
    let names: Vec<&str> = def
        .attributes
        .iter()
        .filter(|a| a.unscopable && !a.is_static)
        .map(|a| a.name)
        .chain(
            def.operations
                .iter()
                .filter(|o| o.unscopable && !o.is_static)
                .map(|o| o.name),
        )
        .collect();
    if names.is_empty() {
        return;
    }
    let u = agent.heap.create_object(None, ObjectKind::Ordinary);
    for n in names {
        define_str(agent, u, n, Value::Boolean(true), true, true, true);
    }
    let key = PropertyKey::Symbol(agent.symbols.unscopables);
    define(agent, proto, key, Value::Object(u), false, false, true);
}

/// The names of the `maplike`/`setlike` methods, by slot index.
const COLLECTION_METHODS: &[&str] = &[
    "size", "entries", "keys", "values", "forEach", "get", "has", "set", "delete", "clear", "add",
];

fn define_iterable(agent: &mut Agent, state: &RealmState, proto: Object, id: Id) {
    let def = &INTERFACES[id as usize];
    let realm = state.realm;
    let iterator = PropertyKey::Symbol(agent.symbols.iterator);
    match &def.iterable {
        Iterable::None => {
            // An indexed getter and an integer `length` make the interface
            // iterable with %Array.prototype.values% (Web IDL §3.7.6).
            let integer_length = def.attributes.iter().any(|a| {
                a.name == "length"
                    && !a.is_static
                    && matches!(
                        a.ty,
                        Ty::Byte
                            | Ty::Octet
                            | Ty::Short
                            | Ty::UnsignedShort
                            | Ty::Long
                            | Ty::UnsignedLong
                            | Ty::LongLong
                            | Ty::UnsignedLongLong
                    )
            });
            if def.special.indexed_getter.is_some() && integer_length {
                let values = intrinsic(agent, realm, Intrinsic::ArrayPrototypeValues);
                define(
                    agent,
                    proto,
                    iterator,
                    Value::Object(values),
                    true,
                    false,
                    true,
                );
            }
        }
        Iterable::Value(_) => {
            let array_proto = intrinsic(agent, realm, Intrinsic::ArrayPrototype);
            for name in ["entries", "keys", "values", "forEach"] {
                let key = agent.heap.key_from_str(name);
                let f = ops::get(agent, array_proto, key).unwrap_or(Value::Undefined);
                define(agent, proto, key, f, true, true, true);
            }
            let values = intrinsic(agent, realm, Intrinsic::ArrayPrototypeValues);
            define(
                agent,
                proto,
                iterator,
                Value::Object(values),
                true,
                false,
                true,
            );
        }
        Iterable::Pair { .. } => {
            let iter_proto_parent = intrinsic(agent, realm, Intrinsic::IteratorPrototype);
            let iter_proto = agent
                .heap
                .create_object(Some(iter_proto_parent), ObjectKind::Ordinary);
            let next_key = PropertyKey::String(agent.heap.names.next);
            let next = make_fn(
                agent,
                realm,
                pair_iterator_next,
                0,
                next_key,
                None,
                vec![num(id as usize)],
            );
            define(
                agent,
                iter_proto,
                next_key,
                Value::Object(next),
                true,
                true,
                true,
            );
            let tag = agent.symbols.to_string_tag;
            let tag_value = str_value(agent, &format!("{} Iterator", def.name));
            define(
                agent,
                iter_proto,
                PropertyKey::Symbol(tag),
                tag_value,
                false,
                false,
                true,
            );
            state.iterator_protos.borrow_mut()[id as usize] = Some(iter_proto);
            let mut entries = None;
            for (kind, name) in [(0usize, "entries"), (1, "keys"), (2, "values")] {
                let key = agent.heap.key_from_str(name);
                let f = make_fn(
                    agent,
                    realm,
                    pair_iterator_method,
                    0,
                    key,
                    None,
                    vec![num(id as usize), num(kind)],
                );
                define(agent, proto, key, Value::Object(f), true, true, true);
                if kind == 0 {
                    entries = Some(f);
                }
            }
            let key = agent.heap.key_from_str("forEach");
            let f = make_fn(
                agent,
                realm,
                pair_for_each,
                1,
                key,
                None,
                vec![num(id as usize)],
            );
            define(agent, proto, key, Value::Object(f), true, true, true);
            if let Some(e) = entries {
                define(agent, proto, iterator, Value::Object(e), true, false, true);
            }
        }
        Iterable::Maplike { readonly, .. } | Iterable::Setlike { readonly, .. } => {
            let map = matches!(def.iterable, Iterable::Maplike { .. });
            let mut methods: Vec<usize> = vec![1, 2, 3, 4, 6];
            if map {
                methods.push(5);
            }
            if !readonly {
                methods.extend([8, 9]);
                methods.push(if map { 7 } else { 10 });
            }
            let mut iterator_fn = None;
            for m in methods {
                let name = COLLECTION_METHODS[m];
                // An operation the interface declares with the same name
                // replaces the default one (Web IDL §3.7.11, §3.7.12).
                if def
                    .operations
                    .iter()
                    .any(|op| op.name == name && !op.is_static)
                {
                    continue;
                }
                let key = agent.heap.key_from_str(name);
                let length = match name {
                    "entries" | "keys" | "values" | "clear" => 0,
                    "set" => 2,
                    _ => 1,
                };
                let f = make_fn(
                    agent,
                    realm,
                    collection_method,
                    length,
                    key,
                    None,
                    vec![num(id as usize), num(m)],
                );
                define(agent, proto, key, Value::Object(f), true, true, true);
                if (map && name == "entries") || (!map && name == "values") {
                    iterator_fn = Some(f);
                }
            }
            if !map {
                // `keys` is the same function as `values` for setlike.
                let values_key = agent.heap.key_from_str("values");
                let keys_key = agent.heap.key_from_str("keys");
                if let Some((_, Slot::Data(v))) = agent.heap.get_own(proto, values_key) {
                    define(agent, proto, keys_key, v, true, true, true);
                }
            }
            let size_key = agent.heap.key_from_str("size");
            let size = make_fn(
                agent,
                realm,
                collection_method,
                0,
                size_key,
                Some("get"),
                vec![num(id as usize), num(0)],
            );
            define_accessor(
                agent,
                proto,
                size_key,
                Value::Object(size),
                Value::Undefined,
                true,
                true,
            );
            if let Some(f) = iterator_fn {
                define(agent, proto, iterator, Value::Object(f), true, false, true);
            }
        }
    }
}

/// Defines the members of `iface` (and its ancestors) that live on its
/// instances: `[LegacyUnforgeable]` ones, and every regular member of a
/// `[Global]` interface when `all` is set.
fn define_instance_members(agent: &mut Agent, state: &RealmState, o: Object, iface: Id, all: bool) {
    let realm = state.realm;
    let mut next = Some(iface);
    while let Some(id) = next {
        let def = &INTERFACES[id as usize];
        let everything = all && def.global;
        for (i, a) in def.attributes.iter().enumerate() {
            if !a.is_static && (a.unforgeable || everything) && !hidden(state, a.secure_context) {
                define_attribute(agent, realm, o, id, i, a);
            }
        }
        for (i, op) in def.operations.iter().enumerate() {
            if !op.is_static && (op.unforgeable || everything) && !hidden(state, op.secure_context)
            {
                define_operation(agent, realm, o, id, i, op);
            }
        }
        if everything {
            define_stringifier(agent, realm, o, id);
        }
        next = def.parent;
    }
}

/// Defines the `[LegacyUnforgeable]` members on a new platform object.
pub fn define_unforgeables(agent: &mut Agent, state: &RealmState, o: Object, iface: Id) {
    let mut any = false;
    let mut next = Some(iface);
    while let Some(id) = next {
        let def = &INTERFACES[id as usize];
        any |= def.attributes.iter().any(|a| a.unforgeable && !a.is_static)
            || def
                .operations
                .iter()
                .any(|op| op.unforgeable && !op.is_static);
        next = def.parent;
    }
    if any {
        define_instance_members(agent, state, o, iface, false);
    }
}

/// GetPrototypeFromConstructor for an interface: `newTarget.prototype`,
/// or the interface prototype object of `newTarget`'s realm.
pub fn proto_from_new_target(
    agent: &mut Agent,
    new_target: Option<Object>,
    iface: Id,
) -> JsResult<Option<Object>> {
    let Some(nt) = new_target else {
        return Ok(None);
    };
    let key = PropertyKey::String(agent.heap.names.prototype);
    if let Value::Object(p) = ops::get(agent, nt, key)? {
        return Ok(Some(p));
    }
    let realm = ops::get_function_realm(agent, nt)?;
    Ok(state_of(agent, realm).and_then(|s| s.proto(iface)))
}

// ----- native behaviours ---------------------------------------------------------

/// The `this` platform object of a regular member of `iface`; `null` and
/// `undefined` mean the global object. With `lenient`, a wrong `this`
/// gives `None` instead of a `TypeError`.
fn this_for(
    agent: &mut Agent,
    this: Value,
    iface: Id,
    lenient: bool,
) -> JsResult<Option<PlatformRef>> {
    let this = if this.is_nullish() {
        Value::Object(agent.global_object())
    } else {
        this
    };
    match platform::platform_implementing(agent, this, iface) {
        Some(p) => Ok(Some(p)),
        None if lenient => Ok(None),
        None => agent.type_error(&format!(
            "Illegal invocation: the object does not implement interface {}.",
            INTERFACES[iface as usize].name
        )),
    }
}

fn invoke(agent: &mut Agent, inv: Invocation) -> JsResult<IdlValue> {
    let mut inv = inv;
    imp::call(agent, &mut inv)
}

fn interface_object(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let id = slot(agent, call.callee, 0) as Id;
    let def = &INTERFACES[id as usize];
    let Some(new_target) = call.new_target else {
        return agent.type_error(&format!(
            "Failed to construct '{}': Please use the 'new' operator.",
            def.name
        ));
    };
    let Some(ctor) = def.constructor else {
        return agent.type_error("Illegal constructor");
    };
    let r = overload::resolve(agent, ctor, call.args)?;
    let v = invoke(
        agent,
        Invocation {
            op: r.overload.op,
            overload: r.overload.index,
            iface: id,
            this: None,
            args: r.args,
            new_target: Some(new_target),
        },
    )?;
    match v {
        IdlValue::Object(o) => Ok(Value::Object(o)),
        _ => agent.type_error("Illegal constructor"),
    }
}

fn legacy_factory(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let id = slot(agent, call.callee, 0) as Id;
    let i = slot(agent, call.callee, 1);
    let f = &INTERFACES[id as usize].legacy_factories[i];
    let Some(new_target) = call.new_target else {
        return agent.type_error(&format!(
            "Failed to construct '{}': Please use the 'new' operator.",
            f.name
        ));
    };
    let r = overload::resolve(agent, f, call.args)?;
    let v = invoke(
        agent,
        Invocation {
            op: r.overload.op,
            overload: r.overload.index,
            iface: id,
            this: None,
            args: r.args,
            new_target: Some(new_target),
        },
    )?;
    match v {
        IdlValue::Object(o) => Ok(Value::Object(o)),
        _ => agent.type_error("Illegal constructor"),
    }
}

fn callback_interface_object(agent: &mut Agent, _call: NativeCall<'_>) -> JsResult<Value> {
    agent.type_error("Illegal constructor")
}

fn attribute_getter(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let id = slot(agent, call.callee, 0) as Id;
    let a = &INTERFACES[id as usize].attributes[slot(agent, call.callee, 1)];
    let is_static = a.is_static || INTERFACES[id as usize].kind == Kind::Namespace;
    let this = if is_static {
        None
    } else {
        match this_for(agent, call.this, id, a.lenient_this) {
            Ok(Some(p)) => Some(p),
            Ok(None) => return Ok(Value::Undefined),
            Err(e) if is_promise_type(a.ty) => return rejected(agent, e),
            Err(e) => return Err(e),
        }
    };
    if a.same_object {
        if let Some(v) = this.as_ref().and_then(|p| p.get().slot(a.name)) {
            return Ok(v);
        }
    }
    let result = invoke(
        agent,
        Invocation {
            op: a.getter,
            overload: 0,
            iface: id,
            this: this.clone(),
            args: Vec::new(),
            new_target: None,
        },
    )
    .and_then(|v| to_js(agent, v, a.ty));
    let v = match result {
        Ok(v) => v,
        Err(e) if is_promise_type(a.ty) => return rejected(agent, e),
        Err(e) => return Err(e),
    };
    if a.same_object {
        if let Some(p) = &this {
            p.get().set_slot(a.name, v);
        }
    }
    Ok(v)
}

fn attribute_setter(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let id = slot(agent, call.callee, 0) as Id;
    let a = &INTERFACES[id as usize].attributes[slot(agent, call.callee, 1)];
    let Some(&v) = call.args.first() else {
        return agent.type_error(&format!(
            "Failed to set the '{}' property: 1 argument required, but only 0 present.",
            a.name
        ));
    };
    let is_static = a.is_static || INTERFACES[id as usize].kind == Kind::Namespace;
    let this = if is_static {
        None
    } else {
        match this_for(agent, call.this, id, a.lenient_this)? {
            Some(p) => Some(p),
            None => return Ok(Value::Undefined),
        }
    };
    if a.lenient_setter && a.setter.is_none() {
        return Ok(Value::Undefined);
    }
    if let (Some(forward), Some(p)) = (a.put_forwards, &this) {
        let key = agent.heap.key_from_str(a.name);
        let q = ops::get(agent, p.object, key)?;
        let Value::Object(q) = q else {
            return agent.type_error(&format!("'{}' is not an object.", a.name));
        };
        let fkey = agent.heap.key_from_str(forward);
        ops::set(agent, q, fkey, v, false)?;
        return Ok(Value::Undefined);
    }
    if let (true, Some(p)) = (a.replaceable, &this) {
        let key = agent.heap.key_from_str(a.name);
        ops::create_data_property_or_throw(agent, p.object, key, v)?;
        return Ok(Value::Undefined);
    }
    let Some(setter) = a.setter else {
        return Ok(Value::Undefined);
    };
    let value = to_idl(agent, v, a.ty)?;
    invoke(
        agent,
        Invocation {
            op: setter,
            overload: 0,
            iface: id,
            this,
            args: vec![value],
            new_target: None,
        },
    )?;
    Ok(Value::Undefined)
}

fn operation(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let id = slot(agent, call.callee, 0) as Id;
    let def = &INTERFACES[id as usize];
    let op = &def.operations[slot(agent, call.callee, 1)];
    let promise = op.overloads.first().is_some_and(|o| is_promise_type(o.ret));
    let result = (|| -> JsResult<Value> {
        let this = if op.is_static || def.kind == Kind::Namespace {
            None
        } else {
            this_for(agent, call.this, id, false)?
        };
        let r = overload::resolve(agent, op, call.args)?;
        let ret = r.overload.ret;
        let v = invoke(
            agent,
            Invocation {
                op: r.overload.op,
                overload: r.overload.index,
                iface: id,
                this,
                args: r.args,
                new_target: None,
            },
        )?;
        to_js(agent, v, ret)
    })();
    match result {
        Err(e) if promise => rejected(agent, e),
        r => r,
    }
}

fn stringifier(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let id = slot(agent, call.callee, 0) as Id;
    let op = match INTERFACES[id as usize].stringifier {
        Stringifier::Attribute(op) | Stringifier::Operation(op) => op,
        Stringifier::None => return agent.type_error("no stringifier"),
    };
    let this = this_for(agent, call.this, id, false)?;
    let v = invoke(
        agent,
        Invocation {
            op,
            overload: 0,
            iface: id,
            this,
            args: Vec::new(),
            new_target: None,
        },
    )?;
    to_js(agent, v, Ty::DomString)
}

// ----- pair iterators -------------------------------------------------------------

/// The value pairs to iterate over of `target`.
fn pairs(agent: &mut Agent, target: &PlatformRef, id: Id) -> JsResult<Vec<(IdlValue, IdlValue)>> {
    let Iterable::Pair { pairs, .. } = INTERFACES[id as usize].iterable else {
        return Ok(Vec::new());
    };
    let v = invoke(
        agent,
        Invocation {
            op: pairs,
            overload: 0,
            iface: id,
            this: Some(target.clone()),
            args: Vec::new(),
            new_target: None,
        },
    )?;
    Ok(match v {
        IdlValue::Sequence(items) => items
            .into_iter()
            .map(|p| match p {
                IdlValue::Sequence(mut kv) if kv.len() == 2 => {
                    let v = kv.pop().expect("value");
                    let k = kv.pop().expect("key");
                    (k, v)
                }
                other => (other, IdlValue::Undefined),
            })
            .collect(),
        _ => Vec::new(),
    })
}

fn pair_types(id: Id) -> (Ty, Ty) {
    match INTERFACES[id as usize].iterable {
        Iterable::Pair { key, value, .. } => (key, value),
        _ => (Ty::Any, Ty::Any),
    }
}

fn pair_iterator_method(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let id = slot(agent, call.callee, 0) as Id;
    let kind = match slot(agent, call.callee, 1) {
        0 => IterKind::Entries,
        1 => IterKind::Keys,
        _ => IterKind::Values,
    };
    let Some(target) = this_for(agent, call.this, id, false)? else {
        return Ok(Value::Undefined);
    };
    let state = super::current_state(agent)?;
    let proto = state
        .iterator_protos
        .borrow()
        .get(id as usize)
        .copied()
        .flatten();
    let it = agent.heap.create_object(
        proto,
        ObjectKind::Host(Rc::new(PlatformObject::new(
            id,
            state.realm,
            Native::Iterator {
                target: target.object,
                kind,
                index: 0,
            },
        ))),
    );
    Ok(Value::Object(it))
}

fn pair_iterator_next(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let id = slot(agent, call.callee, 0) as Id;
    let Some(it) = platform::platform_ref(agent, call.this) else {
        return agent.type_error("Illegal invocation");
    };
    let (target, kind, index) = match &*it.get().native.borrow() {
        Native::Iterator {
            target,
            kind,
            index,
        } if it.get().iface == id => (*target, *kind, *index),
        _ => return agent.type_error("Illegal invocation"),
    };
    let Some(target_ref) = platform::platform_ref(agent, Value::Object(target)) else {
        return agent.type_error("Illegal invocation");
    };
    let list = pairs(agent, &target_ref, id)?;
    if index >= list.len() {
        let r = ops::create_iter_result_object(agent, Value::Undefined, true);
        return Ok(Value::Object(r));
    }
    if let Native::Iterator { index: i, .. } = &mut *it.get().native.borrow_mut() {
        *i = index + 1;
    }
    let (kt, vt) = pair_types(id);
    let (k, v) = list.into_iter().nth(index).expect("in range");
    let result = match kind {
        IterKind::Keys => to_js(agent, k, kt)?,
        IterKind::Values => to_js(agent, v, vt)?,
        IterKind::Entries => {
            let k = to_js(agent, k, kt)?;
            let v = to_js(agent, v, vt)?;
            Value::Object(ops::create_array_from_list(agent, &[k, v]))
        }
    };
    Ok(Value::Object(ops::create_iter_result_object(
        agent, result, false,
    )))
}

fn pair_for_each(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let id = slot(agent, call.callee, 0) as Id;
    let Some(target) = this_for(agent, call.this, id, false)? else {
        return Ok(Value::Undefined);
    };
    let callback = call.arg(0);
    if !vjs::function::is_callable(agent, callback) {
        return agent.type_error("The callback provided as parameter 1 is not a function.");
    }
    let this_arg = call.arg(1);
    let (kt, vt) = pair_types(id);
    let mut i = 0;
    loop {
        let list = pairs(agent, &target, id)?;
        let Some((k, v)) = list.into_iter().nth(i) else {
            break;
        };
        let k = to_js(agent, k, kt)?;
        let v = to_js(agent, v, vt)?;
        ops::call(
            agent,
            callback,
            this_arg,
            &[v, k, Value::Object(target.object)],
        )?;
        i += 1;
    }
    Ok(Value::Undefined)
}

// ----- maplike and setlike --------------------------------------------------------

/// The `[[BackingMap]]` or `[[BackingSet]]` of a platform object.
pub fn backing_collection(agent: &mut Agent, p: &PlatformRef, map: bool) -> JsResult<Object> {
    let key = if map {
        "[[BackingMap]]"
    } else {
        "[[BackingSet]]"
    };
    if let Some(Value::Object(o)) = p.get().slot(key) {
        return Ok(o);
    }
    let ctor = agent.intrinsic(if map { Intrinsic::Map } else { Intrinsic::Set });
    let o = ops::construct(agent, ctor, &[], None)?;
    p.get().set_slot(key, Value::Object(o));
    Ok(o)
}

fn collection_for_each_callback(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let slots = native_slots(agent, call.callee).to_vec();
    let (callback, this_arg, target) = (slots[0], slots[1], slots[2]);
    ops::call(
        agent,
        callback,
        this_arg,
        &[call.arg(0), call.arg(1), target],
    )
}

fn collection_method(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let id = slot(agent, call.callee, 0) as Id;
    let method = COLLECTION_METHODS[slot(agent, call.callee, 1)];
    let Some(target) = this_for(agent, call.this, id, false)? else {
        return Ok(Value::Undefined);
    };
    let (map, key_ty, value_ty) = match INTERFACES[id as usize].iterable {
        Iterable::Maplike { key, value, .. } => (true, key, value),
        Iterable::Setlike { value, .. } => (false, value, value),
        _ => return agent.type_error("Illegal invocation"),
    };
    let backing = backing_collection(agent, &target, map)?;
    let backing_value = Value::Object(backing);
    let name_key = agent.heap.key_from_str(method);
    match method {
        "size" => ops::get(agent, backing, name_key),
        "forEach" => {
            let callback = call.arg(0);
            if !vjs::function::is_callable(agent, callback) {
                return agent.type_error("The callback provided as parameter 1 is not a function.");
            }
            let realm = agent.current_realm();
            let key = agent.heap.key_from_str("forEach");
            let trampoline = make_fn(
                agent,
                realm,
                collection_for_each_callback,
                2,
                key,
                None,
                vec![callback, call.arg(1), Value::Object(target.object)],
            );
            ops::invoke(agent, backing_value, name_key, &[Value::Object(trampoline)])?;
            Ok(Value::Undefined)
        }
        "get" | "has" | "delete" => {
            let k = to_idl(agent, call.arg(0), key_ty)?;
            let k = to_js(agent, k, key_ty)?;
            ops::invoke(agent, backing_value, name_key, &[k])
        }
        "set" => {
            let k = to_idl(agent, call.arg(0), key_ty)?;
            let k = to_js(agent, k, key_ty)?;
            let v = to_idl(agent, call.arg(1), value_ty)?;
            let v = to_js(agent, v, value_ty)?;
            ops::invoke(agent, backing_value, name_key, &[k, v])?;
            Ok(Value::Object(target.object))
        }
        "add" => {
            let v = to_idl(agent, call.arg(0), value_ty)?;
            let v = to_js(agent, v, value_ty)?;
            ops::invoke(agent, backing_value, name_key, &[v])?;
            Ok(Value::Object(target.object))
        }
        _ => ops::invoke(agent, backing_value, name_key, &[]),
    }
}
