//! Execution of one instruction.

use crate::agent::Agent;
use crate::bytecode::Op;
use crate::completion::{JsResult, Throw};
use crate::env::{EnvData, EnvKind};
use crate::function::{ClassFieldDefinition, call_object, construct_object, set_function_name};
use crate::gc::{Env, Object};
use crate::internal;
use crate::interp::env_ops::{self, Found};
use crate::interp::generator::{self, MODE_NEXT, MODE_RETURN, MODE_THROW};
use crate::interp::{Frame, Handler, Step, Suspend, eval, function, iter, operators};
use crate::object::{Attributes, ObjectKind, PropertyDescriptor, Slot};
use crate::ops::{self, IntegrityLevel, IteratorRecord, ListElementTypes};
use crate::realm::Intrinsic;
use crate::value::{PropertyKey, Value};

/// The environment `hops` records out from `env`.
fn walk(agent: &Agent, mut env: Env, hops: u32) -> Env {
    for _ in 0..hops {
        env = agent
            .heap
            .env(env)
            .outer
            .expect("environment chain too short");
    }
    env
}

fn slot_name(agent: &Agent, env: Env, slot: u32) -> String {
    match &agent.heap.env(env).kind {
        EnvKind::Declarative(Some(names)) => names
            .names
            .get(slot as usize)
            .map(|a| agent.heap.atom_to_rust(*a))
            .unwrap_or_default(),
        _ => String::new(),
    }
}

fn tdz<T>(agent: &mut Agent, env: Env, slot: u32) -> JsResult<T> {
    let name = slot_name(agent, env, slot);
    if name.starts_with('%') {
        return agent.reference_error(
            "must call super constructor in derived class before accessing 'this'",
        );
    }
    agent.reference_error(&format!("cannot access '{name}' before initialization"))
}

fn const_message(agent: &mut Agent, frame: &Frame, c: u32) -> String {
    match frame.code.consts[c as usize] {
        Value::String(s) => agent.heap.string_to_rust(s),
        _ => String::new(),
    }
}

fn named_key(frame: &Frame, i: u32) -> PropertyKey {
    PropertyKey::String(frame.code.atoms[i as usize])
}

fn reg_number(frame: &Frame, r: u32) -> i32 {
    match frame.regs[r as usize] {
        Value::Number(n) => n as i32,
        _ => 0,
    }
}

fn as_object(agent: &mut Agent, v: Value, what: &str) -> JsResult<Object> {
    match v {
        Value::Object(o) => Ok(o),
        _ => agent.type_error(what),
    }
}

/// Appends to an array literal or argument list under construction.
fn append(agent: &mut Agent, arr: Object, v: Value) -> JsResult<()> {
    let len = internal::array_length(agent, arr);
    ops::create_data_property_or_throw(agent, arr, PropertyKey::Index(len), v)?;
    Ok(())
}

fn list_of(agent: &mut Agent, arr: Value) -> JsResult<Vec<Value>> {
    ops::create_list_from_array_like(agent, arr, ListElementTypes::All)
}

/// GetValue on a property reference: `base[key]` with `base` kept as the
/// receiver.
fn get_property(agent: &mut Agent, base: Value, key: PropertyKey) -> JsResult<Value> {
    if base.is_nullish() {
        let k = agent.heap.key_to_rust(key);
        let what = if base.is_null() { "null" } else { "undefined" };
        return agent.type_error(&format!("cannot read properties of {what} (reading '{k}')"));
    }
    ops::get_v(agent, base, key)
}

/// PutValue on a property reference.
fn put_property(
    agent: &mut Agent,
    base: Value,
    key: PropertyKey,
    v: Value,
    strict: bool,
) -> JsResult<()> {
    if base.is_nullish() {
        let k = agent.heap.key_to_rust(key);
        let what = if base.is_null() { "null" } else { "undefined" };
        return agent.type_error(&format!("cannot set properties of {what} (setting '{k}')"));
    }
    let o = ops::to_object(agent, base)?;
    let ok = internal::set(agent, o, key, v, base)?;
    if !ok && strict {
        let k = agent.heap.key_to_rust(key);
        return agent.type_error(&format!("cannot assign to read-only property '{k}'"));
    }
    Ok(())
}

fn computed_key(agent: &mut Agent, base: Value, key: Value) -> JsResult<PropertyKey> {
    if base.is_nullish() {
        let what = if base.is_null() { "null" } else { "undefined" };
        return agent.type_error(&format!("cannot read properties of {what}"));
    }
    ops::to_property_key(agent, key)
}

fn delete_property(
    agent: &mut Agent,
    base: Value,
    key: PropertyKey,
    strict: bool,
) -> JsResult<Value> {
    let o = ops::to_object(agent, base)?;
    let ok = internal::delete(agent, o, key)?;
    if !ok && strict {
        let k = agent.heap.key_to_rust(key);
        return agent.type_error(&format!("cannot delete property '{k}'"));
    }
    Ok(Value::Boolean(ok))
}

/// The outcome of a call from bytecode: a frame to run (bytecode
/// functions) or the value a native call returned.
enum Called {
    Frame(Frame),
    Value(Value),
}

impl Called {
    fn into_step(self, frame: &mut Frame) -> Step {
        match self {
            Called::Frame(f) => Step::Call(Box::new(f)),
            Called::Value(v) => {
                frame.push(v);
                Step::Next
            }
        }
    }
}

/// `[[Call]]` from bytecode.
fn call(agent: &mut Agent, f: Value, this: Value, args: Vec<Value>) -> JsResult<Called> {
    if let Value::Object(o) = f {
        if matches!(agent.heap.object(o).kind, ObjectKind::BytecodeFunction(_)) {
            return Ok(Called::Frame(function::prepare_call(
                agent, o, this, args, None,
            )?));
        }
        if agent.heap.object(o).is_callable() {
            return Ok(Called::Value(call_object(agent, o, this, &args)?));
        }
    }
    agent.type_error("value is not a function")
}

/// `[[Construct]]` from bytecode.
fn construct(agent: &mut Agent, f: Value, args: Vec<Value>) -> JsResult<Called> {
    let Value::Object(o) = f else {
        return agent.type_error("value is not a constructor");
    };
    if !agent.heap.object(o).is_constructor() {
        return agent.type_error("value is not a constructor");
    }
    if matches!(agent.heap.object(o).kind, ObjectKind::BytecodeFunction(_)) {
        return Ok(Called::Frame(function::prepare_call(
            agent,
            o,
            Value::Undefined,
            args,
            Some(o),
        )?));
    }
    let r = construct_object(agent, o, &args, o)?;
    Ok(Called::Value(Value::Object(r)))
}

pub(crate) fn step(agent: &mut Agent, frame: &mut Frame, op: Op) -> JsResult<Step> {
    exec(agent, frame, op)
}

