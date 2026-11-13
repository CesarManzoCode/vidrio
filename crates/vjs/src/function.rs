//! Function objects: bytecode functions (executed by the interpreter
//! through [`crate::agent::Evaluator`]), native functions with traced state,
//! and bound function exotic objects (ECMA-262 §10.2–§10.4.1).

use std::any::Any;
use std::rc::Rc;

use crate::agent::{Agent, ExecutionContext};
use crate::completion::JsResult;
use crate::gc::{Env, Object, Realm, Symbol, Trace, Tracer};
use crate::internal;
use crate::object::{Attributes, ObjectKind, PrivateElement, PropertyDescriptor, Slot};
use crate::realm::Intrinsic;
use crate::string::utf16;
use crate::value::{PropertyKey, Value};

/// Arguments of a native function call. `new_target` is `Some` when the
/// function is called through `[[Construct]]`.
pub struct NativeCall<'a> {
    pub callee: Object,
    pub this: Value,
    pub args: &'a [Value],
    pub new_target: Option<Object>,
}

impl NativeCall<'_> {
    /// Argument `i`, or `undefined` if absent.
    pub fn arg(&self, i: usize) -> Value {
        self.args.get(i).copied().unwrap_or(Value::Undefined)
    }
}

/// Behaviour of a native function. For `[[Construct]]` the result must be
/// an object.
pub type NativeFn = fn(&mut Agent, NativeCall<'_>) -> JsResult<Value>;

/// A built-in function object (ECMA-262 §10.3).
pub struct NativeFunctionData {
    pub behavior: NativeFn,
    pub realm: Realm,
    pub constructor: bool,
    /// Traced state (the internal slots of the specification's abstract
    /// closures, for example `[[AlreadyResolved]]` holders).
    pub slots: Vec<Value>,
    /// Untraced Rust state; it must not hold heap handles.
    pub host_data: Option<Rc<dyn Any>>,
    pub initial_name: Option<Value>,
}

impl Trace for NativeFunctionData {
    fn trace(&self, tracer: &mut Tracer) {
        tracer.realm(self.realm);
        for v in &self.slots {
            tracer.value(*v);
        }
        if let Some(n) = self.initial_name {
            tracer.value(n);
        }
    }
}

/// Compiled code of a function; defined by the bytecode compiler.
pub trait CodeBlock: Trace + Any {
    fn as_any(&self) -> &dyn Any;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThisMode {
    Lexical,
    Strict,
    Global,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConstructorKind {
    Base,
    Derived,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ClassFieldName {
    Key(PropertyKey),
    Private(Symbol),
}

/// ClassFieldDefinition Record; `initializer` is a function or `None`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClassFieldDefinition {
    pub name: ClassFieldName,
    pub initializer: Option<Object>,
}

impl Trace for ClassFieldDefinition {
    fn trace(&self, tracer: &mut Tracer) {
        match self.name {
            ClassFieldName::Key(k) => tracer.key(k),
            ClassFieldName::Private(s) => {
                tracer.symbol(s);
            }
        }
        if let Some(i) = self.initializer {
            tracer.object(i);
        }
    }
}

/// ECMAScript function object slots (ECMA-262 §10.2, Table 30).
pub struct BytecodeFunctionData {
    pub code: Rc<dyn CodeBlock>,
    pub env: Option<Env>,
    pub private_env: Option<Env>,
    pub realm: Realm,
    /// The Script or Module record, owned by the host or loader.
    pub script_or_module: Option<Object>,
    pub home_object: Option<Object>,
    pub this_mode: ThisMode,
    pub strict: bool,
    pub constructor_kind: ConstructorKind,
    pub is_constructor: bool,
    pub is_class_constructor: bool,
    pub fields: Vec<ClassFieldDefinition>,
    pub private_methods: Vec<PrivateElement>,
}

impl Trace for BytecodeFunctionData {
    fn trace(&self, tracer: &mut Tracer) {
        self.code.trace(tracer);
        for e in [self.env, self.private_env].into_iter().flatten() {
            tracer.env(e);
        }
        tracer.realm(self.realm);
        for o in [self.script_or_module, self.home_object]
            .into_iter()
            .flatten()
        {
            tracer.object(o);
        }
        self.fields.trace(tracer);
        for m in &self.private_methods {
            m.trace(tracer);
        }
    }
}

/// Bound function exotic object slots.
pub struct BoundFunctionData {
    pub target: Object,
    pub this: Value,
    pub args: Vec<Value>,
    pub constructor: bool,
}

impl Trace for BoundFunctionData {
    fn trace(&self, tracer: &mut Tracer) {
        tracer.object(self.target);
        tracer.value(self.this);
        self.args.trace(tracer);
    }
}

/// CreateBuiltinFunction(behaviour, length, name, …): a native function of
/// `realm` (the current realm if `None`) with prototype `proto`
/// (`%Function.prototype%` of the realm if `None`).
pub fn create_builtin_function(
    agent: &mut Agent,
    behavior: NativeFn,
    length: u32,
    name: PropertyKey,
    realm: Option<Realm>,
    proto: Option<Object>,
    prefix: Option<&str>,
) -> Object {
    let realm = realm.unwrap_or_else(|| agent.current_realm());
    let proto = proto.unwrap_or_else(|| {
        agent
            .heap
            .realm(realm)
            .intrinsics
            .get(Intrinsic::FunctionPrototype)
    });
    let f = agent.heap.create_object(
        Some(proto),
        ObjectKind::NativeFunction(Box::new(NativeFunctionData {
            behavior,
            realm,
            constructor: false,
            slots: Vec::new(),
            host_data: None,
            initial_name: None,
        })),
    );
    set_function_length(agent, f, f64::from(length));
    set_function_name(agent, f, name, prefix);
    f
}

/// A native function with state: `slots` are traced, `host_data` is not.
pub fn create_native_function(
    agent: &mut Agent,
    behavior: NativeFn,
    length: u32,
    name: &str,
    slots: Vec<Value>,
    host_data: Option<Rc<dyn Any>>,
) -> Object {
    let key = agent.heap.key_from_str(name);
    let f = create_builtin_function(agent, behavior, length, key, None, None, None);
    if let ObjectKind::NativeFunction(data) = &mut agent.heap.object_mut(f).kind {
        data.slots = slots;
        data.host_data = host_data;
    }
    f
}

/// The traced state of a native function.
pub fn native_slots(agent: &Agent, f: Object) -> &[Value] {
    match &agent.heap.object(f).kind {
        ObjectKind::NativeFunction(data) => &data.slots,
        _ => &[],
    }
}

pub fn native_slots_mut(agent: &mut Agent, f: Object) -> Option<&mut Vec<Value>> {
    match &mut agent.heap.object_mut(f).kind {
        ObjectKind::NativeFunction(data) => Some(&mut data.slots),
        _ => None,
    }
}

/// The untraced Rust state of a native function.
pub fn native_host_data(agent: &Agent, f: Object) -> Option<Rc<dyn Any>> {
    match &agent.heap.object(f).kind {
        ObjectKind::NativeFunction(data) => data.host_data.clone(),
        _ => None,
    }
}

/// Marks a native function as a constructor (its behaviour then also runs
/// for `[[Construct]]`).
pub fn set_native_constructor(agent: &mut Agent, f: Object) {
    if let ObjectKind::NativeFunction(data) = &mut agent.heap.object_mut(f).kind {
        data.constructor = true;
    }
}

/// SetFunctionName(F, name, prefix).
pub fn set_function_name(agent: &mut Agent, f: Object, name: PropertyKey, prefix: Option<&str>) {
    let mut units: Vec<u16> = match name {
        PropertyKey::Symbol(s) => {
            let data = agent.heap.symbol(s);
            let (description, private) = (data.description, data.private);
            match description {
                Some(d) if private => agent.heap.string_units(d).to_vec(),
                Some(d) => {
                    let mut u = utf16("[");
                    u.extend_from_slice(agent.heap.string_units(d));
                    u.push(u16::from(b']'));
                    u
                }
                None => Vec::new(),
            }
        }
        key => agent.heap.key_units(key).unwrap_or_default(),
    };
    if let Some(prefix) = prefix {
        let mut p = utf16(prefix);
        p.push(u16::from(b' '));
        p.extend_from_slice(&units);
        units = p;
    }
    let value = Value::String(agent.heap.alloc_string_owned(units));
    if let ObjectKind::NativeFunction(data) = &mut agent.heap.object_mut(f).kind {
        data.initial_name = Some(value);
    }
    let key = PropertyKey::String(agent.heap.names.name);
    agent.heap.set_own(
        f,
        key,
        Attributes::data(false, false, true),
        Slot::Data(value),
    );
}

/// SetFunctionLength(F, length).
pub fn set_function_length(agent: &mut Agent, f: Object, length: f64) {
    let key = PropertyKey::String(agent.heap.names.length);
    agent.heap.set_own(
        f,
        key,
        Attributes::data(false, false, true),
        Slot::Data(Value::Number(length)),
    );
}

/// MakeConstructor(F, writablePrototype, prototype): `prototype` `None`
/// creates a fresh object whose `constructor` is `f`.
pub fn make_constructor(
    agent: &mut Agent,
    f: Object,
    writable_prototype: bool,
    prototype: Option<Object>,
) {
    match &mut agent.heap.object_mut(f).kind {
        ObjectKind::BytecodeFunction(data) => {
            data.is_constructor = true;
            data.constructor_kind = ConstructorKind::Base;
        }
        ObjectKind::NativeFunction(data) => data.constructor = true,
        _ => {}
    }
    let prototype = match prototype {
        Some(p) => p,
        None => {
            let object_proto = agent.intrinsic(Intrinsic::ObjectPrototype);
            let p = agent
                .heap
                .create_object(Some(object_proto), ObjectKind::Ordinary);
            let key = PropertyKey::String(agent.heap.names.constructor);
            agent.heap.set_own(
                p,
                key,
                Attributes::data(writable_prototype, false, true),
                Slot::Data(Value::Object(f)),
            );
            p
        }
    };
    let key = PropertyKey::String(agent.heap.names.prototype);
    agent.heap.set_own(
        f,
        key,
        Attributes::data(writable_prototype, false, false),
        Slot::Data(Value::Object(prototype)),
    );
}

/// MakeMethod(F, homeObject).
pub fn make_method(agent: &mut Agent, f: Object, home: Object) {
    if let ObjectKind::BytecodeFunction(data) = &mut agent.heap.object_mut(f).kind {
        data.home_object = Some(home);
    }
}

/// BoundFunctionCreate(targetFunction, boundThis, boundArgs).
pub fn bound_function_create(
    agent: &mut Agent,
    target: Object,
    this: Value,
    args: Vec<Value>,
) -> JsResult<Object> {
    let proto = internal::get_prototype_of(agent, target)?;
    let constructor = agent.heap.object(target).is_constructor();
    Ok(agent.heap.create_object(
        proto,
        ObjectKind::BoundFunction(Box::new(BoundFunctionData {
            target,
            this,
            args,
            constructor,
        })),
    ))
}

pub fn is_callable(agent: &Agent, v: Value) -> bool {
    match v {
        Value::Object(o) => agent.heap.object(o).is_callable(),
        _ => false,
    }
}

pub fn is_constructor(agent: &Agent, v: Value) -> bool {
    match v {
        Value::Object(o) => agent.heap.object(o).is_constructor(),
        _ => false,
    }
}

/// `F.[[Call]](thisArgument, argumentsList)`; `f` must be callable.
pub fn call_object(agent: &mut Agent, f: Object, this: Value, args: &[Value]) -> JsResult<Value> {
    agent.enter_native_call()?;
    let result = call_inner(agent, f, this, args);
    agent.exit_native_call();
    result
}

fn call_inner(agent: &mut Agent, f: Object, this: Value, args: &[Value]) -> JsResult<Value> {
    enum Target {
        Bytecode,
        Native(NativeFn, Realm),
        Bound(Object, Value, Vec<Value>),
        Proxy,
    }
    let target = match &agent.heap.object(f).kind {
        ObjectKind::BytecodeFunction(_) => Target::Bytecode,
        ObjectKind::NativeFunction(data) => Target::Native(data.behavior, data.realm),
        ObjectKind::BoundFunction(data) => Target::Bound(data.target, data.this, data.args.clone()),
        ObjectKind::Proxy(_) => Target::Proxy,
        _ => return agent.type_error("not a function"),
    };
    match target {
        Target::Bytecode => {
            let Some(evaluator) = agent.evaluator else {
                return agent.type_error("bytecode execution is not available");
            };
            (evaluator.call)(agent, f, this, args)
        }
        Target::Native(behavior, realm) => {
            agent.push_context(ExecutionContext {
                realm,
                function: Some(f),
                script_or_module: None,
            });
            let result = behavior(
                agent,
                NativeCall {
                    callee: f,
                    this,
                    args,
                    new_target: None,
                },
            );
            agent.pop_context();
            result
        }
        Target::Bound(target, bound_this, mut bound_args) => {
            bound_args.extend_from_slice(args);
            call_object(agent, target, bound_this, &bound_args)
        }
        Target::Proxy => crate::proxy::proxy_call(agent, f, this, args),
    }
}

/// `F.[[Construct]](argumentsList, newTarget)`; `f` must be a constructor.
pub fn construct_object(
    agent: &mut Agent,
    f: Object,
    args: &[Value],
    new_target: Object,
) -> JsResult<Object> {
    agent.enter_native_call()?;
    let result = construct_inner(agent, f, args, new_target);
    agent.exit_native_call();
    result
}

fn construct_inner(
    agent: &mut Agent,
    f: Object,
    args: &[Value],
    new_target: Object,
) -> JsResult<Object> {
    enum Target {
        Bytecode,
        Native(NativeFn, Realm),
        Bound(Object, Vec<Value>),
        Proxy,
    }
    let target = match &agent.heap.object(f).kind {
        ObjectKind::BytecodeFunction(_) => Target::Bytecode,
        ObjectKind::NativeFunction(data) => Target::Native(data.behavior, data.realm),
        ObjectKind::BoundFunction(data) => Target::Bound(data.target, data.args.clone()),
        ObjectKind::Proxy(_) => Target::Proxy,
        _ => return agent.type_error("not a constructor"),
    };
    match target {
        Target::Bytecode => {
            let Some(evaluator) = agent.evaluator else {
                return agent.type_error("bytecode execution is not available");
            };
            (evaluator.construct)(agent, f, args, new_target)
        }
        Target::Native(behavior, realm) => {
            agent.push_context(ExecutionContext {
                realm,
                function: Some(f),
                script_or_module: None,
            });
            let result = behavior(
                agent,
                NativeCall {
                    callee: f,
                    this: Value::Undefined,
                    args,
                    new_target: Some(new_target),
                },
            );
            agent.pop_context();
            match result? {
                Value::Object(o) => Ok(o),
                _ => agent.type_error("constructor did not return an object"),
            }
        }
        Target::Bound(target, mut bound_args) => {
            bound_args.extend_from_slice(args);
            let new_target = if new_target == f { target } else { new_target };
            construct_object(agent, target, &bound_args, new_target)
        }
        Target::Proxy => crate::proxy::proxy_construct(agent, f, args, new_target),
    }
}

/// The descriptor `{ [[Value]]: f, writable, enumerable: false,
/// configurable }` used for methods of built-in objects.
pub fn method_descriptor(f: Object) -> PropertyDescriptor {
    PropertyDescriptor::data(Value::Object(f), true, false, true)
}
