//! The embedder side of the DOM algorithms that run script or depend on
//! things outside the document: listener, observer and filter callbacks,
//! exception reporting, the `Window` in event paths, activation behavior
//! and timers. The bindings of k3-09 and the event loop of k3-10 implement
//! [`DomHost`]; [`NullHost`] is a host without script.
//!
//! Every method is called while the document is *not* borrowed, except
//! [`DomHost::has_activation_behavior`], which receives it.

use super::abort::SignalId;
use super::events::{Event, EventRef, ListenerRef, Target};
use super::observer::{MutationRecord, ObserverId};
use super::traversal::filter::FILTER_ACCEPT;
use super::{Document, DomException, HostRef, NodeId};

pub trait DomHost {
    /// Calls an event listener's callback ("call a user object's
    /// operation" with `handleEvent`, `this` being `current_target`). The
    /// host also sets and restores the `Window`'s current event unless
    /// `in_shadow_tree`. An exception comes back as `Err`.
    fn call_listener(
        &mut self,
        callback: HostRef,
        current_target: Target,
        event: &EventRef,
        in_shadow_tree: bool,
    ) -> Result<(), HostRef>;

    /// "Report the exception".
    fn report_exception(&mut self, exception: HostRef);

    /// Invokes a mutation observer's callback with its records.
    fn call_mutation_observer(
        &mut self,
        observer: ObserverId,
        callback: HostRef,
        records: Vec<MutationRecord>,
    ) -> Result<(), HostRef>;

    /// Calls a `NodeFilter`'s `acceptNode`.
    fn call_node_filter(&mut self, filter: HostRef, node: NodeId) -> Result<u16, HostRef>;

    /// A new `DOMException` value (abort reasons).
    fn create_exception(&mut self, exception: DomException) -> HostRef;

    /// The current high resolution time, for `Event.timeStamp`.
    fn now(&mut self) -> f64 {
        0.0
    }

    /// The parent of the document in an event path: its `Window`, if the
    /// document has a browsing context ("get the parent" of `Document`;
    /// the `load` exception is handled by the caller).
    fn document_parent(&mut self, _event: &Event) -> Option<Target> {
        None
    }

    /// "Get the parent" of a target that is not a node.
    fn other_parent(&mut self, _target: Target, _event: &Event) -> Option<Target> {
        None
    }

    /// The event listener list of a target that is neither a node nor an
    /// abort signal (`Window`, `XMLHttpRequest`, …).
    fn other_listeners(&mut self, _target: Target) -> Option<&mut Vec<ListenerRef>> {
        None
    }

    /// True if `target` has activation behavior (HTML elements such as
    /// `a` or `input`).
    fn has_activation_behavior(&mut self, _doc: &Document, _target: Target) -> bool {
        false
    }

    /// The legacy-pre-activation behavior of `target`, if any.
    fn legacy_pre_activation(&mut self, _target: Target, _event: &EventRef) {}

    /// The legacy-canceled-activation behavior of `target`, if any.
    fn legacy_canceled_activation(&mut self, _target: Target, _event: &EventRef) {}

    /// The activation behavior of `target`.
    fn activation_behavior(&mut self, _target: Target, _event: &EventRef) {}

    /// Starts the timer of `AbortSignal.timeout(ms)`; when it fires the
    /// host calls [`super::abort::timeout_expired`] from a task.
    fn start_abort_timeout(&mut self, _signal: SignalId, _milliseconds: u64) {}

    /// Runs an abort algorithm the host added to a signal.
    fn run_abort_algorithm(&mut self, _signal: SignalId, _algorithm: u64) {}
}

/// A host without script: callbacks do nothing, filters accept and
/// exceptions are counted.
#[derive(Debug, Default)]
pub struct NullHost {
    pub reported: Vec<HostRef>,
    next_exception: u64,
}

impl DomHost for NullHost {
    fn call_listener(
        &mut self,
        _: HostRef,
        _: Target,
        _: &EventRef,
        _: bool,
    ) -> Result<(), HostRef> {
        Ok(())
    }

    fn report_exception(&mut self, exception: HostRef) {
        self.reported.push(exception);
    }

    fn call_mutation_observer(
        &mut self,
        _: ObserverId,
        _: HostRef,
        _: Vec<MutationRecord>,
    ) -> Result<(), HostRef> {
        Ok(())
    }

    fn call_node_filter(&mut self, _: HostRef, _: NodeId) -> Result<u16, HostRef> {
        Ok(FILTER_ACCEPT)
    }

    fn create_exception(&mut self, _: DomException) -> HostRef {
        self.next_exception += 1;
        HostRef(u64::MAX - self.next_exception)
    }
}
