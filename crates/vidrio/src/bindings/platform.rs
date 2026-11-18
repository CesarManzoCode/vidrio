//! Platform objects (Web IDL §3.8) as vjs host objects, with the internal
//! methods of legacy platform objects (Web IDL §3.9): indexed and named
//! properties exposed through `[[GetOwnProperty]]`, `[[Set]]`,
//! `[[DefineOwnProperty]]`, `[[Delete]]`, `[[PreventExtensions]]` and
//! `[[OwnPropertyKeys]]`.

use std::any::Any;
use std::cell::RefCell;
use std::rc::Rc;

use vjs::exotic::{HostObject, InternalMethods};
use vjs::gc::{Realm, Trace, Tracer};
use vjs::internal;
use vjs::object::{ObjectKind, PropertyDescriptor};
use vjs::{Agent, JsResult, Object, PropertyKey, Value};

use super::convert;
use super::generated::{INTERFACES, Op};
use super::imp;
use super::meta::{Id, Ty};
use super::value::{IdlValue, Invocation};
use super::wrappers::DocTable;
use crate::dom::abort::SignalId;
use crate::dom::collections::{DomTokenList, HtmlCollection, NamedNodeMap, NodeList};
use crate::dom::events::EventRef;
use crate::dom::observer::{MutationRecord, ObserverId};
use crate::dom::range::{RangeId, StaticRange};
use crate::dom::traversal::{IteratorId, WalkerRef};
use crate::dom::{DocRef, HostRef, NodeId};

/// What a pair iterator returns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IterKind {
    Keys,
    Values,
    Entries,
}

/// A JavaScript `MutationObserver`: its callback and, for each document
/// it observes nodes of, its observer there.
#[derive(Debug)]
pub struct ObserverRef {
    pub callback: HostRef,
    pub per_document: RefCell<Vec<(DocRef, ObserverId)>>,
}

/// The native object behind a platform object.
pub enum Native {
    /// An interface without native state (or not implemented yet).
    None,
    Window,
    Node(DocRef, NodeId),
    Event(EventRef),
    /// A plain `EventTarget` (`new EventTarget()`), by host id.
    EventTarget(u64),
    NodeList(DocRef, NodeList),
    HtmlCollection(DocRef, HtmlCollection),
    NamedNodeMap(DocRef, NamedNodeMap),
    TokenList(DocRef, DomTokenList),
    Range(DocRef, RangeId),
    StaticRange(DocRef, StaticRange),
    NodeIterator(DocRef, IteratorId),
    TreeWalker(DocRef, WalkerRef),
    MutationObserver(Rc<ObserverRef>),
    MutationRecord(DocRef, Rc<MutationRecord>),
    AbortController(DocRef, SignalId),
    AbortSignal(DocRef, SignalId),
    Exception {
        name: String,
        message: String,
    },
    Implementation(DocRef),
    /// The native state of an object of the data APIs of k3-11 (fetch,
    /// files, URLs, encodings, streams; see `crate::fetch`).
    Data(Rc<dyn Any>),
    /// An event target with native state (`XMLHttpRequest`,
    /// `FileReader`): its host id, as for [`Native::EventTarget`], and its
    /// state.
    TargetData(u64, Rc<dyn Any>),
    /// An iterator of a pair iterable, maplike or setlike.
    Iterator {
        target: Object,
        kind: IterKind,
        index: usize,
    },
}

impl Native {
    /// The document the object belongs to, if any.
    pub fn document(&self) -> Option<&DocRef> {
        match self {
            Native::Node(d, _)
            | Native::NodeList(d, _)
            | Native::HtmlCollection(d, _)
            | Native::NamedNodeMap(d, _)
            | Native::TokenList(d, _)
            | Native::Range(d, _)
            | Native::StaticRange(d, _)
            | Native::NodeIterator(d, _)
            | Native::TreeWalker(d, _)
            | Native::MutationRecord(d, _)
            | Native::AbortController(d, _)
            | Native::AbortSignal(d, _)
            | Native::Implementation(d) => Some(d),
            _ => None,
        }
    }
}

