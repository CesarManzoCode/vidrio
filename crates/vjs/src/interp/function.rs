//! Function objects of bytecode (OrdinaryFunctionCreate and its callers),
//! the `[[Call]]`/`[[Construct]]` set-up of their frames, `arguments`
//! objects and the class helpers (class constructors, private methods,
//! InitializeInstanceElements).

use std::rc::Rc;

use crate::agent::{Agent, ExecutionContext};
use crate::bytecode::{ClassName, Code, MethodOp};
use crate::completion::JsResult;
use crate::exotic::ArgumentsData;
use crate::function::{
    BytecodeFunctionData, ClassFieldDefinition, ClassFieldName, CodeBlock, ConstructorKind,
    ThisMode, make_constructor, set_function_length, set_function_name,
};
use crate::gc::{Env, Object, Realm, Symbol};
use crate::interp::{Frame, FrameKind, function_code, promise};
use crate::object::{
    Attributes, ObjectKind, PrivateElement, PrivateElementKind, PropertyDescriptor, Slot,
};
use crate::ops;
use crate::realm::Intrinsic;
use crate::value::{PropertyKey, Value};

fn realm_intrinsic(agent: &Agent, realm: Realm, which: Intrinsic) -> Object {
    agent.heap.realm(realm).intrinsics.get(which)
}

/// OrdinaryFunctionCreate plus the naming and `prototype` of the kind of
/// function `code` is. `home` makes it a method.
pub fn make_closure(agent: &mut Agent, code: &Rc<Code>, env: Env, home: Option<Object>) -> Object {
    let realm = agent.current_realm();
    let flags = &code.flags;
    let proto_kind = match (flags.is_async, flags.is_generator) {
        (true, true) => Intrinsic::AsyncGeneratorFunctionPrototype,
        (false, true) => Intrinsic::GeneratorFunctionPrototype,
        (true, false) => Intrinsic::AsyncFunctionPrototype,
        (false, false) => Intrinsic::FunctionPrototype,
    };
    let proto = realm_intrinsic(agent, realm, proto_kind);
    let this_mode = if flags.is_arrow {
        ThisMode::Lexical
    } else if code.strict {
        ThisMode::Strict
    } else {
        ThisMode::Global
    };
    let data = BytecodeFunctionData {
        code: code.clone() as Rc<dyn CodeBlock>,
        env: Some(env),
        private_env: None,
        realm,
        script_or_module: agent.running_context().and_then(|c| c.script_or_module),
        home_object: home,
        this_mode,
        strict: code.strict,
        constructor_kind: if flags.is_derived {
            ConstructorKind::Derived
        } else {
            ConstructorKind::Base
        },
        is_constructor: false,
        is_class_constructor: flags.is_class_constructor,
        fields: Vec::new(),
        private_methods: Vec::new(),
    };
    let f = agent
        .heap
        .create_object(Some(proto), ObjectKind::BytecodeFunction(Box::new(data)));
    set_function_length(agent, f, f64::from(code.length));
    if let Some(name) = code.name {
        set_function_name(agent, f, PropertyKey::String(name), None);
    }
    if flags.is_generator {
        let which = if flags.is_async {
            Intrinsic::AsyncGeneratorPrototype
        } else {
            Intrinsic::GeneratorPrototype
        };
        let gen_proto = realm_intrinsic(agent, realm, which);
        let prototype = ops::ordinary_object_create(agent, Some(gen_proto));
        let key = PropertyKey::String(agent.heap.names.prototype);
        agent.heap.set_own(
            f,
            key,
            Attributes::data(true, false, false),
            Slot::Data(Value::Object(prototype)),
        );
    } else if flags.is_constructor {
        make_constructor(agent, f, true, None);
    }
    f
}

