//! `Function` (ECMA-262 §20.2), CreateDynamicFunction with the
//! `GeneratorFunction`, `AsyncFunction` and `AsyncGeneratorFunction`
//! constructors (§27.3–§27.7), and the `caller`/`arguments` accessors of
//! `%Function.prototype%` (AddRestrictedFunctionProperties).

use std::rc::Rc;

use crate::agent::Agent;
use crate::builtins::{constructor, intrinsic, set_intrinsic, value};
use crate::completion::JsResult;
use crate::function::{
    NativeCall, bound_function_create, create_builtin_function, set_function_length,
    set_function_name,
};
use crate::gc::Realm;
use crate::object::{Attributes, ObjectKind, Slot};
use crate::ops::{self, ListElementTypes};
use crate::parser::{DynamicFunctionKind, parse_dynamic_function};
use crate::realm::Intrinsic;
use crate::string::utf16;
use crate::value::{PropertyKey, Value};

pub(crate) fn install(agent: &mut Agent, realm: Realm) {
    let function_proto = intrinsic(agent, realm, Intrinsic::FunctionPrototype);
    let function = constructor(
        agent,
        realm,
        "Function",
        function_constructor,
        1,
        function_proto,
        None,
    );
    set_intrinsic(agent, realm, Intrinsic::Function, function);

    crate::builtins::method(agent, realm, function_proto, "apply", apply, 2);
    crate::builtins::method(agent, realm, function_proto, "bind", bind, 1);
    crate::builtins::method(agent, realm, function_proto, "call", call, 1);
    crate::builtins::method(agent, realm, function_proto, "toString", to_string, 0);
    let key = PropertyKey::Symbol(agent.symbols.has_instance);
    let f = create_builtin_function(agent, has_instance, 1, key, Some(realm), None, None);
    value(
        agent,
        function_proto,
        key,
        Value::Object(f),
        false,
        false,
        false,
    );

    // AddRestrictedFunctionProperties(%Function.prototype%, realm).
    let thrower = Value::Object(intrinsic(agent, realm, Intrinsic::ThrowTypeError));
    for name in [agent.heap.names.caller, agent.heap.names.arguments] {
        agent.heap.set_own(
            function_proto,
            PropertyKey::String(name),
            Attributes::accessor(false, true),
            Slot::Accessor {
                get: thrower,
                set: thrower,
            },
        );
    }

    // The constructors of the other function kinds: their [[Prototype]] is
    // %Function% and their prototypes link back with non-writable
    // `constructor` properties.
    let kinds: [(&str, crate::function::NativeFn, Intrinsic, Intrinsic); 3] = [
        (
            "GeneratorFunction",
            generator_function_constructor,
            Intrinsic::GeneratorFunction,
            Intrinsic::GeneratorFunctionPrototype,
        ),
        (
            "AsyncFunction",
            async_function_constructor,
            Intrinsic::AsyncFunction,
            Intrinsic::AsyncFunctionPrototype,
        ),
        (
            "AsyncGeneratorFunction",
            async_generator_function_constructor,
            Intrinsic::AsyncGeneratorFunction,
            Intrinsic::AsyncGeneratorFunctionPrototype,
        ),
    ];
    for (name, behavior, which, proto_which) in kinds {
        let proto = intrinsic(agent, realm, proto_which);
        let ctor = constructor(agent, realm, name, behavior, 1, proto, Some(function));
        let key = PropertyKey::String(agent.heap.names.constructor);
        value(agent, proto, key, Value::Object(ctor), false, false, true);
        set_intrinsic(agent, realm, which, ctor);
    }
}

fn function_constructor(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    create_dynamic_function(agent, &call, DynamicFunctionKind::Normal)
}

fn generator_function_constructor(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    create_dynamic_function(agent, &call, DynamicFunctionKind::Generator)
}

fn async_function_constructor(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    create_dynamic_function(agent, &call, DynamicFunctionKind::Async)
}

fn async_generator_function_constructor(
    agent: &mut Agent,
    call: NativeCall<'_>,
) -> JsResult<Value> {
    create_dynamic_function(agent, &call, DynamicFunctionKind::AsyncGenerator)
}

