//! Realms (ECMA-262 §9.3): the table of intrinsics, the global object and
//! the staged initialization.
//!
//! Stages: [`Agent::create_realm`] allocates the realm and creates the
//! intrinsics (fundamental objects first, then the error constructors,
//! then every installer of [`INTRINSIC_INSTALLERS`]);
//! [`Agent::set_realm_global_object`] sets the global object and `this`;
//! [`Agent::set_default_global_bindings`] defines the global properties.
//! [`Agent::initialize_host_defined_realm`] runs the three in order.

use std::rc::Rc;

use crate::agent::{Agent, ErrorKind, ExecutionContext};
use crate::completion::JsResult;
use crate::function::{NativeCall, create_builtin_function};
use crate::gc::{Env, Object, Realm, Trace, Tracer};
use crate::object::{Attributes, ObjectKind, PropertyDescriptor, Slot};
use crate::ops;
use crate::value::{PropertyKey, Value};

macro_rules! intrinsics {
    ($($name:ident $(= $global:literal)?),* $(,)?) => {
        /// The well-known intrinsic objects (ECMA-262 §6.1.7.4) and the
        /// prototypes the engine needs by name.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum Intrinsic {
            $($name,)*
        }

        impl Intrinsic {
            pub const ALL: &'static [Intrinsic] = &[$(Intrinsic::$name,)*];

            /// The global property that holds it, if any.
            pub fn global_name(self) -> Option<&'static str> {
                match self {
                    $(Intrinsic::$name => intrinsics!(@global $($global)?),)*
                }
            }
        }
    };
    (@global) => { None };
    (@global $global:literal) => { Some($global) };
}

intrinsics! {
    AggregateError = "AggregateError",
    AggregateErrorPrototype,
    Array = "Array",
    ArrayPrototype,
    ArrayPrototypeValues,
    ArrayBuffer = "ArrayBuffer",
    ArrayBufferPrototype,
    ArrayIteratorPrototype,
    AsyncDisposableStack = "AsyncDisposableStack",
    AsyncDisposableStackPrototype,
    AsyncFromSyncIteratorPrototype,
    AsyncFunction,
    AsyncFunctionPrototype,
    AsyncGeneratorFunction,
    AsyncGeneratorFunctionPrototype,
    AsyncGeneratorPrototype,
    AsyncIteratorPrototype,
    Atomics = "Atomics",
    BigInt = "BigInt",
    BigIntPrototype,
    BigInt64Array = "BigInt64Array",
    BigInt64ArrayPrototype,
    BigUint64Array = "BigUint64Array",
    BigUint64ArrayPrototype,
    Boolean = "Boolean",
    BooleanPrototype,
    DataView = "DataView",
    DataViewPrototype,
    Date = "Date",
    DatePrototype,
    DecodeUri = "decodeURI",
    DecodeUriComponent = "decodeURIComponent",
    DisposableStack = "DisposableStack",
    DisposableStackPrototype,
    EncodeUri = "encodeURI",
    EncodeUriComponent = "encodeURIComponent",
    Error = "Error",
    ErrorPrototype,
    Escape = "escape",
    Eval = "eval",
    EvalError = "EvalError",
    EvalErrorPrototype,
    FinalizationRegistry = "FinalizationRegistry",
    FinalizationRegistryPrototype,
    Float16Array = "Float16Array",
    Float16ArrayPrototype,
    Float32Array = "Float32Array",
    Float32ArrayPrototype,
    Float64Array = "Float64Array",
    Float64ArrayPrototype,
    ForInIteratorPrototype,
    Function = "Function",
    FunctionPrototype,
    GeneratorFunction,
    GeneratorFunctionPrototype,
    GeneratorPrototype,
    Int8Array = "Int8Array",
    Int8ArrayPrototype,
    Int16Array = "Int16Array",
    Int16ArrayPrototype,
    Int32Array = "Int32Array",
    Int32ArrayPrototype,
    IsFinite = "isFinite",
    IsNaN = "isNaN",
    Iterator = "Iterator",
    IteratorPrototype,
    IteratorHelperPrototype,
    Json = "JSON",
    Map = "Map",
    MapPrototype,
    MapIteratorPrototype,
    Math = "Math",
    Number = "Number",
    NumberPrototype,
    Object = "Object",
    ObjectPrototype,
    ParseFloat = "parseFloat",
    ParseInt = "parseInt",
    Promise = "Promise",
    PromisePrototype,
    Proxy = "Proxy",
    RangeError = "RangeError",
    RangeErrorPrototype,
    ReferenceError = "ReferenceError",
    ReferenceErrorPrototype,
    Reflect = "Reflect",
    RegExp = "RegExp",
    RegExpPrototype,
    RegExpStringIteratorPrototype,
    Set = "Set",
    SetPrototype,
    SetIteratorPrototype,
    SharedArrayBuffer = "SharedArrayBuffer",
    SharedArrayBufferPrototype,
    String = "String",
    StringPrototype,
    StringIteratorPrototype,
    SuppressedError = "SuppressedError",
    SuppressedErrorPrototype,
    Symbol = "Symbol",
    SymbolPrototype,
    SyntaxError = "SyntaxError",
    SyntaxErrorPrototype,
    Temporal = "Temporal",
    ThrowTypeError,
    TypedArray,
    TypedArrayPrototype,
    TypeError = "TypeError",
    TypeErrorPrototype,
    Uint8Array = "Uint8Array",
    Uint8ArrayPrototype,
    Uint8ClampedArray = "Uint8ClampedArray",
    Uint8ClampedArrayPrototype,
    Uint16Array = "Uint16Array",
    Uint16ArrayPrototype,
    Uint32Array = "Uint32Array",
    Uint32ArrayPrototype,
    Unescape = "unescape",
    UriError = "URIError",
    UriErrorPrototype,
    WeakMap = "WeakMap",
    WeakMapPrototype,
    WeakRef = "WeakRef",
    WeakRefPrototype,
    WeakSet = "WeakSet",
    WeakSetPrototype,
    WrapForValidIteratorPrototype,
}

