//! Events (DOM §2): `Event`, `CustomEvent`, `EventTarget` with its listener
//! options, and the dispatch algorithm with event paths through shadow
//! trees, retargeting and activation behavior.
//!
//! An event is shared with its wrapper as an [`EventRef`]. Node listeners
//! live in the document, abort signal listeners in the signal, and the
//! listeners of other targets (`Window`, …) with the host
//! ([`DomHost::other_listeners`]).

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use super::abort::{AbortAlgorithm, SignalId};
use super::host::DomHost;
use super::shadow::ShadowRootMode;
use super::{DocRef, Document, DomException, HostRef, NodeId};

/// An `EventTarget`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Target {
    Node(NodeId),
    Signal(SignalId),
    Window,
    /// Another host object, identified by the host.
    Other(u64),
}

impl Target {
    pub fn node(self) -> Option<NodeId> {
        match self {
            Target::Node(n) => Some(n),
            _ => None,
        }
    }
}

/// An event listener (DOM §2.7).
#[derive(Debug)]
pub struct Listener {
    pub type_: String,
    pub callback: HostRef,
    pub capture: bool,
    pub passive: bool,
    pub once: bool,
    pub signal: Option<SignalId>,
    pub removed: Cell<bool>,
}

/// A listener shared by the list and the dispatches that cloned the list.
pub type ListenerRef = Rc<Listener>;

/// `AddEventListenerOptions` after flattening; `passive` is `None` when not
/// given.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ListenerOptions {
    pub capture: bool,
    pub passive: Option<bool>,
    pub once: bool,
    pub signal: Option<SignalId>,
}

/// `eventPhase` values.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Phase {
    #[default]
    None = 0,
    Capturing = 1,
    AtTarget = 2,
    Bubbling = 3,
}

/// The interface an event object implements, as far as the DOM needs to
/// know it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EventInterface {
    #[default]
    Event,
    CustomEvent,
    UiEvent,
    MouseEvent,
    /// Any other interface, named.
    Other(&'static str),
}

/// `EventInit`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EventInit {
    pub bubbles: bool,
    pub cancelable: bool,
    pub composed: bool,
}

/// One struct of an event's path (DOM §2.2).
#[derive(Clone, Debug, PartialEq)]
pub struct PathEntry {
    pub invocation_target: Target,
    pub invocation_target_in_shadow_tree: bool,
    pub shadow_adjusted_target: Option<Target>,
    pub related_target: Option<Target>,
    pub touch_targets: Vec<Target>,
    pub root_of_closed_tree: bool,
    pub slot_in_closed_tree: bool,
}

/// An `Event`.
#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    pub type_: String,
    pub interface: EventInterface,
    pub target: Option<Target>,
    pub related_target: Option<Target>,
    pub touch_targets: Vec<Target>,
    pub current_target: Option<Target>,
    pub phase: Phase,
    pub path: Vec<PathEntry>,
    pub bubbles: bool,
    pub cancelable: bool,
    pub composed: bool,
    pub is_trusted: bool,
    pub time_stamp: f64,
    /// `CustomEvent.detail`.
    pub detail: Option<HostRef>,
    stop_propagation: bool,
    stop_immediate_propagation: bool,
    canceled: bool,
    in_passive_listener: bool,
    initialized: bool,
    dispatching: bool,
}

/// An event shared with its wrapper.
pub type EventRef = Rc<RefCell<Event>>;

impl Event {
    /// The constructor `new Event(type, init)` ("inner event creation
    /// steps"): initialized, not trusted.
    pub fn new(type_: &str, init: EventInit, time_stamp: f64) -> Event {
        Event {
            type_: type_.to_string(),
            interface: EventInterface::Event,
            target: None,
            related_target: None,
            touch_targets: Vec::new(),
            current_target: None,
            phase: Phase::None,
            path: Vec::new(),
            bubbles: init.bubbles,
            cancelable: init.cancelable,
            composed: init.composed,
            is_trusted: false,
            time_stamp,
            detail: None,
            stop_propagation: false,
            stop_immediate_propagation: false,
            canceled: false,
            in_passive_listener: false,
            initialized: true,
            dispatching: false,
        }
    }