#[allow(clippy::too_many_lines)]
fn exec(agent: &mut Agent, frame: &mut Frame, op: Op) -> JsResult<Step> {
    let strict = frame.code.strict;
    match op {
        // ----- constants and stack -----
        Op::Undefined => frame.push(Value::Undefined),
        Op::Null => frame.push(Value::Null),
        Op::True => frame.push(Value::Boolean(true)),
        Op::False => frame.push(Value::Boolean(false)),
        Op::Int(i) => frame.push(Value::Number(f64::from(i))),
        Op::Num(n) => frame.push(Value::Number(n)),
        Op::Const(i) => {
            let v = frame.code.consts[i as usize];
            frame.push(v);
        }
        Op::Pop => {
            frame.pop();
        }
        Op::PopN(n) => {
            let len = frame.stack.len().saturating_sub(n as usize);
            frame.stack.truncate(len);
        }
        Op::Dup => {
            let v = frame.top();
            frame.push(v);
        }
        Op::Dup2 => {
            let (a, b) = (frame.peek(1), frame.peek(0));
            frame.push(a);
            frame.push(b);
        }
        Op::Swap => {
            let b = frame.pop();
            let a = frame.pop();
            frame.push(b);
            frame.push(a);
        }
        Op::RotUp3 => {
            let c = frame.pop();
            let len = frame.stack.len();
            frame.stack.insert(len.saturating_sub(2), c);
        }
        Op::RotUp4 => {
            let d = frame.pop();
            let len = frame.stack.len();
            frame.stack.insert(len.saturating_sub(3), d);
        }
        Op::LoadReg(r) => {
            let v = frame.regs[r as usize];
            frame.push(v);
        }
        Op::StoreReg(r) => {
            let v = frame.pop();
            frame.regs[r as usize] = v;
        }
        Op::SetCompletion => frame.completion = frame.pop(),
        Op::ClearCompletion => frame.completion = Value::Undefined,
        Op::LoadCompletion => {
            let v = frame.completion;
            frame.push(v);
        }

        // ----- environments -----
        Op::PushScope(i) => {
            let names = frame.code.scopes[i as usize].clone();
            let env = agent
                .heap
                .alloc_env_data(EnvData::for_scope(Some(frame.env), names));
            frame.env = env;
        }
        Op::PopScope => {
            if let Some(outer) = agent.heap.env(frame.env).outer {
                frame.env = outer;
            }
        }
        Op::CopyScope => {
            let data = agent.heap.env(frame.env);
            let copy = EnvData {
                outer: data.outer,
                slots: data.slots.clone(),
                kind: match &data.kind {
                    EnvKind::Declarative(n) => EnvKind::Declarative(n.clone()),
                    _ => EnvKind::Declarative(None),
                },
                eval_vars: None,
            };
            frame.env = agent.heap.alloc_env_data(copy);
        }
        Op::PushWith => {
            let v = frame.pop();
            let o = ops::to_object(agent, v)?;
            frame.env = agent
                .heap
                .alloc_env_data(EnvData::object(Some(frame.env), o, true));
        }
        Op::InitHidden => {
            let env = frame.env;
            let names = match &agent.heap.env(env).kind {
                EnvKind::Declarative(Some(n)) => Some(n.clone()),
                _ => None,
            };
            if let Some(n) = names {
                let data = agent.heap.env_mut(env);
                if let Some(s) = n.this_slot {
                    data.slots[s as usize] = frame.this;
                    frame.this_env = Some(env);
                }
                if let Some(s) = n.new_target_slot {
                    data.slots[s as usize] = Some(frame.new_target);
                }
                if let Some(s) = n.function_slot {
                    data.slots[s as usize] = Some(Value::from_object(frame.function));
                }
            }
        }
        Op::GetLocal { hops, slot } => {
            let env = walk(agent, frame.env, hops);
            let v = agent.heap.env(env).slots[slot as usize];
            match v {
                Some(v) => frame.push(v),
                None => return tdz(agent, env, slot),
            }
        }
        Op::SetLocal { hops, slot } => {
            let env = walk(agent, frame.env, hops);
            if agent.heap.env(env).slots[slot as usize].is_none() {
                return tdz(agent, env, slot);
            }
            let v = frame.top();
            agent.heap.env_mut(env).slots[slot as usize] = Some(v);
        }
        Op::InitLocal { hops, slot } => {
            let env = walk(agent, frame.env, hops);
            let v = frame.pop();
            agent.heap.env_mut(env).slots[slot as usize] = Some(v);
        }
        Op::ThrowConstAssign { hops, slot } => {
            let env = walk(agent, frame.env, hops);
            if agent.heap.env(env).slots[slot as usize].is_none() {
                return tdz(agent, env, slot);
            }
            return agent.type_error("assignment to constant variable");
        }
        Op::CheckLocal { hops, slot } => {
            let env = walk(agent, frame.env, hops);
            if agent.heap.env(env).slots[slot as usize].is_none() {
                return tdz(agent, env, slot);
            }
        }
        Op::GetName(i) => {
            let name = frame.code.atoms[i as usize];
            let found = env_ops::lookup(agent, frame.env, name)?;
            let v = env_ops::get_binding_value(agent, found, name, strict)?;
            frame.push(v);
        }
        Op::GetNameOrUndefined(i) => {
            let name = frame.code.atoms[i as usize];
            let found = env_ops::lookup(agent, frame.env, name)?;
            let v = match found {
                Found::Unresolvable => Value::Undefined,
                f => env_ops::get_binding_value(agent, f, name, strict)?,
            };
            frame.push(v);
        }
        Op::GetNameWithThis(i) => {
            let name = frame.code.atoms[i as usize];
            let found = env_ops::lookup(agent, frame.env, name)?;
            let this = match found {
                Found::Object(o, true) => Value::Object(o),
                _ => Value::Undefined,
            };
            let v = env_ops::get_binding_value(agent, found, name, strict)?;
            frame.push(v);
            frame.push(this);
        }
        Op::ResolveRef(i) => {
            let name = frame.code.atoms[i as usize];
            let base = match env_ops::lookup(agent, frame.env, name)? {
                Found::Object(o, _) => Value::Object(o),
                Found::Unresolvable => Value::Undefined,
                _ => Value::Null,
            };
            frame.push(base);
        }
        Op::GetRef(i) => {
            let name = frame.code.atoms[i as usize];
            let base = frame.top();
            let found = match base {
                Value::Object(o) => Found::Object(o, false),
                Value::Null => env_ops::lookup(agent, frame.env, name)?,
                _ => Found::Unresolvable,
            };
            let v = env_ops::get_binding_value(agent, found, name, strict)?;
            frame.push(v);
        }
        Op::SetRef { name, strict } => {
            let name = frame.code.atoms[name as usize];
            let v = frame.pop();
            let base = frame.pop();
            let found = match base {
                Value::Object(o) => Found::Object(o, false),
                Value::Null => env_ops::lookup(agent, frame.env, name)?,
                _ => Found::Unresolvable,
            };
            env_ops::set_binding(agent, found, name, v, strict)?;
            frame.push(v);
        }
        Op::InitName(i) => {
            let name = frame.code.atoms[i as usize];
            let v = frame.pop();
            env_ops::initialize_binding(agent, frame.env, name, v)?;
        }
        Op::DeleteName(i) => {
            let name = frame.code.atoms[i as usize];
            let r = env_ops::delete_binding(agent, frame.env, name)?;
            frame.push(Value::Boolean(r));
        }
        Op::GetHidden(h) => {
            let v = env_ops::hidden_value(agent, frame.env, h)?;
            frame.push(v);
        }
        Op::BindThisDyn => {
            let v = frame.top();
            env_ops::bind_this_dynamic(agent, frame.env, v)?;
        }
        Op::BindThis { hops, slot } => {
            let v = frame.top();
            let env = walk(agent, frame.env, hops);
            env_ops::bind_this_slot(agent, env, slot, v)?;
        }
        Op::LoadThis => match frame.this {
            Some(v) => frame.push(v),
            None => {
                return agent.reference_error(
                    "must call super constructor in derived class before accessing 'this'",
                );
            }
        },
        Op::LoadNewTarget => {
            let v = frame.new_target;
            frame.push(v);
        }
        Op::LoadFunction => {
            let v = Value::from_object(frame.function);
            frame.push(v);
        }

        // ----- prologue -----
        Op::GetArg(i) => {
            let v = frame
                .args
                .get(i as usize)
                .copied()
                .unwrap_or(Value::Undefined);
            frame.push(v);
        }
        Op::RestArgs(i) => {
            let rest: Vec<Value> = frame.args.iter().skip(i as usize).copied().collect();
            let a = ops::create_array_from_list(agent, &rest);
            frame.push(Value::Object(a));
        }
        Op::CreateArguments { mapped } => {
            let o = function::create_arguments_object(agent, frame, mapped);
            frame.push(Value::Object(o));
        }
        Op::Closure(i) => {
            let code = frame.code.functions[i as usize].clone();
            let f = function::make_closure(agent, &code, frame.env, None);
            frame.push(Value::Object(f));
        }
        Op::ClosureNamed(i) => {
            let code = frame.code.functions[i as usize].clone();
            let f = function::make_named_closure(agent, &code, frame.env);
            frame.push(Value::Object(f));
        }
        Op::ClosureKeyName(i) => {
            let code = frame.code.functions[i as usize].clone();
            let key = function::key_of(agent, frame.top())?;
            let f = function::make_closure(agent, &code, frame.env, None);
            set_function_name(agent, f, key, None);
            frame.push(Value::Object(f));
        }
        Op::GlobalDeclInit(i) => {
            let code = frame.code.clone();
            env_ops::global_declaration_instantiation(
                agent,
                frame.env,
                &code,
                &code.decls[i as usize],
            )?;
        }
        Op::EvalDeclInit(i) => {
            let code = frame.code.clone();
            env_ops::eval_declaration_instantiation(
                agent,
                frame.env,
                &code,
                &code.decls[i as usize],
            )?;
        }
        Op::InitThisFields => {
            if let (Some(Value::Object(this)), Some(f)) = (frame.this, frame.function) {
                function::initialize_instance_elements(agent, this, f)?;
            }
        }

        // ----- operators -----
        Op::Add
        | Op::Sub
        | Op::Mul
        | Op::Div
        | Op::Rem
        | Op::Exp
        | Op::Shl
        | Op::Shr
        | Op::UShr
        | Op::BitAnd
        | Op::BitOr
        | Op::BitXor => {
            let b = frame.pop();
            let a = frame.pop();
            let r = operators::binary(agent, op, a, b)?;
            frame.push(r);
        }
        Op::Eq
        | Op::NotEq
        | Op::StrictEq
        | Op::StrictNotEq
        | Op::Lt
        | Op::Gt
        | Op::LtEq
        | Op::GtEq => {
            let b = frame.pop();
            let a = frame.pop();
            let r = operators::compare(agent, op, a, b)?;
            frame.push(r);
        }
        Op::In => {
            let b = frame.pop();
            let a = frame.pop();
            let r = operators::has_in(agent, a, b)?;
            frame.push(r);
        }
        Op::InstanceOf => {
            let b = frame.pop();
            let a = frame.pop();
            let r = ops::instanceof_operator(agent, a, b)?;
            frame.push(Value::Boolean(r));
        }
        Op::PrivateIn => {
            let obj = frame.pop();
            let name = frame.pop();
            let Value::Object(o) = obj else {
                return agent.type_error(
                    "cannot use 'in' operator to search for a private field in a non-object",
                );
            };
            let found = match name {
                Value::Symbol(s) => ops::private_element_find(agent, o, s).is_some(),
                _ => false,
            };
            frame.push(Value::Boolean(found));
        }
        Op::Neg => {
            let v = frame.pop();
            let r = operators::negate(agent, v)?;
            frame.push(r);
        }
        Op::ToNumber => {
            let v = frame.pop();
            let n = ops::to_number(agent, v)?;
            frame.push(Value::Number(n));
        }
        Op::Not => {
            let v = frame.pop();
            let b = ops::to_boolean(agent, v);
            frame.push(Value::Boolean(!b));
        }
        Op::BitNot => {
            let v = frame.pop();
            let r = operators::bit_not(agent, v)?;
            frame.push(r);
        }
        Op::Typeof => {
            let v = frame.pop();
            let r = operators::type_of(agent, v);
            frame.push(r);
        }
        Op::ToNumeric => {
            let v = frame.pop();
            let r = operators::to_numeric_value(agent, v)?;
            frame.push(r);
        }
        Op::Inc | Op::Dec => {
            let v = frame.pop();
            let delta = if matches!(op, Op::Inc) { 1 } else { -1 };
            let r = operators::step(agent, v, delta)?;
            frame.push(r);
        }
        Op::ToPropertyKey => {
            let v = frame.pop();
            let k = ops::to_property_key(agent, v)?;
            let kv = agent.heap.key_to_value(k);
            frame.push(kv);
        }
        Op::ToStringOp => {
            let v = frame.pop();
            let s = ops::to_string(agent, v)?;
            frame.push(Value::String(s));
        }
        Op::Concat => {
            let b = frame.pop();
            let a = frame.pop();
            let (Value::String(x), Value::String(y)) = (a, b) else {
                let r = operators::binary(agent, Op::Add, a, b)?;
                frame.push(r);
                return Ok(Step::Next);
            };
            let r = operators::concat(agent, x, y)?;
            frame.push(r);
        }
        Op::CheckObject(c) => {
            if !frame.top().is_object() {
                let msg = const_message(agent, frame, c);
                return agent.type_error(&msg);
            }
        }
        Op::RequireObjectCoercible => {
            let v = frame.top();
            ops::require_object_coercible(agent, v)?;
        }

        // ----- jumps -----
        Op::Jump(t) => frame.pc = t as usize,
        Op::JumpIfTrue(t) => {
            let v = frame.pop();
            if ops::to_boolean(agent, v) {
                frame.pc = t as usize;
            }
        }
        Op::JumpIfFalse(t) => {
            let v = frame.pop();
            if !ops::to_boolean(agent, v) {
                frame.pc = t as usize;
            }
        }
        Op::JumpIfTrueKeep(t) => {
            if ops::to_boolean(agent, frame.top()) {
                frame.pc = t as usize;
            } else {
                frame.pop();
            }
        }
        Op::JumpIfFalseKeep(t) => {
            if !ops::to_boolean(agent, frame.top()) {
                frame.pc = t as usize;
            } else {
                frame.pop();
            }
        }
        Op::JumpIfNotNullishKeep(t) => {
            if !frame.top().is_nullish() {
                frame.pc = t as usize;
            } else {
                frame.pop();
            }
        }
        Op::JumpIfNotUndefined(t) => {
            if !frame.top().is_undefined() {
                frame.pc = t as usize;
            }
        }
        Op::JumpIfNullishAt { depth, target } => {
            if frame.peek(depth as usize).is_nullish() {
                frame.pc = target as usize;
            }
        }

        // ----- properties -----
        Op::GetNamed(i) => {
            let base = frame.pop();
            let v = get_property(agent, base, named_key(frame, i))?;
            frame.push(v);
        }
        Op::GetComputed => {
            let key = frame.pop();
            let base = frame.pop();
            let k = computed_key(agent, base, key)?;
            let v = get_property(agent, base, k)?;
            frame.push(v);
        }
        Op::GetNamedKeep(i) => {
            let base = frame.top();
            let v = get_property(agent, base, named_key(frame, i))?;
            frame.push(v);
        }
        Op::GetComputedKeep => {
            let key = frame.pop();
            let base = frame.top();
            let k = computed_key(agent, base, key)?;
            let kv = agent.heap.key_to_value(k);
            frame.push(kv);
            let v = get_property(agent, base, k)?;
            frame.push(v);
        }
        Op::SetNamed { name, strict } => {
            let v = frame.pop();
            let base = frame.pop();
            put_property(agent, base, named_key(frame, name), v, strict)?;
            frame.push(v);
        }
        Op::SetComputed { strict } => {
            let v = frame.pop();
            let key = frame.pop();
            let base = frame.pop();
            let k = computed_key(agent, base, key)?;
            put_property(agent, base, k, v, strict)?;
            frame.push(v);
        }
        Op::GetMethodNamed(i) => {
            let base = frame.pop();
            let f = get_property(agent, base, named_key(frame, i))?;
            frame.push(f);
            frame.push(base);
        }
        Op::GetMethodComputed => {
            let key = frame.pop();
            let base = frame.pop();
            let k = computed_key(agent, base, key)?;
            let f = get_property(agent, base, k)?;
            frame.push(f);
            frame.push(base);
        }
        Op::DeleteNamed { name, strict } => {
            let base = frame.pop();
            let r = delete_property(agent, base, named_key(frame, name), strict)?;
            frame.push(r);
        }
        Op::DeleteComputed { strict } => {
            let key = frame.pop();
            let base = frame.pop();
            let k = computed_key(agent, base, key)?;
            let r = delete_property(agent, base, k, strict)?;
            frame.push(r);
        }
        Op::SuperBase => {
            let func = frame.pop();
            let home = match func {
                Value::Object(f) => match &agent.heap.object(f).kind {
                    ObjectKind::BytecodeFunction(d) => d.home_object,
                    _ => None,
                },
                _ => None,
            };
            let base = match home {
                Some(h) => Value::object_or_null(internal::get_prototype_of(agent, h)?),
                None => Value::Undefined,
            };
            frame.push(base);
        }
        Op::GetSuperNamed(i) => {
            let base = frame.pop();
            let this = frame.pop();
            let v = super_get(agent, base, named_key(frame, i), this)?;
            frame.push(v);
        }
        Op::GetSuperComputed => {
            let base = frame.pop();
            let key = frame.pop();
            let this = frame.pop();
            let k = ops::to_property_key(agent, key)?;
            let v = super_get(agent, base, k, this)?;
            frame.push(v);
        }
        Op::SetSuperNamed { name, strict } => {
            let v = frame.pop();
            let base = frame.pop();
            let this = frame.pop();
            super_set(agent, base, named_key(frame, name), v, this, strict)?;
            frame.push(v);
        }
        Op::SetSuperComputed { strict } => {
            let v = frame.pop();
            let base = frame.pop();
            let key = frame.pop();
            let this = frame.pop();
            let k = ops::to_property_key(agent, key)?;
            super_set(agent, base, k, v, this, strict)?;
            frame.push(v);
        }
        Op::GetSuperMethodNamed(i) => {
            let base = frame.pop();
            let this = frame.pop();
            let f = super_get(agent, base, named_key(frame, i), this)?;
            frame.push(f);
            frame.push(this);
        }
        Op::GetSuperMethodComputed => {
            let base = frame.pop();
            let key = frame.pop();
            let this = frame.pop();
            let k = ops::to_property_key(agent, key)?;
            let f = super_get(agent, base, k, this)?;
            frame.push(f);
            frame.push(this);
        }
        Op::GetPrivate => {
            let name = frame.pop();
            let obj = frame.pop();
            let v = private_get(agent, obj, name)?;
            frame.push(v);
        }
        Op::GetPrivateKeep => {
            let name = frame.peek(0);
            let obj = frame.peek(1);
            let v = private_get(agent, obj, name)?;
            frame.push(v);
        }
        Op::SetPrivate => {
            let v = frame.pop();
            let name = frame.pop();
            let obj = frame.pop();
            let o = as_object(agent, obj, "cannot write a private member of a non-object")?;
            let Value::Symbol(s) = name else {
                return agent.type_error("invalid private name");
            };
            ops::private_set(agent, o, s, v)?;
            frame.push(v);
        }
        Op::GetPrivateMethod => {
            let name = frame.pop();
            let obj = frame.pop();
            let f = private_get(agent, obj, name)?;
            frame.push(f);
            frame.push(obj);
        }

        // ----- literals -----
        Op::NewObject => {
            let o = ops::object_create(agent);
            frame.push(Value::Object(o));
        }
        Op::NewArray => {
            let a = internal::array_create(agent, 0.0, None)?;
            frame.push(Value::Object(a));
        }
        Op::ArrayPush => {
            let v = frame.pop();
            let arr = as_object(agent, frame.top(), "not an array")?;
            append(agent, arr, v)?;
        }
        Op::ArrayHole => {
            let arr = as_object(agent, frame.top(), "not an array")?;
            let len = internal::array_length(agent, arr);
            let key = PropertyKey::String(agent.heap.names.length);
            ops::set(agent, arr, key, Value::Number(f64::from(len) + 1.0), true)?;
        }
        Op::ArraySpread => {
            let iterable = frame.pop();
            let arr = as_object(agent, frame.top(), "not an array")?;
            let mut record = ops::get_iterator(agent, iterable)?;
            while let Some(v) = ops::iterator_step_value(agent, &mut record)? {
                append(agent, arr, v)?;
            }
        }
        Op::DefineField => {
            let v = frame.pop();
            let key = frame.pop();
            let obj = as_object(agent, frame.top(), "not an object")?;
            let k = ops::to_property_key(agent, key)?;
            ops::create_data_property_or_throw(agent, obj, k, v)?;
        }
        Op::DefineMethod { func, kind } => {
            let key = frame.pop();
            let obj = as_object(agent, frame.top(), "not an object")?;
            let k = ops::to_property_key(agent, key)?;
            let code = frame.code.functions[func as usize].clone();
            function::define_method(agent, &code, frame.env, obj, k, kind, true)?;
        }
        Op::SetProtoLiteral => {
            let v = frame.pop();
            let obj = as_object(agent, frame.top(), "not an object")?;
            match v {
                Value::Object(p) => {
                    internal::set_prototype_of(agent, obj, Some(p))?;
                }
                Value::Null => {
                    internal::set_prototype_of(agent, obj, None)?;
                }
                _ => {}
            }
        }
        Op::CopyDataProps => {
            let src = frame.pop();
            let obj = as_object(agent, frame.top(), "not an object")?;
            ops::copy_data_properties(agent, obj, src, &[])?;
        }
        Op::CopyRest => {
            let keys = frame.pop();
            let src = frame.pop();
            let list = list_of(agent, keys)?;
            let mut excluded = Vec::with_capacity(list.len());
            for k in list {
                excluded.push(ops::to_property_key(agent, k)?);
            }
            let o = ops::object_create(agent);
            ops::copy_data_properties(agent, o, src, &excluded)?;
            frame.push(Value::Object(o));
        }
        Op::GetTemplateObject(i) => {
            let o = template_object(agent, frame, i)?;
            frame.push(Value::Object(o));
        }
        Op::RegExp { pattern, flags } => {
            let p = frame.code.consts[pattern as usize];
            let f = frame.code.consts[flags as usize];
            let realm = agent.current_realm();
            let Some(ctor) = agent
                .heap
                .realm(realm)
                .intrinsics
                .try_get(Intrinsic::RegExp)
            else {
                return agent.syntax_error("regular expressions are not available");
            };
            let o = ops::construct(agent, ctor, &[p, f], None)?;
            frame.push(Value::Object(o));
        }

        // ----- calls -----
        Op::Call(argc) => {
            let at = frame.stack.len() - argc as usize;
            let args = frame.stack.split_off(at);
            let this = frame.pop();
            let f = frame.pop();
            return Ok(call(agent, f, this, args)?.into_step(frame));
        }
        Op::CallArray => {
            let arr = frame.pop();
            let this = frame.pop();
            let f = frame.pop();
            let args = list_of(agent, arr)?;
            return Ok(call(agent, f, this, args)?.into_step(frame));
        }
        Op::CallEval { argc, site } => {
            let at = frame.stack.len() - argc as usize;
            let args = frame.stack.split_off(at);
            return call_eval(agent, frame, args, site);
        }
        Op::CallEvalArray { site } => {
            let arr = frame.pop();
            let args = list_of(agent, arr)?;
            return call_eval(agent, frame, args, site);
        }
        Op::New(argc) => {
            let at = frame.stack.len() - argc as usize;
            let args = frame.stack.split_off(at);
            let f = frame.pop();
            return Ok(construct(agent, f, args)?.into_step(frame));
        }
        Op::NewSpread => {
            let arr = frame.pop();
            let f = frame.pop();
            let args = list_of(agent, arr)?;
            return Ok(construct(agent, f, args)?.into_step(frame));
        }
        Op::GetSuperConstructor => {
            let func = frame.pop();
            let proto = match func {
                Value::Object(f) => internal::get_prototype_of(agent, f)?,
                _ => None,
            };
            frame.push(Value::object_or_null(proto));
        }
        Op::SuperCall(argc) => {
            let at = frame.stack.len() - argc as usize;
            let args = frame.stack.split_off(at);
            let nt = frame.pop();
            let ctor = frame.pop();
            let r = super_construct(agent, ctor, nt, &args)?;
            frame.push(r);
        }
        Op::SuperCallSpread => {
            let arr = frame.pop();
            let nt = frame.pop();
            let ctor = frame.pop();
            let args = list_of(agent, arr)?;
            let r = super_construct(agent, ctor, nt, &args)?;
            frame.push(r);
        }
        Op::SuperCallForward => {
            let nt = frame.pop();
            let ctor = frame.pop();
            let args = frame.args.clone();
            let r = super_construct(agent, ctor, nt, &args)?;
            frame.push(r);
        }
        Op::InitInstanceElements => {
            let func = frame.pop();
            if let (Value::Object(this), Value::Object(f)) = (frame.top(), func) {
                function::initialize_instance_elements(agent, this, f)?;
            }
        }
        Op::Return => {
            let v = frame.pop();
            return Ok(Step::Return(v));
        }

        // ----- exceptions -----
        Op::Throw => return Err(Throw(frame.pop())),
        Op::PushHandler(t) => {
            let h = Handler {
                pc: t,
                stack_len: frame.stack.len(),
                env: frame.env,
            };
            frame.handlers.push(h);
        }
        Op::PopHandler => {
            frame.handlers.pop();
        }
        Op::ThrowTypeError(c) => {
            let msg = const_message(agent, frame, c);
            return agent.type_error(&msg);
        }
        Op::ThrowReferenceError(c) => {
            let msg = const_message(agent, frame, c);
            return agent.reference_error(&msg);
        }
        Op::EndFinally { kind, value, table } => match reg_number(frame, kind) {
            0 => {}
            1 => return Err(Throw(frame.regs[value as usize])),
            k => {
                let target = frame.code.exit_tables[table as usize][(k - 2) as usize];
                frame.pc = target as usize;
            }
        },

        // ----- iteration -----
        Op::GetIterator { iter, is_async } => {
            let v = frame.pop();
            let record = if is_async {
                get_async_iterator(agent, v)?
            } else {
                ops::get_iterator(agent, v)?
            };
            frame.regs[iter as usize] = Value::Object(record.iterator);
            frame.regs[iter as usize + 1] = record.next_method;
        }
        Op::IteratorStep { iter, done } => {
            let mut record = record_of(frame, iter);
            match ops::iterator_step_value(agent, &mut record)? {
                Some(v) => frame.push(v),
                None => frame.pc = done as usize,
            }
        }
        Op::IteratorCallNext { iter } => {
            let record = record_of(frame, iter);
            let r = ops::call(
                agent,
                record.next_method,
                Value::Object(record.iterator),
                &[],
            )?;
            frame.push(r);
        }
        Op::IteratorResult { done } => {
            let r = frame.pop();
            let Value::Object(r) = r else {
                return agent.type_error("iterator result is not an object");
            };
            if ops::iterator_complete(agent, r)? {
                frame.pc = done as usize;
            } else {
                let v = ops::iterator_value(agent, r)?;
                frame.push(v);
            }
        }
        Op::IteratorClose { iter } => {
            let record = record_of(frame, iter);
            ops::iterator_close(agent, &record, Ok(()))?;
        }
        Op::IteratorCloseThrow { iter } => {
            let e = frame.pop();
            let record = record_of(frame, iter);
            ops::iterator_close::<()>(agent, &record, Err(Throw(e)))?;
            return Err(Throw(e));
        }
        Op::AsyncIteratorCloseCall { iter, skip } => {
            let record = record_of(frame, iter);
            let it = Value::Object(record.iterator);
            let key = PropertyKey::String(agent.heap.names.return_);
            match ops::get_method(agent, it, key)? {
                None => frame.pc = skip as usize,
                Some(m) => {
                    let r = ops::call(agent, m, it, &[])?;
                    frame.push(r);
                }
            }
        }
        Op::AsyncIteratorCloseThrowCall { iter, skip } => {
            let record = record_of(frame, iter);
            let it = Value::Object(record.iterator);
            let key = PropertyKey::String(agent.heap.names.return_);
            match ops::get_method(agent, it, key) {
                Ok(Some(m)) => match ops::call(agent, m, it, &[]) {
                    Ok(r) => frame.push(r),
                    Err(_) => frame.pc = skip as usize,
                },
                _ => frame.pc = skip as usize,
            }
        }
        Op::DestructIter { base } => {
            let v = frame.pop();
            let record = ops::get_iterator(agent, v)?;
            frame.regs[base as usize] = Value::Object(record.iterator);
            frame.regs[base as usize + 1] = record.next_method;
            frame.regs[base as usize + 2] = Value::Boolean(false);
        }
        Op::DestructStep { base } => {
            let v = destruct_step(agent, frame, base)?;
            frame.push(v.unwrap_or(Value::Undefined));
        }
        Op::DestructRest { base } => {
            let mut values = Vec::new();
            while let Some(v) = destruct_step(agent, frame, base)? {
                values.push(v);
            }
            let a = ops::create_array_from_list(agent, &values);
            frame.push(Value::Object(a));
        }
        Op::DestructClose { base } => {
            if frame.regs[base as usize + 2] != Value::Boolean(true) {
                let record = record_of(frame, base);
                ops::iterator_close(agent, &record, Ok(()))?;
            }
        }
        Op::DestructCloseThrow { base } => {
            let e = frame.pop();
            if frame.regs[base as usize + 2] != Value::Boolean(true) {
                let record = record_of(frame, base);
                ops::iterator_close::<()>(agent, &record, Err(Throw(e)))?;
            }
            return Err(Throw(e));
        }
        Op::ForInPrepare { reg, skip } => {
            let v = frame.pop();
            if v.is_nullish() {
                frame.pc = skip as usize;
            } else {
                let o = ops::to_object(agent, v)?;
                let it = iter::create_for_in_iterator(agent, o);
                frame.regs[reg as usize] = Value::Object(it);
            }
        }
        Op::ForInNext { reg, done } => {
            let Value::Object(it) = frame.regs[reg as usize] else {
                frame.pc = done as usize;
                return Ok(Step::Next);
            };
            match iter::for_in_next(agent, it)? {
                Some(k) => frame.push(k),
                None => frame.pc = done as usize,
            }
        }

        // ----- generators and async -----
        Op::InitialYield => {
            let g = generator::create_generator(agent, frame)?;
            return Ok(Step::Suspend(Suspend::Initial(g)));
        }
        Op::Yield => {
            let v = frame.pop();
            return Ok(Step::Suspend(Suspend::Yield {
                value: v,
                raw: false,
            }));
        }
        Op::YieldRaw => {
            let v = frame.pop();
            return Ok(Step::Suspend(Suspend::Yield {
                value: v,
                raw: true,
            }));
        }
        Op::AsyncGenYield => {
            let v = frame.pop();
            let g = frame.generator.expect("async generator frame");
            match generator::async_generator_yield(agent, g, v) {
                Some((value, mode)) => {
                    frame.push(value);
                    frame.push(Value::Number(f64::from(mode)));
                }
                None => return Ok(Step::Suspend(Suspend::AsyncGenYield)),
            }
        }
        Op::ResumeKind { ret } => {
            let mode = frame.pop();
            match mode {
                Value::Number(n) if n as i32 == MODE_THROW => return Err(Throw(frame.pop())),
                Value::Number(n) if n as i32 == MODE_RETURN => frame.pc = ret as usize,
                _ => {}
            }
        }
        Op::Await => {
            let v = frame.pop();
            generator::await_value(agent, frame, v)?;
            return Ok(Step::Suspend(Suspend::Await));
        }
        Op::AwaitResume => {
            let mode = frame.pop();
            if let Value::Number(n) = mode {
                if n as i32 == MODE_THROW {
                    return Err(Throw(frame.pop()));
                }
            }
        }
        Op::YieldStarCall {
            iter,
            no_throw,
            ret,
        } => {
            let record = record_of(frame, iter);
            let it = Value::Object(record.iterator);
            let received = frame.regs[iter as usize + 2];
            match reg_number(frame, iter + 3) {
                MODE_NEXT => {
                    let r = ops::call(agent, record.next_method, it, &[received])?;
                    frame.push(r);
                }
                MODE_THROW => {
                    let key = PropertyKey::String(agent.heap.names.throw);
                    match ops::get_method(agent, it, key)? {
                        Some(m) => {
                            let r = ops::call(agent, m, it, &[received])?;
                            frame.push(r);
                        }
                        None => frame.pc = no_throw as usize,
                    }
                }
                _ => {
                    let key = PropertyKey::String(agent.heap.names.return_);
                    match ops::get_method(agent, it, key)? {
                        Some(m) => {
                            let r = ops::call(agent, m, it, &[received])?;
                            frame.push(r);
                        }
                        None => {
                            frame.push(received);
                            frame.pc = ret as usize;
                        }
                    }
                }
            }
        }
        Op::YieldStarResult {
            iter,
            done,
            ret,
            is_async,
        } => {
            let r = frame.pop();
            let Value::Object(ro) = r else {
                return agent.type_error("iterator result is not an object");
            };
            if ops::iterator_complete(agent, ro)? {
                let v = ops::iterator_value(agent, ro)?;
                frame.push(v);
                frame.pc = if reg_number(frame, iter + 3) == MODE_RETURN {
                    ret as usize
                } else {
                    done as usize
                };
            } else if is_async {
                let v = ops::iterator_value(agent, ro)?;
                frame.push(v);
            } else {
                frame.push(r);
            }
        }

        // ----- classes -----
        Op::ClassHeritage => {
            let superclass = frame.pop();
            match superclass {
                Value::Null => {
                    let fp = agent.intrinsic(Intrinsic::FunctionPrototype);
                    frame.push(Value::Null);
                    frame.push(Value::Object(fp));
                }
                Value::Object(c) if agent.heap.object(c).is_constructor() => {
                    let key = PropertyKey::String(agent.heap.names.prototype);
                    let proto_parent = ops::get(agent, c, key)?;
                    if !matches!(proto_parent, Value::Object(_) | Value::Null) {
                        return agent
                            .type_error("class extends value does not have a valid prototype");
                    }
                    frame.push(proto_parent);
                    frame.push(superclass);
                }
                _ => return agent.type_error("class extends value is not a constructor or null"),
            }
        }
        Op::ClassNoHeritage => {
            let op_ = agent.intrinsic(Intrinsic::ObjectPrototype);
            let fp = agent.intrinsic(Intrinsic::FunctionPrototype);
            frame.push(Value::Object(op_));
            frame.push(Value::Object(fp));
        }
        Op::CreateClass { func, name, .. } => {
            let ctor_parent = frame.pop();
            let proto_parent = frame.pop();
            let code = frame.code.functions[func as usize].clone();
            let key = function::class_name_key(agent, frame, name)?;
            let ctor_parent = as_object(agent, ctor_parent, "invalid class parent")?;
            let (f, proto) = function::create_class(
                agent,
                &code,
                frame.env,
                proto_parent.as_object(),
                ctor_parent,
                key,
            )?;
            frame.push(Value::Object(f));
            frame.push(Value::Object(proto));
        }
        Op::DefineClassMethod {
            func,
            kind,
            is_static,
        } => {
            let key = frame.pop();
            let target = if is_static {
                frame.peek(1)
            } else {
                frame.peek(0)
            };
            let target = as_object(agent, target, "invalid class")?;
            let k = ops::to_property_key(agent, key)?;
            let code = frame.code.functions[func as usize].clone();
            function::define_method(agent, &code, frame.env, target, k, kind, false)?;
        }
        Op::DefinePrivateMethod {
            func,
            kind,
            is_static,
        } => {
            let name = frame.pop();
            let class = as_object(agent, frame.peek(1), "invalid class")?;
            let proto = as_object(agent, frame.peek(0), "invalid class")?;
            let Value::Symbol(name) = name else {
                return agent.type_error("invalid private name");
            };
            let code = frame.code.functions[func as usize].clone();
            let home = if is_static { class } else { proto };
            function::define_private_method(
                agent, &code, frame.env, class, home, name, kind, is_static,
            )?;
        }
        Op::AddField { func, private } => {
            let key = frame.pop();
            let class = as_object(agent, frame.peek(1), "invalid class")?;
            let proto = as_object(agent, frame.peek(0), "invalid class")?;
            let name = function::field_name(agent, key, private)?;
            let initializer = (func != u32::MAX).then(|| {
                let code = frame.code.functions[func as usize].clone();
                function::make_closure(agent, &code, frame.env, Some(proto))
            });
            function::add_field(agent, class, ClassFieldDefinition { name, initializer });
        }
        Op::DefineStaticField { func, private } => {
            let key = frame.pop();
            let class = as_object(agent, frame.top(), "invalid class")?;
            let name = function::field_name(agent, key, private)?;
            let initializer = (func != u32::MAX).then(|| {
                let code = frame.code.functions[func as usize].clone();
                function::make_closure(agent, &code, frame.env, Some(class))
            });
            function::define_field(agent, class, ClassFieldDefinition { name, initializer })?;
        }
        Op::StaticBlock(func) => {
            let class = as_object(agent, frame.top(), "invalid class")?;
            let code = frame.code.functions[func as usize].clone();
            let f = function::make_closure(agent, &code, frame.env, Some(class));
            ops::call(agent, Value::Object(f), Value::Object(class), &[])?;
        }
        Op::NewPrivateName => {
            let desc = frame.pop();
            let s = match desc {
                Value::String(s) => s,
                _ => agent.heap.empty_string(),
            };
            let sym = agent.heap.alloc_private_name(s);
            frame.push(Value::Symbol(sym));
        }

        // ----- explicit resource management -----
        Op::UsingInit(reg) => {
            let a = ops::create_array_from_list(agent, &[]);
            frame.regs[reg as usize] = Value::Object(a);
        }
        Op::AddDisposable { reg, is_async } => {
            let v = frame.top();
            add_disposable(agent, frame.regs[reg as usize], v, is_async)?;
        }
        Op::DisposeStep { reg, done } => match dispose_step(agent, frame.regs[reg as usize])? {
            Some(r) => frame.push(r),
            None => frame.pc = done as usize,
        },
        Op::DisposeError { kind, value } => {
            let err = frame.pop();
            if reg_number(frame, kind) == 1 {
                let suppressed = frame.regs[value as usize];
                let e = suppressed_error(agent, err, suppressed);
                frame.regs[value as usize] = Value::Object(e);
            } else {
                frame.regs[kind as usize] = Value::Number(1.0);
                frame.regs[value as usize] = err;
            }
        }

        Op::Debugger => {}
    }
    Ok(Step::Next)
}