/// `[[Intrinsics]]` of a realm.
pub struct Intrinsics {
    table: Vec<Option<Object>>,
}

impl Intrinsics {
    fn new() -> Intrinsics {
        Intrinsics {
            table: vec![None; Intrinsic::ALL.len()],
        }
    }

    /// The intrinsic; panics if the realm has not created it.
    pub fn get(&self, which: Intrinsic) -> Object {
        self.table[which as usize].unwrap_or_else(|| panic!("intrinsic {which:?} not created"))
    }

    pub fn try_get(&self, which: Intrinsic) -> Option<Object> {
        self.table[which as usize]
    }

    pub fn set(&mut self, which: Intrinsic, o: Object) {
        self.table[which as usize] = Some(o);
    }
}

/// How far the initialization of a realm got.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum RealmStage {
    Allocated,
    IntrinsicsCreated,
    GlobalObjectSet,
    Ready,
}

/// Realm Record.
pub struct RealmData {
    pub intrinsics: Intrinsics,
    pub global_object: Option<Object>,
    pub global_this: Option<Object>,
    /// The global Environment Record, created by the interpreter.
    pub global_env: Option<Env>,
    /// `[[TemplateMap]]`: template site id → template object.
    pub template_map: Vec<(usize, Object)>,
    /// `[[HostDefined]]`.
    pub host_defined: Option<Rc<dyn Trace>>,
    pub stage: RealmStage,
}

impl Trace for RealmData {
    fn trace(&self, tracer: &mut Tracer) {
        for o in self.intrinsics.table.iter().flatten() {
            tracer.object(*o);
        }
        for o in [self.global_object, self.global_this].into_iter().flatten() {
            tracer.object(o);
        }
        if let Some(e) = self.global_env {
            tracer.env(e);
        }
        for (_, o) in &self.template_map {
            tracer.object(*o);
        }
        if let Some(h) = &self.host_defined {
            h.trace(tracer);
        }
    }
}