    /// `new CustomEvent(type, init)`.
    pub fn new_custom(
        type_: &str,
        init: EventInit,
        detail: Option<HostRef>,
        time_stamp: f64,
    ) -> Event {
        let mut e = Event::new(type_, init, time_stamp);
        e.interface = EventInterface::CustomEvent;
        e.detail = detail;
        e
    }

    /// `document.createEvent(interface)`: not initialized until
    /// `initEvent`.
    pub fn new_uninitialized(interface: EventInterface, time_stamp: f64) -> Event {
        let mut e = Event::new("", EventInit::default(), time_stamp);
        e.interface = interface;
        e.initialized = false;
        e
    }

    /// A trusted event as user agents create it before firing.
    pub fn new_trusted(type_: &str, init: EventInit, time_stamp: f64) -> Event {
        let mut e = Event::new(type_, init, time_stamp);
        e.is_trusted = true;
        e
    }

    pub fn into_ref(self) -> EventRef {
        Rc::new(RefCell::new(self))
    }

    /// `srcElement`: the target.
    pub fn src_element(&self) -> Option<Target> {
        self.target
    }

    /// `stopPropagation()`.
    pub fn stop_propagation(&mut self) {
        self.stop_propagation = true;
    }

    /// `stopImmediatePropagation()`.
    pub fn stop_immediate_propagation(&mut self) {
        self.stop_propagation = true;
        self.stop_immediate_propagation = true;
    }

    /// `cancelBubble` getter.
    pub fn cancel_bubble(&self) -> bool {
        self.stop_propagation
    }

    /// `cancelBubble` setter: only `true` has an effect.
    pub fn set_cancel_bubble(&mut self, value: bool) {
        if value {
            self.stop_propagation = true;
        }
    }

    /// "Set the canceled flag".
    fn set_canceled(&mut self) {
        if self.cancelable && !self.in_passive_listener {
            self.canceled = true;
        }
    }

    /// `preventDefault()`.
    pub fn prevent_default(&mut self) {
        self.set_canceled();
    }

    /// `returnValue` getter.
    pub fn return_value(&self) -> bool {
        !self.canceled
    }

    /// `returnValue` setter: `false` cancels.
    pub fn set_return_value(&mut self, value: bool) {
        if !value {
            self.set_canceled();
        }
    }

    /// `defaultPrevented`.
    pub fn default_prevented(&self) -> bool {
        self.canceled
    }

    /// The dispatch flag.
    pub fn is_dispatching(&self) -> bool {
        self.dispatching
    }

    /// The initialized flag.
    pub fn is_initialized(&self) -> bool {
        self.initialized
    }

    /// `initEvent(type, bubbles, cancelable)`.
    pub fn init_event(&mut self, type_: &str, bubbles: bool, cancelable: bool) {
        if self.dispatching {
            return;
        }
        self.initialized = true;
        self.stop_propagation = false;
        self.stop_immediate_propagation = false;
        self.canceled = false;
        self.is_trusted = false;
        self.target = None;
        self.type_ = type_.to_string();
        self.bubbles = bubbles;
        self.cancelable = cancelable;
    }

    /// `initCustomEvent(type, bubbles, cancelable, detail)`.
    pub fn init_custom_event(
        &mut self,
        type_: &str,
        bubbles: bool,
        cancelable: bool,
        detail: Option<HostRef>,
    ) {
        if self.dispatching {
            return;
        }
        self.init_event(type_, bubbles, cancelable);
        self.detail = detail;
    }