/// The special operations of a legacy platform object, found through the
/// inheritance chain.
#[derive(Clone, Copy, Debug, Default)]
pub struct LegacyOps {
    pub indexed_getter: Option<(Op, Ty)>,
    pub indexed_setter: Option<(Op, Ty)>,
    pub named_getter: Option<(Op, Ty)>,
    pub named_setter: Option<(Op, Ty)>,
    pub named_deleter: Option<Op>,
    pub index_count: Option<Op>,
    pub supported_names: Option<Op>,
    pub unenumerable_names: bool,
    pub override_builtins: bool,
    pub global: bool,
}

/// The legacy operations of `iface`, or `None` if its objects are not
/// legacy platform objects.
pub fn legacy_ops(iface: Id) -> Option<LegacyOps> {
    let mut l = LegacyOps::default();
    let mut next = Some(iface);
    while let Some(i) = next {
        let def = &INTERFACES[i as usize];
        let s = &def.special;
        l.indexed_getter = l.indexed_getter.or(s.indexed_getter);
        l.indexed_setter = l.indexed_setter.or(s.indexed_setter);
        l.named_getter = l.named_getter.or(s.named_getter);
        l.named_setter = l.named_setter.or(s.named_setter);
        l.named_deleter = l.named_deleter.or(s.named_deleter);
        l.index_count = l.index_count.or(s.index_count);
        l.supported_names = l.supported_names.or(s.supported_names);
        l.unenumerable_names |= def.legacy_unenumerable_named_properties;
        l.override_builtins |= def.legacy_override_builtins;
        l.global |= def.global;
        next = def.parent;
    }
    // The named properties of `Window` (HTML §7.2.2.3) belong to the
    // document's named elements, which do not exist yet: the global
    // object stays ordinary (LIMITATIONS.md).
    if l.global {
        return None;
    }
    if l.indexed_getter.is_some() || l.named_getter.is_some() {
        Some(l)
    } else {
        None
    }
}

/// True if interface `iface` is `target` or inherits from it.
pub fn implements(iface: Id, target: Id) -> bool {
    let mut next = Some(iface);
    while let Some(i) = next {
        if i == target {
            return true;
        }
        next = INTERFACES[i as usize].parent;
    }
    false
}

/// A platform object: the interface it implements (its primary
/// interface), its relevant realm, its native object and traced slots
/// (`[SameObject]` caches, backing maps, …).
pub struct PlatformObject {
    pub iface: Id,
    pub realm: Realm,
    pub native: RefCell<Native>,
    pub slots: RefCell<Vec<(&'static str, Value)>>,
    /// The node→wrapper table of the document the object belongs to;
    /// tracing the object traces the table (see [`DocTable`]).
    pub table: RefCell<Option<Rc<DocTable>>>,
    pub legacy: Option<LegacyOps>,
}

impl PlatformObject {
    pub fn new(iface: Id, realm: Realm, native: Native) -> PlatformObject {
        PlatformObject {
            iface,
            realm,
            native: RefCell::new(native),
            slots: RefCell::new(Vec::new()),
            table: RefCell::new(None),
            legacy: legacy_ops(iface),
        }
    }

    pub fn slot(&self, key: &str) -> Option<Value> {
        self.slots
            .borrow()
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| *v)
    }

    pub fn set_slot(&self, key: &'static str, value: Value) {
        let mut slots = self.slots.borrow_mut();
        match slots.iter_mut().find(|(k, _)| *k == key) {
            Some(entry) => entry.1 = value,
            None => slots.push((key, value)),
        }
    }

    pub fn implements(&self, target: Id) -> bool {
        implements(self.iface, target)
    }
}

impl Trace for PlatformObject {
    fn trace(&self, tracer: &mut Tracer) {
        for (_, v) in self.slots.borrow().iter() {
            tracer.value(*v);
        }
        if let Native::Iterator { target, .. } = &*self.native.borrow() {
            tracer.object(*target);
        }
        if let Some(table) = &*self.table.borrow() {
            table.trace_from_wrapper(tracer);
        }
    }
}

