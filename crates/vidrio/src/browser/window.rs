//! The per-`Window` state of the user agent (kept in the bindings'
//! [`RealmState`]) and the `WindowProxy` exotic object (HTML §7.2.3).
//!
//! A `WindowProxy` belongs to a browsing context and forwards every
//! internal method to the context's active `Window`, so that the object
//! scripts hold (`window`, `self`, `globalThis`, `frames`, an iframe's
//! `contentWindow`) keeps its identity across navigations. Indexed
//! properties are the child browsing contexts (`window[0]`). Every window
//! of vidrio is same-origin with every other as far as the proxy is
//! concerned: the cross-origin checks of HTML §7.2.3 are not made
//! (LIMITATIONS.md).

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use vjs::exotic::{HostObject, InternalMethods};
use vjs::gc::{Realm, Trace, Tracer};
use vjs::internal;
use vjs::object::{ObjectKind, PropertyDescriptor};
use vjs::{Agent, JsResult, Object, PropertyKey, Value};

use super::UserAgent;
use super::module::ImportMap;
use super::navigable::ContextId;
use crate::bindings::{RealmState, state_of};
use crate::dom::DocRef;

/// Values kept alive by a temporary root set.
pub struct Roots(pub Vec<Value>);

impl Trace for Roots {
    fn trace(&self, tracer: &mut Tracer) {
        self.0.trace(tracer);
    }
}

/// What the user agent keeps per `Window`.
#[derive(Default)]
pub struct WindowData {
    /// The browsing context whose window this is.
    pub context: Cell<Option<ContextId>>,
    /// The context's `WindowProxy` (the realm's `globalThis`).
    pub proxy: Cell<Option<Object>>,
    /// The time origin, in milliseconds of the event loop's clock.
    pub time_origin: Cell<f64>,
    /// "In error reporting mode".
    pub error_reporting: Cell<bool>,
    /// The about-to-be-notified rejected promises list.
    pub about_to_be_notified: RefCell<Vec<Object>>,
    /// The outstanding rejected promises weak set (held strongly).
    pub outstanding_rejections: RefCell<Vec<Object>>,
    /// The map of animation frame callbacks, in insertion order.
    pub animation_frames: RefCell<Vec<(u32, Value)>>,
    pub animation_frame_id: Cell<u32>,
    /// `[SameObject]`-like caches of the window's objects.
    pub location: Cell<Option<Object>>,
    pub history: Cell<Option<Object>>,
    pub navigator: Cell<Option<Object>>,
    pub performance: Cell<Option<Object>>,
    /// The import map.
    pub import_map: RefCell<ImportMap>,
    /// The console's counters and timers.
    pub console_counts: RefCell<Vec<(String, u64)>>,
    pub console_timers: RefCell<Vec<(String, f64)>>,
    pub console_group: Cell<usize>,
    /// `window.status`.
    pub status: RefCell<String>,
    /// `history.scrollRestoration` (empty: `auto`).
    pub scroll_restoration: RefCell<String>,
    /// `history.state` (`None`: `null`).
    pub history_state: Cell<Option<Value>>,
}

impl Trace for WindowData {
    fn trace(&self, tracer: &mut Tracer) {
        for o in [
            self.proxy.get(),
            self.location.get(),
            self.history.get(),
            self.navigator.get(),
            self.performance.get(),
        ]
        .into_iter()
        .flatten()
        {
            tracer.object(o);
        }
        for o in self.about_to_be_notified.borrow().iter() {
            tracer.object(*o);
        }
        for o in self.outstanding_rejections.borrow().iter() {
            tracer.object(*o);
        }
        for (_, v) in self.animation_frames.borrow().iter() {
            tracer.value(*v);
        }
        if let Some(v) = self.history_state.get() {
            tracer.value(v);
        }
    }
}

/// Sets `history.state` of the window of `state`.
pub fn set_history_state(_agent: &mut Agent, state: &RealmState, value: Value) {
    state.html.history_state.set(Some(value));
}

/// The document of the window whose realm is `realm`.
pub fn document_of_realm(ua: &UserAgent, realm: Realm) -> Option<DocRef> {
    ua.contexts
        .borrow()
        .iter()
        .filter_map(|c| c.active_window())
        .find(|w| w.realm == realm)
        .map(|w| w.doc)
}

/// The realm state of the window whose document is `doc`.
pub fn state_of_document(agent: &Agent, ua: &UserAgent, doc: &DocRef) -> Option<Rc<RealmState>> {
    let realm = ua.doc_info(doc)?.realm.get()?;
    state_of(agent, realm)
}

/// The current time of a window: milliseconds since its time origin
/// (`performance.now()`).
pub fn window_now(state: &RealmState) -> f64 {
    match super::ua() {
        Some(ua) => ua.event_loop.clock.now() - state.html.time_origin.get(),
        None => state.time_origin.elapsed().as_secs_f64() * 1000.0,
    }
}

/// The `WindowProxy` of a browsing context.
pub struct WindowProxy {
    pub context: ContextId,
}

impl Trace for WindowProxy {
    fn trace(&self, _tracer: &mut Tracer) {}
}

impl WindowProxy {
    /// The context's active `Window` (its global object).
    fn window(&self) -> Option<Object> {
        let ua = super::ua()?;
        let ctx = ua.context(self.context)?;
        let w = ctx.active_window()?;
        w.state.global.get()
    }

    /// The `WindowProxy` objects of the context's children, in tree order.
    fn children(&self) -> Vec<Object> {
        let Some(ua) = super::ua() else {
            return Vec::new();
        };
        super::navigable::child_contexts(&ua, self.context)
            .iter()
            .map(|c| c.proxy)
            .collect()
    }
}