/// Creates the intrinsics of one area of the built-ins in a realm whose
/// fundamental objects already exist. Runs with the realm as the current
/// realm.
pub type IntrinsicInstaller = fn(&mut Agent, Realm);

/// Installers run, in order, by [`Agent::create_realm`] after the
/// fundamental objects and the error constructors. Each built-in area adds
/// its installer here.
pub const INTRINSIC_INSTALLERS: &[IntrinsicInstaller] = &[crate::interp::intrinsics::install];

fn function_prototype_behavior(_agent: &mut Agent, _call: NativeCall<'_>) -> JsResult<Value> {
    Ok(Value::Undefined)
}

fn throw_type_error_behavior(agent: &mut Agent, _call: NativeCall<'_>) -> JsResult<Value> {
    agent.type_error("'caller', 'callee' and 'arguments' may not be accessed here")
}

const ERROR_KINDS: [(ErrorKind, Intrinsic, Intrinsic, &str); 7] = [
    (
        ErrorKind::Error,
        Intrinsic::Error,
        Intrinsic::ErrorPrototype,
        "Error",
    ),
    (
        ErrorKind::Eval,
        Intrinsic::EvalError,
        Intrinsic::EvalErrorPrototype,
        "EvalError",
    ),
    (
        ErrorKind::Range,
        Intrinsic::RangeError,
        Intrinsic::RangeErrorPrototype,
        "RangeError",
    ),
    (
        ErrorKind::Reference,
        Intrinsic::ReferenceError,
        Intrinsic::ReferenceErrorPrototype,
        "ReferenceError",
    ),
    (
        ErrorKind::Syntax,
        Intrinsic::SyntaxError,
        Intrinsic::SyntaxErrorPrototype,
        "SyntaxError",
    ),
    (
        ErrorKind::Type,
        Intrinsic::TypeError,
        Intrinsic::TypeErrorPrototype,
        "TypeError",
    ),
    (
        ErrorKind::Uri,
        Intrinsic::UriError,
        Intrinsic::UriErrorPrototype,
        "URIError",
    ),
];

/// The prototype intrinsic of an error kind.
pub(crate) fn error_prototype(kind: ErrorKind) -> Intrinsic {
    ERROR_KINDS
        .iter()
        .find(|(k, ..)| *k == kind)
        .map_or(Intrinsic::ErrorPrototype, |e| e.2)
}

/// The `Error` and NativeError constructors (ECMA-262 §20.5.1.1,
/// §20.5.6.1.1). Slot 0 holds the index of the kind in `ERROR_KINDS`.
fn error_constructor_behavior(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let index = match crate::function::native_slots(agent, call.callee).first() {
        Some(Value::Number(n)) => *n as usize,
        _ => 0,
    };
    let proto = ERROR_KINDS[index].2;
    let new_target = call.new_target.unwrap_or(call.callee);
    let o = ops::ordinary_create_from_constructor(agent, new_target, proto, ObjectKind::Error)?;
    let message = call.arg(0);
    if !message.is_undefined() {
        let msg = ops::to_string(agent, message)?;
        let key = PropertyKey::String(agent.heap.names.message);
        ops::create_non_enumerable_data_property_or_throw(agent, o, key, Value::String(msg))?;
    }
    ops::install_error_cause(agent, o, call.arg(1))?;
    Ok(Value::Object(o))
}

fn define_hidden(agent: &mut Agent, o: Object, name: &str, value: Value) {
    let key = agent.heap.key_from_str(name);
    agent.heap.set_own(
        o,
        key,
        Attributes::data(true, false, true),
        Slot::Data(value),
    );
}