/// A platform object with the handle of its JS object.
#[derive(Clone)]
pub struct PlatformRef {
    pub object: Object,
    host: Rc<dyn HostObject>,
}

impl PlatformRef {
    pub fn get(&self) -> &PlatformObject {
        self.host
            .as_any()
            .downcast_ref::<PlatformObject>()
            .expect("a platform object")
    }
}

/// The platform object `v` is, if it is one. A `WindowProxy` stands for
/// its browsing context's active `Window`.
pub fn platform_ref(agent: &Agent, v: Value) -> Option<PlatformRef> {
    let Value::Object(o) = v else {
        return None;
    };
    match &agent.heap.object(o).kind {
        ObjectKind::Host(h) if h.as_any().is::<PlatformObject>() => Some(PlatformRef {
            object: o,
            host: h.clone(),
        }),
        ObjectKind::Host(h) if h.as_any().is::<crate::browser::window::WindowProxy>() => {
            let w = crate::browser::window::proxy_target(agent, o)?;
            match &agent.heap.object(w).kind {
                ObjectKind::Host(h) if h.as_any().is::<PlatformObject>() => Some(PlatformRef {
                    object: w,
                    host: h.clone(),
                }),
                _ => None,
            }
        }
        _ => None,
    }
}

/// The platform object `v` is if it implements `iface`.
pub fn platform_implementing(agent: &Agent, v: Value, iface: Id) -> Option<PlatformRef> {
    platform_ref(agent, v).filter(|p| p.get().implements(iface))
}

/// Creates a platform object of `iface` in the realm of `state` with
/// prototype `proto` (the interface prototype object if `None`) and
/// defines its `[LegacyUnforgeable]` members.
pub fn create(
    agent: &mut Agent,
    state: &super::RealmState,
    iface: Id,
    native: Native,
    proto: Option<Object>,
) -> Object {
    let proto = proto.or_else(|| state.proto(iface)).unwrap_or_else(|| {
        agent
            .heap
            .realm(state.realm)
            .intrinsics
            .get(vjs::realm::Intrinsic::ObjectPrototype)
    });
    let object = PlatformObject::new(iface, state.realm, native);
    let o = agent
        .heap
        .create_object(Some(proto), ObjectKind::Host(Rc::new(object)));
    super::install::define_unforgeables(agent, state, o, iface);
    o
}

// ----- legacy platform objects ------------------------------------------------

/// Calls a special operation on `o`.
fn special_call(agent: &mut Agent, o: Object, op: Op, args: Vec<IdlValue>) -> JsResult<IdlValue> {
    let this = platform_ref(agent, Value::Object(o));
    let iface = this.as_ref().map_or(0, |p| p.get().iface);
    let mut inv = Invocation {
        op,
        overload: 0,
        iface,
        this,
        args,
        new_target: None,
    };
    imp::call(agent, &mut inv)
}

fn is_supported_index(agent: &mut Agent, o: Object, l: &LegacyOps, index: u32) -> JsResult<bool> {
    let Some(op) = l.index_count else {
        return Ok(false);
    };
    let count = special_call(agent, o, op, Vec::new())?.as_u64();
    Ok(u64::from(index) < count)
}