/// CreateDynamicFunction(constructor, newTarget, kind, parameterArgs,
/// bodyArg) (§20.2.1.1.1).
pub fn create_dynamic_function(
    agent: &mut Agent,
    call: &NativeCall<'_>,
    kind: DynamicFunctionKind,
) -> JsResult<Value> {
    let new_target = call.new_target.unwrap_or(call.callee);
    let (parameter_args, body_arg) = match call.args.split_last() {
        Some((body, params)) => (params, *body),
        None => (&[][..], Value::String(agent.heap.empty_string())),
    };
    let mut params = String::new();
    for (i, &arg) in parameter_args.iter().enumerate() {
        let s = ops::to_string(agent, arg)?;
        if i > 0 {
            params.push(',');
        }
        params.push_str(&agent.heap.string_to_rust(s));
    }
    let body = ops::to_string(agent, body_arg)?;
    let body = agent.heap.string_to_rust(body);
    let options = crate::regexp::parse_options();
    let (function, source) = match parse_dynamic_function(kind, &params, &body, &options) {
        Ok(parsed) => parsed,
        Err(e) => return agent.syntax_error(&e.message),
    };
    let fallback = match kind {
        DynamicFunctionKind::Normal => Intrinsic::FunctionPrototype,
        DynamicFunctionKind::Generator => Intrinsic::GeneratorFunctionPrototype,
        DynamicFunctionKind::Async => Intrinsic::AsyncFunctionPrototype,
        DynamicFunctionKind::AsyncGenerator => Intrinsic::AsyncGeneratorFunctionPrototype,
    };
    let proto = ops::get_prototype_from_constructor(agent, new_target, fallback)?;
    let code = crate::compiler::compile_dynamic_function(agent, &function, Rc::from(source));
    let realm = agent.current_realm();
    let env = crate::interp::global_env(agent, realm);
    let f = crate::interp::function::make_closure(agent, &code, env, None);
    let script_or_module = agent.active_script_or_module();
    if let ObjectKind::BytecodeFunction(data) = &mut agent.heap.object_mut(f).kind {
        data.script_or_module = script_or_module;
    }
    agent.heap.set_proto(f, Some(proto));
    Ok(Value::Object(f))
}

/// The function `this` must be, or a TypeError.
fn this_callable(agent: &mut Agent, call: &NativeCall<'_>, method: &str) -> JsResult<Value> {
    if !ops::is_callable(agent, call.this) {
        return agent.type_error(&format!(
            "Function.prototype.{method} called on a non-function"
        ));
    }
    Ok(call.this)
}

fn apply(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let func = this_callable(agent, &call, "apply")?;
    let this_arg = call.arg(0);
    let arg_array = call.arg(1);
    if arg_array.is_nullish() {
        return ops::call(agent, func, this_arg, &[]);
    }
    let list = ops::create_list_from_array_like(agent, arg_array, ListElementTypes::All)?;
    ops::call(agent, func, this_arg, &list)
}

fn call(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let func = this_callable(agent, &call, "call")?;
    let rest = call.args.get(1..).unwrap_or(&[]);
    ops::call(agent, func, call.arg(0), rest)
}

/// `Function.prototype.bind` (§20.2.3.2).
fn bind(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let Value::Object(target) = this_callable(agent, &call, "bind")? else {
        unreachable!("callable values are objects");
    };
    let args = call.args.get(1..).unwrap_or(&[]).to_vec();
    let arg_count = args.len() as f64;
    let f = bound_function_create(agent, target, call.arg(0), args)?;
    let length_key = PropertyKey::String(agent.heap.names.length);
    let mut length = 0.0;
    if ops::has_own_property(agent, target, length_key)? {
        if let Value::Number(target_len) = ops::get(agent, target, length_key)? {
            if target_len == f64::INFINITY {
                length = f64::INFINITY;
            } else if target_len != f64::NEG_INFINITY {
                let as_int = ops::f64_to_integer_or_infinity(target_len);
                length = (as_int - arg_count).max(0.0);
            }
        }
    }
    set_function_length(agent, f, length);
    let name_key = PropertyKey::String(agent.heap.names.name);
    let target_name = match ops::get(agent, target, name_key)? {
        Value::String(s) => s,
        _ => agent.heap.empty_string(),
    };
    let key = agent.heap.key_from_string(target_name);
    set_function_name(agent, f, key, Some("bound"));
    Ok(Value::Object(f))
}

/// `Function.prototype.toString` (§20.2.3.5): the source text of
/// functions defined in code, the NativeFunction form for the others.
fn to_string(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let Value::Object(f) = call.this else {
        return agent.type_error("Function.prototype.toString requires a function");
    };
    if let Some(code) = crate::interp::function_code(agent, f) {
        if let Some(source) = &code.source {
            let text = source.text().to_string();
            return Ok(Value::String(agent.heap.alloc_str(&text)));
        }
    }
    let data = agent.heap.object(f);
    let name = match &data.kind {
        ObjectKind::NativeFunction(native) => Some(native.initial_name),
        _ if data.is_callable() => Some(None),
        _ => None,
    };
    let Some(name) = name else {
        return agent.type_error("Function.prototype.toString requires a function");
    };
    Ok(Value::String(native_function_text(agent, name)))
}

/// `function <name>() { [native code] }`, the NativeFunction form.
pub(crate) fn native_function_text(agent: &mut Agent, name: Option<Value>) -> crate::gc::JsString {
    let mut units = utf16("function ");
    if let Some(Value::String(s)) = name {
        units.extend_from_slice(agent.heap.string_units(s));
    }
    units.extend_from_slice(&utf16("() { [native code] }"));
    agent.heap.alloc_string_owned(units)
}

/// `Function.prototype[@@hasInstance]` (§20.2.3.6).
fn has_instance(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    Ok(Value::Boolean(ops::ordinary_has_instance(
        agent,
        call.this,
        call.arg(0),
    )?))
}