/// A closure for a named function expression: its own environment holds
/// the name.
pub fn make_named_closure(agent: &mut Agent, code: &Rc<Code>, env: Env) -> Object {
    let Some(names) = code.name_scope.clone() else {
        return make_closure(agent, code, env, None);
    };
    let func_env = agent
        .heap
        .alloc_env_data(crate::env::EnvData::for_scope(Some(env), names));
    let f = make_closure(agent, code, func_env, None);
    agent.heap.env_mut(func_env).slots[0] = Some(Value::Object(f));
    f
}

/// The property key named by a key value (string or symbol).
pub(crate) fn key_of(agent: &mut Agent, v: Value) -> JsResult<PropertyKey> {
    ops::to_property_key(agent, v)
}

fn method_prefix(kind: MethodOp) -> Option<&'static str> {
    match kind {
        MethodOp::Method => None,
        MethodOp::Get => Some("get"),
        MethodOp::Set => Some("set"),
    }
}

/// DefineMethodProperty / the accessor cases of MethodDefinitionEvaluation
/// for a public key.
pub fn define_method(
    agent: &mut Agent,
    code: &Rc<Code>,
    env: Env,
    target: Object,
    key: PropertyKey,
    kind: MethodOp,
    enumerable: bool,
) -> JsResult<()> {
    let f = make_closure(agent, code, env, Some(target));
    set_function_name(agent, f, key, method_prefix(kind));
    let fv = Value::Object(f);
    let desc = match kind {
        MethodOp::Method => PropertyDescriptor::data(fv, true, enumerable, true),
        MethodOp::Get => PropertyDescriptor {
            get: Some(fv),
            enumerable: Some(enumerable),
            configurable: Some(true),
            ..PropertyDescriptor::default()
        },
        MethodOp::Set => PropertyDescriptor {
            set: Some(fv),
            enumerable: Some(enumerable),
            configurable: Some(true),
            ..PropertyDescriptor::default()
        },
    };
    ops::define_property_or_throw(agent, target, key, desc)
}

/// A private method or accessor of a class: added to the class itself
/// (static) or to its `[[PrivateMethods]]`.
#[allow(clippy::too_many_arguments)]
pub fn define_private_method(
    agent: &mut Agent,
    code: &Rc<Code>,
    env: Env,
    class: Object,
    home: Object,
    name: Symbol,
    kind: MethodOp,
    is_static: bool,
) -> JsResult<()> {
    let f = make_closure(agent, code, env, Some(home));
    set_function_name(agent, f, PropertyKey::Symbol(name), method_prefix(kind));
    let fv = Value::Object(f);
    let element_kind = match kind {
        MethodOp::Method => PrivateElementKind::Method(fv),
        MethodOp::Get => PrivateElementKind::Accessor {
            get: fv,
            set: Value::Undefined,
        },
        MethodOp::Set => PrivateElementKind::Accessor {
            get: Value::Undefined,
            set: fv,
        },
    };
    let combine = |list: &mut Vec<PrivateElement>| {
        if let Some(existing) = list.iter_mut().find(|e| e.key == name) {
            if let (
                PrivateElementKind::Accessor { get, set },
                PrivateElementKind::Accessor {
                    get: new_get,
                    set: new_set,
                },
            ) = (&mut existing.kind, element_kind)
            {
                if !new_get.is_undefined() {
                    *get = new_get;
                }
                if !new_set.is_undefined() {
                    *set = new_set;
                }
                return;
            }
        }
        list.push(PrivateElement {
            key: name,
            kind: element_kind,
        });
    };
    if is_static {
        combine(agent.heap.private_elements_mut(class));
    } else if let ObjectKind::BytecodeFunction(data) = &mut agent.heap.object_mut(class).kind {
        combine(&mut data.private_methods);
    }
    Ok(())
}