    /// `composedPath()` (DOM §2.2).
    pub fn composed_path(&self) -> Vec<Target> {
        let path = &self.path;
        let Some(current_target) = self.current_target else {
            return Vec::new();
        };
        if path.is_empty() {
            return Vec::new();
        }
        let mut composed = vec![current_target];
        let mut current_target_index = 0usize;
        let mut current_target_hidden_level = 0i64;
        let mut index = path.len() as i64 - 1;
        while index >= 0 {
            let entry = &path[index as usize];
            if entry.root_of_closed_tree {
                current_target_hidden_level += 1;
            }
            if entry.invocation_target == current_target {
                current_target_index = index as usize;
                break;
            }
            if entry.slot_in_closed_tree {
                current_target_hidden_level -= 1;
            }
            index -= 1;
        }
        let mut current_hidden = current_target_hidden_level;
        let mut max_hidden = current_target_hidden_level;
        let mut index = current_target_index as i64 - 1;
        while index >= 0 {
            let entry = &path[index as usize];
            if entry.root_of_closed_tree {
                current_hidden += 1;
            }
            if current_hidden <= max_hidden {
                composed.insert(0, entry.invocation_target);
            }
            if entry.slot_in_closed_tree {
                current_hidden -= 1;
                if current_hidden < max_hidden {
                    max_hidden = current_hidden;
                }
            }
            index -= 1;
        }
        current_hidden = current_target_hidden_level;
        max_hidden = current_target_hidden_level;
        let mut index = current_target_index + 1;
        while index < path.len() {
            let entry = &path[index];
            if entry.slot_in_closed_tree {
                current_hidden += 1;
            }
            if current_hidden <= max_hidden {
                composed.push(entry.invocation_target);
            }
            if entry.root_of_closed_tree {
                current_hidden -= 1;
                if current_hidden < max_hidden {
                    max_hidden = current_hidden;
                }
            }
            index += 1;
        }
        composed
    }
}

// ----- listener lists -------------------------------------------------------

/// Runs `f` on the event listener list of `target`.
fn with_listener_list<R>(
    doc: &mut Document,
    host: &mut dyn DomHost,
    target: Target,
    f: impl FnOnce(&mut Vec<ListenerRef>) -> R,
) -> Option<R> {
    match target {
        Target::Node(n) => Some(f(doc.listeners.entry(n).or_default())),
        Target::Signal(s) => doc.signals.get_mut(s).map(|data| f(&mut data.listeners)),
        Target::Window | Target::Other(_) => host.other_listeners(target).map(f),
    }
}

/// A clone of `target`'s event listener list.
fn listeners_snapshot(doc: &DocRef, host: &mut dyn DomHost, target: Target) -> Vec<ListenerRef> {
    match target {
        Target::Node(n) => doc.borrow().listeners.get(&n).cloned().unwrap_or_default(),
        Target::Signal(s) => doc
            .borrow()
            .signals
            .get(s)
            .map(|d| d.listeners.clone())
            .unwrap_or_default(),
        Target::Window | Target::Other(_) => host
            .other_listeners(target)
            .map(|l| l.clone())
            .unwrap_or_default(),
    }
}

/// The listeners of a node, for inspection.
pub fn node_listeners(doc: &Document, node: NodeId) -> Vec<ListenerRef> {
    doc.listeners.get(&node).cloned().unwrap_or_default()
}

/// The default passive value (DOM §2.7).
fn default_passive_value(doc: &Document, type_: &str, target: Target) -> bool {
    if !matches!(type_, "touchstart" | "touchmove" | "wheel" | "mousewheel") {
        return false;
    }
    match target {
        Target::Window => true,
        Target::Node(n) => {
            n == NodeId::DOCUMENT || Some(n) == doc.document_element() || Some(n) == doc.body()
        }
        _ => false,
    }
}

