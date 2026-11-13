//! The trait of internal methods and the exotic objects of ECMA-262 §10.4
//! (Array, String, mapped arguments, TypedArray, module namespace,
//! immutable prototype) plus host objects. Proxy is in `proxy.rs`.
//!
//! Every method defaults to the ordinary one, so an exotic object only
//! overrides what the specification changes.

use std::any::Any;
use std::collections::HashMap;
use std::rc::Rc;

use crate::agent::Agent;
use crate::buffer;
use crate::completion::JsResult;
use crate::gc::{Env, Object, Trace, Tracer};
use crate::internal::{self, ordinary_get_own_property};
use crate::object::{ObjectKind, PropertyDescriptor};
use crate::ops;
use crate::proxy::ProxyMethods;
use crate::value::{PropertyKey, Value};

/// The eleven essential internal methods of an object (ECMA-262 Table 4).
/// `[[Call]]` and `[[Construct]]` are handled by `function.rs`.
pub trait InternalMethods {
    fn get_prototype_of(&self, agent: &mut Agent, o: Object) -> JsResult<Option<Object>> {
        Ok(agent.heap.object(o).proto)
    }

    fn set_prototype_of(
        &self,
        agent: &mut Agent,
        o: Object,
        proto: Option<Object>,
    ) -> JsResult<bool> {
        Ok(internal::ordinary_set_prototype_of(agent, o, proto))
    }

    fn is_extensible(&self, agent: &mut Agent, o: Object) -> JsResult<bool> {
        Ok(agent.heap.object(o).extensible)
    }

    fn prevent_extensions(&self, agent: &mut Agent, o: Object) -> JsResult<bool> {
        Ok(internal::ordinary_prevent_extensions(agent, o))
    }

    fn get_own_property(
        &self,
        agent: &mut Agent,
        o: Object,
        key: PropertyKey,
    ) -> JsResult<Option<PropertyDescriptor>> {
        Ok(ordinary_get_own_property(agent, o, key))
    }

    fn define_own_property(
        &self,
        agent: &mut Agent,
        o: Object,
        key: PropertyKey,
        desc: PropertyDescriptor,
    ) -> JsResult<bool> {
        internal::ordinary_define_own_property(agent, o, key, desc)
    }

    fn has_property(&self, agent: &mut Agent, o: Object, key: PropertyKey) -> JsResult<bool> {
        internal::ordinary_has_property(agent, o, key)
    }

    fn get(
        &self,
        agent: &mut Agent,
        o: Object,
        key: PropertyKey,
        receiver: Value,
    ) -> JsResult<Value> {
        internal::ordinary_get(agent, o, key, receiver)
    }

    fn set(
        &self,
        agent: &mut Agent,
        o: Object,
        key: PropertyKey,
        value: Value,
        receiver: Value,
    ) -> JsResult<bool> {
        internal::ordinary_set(agent, o, key, value, receiver)
    }

    fn delete(&self, agent: &mut Agent, o: Object, key: PropertyKey) -> JsResult<bool> {
        internal::ordinary_delete(agent, o, key)
    }

    fn own_property_keys(&self, agent: &mut Agent, o: Object) -> JsResult<Vec<PropertyKey>> {
        Ok(internal::ordinary_own_property_keys(agent, o))
    }

    /// Whether `[[GetPrototypeOf]]` is the ordinary one (used by the
    /// cycle check of OrdinarySetPrototypeOf).
    fn has_ordinary_get_prototype_of(&self) -> bool {
        true
    }
}

/// An object of the host (a DOM wrapper, for example). Its handles are
/// traced through [`Trace`]; it may override any internal method.
pub trait HostObject: InternalMethods + Trace + Any {
    fn as_any(&self) -> &dyn Any;

    /// `[[IsHTMLDDA]]` (only `document.all`).
    fn is_htmldda(&self) -> bool {
        false
    }
}

pub(crate) struct OrdinaryMethods;
impl InternalMethods for OrdinaryMethods {}

pub(crate) struct ArrayMethods;
pub(crate) struct ImmutablePrototypeMethods;
pub(crate) struct StringMethods;
pub(crate) struct ArgumentsMethods;
pub(crate) struct TypedArrayMethods;
pub(crate) struct NamespaceMethods;