fn record_of(frame: &Frame, iter: u32) -> IteratorRecord {
    let iterator = match frame.regs[iter as usize] {
        Value::Object(o) => o,
        _ => panic!("iterator register holds no object"),
    };
    IteratorRecord {
        iterator,
        next_method: frame.regs[iter as usize + 1],
        done: false,
    }
}

/// One element of an array destructuring: the next value, or `None` once
/// the iterator is done (the done flag lives in register `base + 2`).
fn destruct_step(agent: &mut Agent, frame: &mut Frame, base: u32) -> JsResult<Option<Value>> {
    let done_reg = base as usize + 2;
    if frame.regs[done_reg] == Value::Boolean(true) {
        return Ok(None);
    }
    let mut record = record_of(frame, base);
    let r = ops::iterator_step_value(agent, &mut record);
    if record.done || r.is_err() {
        frame.regs[done_reg] = Value::Boolean(true);
    }
    r
}

/// GetIterator(obj, async).
fn get_async_iterator(agent: &mut Agent, v: Value) -> JsResult<IteratorRecord> {
    let key = PropertyKey::Symbol(agent.symbols.async_iterator);
    if let Some(method) = ops::get_method(agent, v, key)? {
        return ops::get_iterator_from_method(agent, v, method);
    }
    let key = PropertyKey::Symbol(agent.symbols.iterator);
    let Some(method) = ops::get_method(agent, v, key)? else {
        return agent.type_error("value is not async iterable");
    };
    let record = ops::get_iterator_from_method(agent, v, method)?;
    Ok(iter::create_async_from_sync_iterator(agent, record))
}