/// `addEventListener(type, callback, options)` ("add an event listener").
pub fn add_event_listener(
    doc: &mut Document,
    host: &mut dyn DomHost,
    target: Target,
    type_: &str,
    callback: Option<HostRef>,
    options: ListenerOptions,
) {
    if let Some(signal) = options.signal {
        if doc.is_aborted(signal) {
            return;
        }
    }
    let Some(callback) = callback else {
        return;
    };
    let passive = options
        .passive
        .unwrap_or_else(|| default_passive_value(doc, type_, target));
    let listener = Rc::new(Listener {
        type_: type_.to_string(),
        callback,
        capture: options.capture,
        passive,
        once: options.once,
        signal: options.signal,
        removed: Cell::new(false),
    });
    let added = with_listener_list(doc, host, target, |list| {
        let duplicate = list
            .iter()
            .any(|l| l.type_ == type_ && l.callback == callback && l.capture == options.capture);
        if duplicate {
            false
        } else {
            list.push(Rc::clone(&listener));
            true
        }
    });
    if added == Some(true) {
        if let Some(signal) = options.signal {
            doc.add_abort_algorithm(signal, AbortAlgorithm::RemoveListener { target, listener });
        }
    }
}

/// "Remove an event listener".
pub fn remove_listener(
    doc: &mut Document,
    host: &mut dyn DomHost,
    target: Target,
    listener: &ListenerRef,
) {
    listener.removed.set(true);
    with_listener_list(doc, host, target, |list| {
        list.retain(|l| !Rc::ptr_eq(l, listener))
    });
}

/// `removeEventListener(type, callback, options)`.
pub fn remove_event_listener(
    doc: &mut Document,
    host: &mut dyn DomHost,
    target: Target,
    type_: &str,
    callback: Option<HostRef>,
    capture: bool,
) {
    let Some(callback) = callback else {
        return;
    };
    let found = with_listener_list(doc, host, target, |list| {
        list.iter()
            .find(|l| l.type_ == type_ && l.callback == callback && l.capture == capture)
            .cloned()
    })
    .flatten();
    if let Some(listener) = found {
        remove_listener(doc, host, target, &listener);
    }
}

/// "Remove all event listeners" of `target`.
pub fn remove_all_listeners(doc: &mut Document, host: &mut dyn DomHost, target: Target) {
    let all = with_listener_list(doc, host, target, std::mem::take).unwrap_or_default();
    for l in all {
        l.removed.set(true);
    }
}

// ----- dispatch (DOM §2.9) --------------------------------------------------

/// "Retarget" `a` against `b`.
fn retarget(doc: &Document, a: Target, b: Target) -> Target {
    let Target::Node(mut an) = a else {
        return a;
    };
    loop {
        let root = doc.root(an);
        let Some(host) = doc.shadow_root_host(root) else {
            return Target::Node(an);
        };
        if let Target::Node(bn) = b {
            if doc.is_shadow_including_inclusive_ancestor(root, bn) {
                return Target::Node(an);
            }
        }
        an = host;
    }
}

/// True if `t` is a node whose root is a shadow root.
fn in_shadow_tree(doc: &Document, t: Target) -> bool {
    t.node().is_some_and(|n| doc.is_shadow_root(doc.root(n)))
}

/// "Get the parent" of `target` for `event`.
fn get_the_parent(
    doc: &Document,
    host: &mut dyn DomHost,
    target: Target,
    event: &Event,
) -> Option<Target> {
    match target {
        Target::Node(n) if n == NodeId::DOCUMENT => {
            if event.type_ == "load" {
                None
            } else {
                host.document_parent(event)
            }
        }
        Target::Node(n) if doc.is_shadow_root(n) => {
            let first_root = event
                .path
                .first()
                .and_then(|p| p.invocation_target.node())
                .map(|t| doc.root(t));
            if !event.composed && first_root == Some(n) {
                None
            } else {
                doc.shadow_root_host(n).map(Target::Node)
            }
        }
        Target::Node(n) => match doc.assigned_slot_internal(n) {
            Some(slot) => Some(Target::Node(slot)),
            None => doc.parent(n).map(Target::Node),
        },
        Target::Signal(_) => None,
        Target::Window | Target::Other(_) => host.other_parent(target, event),
    }
}