/// The internal methods of one object.
pub(crate) enum Methods {
    Static(&'static dyn InternalMethods),
    Host(Rc<dyn HostObject>),
}

impl Methods {
    pub(crate) fn get(&self) -> &dyn InternalMethods {
        match self {
            Methods::Static(m) => *m,
            Methods::Host(h) => &**h,
        }
    }
}

pub(crate) fn methods_of(agent: &Agent, o: Object) -> Methods {
    let methods: &'static dyn InternalMethods = match &agent.heap.object(o).kind {
        ObjectKind::Array => &ArrayMethods,
        ObjectKind::ImmutablePrototype => &ImmutablePrototypeMethods,
        ObjectKind::String(_) => &StringMethods,
        ObjectKind::Arguments(_) => &ArgumentsMethods,
        ObjectKind::TypedArray(_) => &TypedArrayMethods,
        ObjectKind::ModuleNamespace(_) => &NamespaceMethods,
        ObjectKind::Proxy(_) => &ProxyMethods,
        ObjectKind::Host(h) => return Methods::Host(h.clone()),
        _ => &OrdinaryMethods,
    };
    Methods::Static(methods)
}

impl InternalMethods for ArrayMethods {
    fn define_own_property(
        &self,
        agent: &mut Agent,
        o: Object,
        key: PropertyKey,
        desc: PropertyDescriptor,
    ) -> JsResult<bool> {
        internal::array_define_own_property(agent, o, key, desc)
    }
}

impl InternalMethods for ImmutablePrototypeMethods {
    /// SetImmutablePrototype(O, V).
    fn set_prototype_of(
        &self,
        agent: &mut Agent,
        o: Object,
        proto: Option<Object>,
    ) -> JsResult<bool> {
        let current = internal::get_prototype_of(agent, o)?;
        Ok(current == proto)
    }
}

impl InternalMethods for StringMethods {
    fn get_own_property(
        &self,
        agent: &mut Agent,
        o: Object,
        key: PropertyKey,
    ) -> JsResult<Option<PropertyDescriptor>> {
        if let Some(desc) = ordinary_get_own_property(agent, o, key) {
            return Ok(Some(desc));
        }
        Ok(internal::string_get_own_property(agent, o, key))
    }

    fn define_own_property(
        &self,
        agent: &mut Agent,
        o: Object,
        key: PropertyKey,
        desc: PropertyDescriptor,
    ) -> JsResult<bool> {
        if let Some(string_desc) = internal::string_get_own_property(agent, o, key) {
            let extensible = agent.heap.object(o).extensible;
            return Ok(internal::is_compatible_property_descriptor(
                agent,
                extensible,
                desc,
                Some(string_desc),
            ));
        }
        internal::ordinary_define_own_property(agent, o, key, desc)
    }

    fn own_property_keys(&self, agent: &mut Agent, o: Object) -> JsResult<Vec<PropertyKey>> {
        let ObjectKind::String(s) = agent.heap.object(o).kind else {
            return Ok(internal::ordinary_own_property_keys(agent, o));
        };
        let len = agent.heap.string_len(s) as u32;
        let mut keys: Vec<PropertyKey> = (0..len).map(PropertyKey::Index).collect();
        // Stored index keys are all >= len: smaller ones are the string's.
        keys.extend(internal::ordinary_own_property_keys(agent, o));
        Ok(keys)
    }
}

/// `[[ParameterMap]]` of a mapped arguments object: argument index →
/// slot of the function's environment.
pub struct ArgumentsData {
    pub env: Env,
    pub map: Vec<Option<u32>>,
}

impl Trace for ArgumentsData {
    fn trace(&self, tracer: &mut Tracer) {
        tracer.env(self.env);
    }
}

/// The environment slot `key` is mapped to, if any.
fn mapped_slot(agent: &Agent, o: Object, key: PropertyKey) -> Option<(Env, u32)> {
    let PropertyKey::Index(i) = key else {
        return None;
    };
    match &agent.heap.object(o).kind {
        ObjectKind::Arguments(data) => data
            .map
            .get(i as usize)
            .copied()
            .flatten()
            .map(|slot| (data.env, slot)),
        _ => None,
    }
}

fn unmap(agent: &mut Agent, o: Object, key: PropertyKey) {
    if let (PropertyKey::Index(i), ObjectKind::Arguments(data)) =
        (key, &mut agent.heap.object_mut(o).kind)
    {
        if let Some(entry) = data.map.get_mut(i as usize) {
            *entry = None;
        }
    }
}

fn read_slot(agent: &Agent, env: Env, slot: u32) -> Value {
    agent.heap.env(env).slots[slot as usize].unwrap_or(Value::Undefined)
}

fn write_slot(agent: &mut Agent, env: Env, slot: u32, value: Value) {
    agent.heap.env_mut(env).slots[slot as usize] = Some(value);
}