/// The index a key names, if it is an array index.
fn array_index(key: PropertyKey) -> Option<u32> {
    match key {
        PropertyKey::Index(i) => Some(i),
        _ => None,
    }
}

impl InternalMethods for WindowProxy {
    fn get_prototype_of(&self, agent: &mut Agent, _o: Object) -> JsResult<Option<Object>> {
        match self.window() {
            Some(w) => internal::get_prototype_of(agent, w),
            None => Ok(None),
        }
    }

    fn set_prototype_of(
        &self,
        agent: &mut Agent,
        o: Object,
        proto: Option<Object>,
    ) -> JsResult<bool> {
        // SetImmutablePrototype.
        let current = self.get_prototype_of(agent, o)?;
        Ok(current == proto)
    }

    fn is_extensible(&self, _agent: &mut Agent, _o: Object) -> JsResult<bool> {
        Ok(true)
    }

    fn prevent_extensions(&self, _agent: &mut Agent, _o: Object) -> JsResult<bool> {
        Ok(false)
    }

    fn get_own_property(
        &self,
        agent: &mut Agent,
        _o: Object,
        key: PropertyKey,
    ) -> JsResult<Option<PropertyDescriptor>> {
        if let Some(i) = array_index(key) {
            let children = self.children();
            return Ok(children.get(i as usize).map(|c| PropertyDescriptor {
                value: Some(Value::Object(*c)),
                writable: Some(false),
                get: None,
                set: None,
                enumerable: Some(true),
                configurable: Some(true),
            }));
        }
        let Some(w) = self.window() else {
            return Ok(None);
        };
        let mut desc = internal::get_own_property(agent, w, key)?;
        if let Some(d) = &mut desc {
            // A non-configurable property of the Window would break the
            // proxy's invariants after a navigation.
            if d.configurable == Some(false) {
                d.configurable = Some(true);
            }
        }
        Ok(desc)
    }

    fn define_own_property(
        &self,
        agent: &mut Agent,
        _o: Object,
        key: PropertyKey,
        desc: PropertyDescriptor,
    ) -> JsResult<bool> {
        if array_index(key).is_some() {
            return Ok(false);
        }
        match self.window() {
            Some(w) => internal::define_own_property(agent, w, key, desc),
            None => Ok(false),
        }
    }

    fn delete(&self, agent: &mut Agent, o: Object, key: PropertyKey) -> JsResult<bool> {
        if array_index(key).is_some() {
            return Ok(self.get_own_property(agent, o, key)?.is_none());
        }
        match self.window() {
            Some(w) => internal::delete(agent, w, key),
            None => Ok(true),
        }
    }

    fn has_property(&self, agent: &mut Agent, o: Object, key: PropertyKey) -> JsResult<bool> {
        if array_index(key).is_some() && self.get_own_property(agent, o, key)?.is_some() {
            return Ok(true);
        }
        match self.window() {
            Some(w) => internal::has_property(agent, w, key),
            None => Ok(false),
        }
    }

    fn get(
        &self,
        agent: &mut Agent,
        o: Object,
        key: PropertyKey,
        receiver: Value,
    ) -> JsResult<Value> {
        if let Some(i) = array_index(key) {
            if let Some(c) = self.children().get(i as usize) {
                return Ok(Value::Object(*c));
            }
        }
        // The receiver stays the proxy, so accessors see it as `this`.
        match self.window() {
            Some(w) => internal::get(agent, w, key, receiver),
            None => Ok(Value::Undefined),
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
        if array_index(key).is_some() {
            return Ok(false);
        }
        let receiver = if receiver == Value::Object(o) {
            match self.window() {
                Some(w) => Value::Object(w),
                None => return Ok(false),
            }
        } else {
            receiver
        };
        match self.window() {
            Some(w) => internal::set(agent, w, key, value, receiver),
            None => Ok(false),
        }
    }

    fn own_property_keys(&self, agent: &mut Agent, _o: Object) -> JsResult<Vec<PropertyKey>> {
        let mut keys: Vec<PropertyKey> = (0..self.children().len() as u32)
            .map(PropertyKey::Index)
            .collect();
        if let Some(w) = self.window() {
            keys.extend(internal::own_property_keys(agent, w)?);
        }
        Ok(keys)
    }

    fn has_ordinary_get_prototype_of(&self) -> bool {
        false
    }
}

impl HostObject for WindowProxy {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// A new `WindowProxy` object for context `id`.
pub fn create_proxy(agent: &mut Agent, id: ContextId) -> Object {
    agent
        .heap
        .create_object(None, ObjectKind::Host(Rc::new(WindowProxy { context: id })))
}

/// The context of a `WindowProxy` object.
pub fn proxy_context(agent: &Agent, o: Object) -> Option<ContextId> {
    match &agent.heap.object(o).kind {
        ObjectKind::Host(h) => h.as_any().downcast_ref::<WindowProxy>().map(|p| p.context),
        _ => None,
    }
}

/// The active `Window` of the `WindowProxy` `o` (the global object), if
/// `o` is one.
pub fn proxy_target(agent: &Agent, o: Object) -> Option<Object> {
    let ctx = proxy_context(agent, o)?;
    let ua = super::ua()?;
    let w = ua.context(ctx)?.active_window()?;
    w.state.global.get()
}

/// The `WindowProxy` of a window state (the global itself without a
/// browsing context).
pub fn proxy_of(state: &RealmState) -> Value {
    state
        .html
        .proxy
        .get()
        .or(state.global.get())
        .map_or(Value::Null, Value::Object)
}