impl Agent {
    /// CreateRealm plus CreateIntrinsics: a realm whose intrinsics exist
    /// but has no global object yet.
    pub fn create_realm(&mut self) -> Realm {
        let realm = self.heap.alloc_realm(RealmData {
            intrinsics: Intrinsics::new(),
            global_object: None,
            global_this: None,
            global_env: None,
            template_map: Vec::new(),
            host_defined: None,
            stage: RealmStage::Allocated,
        });
        self.realms.push(realm);
        self.push_context(ExecutionContext {
            realm,
            function: None,
            script_or_module: None,
        });
        self.create_fundamental_objects(realm);
        self.create_error_constructors(realm);
        for install in INTRINSIC_INSTALLERS {
            install(self, realm);
        }
        self.pop_context();
        self.heap.realm_mut(realm).stage = RealmStage::IntrinsicsCreated;
        realm
    }

    fn set_intrinsic(&mut self, realm: Realm, which: Intrinsic, o: Object) {
        self.heap.realm_mut(realm).intrinsics.set(which, o);
    }

    fn create_fundamental_objects(&mut self, realm: Realm) {
        let object_proto = self
            .heap
            .create_object(None, ObjectKind::ImmutablePrototype);
        self.set_intrinsic(realm, Intrinsic::ObjectPrototype, object_proto);
        let empty = self.heap.names.empty;
        let function_proto = create_builtin_function(
            self,
            function_prototype_behavior,
            0,
            PropertyKey::String(empty),
            Some(realm),
            Some(object_proto),
            None,
        );
        self.set_intrinsic(realm, Intrinsic::FunctionPrototype, function_proto);

        let thrower = create_builtin_function(
            self,
            throw_type_error_behavior,
            0,
            PropertyKey::String(empty),
            Some(realm),
            Some(function_proto),
            None,
        );
        for key in [self.heap.names.length, self.heap.names.name] {
            let key = PropertyKey::String(key);
            if let Some((_, slot)) = self.heap.get_own(thrower, key) {
                self.heap
                    .set_own(thrower, key, Attributes::data(false, false, false), slot);
            }
        }
        self.heap.set_extensible(thrower, false);
        self.set_intrinsic(realm, Intrinsic::ThrowTypeError, thrower);

        let array_proto = self
            .heap
            .create_object(Some(object_proto), ObjectKind::Array);
        let length = PropertyKey::String(self.heap.names.length);
        self.heap.set_own(
            array_proto,
            length,
            Attributes::data(true, false, false),
            Slot::Data(Value::Number(0.0)),
        );
        self.set_intrinsic(realm, Intrinsic::ArrayPrototype, array_proto);

        let empty_string = self.heap.empty_string();
        let string_proto = crate::internal::string_create(self, empty_string, Some(object_proto));
        self.set_intrinsic(realm, Intrinsic::StringPrototype, string_proto);

        let simple: [(Intrinsic, ObjectKind); 5] = [
            (Intrinsic::BooleanPrototype, ObjectKind::Boolean(false)),
            (Intrinsic::NumberPrototype, ObjectKind::Number(0.0)),
            (Intrinsic::SymbolPrototype, ObjectKind::Ordinary),
            (Intrinsic::BigIntPrototype, ObjectKind::Ordinary),
            (Intrinsic::IteratorPrototype, ObjectKind::Ordinary),
        ];
        for (which, kind) in simple {
            let o = self.heap.create_object(Some(object_proto), kind);
            self.set_intrinsic(realm, which, o);
        }
    }