impl InternalMethods for ArgumentsMethods {
    fn get_own_property(
        &self,
        agent: &mut Agent,
        o: Object,
        key: PropertyKey,
    ) -> JsResult<Option<PropertyDescriptor>> {
        let Some(mut desc) = ordinary_get_own_property(agent, o, key) else {
            return Ok(None);
        };
        if let Some((env, slot)) = mapped_slot(agent, o, key) {
            desc.value = Some(read_slot(agent, env, slot));
        }
        Ok(Some(desc))
    }

    fn define_own_property(
        &self,
        agent: &mut Agent,
        o: Object,
        key: PropertyKey,
        desc: PropertyDescriptor,
    ) -> JsResult<bool> {
        let mapped = mapped_slot(agent, o, key);
        let mut new_desc = desc;
        if let Some((env, slot)) = mapped {
            if desc.is_data() && desc.value.is_none() && desc.writable == Some(false) {
                new_desc.value = Some(read_slot(agent, env, slot));
            }
        }
        if !internal::ordinary_define_own_property(agent, o, key, new_desc)? {
            return Ok(false);
        }
        if let Some((env, slot)) = mapped {
            if desc.is_accessor() {
                unmap(agent, o, key);
            } else {
                if let Some(v) = desc.value {
                    write_slot(agent, env, slot, v);
                }
                if desc.writable == Some(false) {
                    unmap(agent, o, key);
                }
            }
        }
        Ok(true)
    }

    fn get(
        &self,
        agent: &mut Agent,
        o: Object,
        key: PropertyKey,
        receiver: Value,
    ) -> JsResult<Value> {
        match mapped_slot(agent, o, key) {
            Some((env, slot)) => Ok(read_slot(agent, env, slot)),
            None => internal::ordinary_get(agent, o, key, receiver),
        }
    }

    fn set(
        &self,
        agent: &mut Agent,
        o: Object,
        key: PropertyKey,
        value: Value,
        receiver: Value,
    ) -> JsResult<bool> {
        let mapped = if receiver == Value::Object(o) {
            mapped_slot(agent, o, key)
        } else {
            None
        };
        if let Some((env, slot)) = mapped {
            write_slot(agent, env, slot, value);
        }
        internal::ordinary_set(agent, o, key, value, receiver)
    }

    fn delete(&self, agent: &mut Agent, o: Object, key: PropertyKey) -> JsResult<bool> {
        let mapped = mapped_slot(agent, o, key).is_some();
        let result = internal::ordinary_delete(agent, o, key)?;
        if result && mapped {
            unmap(agent, o, key);
        }
        Ok(result)
    }
}

/// The numeric index named by a key of a TypedArray
/// (CanonicalNumericIndexString), if any.
fn numeric_key(agent: &Agent, key: PropertyKey) -> Option<f64> {
    match key {
        PropertyKey::Index(i) => Some(f64::from(i)),
        PropertyKey::String(a) => ops::canonical_numeric_index_string(agent.heap.atom_units(a)),
        PropertyKey::Symbol(_) => None,
    }
}

impl InternalMethods for TypedArrayMethods {
    fn prevent_extensions(&self, agent: &mut Agent, o: Object) -> JsResult<bool> {
        if !buffer::is_typed_array_fixed_length(agent, o) {
            return Ok(false);
        }
        Ok(internal::ordinary_prevent_extensions(agent, o))
    }

    fn get_own_property(
        &self,
        agent: &mut Agent,
        o: Object,
        key: PropertyKey,
    ) -> JsResult<Option<PropertyDescriptor>> {
        if let Some(index) = numeric_key(agent, key) {
            let value = buffer::typed_array_get_element(agent, o, index);
            return Ok(value.map(|v| PropertyDescriptor::data(v, true, true, true)));
        }
        Ok(ordinary_get_own_property(agent, o, key))
    }

    fn has_property(&self, agent: &mut Agent, o: Object, key: PropertyKey) -> JsResult<bool> {
        if let Some(index) = numeric_key(agent, key) {
            return Ok(buffer::is_valid_integer_index(agent, o, index));
        }
        internal::ordinary_has_property(agent, o, key)
    }