fn super_get(agent: &mut Agent, base: Value, key: PropertyKey, this: Value) -> JsResult<Value> {
    let o = ops::to_object(agent, base)?;
    internal::get(agent, o, key, this)
}

fn super_set(
    agent: &mut Agent,
    base: Value,
    key: PropertyKey,
    v: Value,
    this: Value,
    strict: bool,
) -> JsResult<()> {
    let o = ops::to_object(agent, base)?;
    let ok = internal::set(agent, o, key, v, this)?;
    if !ok && strict {
        let k = agent.heap.key_to_rust(key);
        return agent.type_error(&format!("cannot assign to read-only property '{k}'"));
    }
    Ok(())
}

fn private_get(agent: &mut Agent, obj: Value, name: Value) -> JsResult<Value> {
    let o = as_object(agent, obj, "cannot read a private member of a non-object")?;
    let Value::Symbol(s) = name else {
        return agent.type_error("invalid private name");
    };
    ops::private_get(agent, o, s)
}

/// `super(...args)`: Construct(func, args, newTarget).
fn super_construct(agent: &mut Agent, ctor: Value, nt: Value, args: &[Value]) -> JsResult<Value> {
    let Value::Object(c) = ctor else {
        return agent.type_error("super constructor is not a constructor");
    };
    if !agent.heap.object(c).is_constructor() {
        return agent.type_error("super constructor is not a constructor");
    }
    let Value::Object(nt) = nt else {
        return agent.type_error("'super' keyword unexpected here");
    };
    Ok(Value::Object(construct_object(agent, c, args, nt)?))
}