    fn create_error_constructors(&mut self, realm: Realm) {
        let object_proto = self
            .heap
            .realm(realm)
            .intrinsics
            .get(Intrinsic::ObjectPrototype);
        let mut error_ctor = None;
        let mut error_proto = None;
        for (index, (_, ctor_id, proto_id, name)) in ERROR_KINDS.iter().enumerate() {
            let proto = self.heap.create_object(
                Some(error_proto.unwrap_or(object_proto)),
                ObjectKind::Ordinary,
            );
            let key = self.heap.key_from_str(name);
            let ctor = create_builtin_function(
                self,
                error_constructor_behavior,
                1,
                key,
                Some(realm),
                error_ctor,
                None,
            );
            if let Some(slots) = crate::function::native_slots_mut(self, ctor) {
                slots.push(Value::Number(index as f64));
            }
            crate::function::make_constructor(self, ctor, false, Some(proto));
            define_hidden(self, proto, "constructor", Value::Object(ctor));
            let name_value = Value::String(self.heap.alloc_str(name));
            define_hidden(self, proto, "name", name_value);
            let empty = Value::String(self.heap.empty_string());
            define_hidden(self, proto, "message", empty);
            self.set_intrinsic(realm, *ctor_id, ctor);
            self.set_intrinsic(realm, *proto_id, proto);
            if index == 0 {
                error_ctor = Some(ctor);
                error_proto = Some(proto);
            }
        }
    }

    /// SetRealmGlobalObject(realm, globalObj, thisValue).
    pub fn set_realm_global_object(
        &mut self,
        realm: Realm,
        global: Option<Object>,
        this_value: Option<Object>,
    ) {
        let global = match global {
            Some(g) => g,
            None => {
                let proto = self
                    .heap
                    .realm(realm)
                    .intrinsics
                    .get(Intrinsic::ObjectPrototype);
                self.heap.create_object(Some(proto), ObjectKind::Ordinary)
            }
        };
        let this_value = this_value.unwrap_or(global);
        let data = self.heap.realm_mut(realm);
        data.global_object = Some(global);
        data.global_this = Some(this_value);
        data.stage = RealmStage::GlobalObjectSet;
    }

    /// SetDefaultGlobalBindings(realm): the value properties of the global
    /// object and every intrinsic with a global name.
    pub fn set_default_global_bindings(&mut self, realm: Realm) -> JsResult<Object> {
        let global = self
            .heap
            .realm(realm)
            .global_object
            .expect("set the global object first");
        let this_value = self.heap.realm(realm).global_this.unwrap_or(global);
        self.push_context(ExecutionContext {
            realm,
            function: None,
            script_or_module: None,
        });
        let result = (|| -> JsResult<()> {
            let global_this = PropertyKey::String(self.heap.names.global_this);
            ops::define_property_or_throw(
                self,
                global,
                global_this,
                PropertyDescriptor::data(Value::Object(this_value), true, false, true),
            )?;
            let values = [
                (self.heap.names.infinity, Value::Number(f64::INFINITY)),
                (self.heap.names.nan, Value::Number(f64::NAN)),
                (self.heap.names.undefined, Value::Undefined),
            ];
            for (name, value) in values {
                ops::define_property_or_throw(
                    self,
                    global,
                    PropertyKey::String(name),
                    PropertyDescriptor::data(value, false, false, false),
                )?;
            }
            for &which in Intrinsic::ALL {
                let Some(name) = which.global_name() else {
                    continue;
                };
                let Some(o) = self.heap.realm(realm).intrinsics.try_get(which) else {
                    continue;
                };
                let key = self.heap.key_from_str(name);
                ops::define_property_or_throw(
                    self,
                    global,
                    key,
                    PropertyDescriptor::data(Value::Object(o), true, false, true),
                )?;
            }
            Ok(())
        })();
        self.pop_context();
        result?;
        self.heap.realm_mut(realm).stage = RealmStage::Ready;
        Ok(global)
    }

    /// InitializeHostDefinedRealm with an ordinary global object.
    pub fn initialize_host_defined_realm(&mut self) -> Realm {
        let realm = self.create_realm();
        self.set_realm_global_object(realm, None, None);
        self.set_default_global_bindings(realm)
            .expect("defining the default globals on an ordinary object cannot fail");
        realm
    }
}