    fn define_own_property(
        &self,
        agent: &mut Agent,
        o: Object,
        key: PropertyKey,
        desc: PropertyDescriptor,
    ) -> JsResult<bool> {
        if let Some(index) = numeric_key(agent, key) {
            if !buffer::is_valid_integer_index(agent, o, index) {
                return Ok(false);
            }
            if desc.configurable == Some(false)
                || desc.enumerable == Some(false)
                || desc.is_accessor()
                || desc.writable == Some(false)
            {
                return Ok(false);
            }
            if let Some(v) = desc.value {
                buffer::typed_array_set_element(agent, o, index, v)?;
            }
            return Ok(true);
        }
        internal::ordinary_define_own_property(agent, o, key, desc)
    }

    fn get(
        &self,
        agent: &mut Agent,
        o: Object,
        key: PropertyKey,
        receiver: Value,
    ) -> JsResult<Value> {
        if let Some(index) = numeric_key(agent, key) {
            return Ok(buffer::typed_array_get_element(agent, o, index).unwrap_or(Value::Undefined));
        }
        internal::ordinary_get(agent, o, key, receiver)
    }

    fn set(
        &self,
        agent: &mut Agent,
        o: Object,
        key: PropertyKey,
        value: Value,
        receiver: Value,
    ) -> JsResult<bool> {
        if let Some(index) = numeric_key(agent, key) {
            if receiver == Value::Object(o) {
                buffer::typed_array_set_element(agent, o, index, value)?;
                return Ok(true);
            }
            if !buffer::is_valid_integer_index(agent, o, index) {
                return Ok(true);
            }
        }
        internal::ordinary_set(agent, o, key, value, receiver)
    }

    fn delete(&self, agent: &mut Agent, o: Object, key: PropertyKey) -> JsResult<bool> {
        if let Some(index) = numeric_key(agent, key) {
            return Ok(!buffer::is_valid_integer_index(agent, o, index));
        }
        internal::ordinary_delete(agent, o, key)
    }

    fn own_property_keys(&self, agent: &mut Agent, o: Object) -> JsResult<Vec<PropertyKey>> {
        let mut keys = Vec::new();
        if let Some(len) = buffer::typed_array_length(agent, o) {
            keys.extend((0..len as u32).map(PropertyKey::Index));
        }
        keys.extend(internal::ordinary_own_property_keys(agent, o));
        Ok(keys)
    }
}

/// Where an export of a module namespace reads its value.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum NamespaceBinding {
    /// A binding of a module environment (`None` slot = uninitialized).
    Slot { env: Env, slot: u32 },
    /// `export * as ns`: another namespace object.
    Namespace(Object),
}

/// Slots of a module namespace exotic object. `exports` is sorted by the
/// code units of the export names.
pub struct NamespaceData {
    pub module: Option<Object>,
    pub exports: Vec<(PropertyKey, NamespaceBinding)>,
    pub(crate) index: HashMap<PropertyKey, usize>,
}

impl NamespaceData {
    pub fn new(module: Option<Object>, exports: Vec<(PropertyKey, NamespaceBinding)>) -> Self {
        let index = exports
            .iter()
            .enumerate()
            .map(|(i, (k, _))| (*k, i))
            .collect();
        NamespaceData {
            module,
            exports,
            index,
        }
    }
}

impl Trace for NamespaceData {
    fn trace(&self, tracer: &mut Tracer) {
        if let Some(m) = self.module {
            tracer.object(m);
        }
        for (key, binding) in &self.exports {
            tracer.key(*key);
            match *binding {
                NamespaceBinding::Slot { env, .. } => {
                    tracer.env(env);
                }
                NamespaceBinding::Namespace(o) => {
                    tracer.object(o);
                }
            }
        }
    }
}

/// ModuleNamespaceCreate: `exports` in any order; they are sorted here.
pub fn module_namespace_create(
    agent: &mut Agent,
    module: Option<Object>,
    mut exports: Vec<(PropertyKey, NamespaceBinding)>,
) -> Object {
    let mut named: Vec<(Vec<u16>, (PropertyKey, NamespaceBinding))> = exports
        .drain(..)
        .map(|(k, b)| (agent.heap.key_units(k).unwrap_or_default(), (k, b)))
        .collect();
    named.sort_by(|a, b| a.0.cmp(&b.0));
    let exports = named.into_iter().map(|(_, e)| e).collect();
    let o = agent.heap.create_object(
        None,
        ObjectKind::ModuleNamespace(Box::new(NamespaceData::new(module, exports))),
    );
    // @@toStringTag: "Module", non-writable, non-enumerable, non-configurable.
    let tag = PropertyKey::Symbol(agent.symbols.to_string_tag);
    let value = Value::String(agent.heap.alloc_str("Module"));
    agent.heap.set_own(
        o,
        tag,
        crate::object::Attributes::data(false, false, false),
        crate::object::Slot::Data(value),
    );
    agent.heap.set_extensible(o, false);
    o
}

