//! Aborting ongoing activities (DOM §3): `AbortController` and
//! `AbortSignal` with `abort`, `any` and `timeout`.
//!
//! Signals live in their realm's document by [`SignalId`]. An abort
//! controller is just its signal. Reasons are host values; the default
//! reasons (`AbortError`, `TimeoutError`) come from
//! [`DomHost::create_exception`]. `AbortSignal.timeout` asks the host to
//! start a timer ([`DomHost::start_abort_timeout`]); the host calls
//! [`timeout_expired`] from the timer task.

use super::events::{self, EventInit, ListenerRef, Target};
use super::host::DomHost;
use super::{DocRef, Document, DomException, HostRef};

/// An abort signal of a document's realm.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SignalId(pub u32);

/// An abort algorithm.
#[derive(Clone, Debug)]
pub enum AbortAlgorithm {
    /// Removes an event listener added with this signal.
    RemoveListener {
        target: Target,
        listener: ListenerRef,
    },
    /// An algorithm of the host (fetch, streams, …), by host id.
    Host(u64),
}

/// The state of an `AbortSignal`.
#[derive(Clone, Debug, Default)]
pub struct SignalData {
    /// The abort reason; the signal is aborted when it is set.
    pub reason: Option<HostRef>,
    pub(crate) algorithms: Vec<AbortAlgorithm>,
    pub(crate) source_signals: Vec<SignalId>,
    pub(crate) dependent_signals: Vec<SignalId>,
    pub dependent: bool,
    pub(crate) listeners: Vec<ListenerRef>,
}

/// The abort signals of a realm.
#[derive(Clone, Debug, Default)]
pub struct SignalStore {
    signals: Vec<SignalData>,
}

impl SignalStore {
    pub fn get(&self, id: SignalId) -> Option<&SignalData> {
        self.signals.get(id.0 as usize)
    }

    pub fn get_mut(&mut self, id: SignalId) -> Option<&mut SignalData> {
        self.signals.get_mut(id.0 as usize)
    }
}

impl Document {
    /// A new, not aborted `AbortSignal` (also `new AbortController()`'s
    /// signal).
    pub fn create_abort_signal(&mut self) -> SignalId {
        self.signals.signals.push(SignalData::default());
        SignalId(u32::try_from(self.signals.signals.len() - 1).expect("too many signals"))
    }

    /// The state of a signal.
    pub fn signal(&self, id: SignalId) -> &SignalData {
        &self.signals.signals[id.0 as usize]
    }

    /// `aborted`.
    pub fn is_aborted(&self, id: SignalId) -> bool {
        self.signals.get(id).is_some_and(|s| s.reason.is_some())
    }

    /// `reason`: `None` stands for undefined.
    pub fn abort_reason(&self, id: SignalId) -> Option<HostRef> {
        self.signals.get(id).and_then(|s| s.reason)
    }

    /// `throwIfAborted()`: the reason to throw, if aborted.
    pub fn throw_if_aborted(&self, id: SignalId) -> Result<(), HostRef> {
        match self.abort_reason(id) {
            Some(reason) => Err(reason),
            None => Ok(()),
        }
    }

    /// "Add" an algorithm to a signal; nothing happens if it is aborted.
    pub fn add_abort_algorithm(&mut self, id: SignalId, algorithm: AbortAlgorithm) {
        if self.is_aborted(id) {
            return;
        }
        if let Some(s) = self.signals.get_mut(id) {
            s.algorithms.push(algorithm);
        }
    }

    /// "Remove" a host algorithm from a signal.
    pub fn remove_abort_algorithm(&mut self, id: SignalId, algorithm: u64) {
        if let Some(s) = self.signals.get_mut(id) {
            s.algorithms
                .retain(|a| !matches!(a, AbortAlgorithm::Host(h) if *h == algorithm));
        }
    }

    /// `AbortSignal.any(signals)` ("create a dependent abort signal").
    pub fn abort_signal_any(&mut self, signals: &[SignalId]) -> SignalId {
        let result = self.create_abort_signal();
        for &s in signals {
            if let Some(reason) = self.abort_reason(s) {
                self.signals.signals[result.0 as usize].reason = Some(reason);
                return result;
            }
        }
        self.signals.signals[result.0 as usize].dependent = true;
        for &s in signals {
            let data = &self.signals.signals[s.0 as usize];
            let sources: Vec<SignalId> = if data.dependent {
                data.source_signals.clone()
            } else {
                vec![s]
            };
            for source in sources {
                let r = &mut self.signals.signals[result.0 as usize];
                if !r.source_signals.contains(&source) {
                    r.source_signals.push(source);
                }
                let src = &mut self.signals.signals[source.0 as usize];
                if !src.dependent_signals.contains(&result) {
                    src.dependent_signals.push(result);
                }
            }
        }
        result
    }
}

