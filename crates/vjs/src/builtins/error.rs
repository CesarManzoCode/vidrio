//! The parts of the error objects (ECMA-262 §20.5) that the realm's
//! bootstrap does not create: `Error.prototype.toString`, `Error.isError`,
//! `AggregateError` and `SuppressedError`. `Error` and the NativeError
//! constructors themselves are created with the fundamental objects
//! (`realm.rs`).

use crate::agent::Agent;
use crate::builtins::{constructor, intrinsic, method, plain_object, set_intrinsic, str_value};
use crate::completion::JsResult;
use crate::function::NativeCall;
use crate::gc::Realm;
use crate::object::ObjectKind;
use crate::ops;
use crate::realm::Intrinsic;
use crate::string::utf16;
use crate::value::{PropertyKey, Value};

pub(crate) fn install(agent: &mut Agent, realm: Realm) {
    let error = intrinsic(agent, realm, Intrinsic::Error);
    let error_proto = intrinsic(agent, realm, Intrinsic::ErrorPrototype);
    method(agent, realm, error_proto, "toString", to_string, 0);
    method(agent, realm, error, "isError", is_error, 1);

    let kinds: [(&str, crate::function::NativeFn, u32, Intrinsic, Intrinsic); 2] = [
        (
            "AggregateError",
            aggregate_error_constructor,
            2,
            Intrinsic::AggregateError,
            Intrinsic::AggregateErrorPrototype,
        ),
        (
            "SuppressedError",
            suppressed_error_constructor,
            3,
            Intrinsic::SuppressedError,
            Intrinsic::SuppressedErrorPrototype,
        ),
    ];
    for (name, behavior, length, which, proto_which) in kinds {
        let proto = plain_object(agent, error_proto);
        let ctor = constructor(agent, realm, name, behavior, length, proto, Some(error));
        let key = PropertyKey::String(agent.heap.names.name);
        let v = str_value(agent, name);
        crate::builtins::value(agent, proto, key, v, true, false, true);
        let key = PropertyKey::String(agent.heap.names.message);
        let v = Value::String(agent.heap.empty_string());
        crate::builtins::value(agent, proto, key, v, true, false, true);
        set_intrinsic(agent, realm, which, ctor);
        set_intrinsic(agent, realm, proto_which, proto);
    }
}

/// `Error.prototype.toString` (§20.5.3.4).
fn to_string(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let Value::Object(o) = call.this else {
        return agent.type_error("Error.prototype.toString called on a non-object");
    };
    let key = PropertyKey::String(agent.heap.names.name);
    let name = match ops::get(agent, o, key)? {
        Value::Undefined => utf16("Error"),
        v => {
            let s = ops::to_string(agent, v)?;
            agent.heap.string_units(s).to_vec()
        }
    };
    let key = PropertyKey::String(agent.heap.names.message);
    let msg = match ops::get(agent, o, key)? {
        Value::Undefined => Vec::new(),
        v => {
            let s = ops::to_string(agent, v)?;
            agent.heap.string_units(s).to_vec()
        }
    };
    let units = if name.is_empty() {
        msg
    } else if msg.is_empty() {
        name
    } else {
        let mut u = name;
        u.extend_from_slice(&utf16(": "));
        u.extend_from_slice(&msg);
        u
    };
    Ok(Value::String(agent.heap.alloc_string_owned(units)))
}

/// `Error.isError(arg)`: whether `arg` has an `[[ErrorData]]` slot.
fn is_error(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let result = match call.arg(0) {
        Value::Object(o) => matches!(agent.heap.object(o).kind, ObjectKind::Error),
        _ => false,
    };
    Ok(Value::Boolean(result))
}

/// The error object of a NativeError-like constructor with its `message`.
fn create_error_object(
    agent: &mut Agent,
    call: &NativeCall<'_>,
    proto: Intrinsic,
    message: Value,
) -> JsResult<crate::gc::Object> {
    let new_target = call.new_target.unwrap_or(call.callee);
    let o = ops::ordinary_create_from_constructor(agent, new_target, proto, ObjectKind::Error)?;
    if !message.is_undefined() {
        let msg = ops::to_string(agent, message)?;
        let key = PropertyKey::String(agent.heap.names.message);
        ops::create_non_enumerable_data_property_or_throw(agent, o, key, Value::String(msg))?;
    }
    Ok(o)
}

/// `AggregateError(errors, message, options)` (§20.5.7.1.1).
fn aggregate_error_constructor(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let o = create_error_object(
        agent,
        &call,
        Intrinsic::AggregateErrorPrototype,
        call.arg(1),
    )?;
    ops::install_error_cause(agent, o, call.arg(2))?;
    let mut record = ops::get_iterator(agent, call.arg(0))?;
    let errors = ops::iterator_to_list(agent, &mut record)?;
    let list = ops::create_array_from_list(agent, &errors);
    let key = PropertyKey::String(agent.heap.names.errors);
    ops::define_property_or_throw(
        agent,
        o,
        key,
        crate::object::PropertyDescriptor::data(Value::Object(list), true, false, true),
    )?;
    Ok(Value::Object(o))
}

/// `SuppressedError(error, suppressed, message)` (explicit resource
/// management).
fn suppressed_error_constructor(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let o = create_error_object(
        agent,
        &call,
        Intrinsic::SuppressedErrorPrototype,
        call.arg(2),
    )?;
    let key = PropertyKey::String(agent.heap.names.error);
    ops::create_non_enumerable_data_property_or_throw(agent, o, key, call.arg(0))?;
    let key = PropertyKey::String(agent.heap.names.suppressed);
    ops::create_non_enumerable_data_property_or_throw(agent, o, key, call.arg(1))?;
    Ok(Value::Object(o))
}
