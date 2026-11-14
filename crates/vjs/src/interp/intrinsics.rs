//! Intrinsics the interpreter itself needs: the prototypes of generator
//! and async functions and their objects, `%IteratorPrototype%` and
//! `%AsyncIteratorPrototype%` iteration methods, the array iterator behind
//! `Array.prototype.values`, `%ForInIteratorPrototype%`,
//! `%AsyncFromSyncIteratorPrototype%`, `%eval%` and the core of `Promise`.

use crate::agent::Agent;
use crate::function::{
    NativeFn, create_builtin_function, make_constructor, set_native_constructor,
};
use crate::gc::{Object, Realm};
use crate::interp::{eval, generator, iter, promise};
use crate::object::{Attributes, ObjectKind, Slot};
use crate::ops;
use crate::realm::Intrinsic;
use crate::value::{PropertyKey, Value};

fn get(agent: &Agent, realm: Realm, which: Intrinsic) -> Object {
    agent.heap.realm(realm).intrinsics.get(which)
}

fn set(agent: &mut Agent, realm: Realm, which: Intrinsic, o: Object) {
    agent.heap.realm_mut(realm).intrinsics.set(which, o);
}

fn object(agent: &mut Agent, proto: Object) -> Object {
    agent.heap.create_object(Some(proto), ObjectKind::Ordinary)
}