/// `AbortSignal.abort(reason)`: a signal already aborted with `reason`, or
/// with a new `AbortError`.
pub fn abort_signal_abort(
    doc: &DocRef,
    host: &mut dyn DomHost,
    reason: Option<HostRef>,
) -> SignalId {
    let reason = reason.unwrap_or_else(|| host.create_exception(DomException::AbortError));
    let mut d = doc.borrow_mut();
    let id = d.create_abort_signal();
    d.signals.signals[id.0 as usize].reason = Some(reason);
    id
}

/// `AbortSignal.timeout(milliseconds)`.
pub fn abort_signal_timeout(doc: &DocRef, host: &mut dyn DomHost, milliseconds: u64) -> SignalId {
    let id = doc.borrow_mut().create_abort_signal();
    host.start_abort_timeout(id, milliseconds);
    id
}

/// The timer task of `AbortSignal.timeout`: aborts with a `TimeoutError`.
pub fn timeout_expired(doc: &DocRef, host: &mut dyn DomHost, signal: SignalId) {
    let reason = host.create_exception(DomException::TimeoutError);
    signal_abort(doc, host, signal, Some(reason));
}

/// `AbortController.abort(reason)` and "signal abort".
pub fn signal_abort(
    doc: &DocRef,
    host: &mut dyn DomHost,
    signal: SignalId,
    reason: Option<HostRef>,
) {
    if doc.borrow().is_aborted(signal) {
        return;
    }
    let reason = reason.unwrap_or_else(|| host.create_exception(DomException::AbortError));
    let to_abort = {
        let mut d = doc.borrow_mut();
        d.signals.signals[signal.0 as usize].reason = Some(reason);
        let dependents = d.signals.signals[signal.0 as usize]
            .dependent_signals
            .clone();
        let mut to_abort = Vec::new();
        for dep in dependents {
            let data = &mut d.signals.signals[dep.0 as usize];
            if data.reason.is_none() {
                data.reason = Some(reason);
                to_abort.push(dep);
            }
        }
        to_abort
    };
    run_abort_steps(doc, host, signal);
    for dep in to_abort {
        run_abort_steps(doc, host, dep);
    }
}

/// "Run the abort steps": the algorithms, then an `abort` event.
fn run_abort_steps(doc: &DocRef, host: &mut dyn DomHost, signal: SignalId) {
    let algorithms =
        std::mem::take(&mut doc.borrow_mut().signals.signals[signal.0 as usize].algorithms);
    for algorithm in algorithms {
        match algorithm {
            AbortAlgorithm::RemoveListener { target, listener } => {
                events::remove_listener(&mut doc.borrow_mut(), host, target, &listener);
            }
            AbortAlgorithm::Host(id) => host.run_abort_algorithm(signal, id),
        }
    }
    events::fire_event(
        doc,
        host,
        Target::Signal(signal),
        "abort",
        EventInit::default(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dom::events::{ListenerOptions, add_event_listener, node_listeners};
    use crate::dom::host::NullHost;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[test]
    fn any_follows_sources_and_abort_removes_listeners() {
        let doc: DocRef = Rc::new(RefCell::new(Document::new_html()));
        let mut host = NullHost::default();
        let (a, b, any, div) = {
            let mut d = doc.borrow_mut();
            let a = d.create_abort_signal();
            let b = d.create_abort_signal();
            let any = d.abort_signal_any(&[a, b]);
            let any2 = d.abort_signal_any(&[any]);
            // A dependent signal depends on the sources, not on `any`.
            assert_eq!(d.signal(any2).source_signals, vec![a, b]);
            let div = d.create_html_element("div");
            let opts = ListenerOptions {
                signal: Some(any),
                ..ListenerOptions::default()
            };
            add_event_listener(
                &mut d,
                &mut host,
                Target::Node(div),
                "x",
                Some(HostRef(7)),
                opts,
            );
            assert_eq!(node_listeners(&d, div).len(), 1);
            (a, b, any, div)
        };
        signal_abort(&doc, &mut host, b, Some(HostRef(42)));
        let d = doc.borrow();
        assert_eq!(d.abort_reason(b), Some(HostRef(42)));
        assert_eq!(d.abort_reason(any), Some(HostRef(42)));
        assert!(!d.is_aborted(a));
        assert!(node_listeners(&d, div).is_empty());
        assert_eq!(d.throw_if_aborted(any), Err(HostRef(42)));
    }
}