/// A call whose callee is named `eval`: PerformEval if it is `%eval%`.
fn call_eval(agent: &mut Agent, frame: &mut Frame, args: Vec<Value>, site: u32) -> JsResult<Step> {
    let this = frame.pop();
    let f = frame.pop();
    let eval_fn = agent.intrinsic(Intrinsic::Eval);
    if f != Value::Object(eval_fn) {
        return Ok(call(agent, f, this, args)?.into_step(frame));
    }
    let Some(&x) = args.first() else {
        frame.push(Value::Undefined);
        return Ok(Step::Next);
    };
    let code = frame.code.clone();
    let site = &code.eval_sites[site as usize];
    match eval::prepare_eval(agent, x, Some((frame.env, site)))? {
        Err(v) => {
            frame.push(v);
            Ok(Step::Next)
        }
        Ok(eval_frame) => Ok(Step::Call(Box::new(eval_frame))),
    }
}

/// GetTemplateObject(templateLiteral).
fn template_object(agent: &mut Agent, frame: &Frame, i: u32) -> JsResult<Object> {
    let site = &frame.code.templates[i as usize];
    let realm = agent.current_realm();
    if let Some((_, o)) = agent
        .heap
        .realm(realm)
        .template_map
        .iter()
        .find(|(id, _)| *id == site.id)
    {
        return Ok(*o);
    }
    let cooked: Vec<Value> = site
        .cooked
        .iter()
        .map(|c| c.map_or(Value::Undefined, |c| frame.code.consts[c as usize]))
        .collect();
    let raw: Vec<Value> = site
        .raw
        .iter()
        .map(|c| frame.code.consts[*c as usize])
        .collect();
    let template = ops::create_array_from_list(agent, &cooked);
    let raw_obj = ops::create_array_from_list(agent, &raw);
    ops::set_integrity_level(agent, raw_obj, IntegrityLevel::Frozen)?;
    let key = agent.heap.key_from_str("raw");
    ops::define_property_or_throw(
        agent,
        template,
        key,
        PropertyDescriptor::data(Value::Object(raw_obj), false, false, false),
    )?;
    ops::set_integrity_level(agent, template, IntegrityLevel::Frozen)?;
    agent
        .heap
        .realm_mut(realm)
        .template_map
        .push((site.id, template));
    Ok(template)
}