/// Creates the class constructor F and its prototype (steps of
/// ClassDefinitionEvaluation up to CreateMethodProperty of
/// `constructor`).
pub fn create_class(
    agent: &mut Agent,
    code: &Rc<Code>,
    env: Env,
    proto_parent: Option<Object>,
    ctor_parent: Object,
    name: PropertyKey,
) -> JsResult<(Object, Object)> {
    let proto = ops::ordinary_object_create(agent, proto_parent);
    let f = make_closure(agent, code, env, Some(proto));
    agent.heap.set_proto(f, Some(ctor_parent));
    set_function_name(agent, f, name, None);
    make_constructor(agent, f, false, Some(proto));
    if let ObjectKind::BytecodeFunction(data) = &mut agent.heap.object_mut(f).kind {
        data.is_class_constructor = true;
        if code.flags.is_derived {
            data.constructor_kind = ConstructorKind::Derived;
        }
    }
    let key = PropertyKey::String(agent.heap.names.constructor);
    agent.heap.set_own(
        proto,
        key,
        Attributes::data(true, false, true),
        Slot::Data(Value::Object(f)),
    );
    Ok((f, proto))
}

/// The class name given to `CreateClass`.
pub(crate) fn class_name_key(
    agent: &mut Agent,
    frame: &Frame,
    name: ClassName,
) -> JsResult<PropertyKey> {
    match name {
        ClassName::Atom(i) => Ok(PropertyKey::String(frame.code.atoms[i as usize])),
        ClassName::Reg(r) => key_of(agent, frame.regs[r as usize]),
        ClassName::Empty => Ok(PropertyKey::String(agent.heap.names.empty)),
    }
}

/// Adds a field definition to a class constructor.
pub fn add_field(agent: &mut Agent, class: Object, field: ClassFieldDefinition) {
    if let ObjectKind::BytecodeFunction(data) = &mut agent.heap.object_mut(class).kind {
        data.fields.push(field);
    }
}

/// The ClassFieldName of a key value.
pub(crate) fn field_name(agent: &mut Agent, key: Value, private: bool) -> JsResult<ClassFieldName> {
    if private {
        if let Value::Symbol(s) = key {
            return Ok(ClassFieldName::Private(s));
        }
    }
    Ok(ClassFieldName::Key(key_of(agent, key)?))
}

fn field_key_value(agent: &mut Agent, name: ClassFieldName) -> Value {
    match name {
        ClassFieldName::Key(k) => agent.heap.key_to_value(k),
        ClassFieldName::Private(s) => Value::Symbol(s),
    }
}

/// DefineField(receiver, fieldRecord).
pub fn define_field(
    agent: &mut Agent,
    receiver: Object,
    field: ClassFieldDefinition,
) -> JsResult<()> {
    let value = match field.initializer {
        Some(init) => {
            let key = field_key_value(agent, field.name);
            ops::call(agent, Value::Object(init), Value::Object(receiver), &[key])?
        }
        None => Value::Undefined,
    };
    match field.name {
        ClassFieldName::Private(s) => ops::private_field_add(agent, receiver, s, value),
        ClassFieldName::Key(k) => {
            ops::create_data_property_or_throw(agent, receiver, k, value)?;
            Ok(())
        }
    }
}

/// InitializeInstanceElements(O, constructor).
pub fn initialize_instance_elements(
    agent: &mut Agent,
    o: Object,
    constructor: Object,
) -> JsResult<()> {
    let (methods, fields) = match &agent.heap.object(constructor).kind {
        ObjectKind::BytecodeFunction(data) => (data.private_methods.clone(), data.fields.clone()),
        _ => return Ok(()),
    };
    for m in methods {
        ops::private_method_or_accessor_add(agent, o, m)?;
    }
    for f in fields {
        define_field(agent, o, f)?;
    }
    Ok(())
}