fn namespace_binding(agent: &Agent, o: Object, key: PropertyKey) -> Option<NamespaceBinding> {
    match &agent.heap.object(o).kind {
        ObjectKind::ModuleNamespace(ns) => ns.index.get(&key).map(|&i| ns.exports[i].1),
        _ => None,
    }
}

impl InternalMethods for NamespaceMethods {
    fn get_prototype_of(&self, _agent: &mut Agent, _o: Object) -> JsResult<Option<Object>> {
        Ok(None)
    }

    fn set_prototype_of(
        &self,
        _agent: &mut Agent,
        _o: Object,
        proto: Option<Object>,
    ) -> JsResult<bool> {
        Ok(proto.is_none())
    }

    fn is_extensible(&self, _agent: &mut Agent, _o: Object) -> JsResult<bool> {
        Ok(false)
    }

    fn prevent_extensions(&self, _agent: &mut Agent, _o: Object) -> JsResult<bool> {
        Ok(true)
    }

    fn get_own_property(
        &self,
        agent: &mut Agent,
        o: Object,
        key: PropertyKey,
    ) -> JsResult<Option<PropertyDescriptor>> {
        if key.is_symbol() {
            return Ok(ordinary_get_own_property(agent, o, key));
        }
        if namespace_binding(agent, o, key).is_none() {
            return Ok(None);
        }
        let value = self.get(agent, o, key, Value::Object(o))?;
        Ok(Some(PropertyDescriptor::data(value, true, true, false)))
    }

    fn define_own_property(
        &self,
        agent: &mut Agent,
        o: Object,
        key: PropertyKey,
        desc: PropertyDescriptor,
    ) -> JsResult<bool> {
        if key.is_symbol() {
            return internal::ordinary_define_own_property(agent, o, key, desc);
        }
        let Some(current) = self.get_own_property(agent, o, key)? else {
            return Ok(false);
        };
        if desc.configurable == Some(true)
            || desc.enumerable == Some(false)
            || desc.is_accessor()
            || desc.writable == Some(false)
        {
            return Ok(false);
        }
        if let Some(v) = desc.value {
            return Ok(ops::same_value(
                agent,
                v,
                current.value.unwrap_or(Value::Undefined),
            ));
        }
        Ok(true)
    }

    fn has_property(&self, agent: &mut Agent, o: Object, key: PropertyKey) -> JsResult<bool> {
        if key.is_symbol() {
            return internal::ordinary_has_property(agent, o, key);
        }
        Ok(namespace_binding(agent, o, key).is_some())
    }

    fn get(
        &self,
        agent: &mut Agent,
        o: Object,
        key: PropertyKey,
        receiver: Value,
    ) -> JsResult<Value> {
        if key.is_symbol() {
            return internal::ordinary_get(agent, o, key, receiver);
        }
        match namespace_binding(agent, o, key) {
            None => Ok(Value::Undefined),
            Some(NamespaceBinding::Namespace(ns)) => Ok(Value::Object(ns)),
            Some(NamespaceBinding::Slot { env, slot }) => {
                let binding = agent.heap.env(env).slots[slot as usize];
                match binding {
                    Some(v) => Ok(v),
                    None => {
                        let name = agent.heap.key_to_rust(key);
                        agent.reference_error(&format!("{name} is not initialized"))
                    }
                }
            }
        }
    }

    fn set(
        &self,
        _agent: &mut Agent,
        _o: Object,
        _key: PropertyKey,
        _value: Value,
        _receiver: Value,
    ) -> JsResult<bool> {
        Ok(false)
    }

    fn delete(&self, agent: &mut Agent, o: Object, key: PropertyKey) -> JsResult<bool> {
        if key.is_symbol() {
            return internal::ordinary_delete(agent, o, key);
        }
        Ok(namespace_binding(agent, o, key).is_none())
    }

    fn own_property_keys(&self, agent: &mut Agent, o: Object) -> JsResult<Vec<PropertyKey>> {
        let mut keys: Vec<PropertyKey> = match &agent.heap.object(o).kind {
            ObjectKind::ModuleNamespace(ns) => ns.exports.iter().map(|(k, _)| *k).collect(),
            _ => Vec::new(),
        };
        keys.extend(
            internal::ordinary_own_property_keys(agent, o)
                .into_iter()
                .filter(|k| k.is_symbol()),
        );
        Ok(keys)
    }
}