fn supported_names(agent: &mut Agent, o: Object, l: &LegacyOps) -> JsResult<Vec<String>> {
    let Some(op) = l.supported_names else {
        return Ok(Vec::new());
    };
    Ok(match special_call(agent, o, op, Vec::new())? {
        IdlValue::Sequence(items) => items
            .into_iter()
            .filter_map(|v| match v {
                IdlValue::String(s) => Some(s),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    })
}

/// The string a key names, for named properties (symbols name none).
fn key_name(agent: &mut Agent, key: PropertyKey) -> Option<String> {
    match key {
        PropertyKey::Symbol(_) => None,
        _ => Some(agent.heap.key_to_rust(key)),
    }
}

/// The named property visibility algorithm (Web IDL §3.9).
fn named_visible(agent: &mut Agent, o: Object, l: &LegacyOps, key: PropertyKey) -> JsResult<bool> {
    let Some(name) = key_name(agent, key) else {
        return Ok(false);
    };
    if l.named_getter.is_none() || !supported_names(agent, o, l)?.contains(&name) {
        return Ok(false);
    }
    if internal::ordinary_get_own_property(agent, o, key).is_some() {
        return Ok(false);
    }
    if l.override_builtins {
        return Ok(true);
    }
    let mut proto = agent.heap.object(o).proto();
    while let Some(p) = proto {
        if internal::get_own_property(agent, p, key)?.is_some() {
            return Ok(false);
        }
        proto = internal::get_prototype_of(agent, p)?;
    }
    Ok(true)
}

/// LegacyPlatformObjectGetOwnProperty(O, P, ignoreNamedProps).
fn legacy_get_own_property(
    agent: &mut Agent,
    o: Object,
    l: &LegacyOps,
    key: PropertyKey,
    mut ignore_named: bool,
) -> JsResult<Option<PropertyDescriptor>> {
    if let (Some((getter, ty)), PropertyKey::Index(index)) = (l.indexed_getter, key) {
        if is_supported_index(agent, o, l, index)? {
            let v = special_call(agent, o, getter, vec![IdlValue::UInt(u64::from(index))])?;
            let value = convert::to_js(agent, v, ty)?;
            return Ok(Some(PropertyDescriptor::data(
                value,
                l.indexed_setter.is_some(),
                true,
                true,
            )));
        }
        ignore_named = true;
    }
    if let (Some((getter, ty)), false) = (l.named_getter, ignore_named) {
        if named_visible(agent, o, l, key)? {
            let name = key_name(agent, key).unwrap_or_default();
            let v = special_call(agent, o, getter, vec![IdlValue::String(name)])?;
            let value = convert::to_js(agent, v, ty)?;
            return Ok(Some(PropertyDescriptor::data(
                value,
                l.named_setter.is_some(),
                !l.unenumerable_names,
                true,
            )));
        }
    }
    Ok(internal::ordinary_get_own_property(agent, o, key))
}

fn invoke_indexed_setter(
    agent: &mut Agent,
    o: Object,
    (op, ty): (Op, Ty),
    index: u32,
    value: Value,
) -> JsResult<()> {
    let v = convert::to_idl(agent, value, ty)?;
    special_call(agent, o, op, vec![IdlValue::UInt(u64::from(index)), v])?;
    Ok(())
}

fn invoke_named_setter(
    agent: &mut Agent,
    o: Object,
    (op, ty): (Op, Ty),
    name: String,
    value: Value,
) -> JsResult<()> {
    let v = convert::to_idl(agent, value, ty)?;
    special_call(agent, o, op, vec![IdlValue::String(name), v])?;
    Ok(())
}

impl InternalMethods for PlatformObject {
    fn get_own_property(
        &self,
        agent: &mut Agent,
        o: Object,
        key: PropertyKey,
    ) -> JsResult<Option<PropertyDescriptor>> {
        match &self.legacy {
            Some(l) => legacy_get_own_property(agent, o, l, key, false),
            None => Ok(internal::ordinary_get_own_property(agent, o, key)),
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
        let Some(l) = &self.legacy else {
            return internal::ordinary_set(agent, o, key, value, receiver);
        };
        if receiver == Value::Object(o) {
            if let (Some(setter), PropertyKey::Index(index)) = (l.indexed_setter, key) {
                invoke_indexed_setter(agent, o, setter, index, value)?;
                return Ok(true);
            }
            if let Some(setter) = l.named_setter {
                if let Some(name) = key_name(agent, key) {
                    invoke_named_setter(agent, o, setter, name, value)?;
                    return Ok(true);
                }
            }
        }
        let own = legacy_get_own_property(agent, o, l, key, true)?;
        internal::ordinary_set_with_own_descriptor(agent, o, key, value, receiver, own)
    }

    fn define_own_property(
        &self,
        agent: &mut Agent,
        o: Object,
        key: PropertyKey,
        desc: PropertyDescriptor,
    ) -> JsResult<bool> {
        let Some(l) = &self.legacy else {
            return internal::ordinary_define_own_property(agent, o, key, desc);
        };
        if let (Some(_), PropertyKey::Index(index)) = (l.indexed_getter, key) {
            if !desc.is_data() {
                return Ok(false);
            }
            let Some(setter) = l.indexed_setter else {
                return Ok(false);
            };
            invoke_indexed_setter(
                agent,
                o,
                setter,
                index,
                desc.value.unwrap_or(Value::Undefined),
            )?;
            return Ok(true);
        }
        if l.named_getter.is_some() && !l.global {
            if let Some(name) = key_name(agent, key) {
                let unforgeable = internal::ordinary_get_own_property(agent, o, key)
                    .is_some_and(|d| d.configurable == Some(false));
                if !unforgeable {
                    let creating = !supported_names(agent, o, l)?.contains(&name);
                    let has_own = internal::ordinary_get_own_property(agent, o, key).is_some();
                    if l.override_builtins || !has_own {
                        if !creating && l.named_setter.is_none() {
                            return Ok(false);
                        }
                        if let Some(setter) = l.named_setter {
                            if !desc.is_data() {
                                return Ok(false);
                            }
                            invoke_named_setter(
                                agent,
                                o,
                                setter,
                                name,
                                desc.value.unwrap_or(Value::Undefined),
                            )?;
                            return Ok(true);
                        }
                    }
                }
            }
        }
        internal::ordinary_define_own_property(agent, o, key, desc)
    }

    fn delete(&self, agent: &mut Agent, o: Object, key: PropertyKey) -> JsResult<bool> {
        let Some(l) = &self.legacy else {
            return internal::ordinary_delete(agent, o, key);
        };
        if let (Some(_), PropertyKey::Index(index)) = (l.indexed_getter, key) {
            return Ok(!is_supported_index(agent, o, l, index)?);
        }
        if l.named_getter.is_some() && !l.global && named_visible(agent, o, l, key)? {
            let Some(deleter) = l.named_deleter else {
                return Ok(false);
            };
            let name = key_name(agent, key).unwrap_or_default();
            let r = special_call(agent, o, deleter, vec![IdlValue::String(name)])?;
            return Ok(!matches!(r, IdlValue::Bool(false)));
        }
        internal::ordinary_delete(agent, o, key)
    }

    fn prevent_extensions(&self, agent: &mut Agent, o: Object) -> JsResult<bool> {
        if self.legacy.is_some() {
            return Ok(false);
        }
        Ok(internal::ordinary_prevent_extensions(agent, o))
    }

    fn own_property_keys(&self, agent: &mut Agent, o: Object) -> JsResult<Vec<PropertyKey>> {
        let Some(l) = &self.legacy else {
            return Ok(internal::ordinary_own_property_keys(agent, o));
        };
        let mut keys = Vec::new();
        if let (Some(_), Some(op)) = (l.indexed_getter, l.index_count) {
            let count = special_call(agent, o, op, Vec::new())?.as_u64();
            for i in 0..count.min(u64::from(u32::MAX - 1)) {
                keys.push(PropertyKey::Index(i as u32));
            }
        }
        if l.named_getter.is_some() {
            for name in supported_names(agent, o, l)? {
                let key = agent.heap.key_from_str(&name);
                if named_visible(agent, o, l, key)? && !keys.contains(&key) {
                    keys.push(key);
                }
            }
        }
        for key in internal::ordinary_own_property_keys(agent, o) {
            if !keys.contains(&key) {
                keys.push(key);
            }
        }
        Ok(keys)
    }
}

impl HostObject for PlatformObject {
    fn as_any(&self) -> &dyn Any {
        self
    }
}