/// Sets up the frame of `[[Call]]` (`new_target` `None`) or
/// `[[Construct]]` of a bytecode function: PrepareForOrdinaryCall and
/// OrdinaryCallBindThis, plus the promise capability of async functions.
pub fn prepare_call(
    agent: &mut Agent,
    f: Object,
    this: Value,
    args: Vec<Value>,
    new_target: Option<Object>,
) -> JsResult<Frame> {
    let Some(code) = function_code(agent, f) else {
        return agent.type_error("not a function");
    };
    let (env, realm, this_mode, is_class_ctor, script_or_module) = match &agent.heap.object(f).kind
    {
        ObjectKind::BytecodeFunction(d) => (
            d.env,
            d.realm,
            d.this_mode,
            d.is_class_constructor,
            d.script_or_module,
        ),
        _ => return agent.type_error("not a function"),
    };
    let env = env.expect("bytecode function without environment");
    if new_target.is_none() && is_class_ctor {
        agent.push_context(ExecutionContext {
            realm,
            function: Some(f),
            script_or_module,
        });
        let e = agent.type_error("class constructors cannot be invoked without 'new'");
        agent.pop_context();
        return e;
    }
    let this_value = match new_target {
        Some(nt) => {
            if code.flags.is_derived {
                None
            } else {
                let o = ops::ordinary_create_from_constructor(
                    agent,
                    nt,
                    Intrinsic::ObjectPrototype,
                    ObjectKind::Ordinary,
                )?;
                Some(Value::Object(o))
            }
        }
        None => match this_mode {
            ThisMode::Lexical => None,
            ThisMode::Strict => Some(this),
            ThisMode::Global => {
                if this.is_nullish() {
                    let data = agent.heap.realm(realm);
                    data.global_this.or(data.global_object).map(Value::Object)
                } else {
                    agent.push_context(ExecutionContext {
                        realm,
                        function: Some(f),
                        script_or_module,
                    });
                    let o = ops::to_object(agent, this);
                    agent.pop_context();
                    Some(Value::Object(o?))
                }
            }
        },
    };
    let mut frame = Frame::new(
        code.clone(),
        env,
        realm,
        if new_target.is_some() {
            FrameKind::Construct
        } else {
            FrameKind::Call
        },
    );
    frame.function = Some(f);
    frame.this = this_value;
    frame.new_target = Value::from_object(new_target);
    frame.args = args;
    frame.script_or_module = script_or_module;
    if code.flags.is_async && !code.flags.is_generator {
        frame.async_cap = Some(promise::new_intrinsic_capability(agent, realm));
    }
    Ok(frame)
}

/// CreateUnmappedArgumentsObject / CreateMappedArgumentsObject.
pub fn create_arguments_object(agent: &mut Agent, frame: &Frame, mapped: bool) -> Object {
    let proto = agent.intrinsic(Intrinsic::ObjectPrototype);
    let kind = if mapped {
        let n = frame.args.len().min(frame.code.arg_map.len());
        let map = frame.code.arg_map[..n].to_vec();
        ObjectKind::Arguments(Box::new(ArgumentsData {
            env: frame.env,
            map,
        }))
    } else {
        ObjectKind::UnmappedArguments
    };
    let o = agent.heap.create_object(Some(proto), kind);
    for (i, v) in frame.args.iter().enumerate() {
        agent.heap.set_own(
            o,
            PropertyKey::Index(i as u32),
            Attributes::ALL,
            Slot::Data(*v),
        );
    }
    let length = PropertyKey::String(agent.heap.names.length);
    agent.heap.set_own(
        o,
        length,
        Attributes::data(true, false, true),
        Slot::Data(Value::Number(frame.args.len() as f64)),
    );
    let values = agent.intrinsic(Intrinsic::ArrayPrototypeValues);
    let iterator = PropertyKey::Symbol(agent.symbols.iterator);
    agent.heap.set_own(
        o,
        iterator,
        Attributes::data(true, false, true),
        Slot::Data(Value::Object(values)),
    );
    let callee = PropertyKey::String(agent.heap.names.callee);
    if mapped {
        agent.heap.set_own(
            o,
            callee,
            Attributes::data(true, false, true),
            Slot::Data(Value::from_object(frame.function)),
        );
    } else {
        let thrower = Value::Object(agent.intrinsic(Intrinsic::ThrowTypeError));
        agent.heap.set_own(
            o,
            callee,
            Attributes::accessor(false, false),
            Slot::Accessor {
                get: thrower,
                set: thrower,
            },
        );
    }
    o
}