/// "Append to an event path".
fn append_to_path(
    doc: &Document,
    event: &mut Event,
    invocation_target: Target,
    shadow_adjusted_target: Option<Target>,
    related_target: Option<Target>,
    touch_targets: Vec<Target>,
    slot_in_closed_tree: bool,
) {
    let invocation_target_in_shadow_tree = in_shadow_tree(doc, invocation_target);
    let root_of_closed_tree = invocation_target.node().is_some_and(|n| {
        doc.shadow_root_data(n)
            .is_some_and(|d| d.mode == ShadowRootMode::Closed)
    });
    event.path.push(PathEntry {
        invocation_target,
        invocation_target_in_shadow_tree,
        shadow_adjusted_target,
        related_target,
        touch_targets,
        root_of_closed_tree,
        slot_in_closed_tree,
    });
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum InvokePhase {
    Capturing,
    Bubbling,
}

/// `dispatchEvent(event)`: fails if the event is being dispatched or was
/// not initialized; otherwise marks it untrusted and dispatches it.
pub fn dispatch_event(
    doc: &DocRef,
    host: &mut dyn DomHost,
    target: Target,
    event: &EventRef,
) -> Result<bool, DomException> {
    {
        let mut e = event.borrow_mut();
        if e.dispatching || !e.initialized {
            return Err(DomException::InvalidStateError);
        }
        e.is_trusted = false;
    }
    Ok(dispatch(doc, host, event, target, false))
}

/// "Dispatch" `event` to `target`. Returns false if the event was
/// canceled.
pub fn dispatch(
    doc: &DocRef,
    host: &mut dyn DomHost,
    event: &EventRef,
    target: Target,
    legacy_target_override: bool,
) -> bool {
    event.borrow_mut().dispatching = true;
    let target_override = if legacy_target_override {
        Target::Node(NodeId::DOCUMENT)
    } else {
        target
    };
    let mut activation_target: Option<Target> = None;
    let mut clear_targets = false;
    let original_related = event.borrow().related_target;
    {
        let d = doc.borrow();
        let mut e = event.borrow_mut();
        let related_target = original_related.map(|r| retarget(&d, r, target));
        if Some(target) != related_target || Some(target) == original_related {
            let touch_targets: Vec<Target> = e
                .touch_targets
                .iter()
                .map(|&t| retarget(&d, t, target))
                .collect();
            append_to_path(
                &d,
                &mut e,
                target,
                Some(target_override),
                related_target,
                touch_targets,
                false,
            );
            let is_activation_event =
                e.interface == EventInterface::MouseEvent && e.type_ == "click";
            if is_activation_event && host.has_activation_behavior(&d, target) {
                activation_target = Some(target);
            }
            let mut slottable = target.node().filter(|&n| d.is_assigned(n));
            let mut slot_in_closed_tree = false;
            let mut target = target;
            let mut parent = get_the_parent(&d, host, target, &e);
            while let Some(p) = parent {
                if slottable.is_some() {
                    slottable = None;
                    if let Some(pn) = p.node() {
                        let root = d.root(pn);
                        if d.shadow_root_data(root)
                            .is_some_and(|s| s.mode == ShadowRootMode::Closed)
                        {
                            slot_in_closed_tree = true;
                        }
                    }
                }
                if let Some(pn) = p.node() {
                    if d.is_assigned(pn) {
                        slottable = Some(pn);
                    }
                }
                let related_target = original_related.map(|r| retarget(&d, r, p));
                let touch_targets: Vec<Target> = e
                    .touch_targets
                    .iter()
                    .map(|&t| retarget(&d, t, p))
                    .collect();
                let root_includes_parent = match (target, p) {
                    (Target::Node(t), Target::Node(pn)) => {
                        d.is_shadow_including_inclusive_ancestor(d.root(t), pn)
                    }
                    _ => false,
                };
                let mut keep_going = true;
                if p == Target::Window || root_includes_parent {
                    if is_activation_event
                        && e.bubbles
                        && activation_target.is_none()
                        && host.has_activation_behavior(&d, p)
                    {
                        activation_target = Some(p);
                    }
                    append_to_path(
                        &d,
                        &mut e,
                        p,
                        None,
                        related_target,
                        touch_targets,
                        slot_in_closed_tree,
                    );
                } else if Some(p) == related_target {
                    keep_going = false;
                } else {
                    target = p;
                    if is_activation_event
                        && activation_target.is_none()
                        && host.has_activation_behavior(&d, target)
                    {
                        activation_target = Some(target);
                    }
                    append_to_path(
                        &d,
                        &mut e,
                        p,
                        Some(p),
                        related_target,
                        touch_targets,
                        slot_in_closed_tree,
                    );
                }
                parent = if keep_going {
                    get_the_parent(&d, host, p, &e)
                } else {
                    None
                };
                slot_in_closed_tree = false;
            }
            let clear_struct = e
                .path
                .iter()
                .rev()
                .find(|s| s.shadow_adjusted_target.is_some());
            if let Some(s) = clear_struct {
                let shadowed = |t: &Target| in_shadow_tree(&d, *t);
                if s.shadow_adjusted_target.iter().any(shadowed)
                    || s.related_target.iter().any(shadowed)
                    || s.touch_targets.iter().any(shadowed)
                {
                    clear_targets = true;
                }
            }
        }
    }
    let has_path = !event.borrow().path.is_empty();
    if has_path {
        if let Some(t) = activation_target {
            host.legacy_pre_activation(t, event);
        }
        let len = event.borrow().path.len();
        for i in (0..len).rev() {
            {
                let mut e = event.borrow_mut();
                e.phase = if e.path[i].shadow_adjusted_target.is_some() {
                    Phase::AtTarget
                } else {
                    Phase::Capturing
                };
            }
            invoke(doc, host, event, i, InvokePhase::Capturing);
        }
        for i in 0..len {
            {
                let mut e = event.borrow_mut();
                if e.path[i].shadow_adjusted_target.is_some() {
                    e.phase = Phase::AtTarget;
                } else {
                    if !e.bubbles {
                        continue;
                    }
                    e.phase = Phase::Bubbling;
                }
            }
            invoke(doc, host, event, i, InvokePhase::Bubbling);
        }
    }
    let canceled = {
        let mut e = event.borrow_mut();
        e.phase = Phase::None;
        e.current_target = None;
        e.path.clear();
        e.dispatching = false;
        e.stop_propagation = false;
        e.stop_immediate_propagation = false;
        if clear_targets {
            e.target = None;
            e.related_target = None;
            e.touch_targets.clear();
        }
        e.canceled
    };
    if let Some(t) = activation_target {
        if !canceled {
            host.activation_behavior(t, event);
        } else {
            host.legacy_canceled_activation(t, event);
        }
    }
    !canceled
}

/// "Invoke" the listeners of path entry `index`.
fn invoke(
    doc: &DocRef,
    host: &mut dyn DomHost,
    event: &EventRef,
    index: usize,
    phase: InvokePhase,
) {
    let (current, in_shadow) = {
        let mut e = event.borrow_mut();
        let target = e.path[..=index]
            .iter()
            .rev()
            .find_map(|s| s.shadow_adjusted_target);
        e.target = target;
        e.related_target = e.path[index].related_target;
        e.touch_targets = e.path[index].touch_targets.clone();
        if e.stop_propagation {
            return;
        }
        let entry = &e.path[index];
        let current = entry.invocation_target;
        let in_shadow = entry.invocation_target_in_shadow_tree;
        e.current_target = Some(current);
        (current, in_shadow)
    };
    let listeners = listeners_snapshot(doc, host, current);
    let found = inner_invoke(doc, host, event, &listeners, phase, current, in_shadow);
    let trusted = event.borrow().is_trusted;
    if found || !trusted {
        return;
    }
    let original = event.borrow().type_.clone();
    let legacy = match original.as_str() {
        "animationend" => "webkitAnimationEnd",
        "animationiteration" => "webkitAnimationIteration",
        "animationstart" => "webkitAnimationStart",
        "transitionend" => "webkitTransitionEnd",
        _ => return,
    };
    event.borrow_mut().type_ = legacy.to_string();
    inner_invoke(doc, host, event, &listeners, phase, current, in_shadow);
    event.borrow_mut().type_ = original;
}

/// "Inner invoke": calls the matching listeners; true if any listener of
/// the event's type was found.
fn inner_invoke(
    doc: &DocRef,
    host: &mut dyn DomHost,
    event: &EventRef,
    listeners: &[ListenerRef],
    phase: InvokePhase,
    current: Target,
    in_shadow: bool,
) -> bool {
    let mut found = false;
    for listener in listeners {
        if listener.removed.get() {
            continue;
        }
        if event.borrow().type_ != listener.type_ {
            continue;
        }
        found = true;
        if (phase == InvokePhase::Capturing && !listener.capture)
            || (phase == InvokePhase::Bubbling && listener.capture)
        {
            continue;
        }
        if listener.once {
            remove_listener(&mut doc.borrow_mut(), host, current, listener);
        }
        if listener.passive {
            event.borrow_mut().in_passive_listener = true;
        }
        if let Err(exception) = host.call_listener(listener.callback, current, event, in_shadow) {
            host.report_exception(exception);
        }
        let mut e = event.borrow_mut();
        e.in_passive_listener = false;
        if e.stop_immediate_propagation {
            break;
        }
    }
    found
}

/// "Fire an event" named `type_` at `target`: a trusted `Event` with
/// `init`. Returns false if it was canceled.
pub fn fire_event(
    doc: &DocRef,
    host: &mut dyn DomHost,
    target: Target,
    type_: &str,
    init: EventInit,
) -> bool {
    let event = Event::new_trusted(type_, init, host.now()).into_ref();
    dispatch(doc, host, &event, target, false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dom::host::NullHost;

    #[test]
    fn listeners_are_deduplicated_and_removed() {
        let mut doc = Document::new_html();
        let mut host = NullHost::default();
        let div = doc.create_html_element("div");
        let t = Target::Node(div);
        let opts = ListenerOptions::default();
        add_event_listener(&mut doc, &mut host, t, "x", Some(HostRef(1)), opts);
        add_event_listener(&mut doc, &mut host, t, "x", Some(HostRef(1)), opts);
        let capture = ListenerOptions {
            capture: true,
            ..opts
        };
        add_event_listener(&mut doc, &mut host, t, "x", Some(HostRef(1)), capture);
        assert_eq!(node_listeners(&doc, div).len(), 2);
        remove_event_listener(&mut doc, &mut host, t, "x", Some(HostRef(1)), true);
        let left = node_listeners(&doc, div);
        assert_eq!(left.len(), 1);
        assert!(!left[0].capture);
    }

    #[test]
    fn dispatch_rejects_uninitialized_events() {
        let doc: DocRef = Rc::new(RefCell::new(Document::new_html()));
        let mut host = NullHost::default();
        let e = Event::new_uninitialized(EventInterface::Event, 0.0).into_ref();
        assert_eq!(
            dispatch_event(&doc, &mut host, Target::Node(NodeId::DOCUMENT), &e),
            Err(DomException::InvalidStateError)
        );
        e.borrow_mut().init_event("x", true, true);
        assert_eq!(
            dispatch_event(&doc, &mut host, Target::Node(NodeId::DOCUMENT), &e),
            Ok(true)
        );
        assert!(!e.borrow().is_trusted);
        assert_eq!(e.borrow().phase, Phase::None);
    }

    #[test]
    fn passive_listeners_cannot_cancel() {
        let mut e = Event::new(
            "wheel",
            EventInit {
                cancelable: true,
                ..EventInit::default()
            },
            0.0,
        );
        e.in_passive_listener = true;
        e.prevent_default();
        assert!(!e.default_prevented());
        e.in_passive_listener = false;
        e.set_return_value(false);
        assert!(e.default_prevented());
    }
}
