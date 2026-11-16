//! `Symbol` (ECMA-262 §20.4) and `Boolean` (§20.3).

use crate::agent::Agent;
use crate::builtins::{
    accessor, constructor, intrinsic, method, set_intrinsic, to_string_tag, value,
};
use crate::completion::JsResult;
use crate::function::{NativeCall, create_builtin_function};
use crate::gc::{Realm, Symbol};
use crate::object::ObjectKind;
use crate::ops;
use crate::realm::Intrinsic;
use crate::string::utf16;
use crate::value::{PropertyKey, Value};

pub(crate) fn install(agent: &mut Agent, realm: Realm) {
    install_symbol(agent, realm);
    install_boolean(agent, realm);
}

fn install_symbol(agent: &mut Agent, realm: Realm) {
    let proto = intrinsic(agent, realm, Intrinsic::SymbolPrototype);
    let ctor = constructor(agent, realm, "Symbol", symbol_constructor, 0, proto, None);
    set_intrinsic(agent, realm, Intrinsic::Symbol, ctor);
    method(agent, realm, ctor, "for", symbol_for, 1);
    method(agent, realm, ctor, "keyFor", key_for, 1);
    let s = agent.symbols;
    let well_known = [
        ("asyncDispose", s.async_dispose),
        ("asyncIterator", s.async_iterator),
        ("dispose", s.dispose),
        ("hasInstance", s.has_instance),
        ("isConcatSpreadable", s.is_concat_spreadable),
        ("iterator", s.iterator),
        ("match", s.match_),
        ("matchAll", s.match_all),
        ("replace", s.replace),
        ("search", s.search),
        ("species", s.species),
        ("split", s.split),
        ("toPrimitive", s.to_primitive),
        ("toStringTag", s.to_string_tag),
        ("unscopables", s.unscopables),
    ];
    for (name, sym) in well_known {
        let key = agent.heap.key_from_str(name);
        value(agent, ctor, key, Value::Symbol(sym), false, false, false);
    }

    let key = agent.heap.key_from_str("description");
    accessor(agent, realm, proto, key, Some(description), None);
    method(agent, realm, proto, "toString", symbol_to_string, 0);
    method(agent, realm, proto, "valueOf", symbol_value_of, 0);
    let key = PropertyKey::Symbol(agent.symbols.to_primitive);
    let f = create_builtin_function(agent, symbol_to_primitive, 1, key, Some(realm), None, None);
    value(agent, proto, key, Value::Object(f), false, false, true);
    to_string_tag(agent, proto, "Symbol");
}

fn install_boolean(agent: &mut Agent, realm: Realm) {
    let proto = intrinsic(agent, realm, Intrinsic::BooleanPrototype);
    let ctor = constructor(agent, realm, "Boolean", boolean_constructor, 1, proto, None);
    set_intrinsic(agent, realm, Intrinsic::Boolean, ctor);
    method(agent, realm, proto, "toString", boolean_to_string, 0);
    method(agent, realm, proto, "valueOf", boolean_value_of, 0);
}

/// `Symbol([description])` (§20.4.1.1).
fn symbol_constructor(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    if call.new_target.is_some() {
        return agent.type_error("Symbol is not a constructor");
    }
    let description = match call.arg(0) {
        Value::Undefined => None,
        v => Some(ops::to_string(agent, v)?),
    };
    Ok(Value::Symbol(agent.heap.alloc_symbol(description)))
}

fn symbol_for(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let key = ops::to_string(agent, call.arg(0))?;
    Ok(Value::Symbol(agent.symbol_for(key)))
}

fn key_for(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let Value::Symbol(s) = call.arg(0) else {
        return agent.type_error("Symbol.keyFor argument is not a symbol");
    };
    Ok(match agent.key_for_symbol(s) {
        Some(k) => Value::String(k),
        None => Value::Undefined,
    })
}

/// ThisSymbolValue(value).
fn this_symbol_value(agent: &mut Agent, v: Value) -> JsResult<Symbol> {
    match v {
        Value::Symbol(s) => Ok(s),
        Value::Object(o) => {
            let inner = match &agent.heap.object(o).kind {
                ObjectKind::Symbol(s) => Some(*s),
                _ => None,
            };
            match inner {
                Some(s) => Ok(s),
                None => agent.type_error("not a Symbol object"),
            }
        }
        _ => agent.type_error("not a symbol"),
    }
}

fn description(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let s = this_symbol_value(agent, call.this)?;
    Ok(match agent.heap.symbol(s).description {
        Some(d) => Value::String(d),
        None => Value::Undefined,
    })
}

/// SymbolDescriptiveString(sym).
pub(crate) fn symbol_descriptive_string(agent: &mut Agent, s: Symbol) -> Value {
    let mut units = utf16("Symbol(");
    let description = agent.heap.symbol(s).description;
    if let Some(d) = description {
        units.extend_from_slice(agent.heap.string_units(d));
    }
    units.push(u16::from(b')'));
    Value::String(agent.heap.alloc_string_owned(units))
}

fn symbol_to_string(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let s = this_symbol_value(agent, call.this)?;
    Ok(symbol_descriptive_string(agent, s))
}

fn symbol_value_of(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    Ok(Value::Symbol(this_symbol_value(agent, call.this)?))
}

fn symbol_to_primitive(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    Ok(Value::Symbol(this_symbol_value(agent, call.this)?))
}

/// `Boolean(value)` (§20.3.1.1).
fn boolean_constructor(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let b = ops::to_boolean(agent, call.arg(0));
    let Some(new_target) = call.new_target else {
        return Ok(Value::Boolean(b));
    };
    let o = ops::ordinary_create_from_constructor(
        agent,
        new_target,
        Intrinsic::BooleanPrototype,
        ObjectKind::Boolean(b),
    )?;
    Ok(Value::Object(o))
}

/// ThisBooleanValue(value).
fn this_boolean_value(agent: &mut Agent, v: Value) -> JsResult<bool> {
    match v {
        Value::Boolean(b) => Ok(b),
        Value::Object(o) => {
            let inner = match &agent.heap.object(o).kind {
                ObjectKind::Boolean(b) => Some(*b),
                _ => None,
            };
            match inner {
                Some(b) => Ok(b),
                None => agent.type_error("not a Boolean object"),
            }
        }
        _ => agent.type_error("not a boolean"),
    }
}

fn boolean_to_string(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let b = this_boolean_value(agent, call.this)?;
    Ok(crate::builtins::str_value(
        agent,
        if b { "true" } else { "false" },
    ))
}

fn boolean_value_of(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    Ok(Value::Boolean(this_boolean_value(agent, call.this)?))
}