/// A built-in method defined as `{ writable, !enumerable, configurable }`.
fn method(
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

/// A method keyed by a well-known symbol (named `[Symbol.x]`).
fn symbol_method(
    agent: &mut Agent,
    realm: Realm,
    target: Object,
    symbol: crate::gc::Symbol,
    f: NativeFn,
    length: u32,
) -> Object {
    let key = PropertyKey::Symbol(symbol);
    let func = create_builtin_function(agent, f, length, key, Some(realm), None, None);
    ops::define_builtin_method(agent, target, key, func);
    func
}

fn to_string_tag(agent: &mut Agent, target: Object, tag: &str) {
    let key = PropertyKey::Symbol(agent.symbols.to_string_tag);
    let v = Value::String(agent.heap.alloc_str(tag));
    agent.heap.set_own(
        target,
        key,
        Attributes::data(false, false, true),
        Slot::Data(v),
    );
}

/// `{ writable: false, enumerable: false, configurable: true }` link.
fn link(agent: &mut Agent, from: Object, name: &str, to: Object) {
    let key = agent.heap.key_from_str(name);
    agent.heap.set_own(
        from,
        key,
        Attributes::data(false, false, true),
        Slot::Data(Value::Object(to)),
    );
}

pub fn install(agent: &mut Agent, realm: Realm) {
    let object_proto = get(agent, realm, Intrinsic::ObjectPrototype);
    let function_proto = get(agent, realm, Intrinsic::FunctionPrototype);
    let iterator_proto = get(agent, realm, Intrinsic::IteratorPrototype);
    let array_proto = get(agent, realm, Intrinsic::ArrayPrototype);

    // %IteratorPrototype%[@@iterator] and %AsyncIteratorPrototype%.
    let sym = agent.symbols.iterator;
    symbol_method(agent, realm, iterator_proto, sym, iter::return_this, 0);
    let async_iterator_proto = object(agent, object_proto);
    let sym = agent.symbols.async_iterator;
    symbol_method(
        agent,
        realm,
        async_iterator_proto,
        sym,
        iter::return_this,
        0,
    );
    set(
        agent,
        realm,
        Intrinsic::AsyncIteratorPrototype,
        async_iterator_proto,
    );

    // Generators.
    let generator_function_proto = object(agent, function_proto);
    let generator_proto = object(agent, iterator_proto);
    link(
        agent,
        generator_function_proto,
        "prototype",
        generator_proto,
    );
    link(
        agent,
        generator_proto,
        "constructor",
        generator_function_proto,
    );
    to_string_tag(agent, generator_function_proto, "GeneratorFunction");
    to_string_tag(agent, generator_proto, "Generator");
    method(
        agent,
        realm,
        generator_proto,
        "next",
        generator::generator_next,
        1,
    );
    method(
        agent,
        realm,
        generator_proto,
        "return",
        generator::generator_return,
        1,
    );
    method(
        agent,
        realm,
        generator_proto,
        "throw",
        generator::generator_throw,
        1,
    );
    set(
        agent,
        realm,
        Intrinsic::GeneratorFunctionPrototype,
        generator_function_proto,
    );
    set(agent, realm, Intrinsic::GeneratorPrototype, generator_proto);

    // Async generators.
    let async_generator_function_proto = object(agent, function_proto);
    let async_generator_proto = object(agent, async_iterator_proto);
    link(
        agent,
        async_generator_function_proto,
        "prototype",
        async_generator_proto,
    );
    link(
        agent,
        async_generator_proto,
        "constructor",
        async_generator_function_proto,
    );
    to_string_tag(
        agent,
        async_generator_function_proto,
        "AsyncGeneratorFunction",
    );
    to_string_tag(agent, async_generator_proto, "AsyncGenerator");
    method(
        agent,
        realm,
        async_generator_proto,
        "next",
        generator::async_generator_next,
        1,
    );
    method(
        agent,
        realm,
        async_generator_proto,
        "return",
        generator::async_generator_return,
        1,
    );
    method(
        agent,
        realm,
        async_generator_proto,
        "throw",
        generator::async_generator_throw,
        1,
    );
    set(
        agent,
        realm,
        Intrinsic::AsyncGeneratorFunctionPrototype,
        async_generator_function_proto,
    );
    set(
        agent,
        realm,
        Intrinsic::AsyncGeneratorPrototype,
        async_generator_proto,
    );

    // Async functions.
    let async_function_proto = object(agent, function_proto);
    to_string_tag(agent, async_function_proto, "AsyncFunction");
    set(
        agent,
        realm,
        Intrinsic::AsyncFunctionPrototype,
        async_function_proto,
    );

    // Array iterators and Array.prototype.values.
    let array_iterator_proto = object(agent, iterator_proto);
    method(
        agent,
        realm,
        array_iterator_proto,
        "next",
        iter::array_iterator_next,
        0,
    );
    to_string_tag(agent, array_iterator_proto, "Array Iterator");
    set(
        agent,
        realm,
        Intrinsic::ArrayIteratorPrototype,
        array_iterator_proto,
    );
    let values = method(agent, realm, array_proto, "values", iter::array_values, 0);
    let iterator_key = PropertyKey::Symbol(agent.symbols.iterator);
    ops::define_builtin_method(agent, array_proto, iterator_key, values);
    set(agent, realm, Intrinsic::ArrayPrototypeValues, values);

    // %ForInIteratorPrototype%.
    let for_in_proto = object(agent, iterator_proto);
    method(
        agent,
        realm,
        for_in_proto,
        "next",
        iter::for_in_iterator_next,
        0,
    );
    set(
        agent,
        realm,
        Intrinsic::ForInIteratorPrototype,
        for_in_proto,
    );

    // %AsyncFromSyncIteratorPrototype%.
    let async_from_sync_proto = object(agent, async_iterator_proto);
    method(
        agent,
        realm,
        async_from_sync_proto,
        "next",
        iter::async_from_sync_next,
        1,
    );
    method(
        agent,
        realm,
        async_from_sync_proto,
        "return",
        iter::async_from_sync_return,
        1,
    );
    method(
        agent,
        realm,
        async_from_sync_proto,
        "throw",
        iter::async_from_sync_throw,
        1,
    );
    set(
        agent,
        realm,
        Intrinsic::AsyncFromSyncIteratorPrototype,
        async_from_sync_proto,
    );

    // %eval%.
    let key = agent.heap.key_from_str("eval");
    let eval_fn =
        create_builtin_function(agent, eval::eval_function, 1, key, Some(realm), None, None);
    set(agent, realm, Intrinsic::Eval, eval_fn);

    // Promise (constructor and `then`).
    let promise_proto = object(agent, object_proto);
    let key = agent.heap.key_from_str("Promise");
    let promise_ctor = create_builtin_function(
        agent,
        promise::promise_constructor,
        1,
        key,
        Some(realm),
        None,
        None,
    );
    set_native_constructor(agent, promise_ctor);
    make_constructor(agent, promise_ctor, false, Some(promise_proto));
    let ctor_key = PropertyKey::String(agent.heap.names.constructor);
    agent.heap.set_own(
        promise_proto,
        ctor_key,
        Attributes::data(true, false, true),
        Slot::Data(Value::Object(promise_ctor)),
    );
    method(
        agent,
        realm,
        promise_proto,
        "then",
        promise::promise_then,
        2,
    );
    to_string_tag(agent, promise_proto, "Promise");
    set(agent, realm, Intrinsic::Promise, promise_ctor);
    set(agent, realm, Intrinsic::PromisePrototype, promise_proto);
}