// ----- explicit resource management -----

/// AddDisposableResource(disposeCapability, v, hint). Resources are kept
/// as triples `[value, method, needs-result]` in an array.
fn add_disposable(agent: &mut Agent, stack: Value, v: Value, is_async: bool) -> JsResult<()> {
    let Value::Object(list) = stack else {
        return Ok(());
    };
    let (method, use_result) = if v.is_nullish() {
        if !is_async {
            return Ok(());
        }
        (Value::Undefined, false)
    } else {
        if !v.is_object() {
            return agent.type_error("using declarations need an object, null or undefined");
        }
        let mut found = None;
        if is_async {
            let key = PropertyKey::Symbol(agent.symbols.async_dispose);
            found = ops::get_method(agent, v, key)?.map(|m| (m, true));
        }
        if found.is_none() {
            let key = PropertyKey::Symbol(agent.symbols.dispose);
            found = ops::get_method(agent, v, key)?.map(|m| (m, !is_async));
        }
        match found {
            Some(m) => m,
            None => return agent.type_error("the value is not disposable"),
        }
    };
    append(agent, list, v)?;
    append(agent, list, method)?;
    append(agent, list, Value::Boolean(use_result))?;
    Ok(())
}

/// Disposes the newest resource: its dispose method's result (or
/// `undefined`), or `None` when the stack is empty.
fn dispose_step(agent: &mut Agent, stack: Value) -> JsResult<Option<Value>> {
    let Value::Object(list) = stack else {
        return Ok(None);
    };
    let len = internal::array_length(agent, list);
    if len < 3 {
        return Ok(None);
    }
    let v = ops::get(agent, list, PropertyKey::Index(len - 3))?;
    let method = ops::get(agent, list, PropertyKey::Index(len - 2))?;
    let use_result = ops::get(agent, list, PropertyKey::Index(len - 1))?;
    let key = PropertyKey::String(agent.heap.names.length);
    ops::set(agent, list, key, Value::Number(f64::from(len - 3)), true)?;
    if method.is_undefined() {
        return Ok(Some(Value::Undefined));
    }
    let r = ops::call(agent, method, v, &[])?;
    Ok(Some(if use_result == Value::Boolean(true) {
        r
    } else {
        Value::Undefined
    }))
}

/// A SuppressedError with `error` and `suppressed`.
fn suppressed_error(agent: &mut Agent, error: Value, suppressed: Value) -> Object {
    let realm = agent.current_realm();
    let proto = agent
        .heap
        .realm(realm)
        .intrinsics
        .try_get(Intrinsic::SuppressedErrorPrototype)
        .unwrap_or_else(|| agent.intrinsic(Intrinsic::ErrorPrototype));
    let o = agent.heap.create_object(Some(proto), ObjectKind::Error);
    let key = PropertyKey::String(agent.heap.names.error);
    agent.heap.set_own(
        o,
        key,
        Attributes::data(true, false, true),
        Slot::Data(error),
    );
    let key = PropertyKey::String(agent.heap.names.suppressed);
    agent.heap.set_own(
        o,
        key,
        Attributes::data(true, false, true),
        Slot::Data(suppressed),
    );
    o
}
