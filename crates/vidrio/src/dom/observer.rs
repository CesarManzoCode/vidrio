//! `MutationObserver` (DOM §4.3): registrations, mutation records and
//! their delivery.
//!
//! Records are queued by the mutation algorithms while they hold the
//! document. Queuing sets the "mutation observer microtask queued" flag and
//! asks for a microtask: the event loop of k3-10 polls
//! [`Document::take_mutation_observer_microtask`] after each piece of work
//! and, when it returns true, queues a microtask that runs
//! [`notify_mutation_observers`].
//!
//! The observers live in the document: an observer that watches nodes of
//! two documents has one registry entry in each (see `LIMITATIONS.md`).

use std::collections::HashMap;

use super::events::{self, EventInit};
use super::host::DomHost;
use super::{DocRef, Document, DomError, HostRef, Namespace, NodeId};

/// A mutation observer of a document.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ObserverId(pub u32);

/// `MutationObserverInit` as given: absent members are `None`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ObserverInit {
    pub child_list: bool,
    pub attributes: Option<bool>,
    pub character_data: Option<bool>,
    pub subtree: bool,
    pub attribute_old_value: Option<bool>,
    pub character_data_old_value: Option<bool>,
    pub attribute_filter: Option<Vec<String>>,
}

/// The options of a registration once `observe` resolved them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ObserverOptions {
    pub child_list: bool,
    pub attributes: bool,
    pub character_data: bool,
    pub subtree: bool,
    pub attribute_old_value: bool,
    pub character_data_old_value: bool,
    pub attribute_filter: Option<Vec<String>>,
}

/// A `MutationRecord`'s `type`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordType {
    Attributes,
    CharacterData,
    ChildList,
}

impl RecordType {
    pub fn as_str(self) -> &'static str {
        match self {
            RecordType::Attributes => "attributes",
            RecordType::CharacterData => "characterData",
            RecordType::ChildList => "childList",
        }
    }
}

/// A `MutationRecord`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MutationRecord {
    pub kind: RecordType,
    pub target: NodeId,
    pub added_nodes: Vec<NodeId>,
    pub removed_nodes: Vec<NodeId>,
    pub previous_sibling: Option<NodeId>,
    pub next_sibling: Option<NodeId>,
    pub attribute_name: Option<String>,
    pub attribute_namespace: Option<String>,
    pub old_value: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct ObserverData {
    callback: HostRef,
    records: Vec<MutationRecord>,
    /// The nodes this observer is registered on (the "node list").
    nodes: Vec<NodeId>,
}

/// A registered observer; `source` is set for a transient one.
#[derive(Clone, Debug)]
pub(crate) struct Registered {
    id: u64,
    observer: ObserverId,
    options: ObserverOptions,
    source: Option<u64>,
}

/// The observers of a document and the agent-level delivery state.
#[derive(Clone, Debug, Default)]
pub struct ObserverState {
    observers: Vec<Option<ObserverData>>,
    /// Each node's registered observer list.
    registered: HashMap<NodeId, Vec<Registered>>,
    next_registration: u64,
    /// Pending mutation observers, in the order they got records.
    pending: Vec<ObserverId>,
    /// Signal slots (DOM §4.2.2.5).
    pub(crate) signal_slots: Vec<NodeId>,
    /// The "mutation observer microtask queued" flag.
    microtask_queued: bool,
    /// A microtask was asked for and the event loop has not taken it.
    microtask_requested: bool,
}

impl Document {
    /// `new MutationObserver(callback)`.
    pub fn create_mutation_observer(&mut self, callback: HostRef) -> ObserverId {
        let id =
            ObserverId(u32::try_from(self.observers.observers.len()).expect("too many observers"));
        self.observers.observers.push(Some(ObserverData {
            callback,
            records: Vec::new(),
            nodes: Vec::new(),
        }));
        id
    }

    /// Drops an observer the bindings no longer reach, with its
    /// registrations.
    pub fn release_mutation_observer(&mut self, observer: ObserverId) {
        self.disconnect_mutation_observer(observer);
        if let Some(slot) = self.observers.observers.get_mut(observer.0 as usize) {
            *slot = None;
        }
        self.observers.pending.retain(|&o| o != observer);
    }

    fn observer_data(&mut self, observer: ObserverId) -> Option<&mut ObserverData> {
        self.observers
            .observers
            .get_mut(observer.0 as usize)?
            .as_mut()
    }

    /// `observe(target, options)`.
    pub fn observe(
        &mut self,
        observer: ObserverId,
        target: NodeId,
        init: ObserverInit,
    ) -> Result<(), DomError> {
        let mut attributes = init.attributes;
        let mut character_data = init.character_data;
        if (init.attribute_old_value.is_some() || init.attribute_filter.is_some())
            && attributes.is_none()
        {
            attributes = Some(true);
        }
        if init.character_data_old_value.is_some() && character_data.is_none() {
            character_data = Some(true);
        }
        let attributes = attributes == Some(true);
        let character_data = character_data == Some(true);
        if !init.child_list && !attributes && !character_data {
            return Err(DomError::Type(
                "The options object must set at least one of 'attributes', 'characterData', or 'childList' to true.".into(),
            ));
        }
        if init.attribute_old_value == Some(true) && !attributes {
            return Err(DomError::Type(
                "The options object may only set 'attributeOldValue' to true when 'attributes' is true or not present.".into(),
            ));
        }
        if init.attribute_filter.is_some() && !attributes {
            return Err(DomError::Type(
                "The options object may only set 'attributeFilter' when 'attributes' is true or not present.".into(),
            ));
        }
        if init.character_data_old_value == Some(true) && !character_data {
            return Err(DomError::Type(
                "The options object may only set 'characterDataOldValue' to true when 'characterData' is true or not present.".into(),
            ));
        }
        let options = ObserverOptions {
            child_list: init.child_list,
            attributes,
            character_data,
            subtree: init.subtree,
            attribute_old_value: init.attribute_old_value == Some(true),
            character_data_old_value: init.character_data_old_value == Some(true),
            attribute_filter: init.attribute_filter,
        };
        let existing = self
            .observers
            .registered
            .get(&target)
            .and_then(|list| {
                list.iter()
                    .find(|r| r.observer == observer && r.source.is_none())
            })
            .map(|r| r.id);
        if let Some(id) = existing {
            let nodes = self
                .observer_data(observer)
                .map(|d| d.nodes.clone())
                .unwrap_or_default();
            for node in nodes {
                if let Some(list) = self.observers.registered.get_mut(&node) {
                    list.retain(|r| r.source != Some(id));
                }
            }
            if let Some(r) = self
                .observers
                .registered
                .get_mut(&target)
                .and_then(|list| list.iter_mut().find(|r| r.id == id))
            {
                r.options = options;
            }
            return Ok(());
        }
        let id = self.observers.next_registration;
        self.observers.next_registration += 1;
        self.observers
            .registered
            .entry(target)
            .or_default()
            .push(Registered {
                id,
                observer,
                options,
                source: None,
            });
        if let Some(data) = self.observer_data(observer) {
            if !data.nodes.contains(&target) {
                data.nodes.push(target);
            }
        }
        Ok(())
    }

    /// `disconnect()`.
    pub fn disconnect_mutation_observer(&mut self, observer: ObserverId) {
        let Some(data) = self.observer_data(observer) else {
            return;
        };
        let nodes = std::mem::take(&mut data.nodes);
        data.records.clear();
        for node in nodes {
            if let Some(list) = self.observers.registered.get_mut(&node) {
                list.retain(|r| r.observer != observer);
                if list.is_empty() {
                    self.observers.registered.remove(&node);
                }
            }
        }
    }

    /// `takeRecords()`.
    pub fn take_mutation_records(&mut self, observer: ObserverId) -> Vec<MutationRecord> {
        self.observer_data(observer)
            .map(|d| std::mem::take(&mut d.records))
            .unwrap_or_default()
    }

    /// The registered observers of `node` as (observer, options,
    /// transient) triples, for inspection.
    pub fn registered_observers(&self, node: NodeId) -> Vec<(ObserverId, ObserverOptions, bool)> {
        self.observers
            .registered
            .get(&node)
            .map(|list| {
                list.iter()
                    .map(|r| (r.observer, r.options.clone(), r.source.is_some()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// True if some node has a registered observer.
    pub(crate) fn has_registered_observers(&self) -> bool {
        !self.observers.registered.is_empty()
    }

    /// The transient registrations of remove step 15: each registration
    /// with `subtree` on an inclusive ancestor of `parent` follows `node`.
    pub(crate) fn add_transient_observers(&mut self, parent: NodeId, node: NodeId) {
        if !self.has_registered_observers() {
            return;
        }
        let mut chain = vec![parent];
        chain.extend(self.ancestors(parent));
        let mut transient = Vec::new();
        for ancestor in chain {
            if let Some(list) = self.observers.registered.get(&ancestor) {
                for r in list.iter().filter(|r| r.options.subtree) {
                    transient.push((r.observer, r.options.clone(), r.id));
                }
            }
        }
        for (observer, options, source) in transient {
            let id = self.observers.next_registration;
            self.observers.next_registration += 1;
            self.observers
                .registered
                .entry(node)
                .or_default()
                .push(Registered {
                    id,
                    observer,
                    options,
                    source: Some(source),
                });
            if let Some(data) = self.observer_data(observer) {
                if !data.nodes.contains(&node) {
                    data.nodes.push(node);
                }
            }
        }
    }

    /// "Queue a mutation record" (DOM §4.3.2).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn queue_mutation_record(
        &mut self,
        kind: RecordType,
        target: NodeId,
        name: Option<&str>,
        namespace: Option<Namespace>,
        old_value: Option<&str>,
        added: Vec<NodeId>,
        removed: Vec<NodeId>,
        previous_sibling: Option<NodeId>,
        next_sibling: Option<NodeId>,
    ) {
        if !self.has_registered_observers() {
            return;
        }
        let mut interested: Vec<(ObserverId, Option<String>)> = Vec::new();
        let mut nodes = vec![target];
        nodes.extend(self.ancestors(target));
        for node in nodes {
            let Some(list) = self.observers.registered.get(&node) else {
                continue;
            };
            for r in list {
                let o = &r.options;
                let skip = (node != target && !o.subtree)
                    || (kind == RecordType::Attributes && !o.attributes)
                    || (kind == RecordType::Attributes
                        && o.attribute_filter.as_ref().is_some_and(|filter| {
                            namespace.is_some() || !filter.iter().any(|f| Some(f.as_str()) == name)
                        }))
                    || (kind == RecordType::CharacterData && !o.character_data)
                    || (kind == RecordType::ChildList && !o.child_list);
                if skip {
                    continue;
                }
                let index = match interested.iter().position(|(obs, _)| *obs == r.observer) {
                    Some(i) => i,
                    None => {
                        interested.push((r.observer, None));
                        interested.len() - 1
                    }
                };
                if (kind == RecordType::Attributes && o.attribute_old_value)
                    || (kind == RecordType::CharacterData && o.character_data_old_value)
                {
                    interested[index].1 = old_value.map(str::to_string);
                }
            }
        }
        if interested.is_empty() {
            return;
        }
        for (observer, mapped_old_value) in interested {
            let record = MutationRecord {
                kind,
                target,
                added_nodes: added.clone(),
                removed_nodes: removed.clone(),
                previous_sibling,
                next_sibling,
                attribute_name: name.map(str::to_string),
                attribute_namespace: namespace.map(|ns| ns.url().to_string()),
                old_value: mapped_old_value,
            };
            let Some(data) = self.observer_data(observer) else {
                continue;
            };
            data.records.push(record);
            if !self.observers.pending.contains(&observer) {
                self.observers.pending.push(observer);
            }
        }
        self.queue_mutation_observer_microtask();
    }

    /// "Queue a tree mutation record".
    pub(crate) fn queue_tree_mutation_record(
        &mut self,
        target: NodeId,
        added: Vec<NodeId>,
        removed: Vec<NodeId>,
        previous_sibling: Option<NodeId>,
        next_sibling: Option<NodeId>,
    ) {
        debug_assert!(!added.is_empty() || !removed.is_empty());
        self.queue_mutation_record(
            RecordType::ChildList,
            target,
            None,
            None,
            None,
            added,
            removed,
            previous_sibling,
            next_sibling,
        );
    }

    /// "Queue a mutation observer microtask".
    pub(crate) fn queue_mutation_observer_microtask(&mut self) {
        if self.observers.microtask_queued {
            return;
        }
        self.observers.microtask_queued = true;
        self.observers.microtask_requested = true;
    }

    /// The hook for the event loop: true once each time a mutation observer
    /// microtask has been queued; the caller then queues a microtask that
    /// runs [`notify_mutation_observers`].
    pub fn take_mutation_observer_microtask(&mut self) -> bool {
        std::mem::take(&mut self.observers.microtask_requested)
    }
}

/// "Notify mutation observers" (DOM §4.3): delivers each pending
/// observer's records to its callback, then fires `slotchange` at the
/// signal slots.
pub fn notify_mutation_observers(doc: &DocRef, host: &mut dyn DomHost) {
    let (notify_set, signal_set) = {
        let mut d = doc.borrow_mut();
        d.observers.microtask_queued = false;
        (
            std::mem::take(&mut d.observers.pending),
            std::mem::take(&mut d.observers.signal_slots),
        )
    };
    for mo in notify_set {
        let delivery = {
            let mut d = doc.borrow_mut();
            let Some(data) = d.observer_data(mo) else {
                continue;
            };
            let records = std::mem::take(&mut data.records);
            let callback = data.callback;
            let nodes = data.nodes.clone();
            for node in nodes {
                if let Some(list) = d.observers.registered.get_mut(&node) {
                    list.retain(|r| !(r.observer == mo && r.source.is_some()));
                    if list.is_empty() {
                        d.observers.registered.remove(&node);
                    }
                }
            }
            (!records.is_empty()).then_some((callback, records))
        };
        if let Some((callback, records)) = delivery {
            if let Err(exception) = host.call_mutation_observer(mo, callback, records) {
                host.report_exception(exception);
            }
        }
    }
    for slot in signal_set {
        let init = EventInit {
            bubbles: true,
            ..EventInit::default()
        };
        events::fire_event(doc, host, events::Target::Node(slot), "slotchange", init);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observe_validates_options() {
        let mut doc = Document::new_html();
        let mo = doc.create_mutation_observer(HostRef(1));
        let target = doc.create_html_element("div");
        assert!(matches!(
            doc.observe(mo, target, ObserverInit::default()),
            Err(DomError::Type(_))
        ));
        let init = ObserverInit {
            attribute_old_value: Some(true),
            ..ObserverInit::default()
        };
        assert!(doc.observe(mo, target, init).is_ok());
        let regs = doc.registered_observers(target);
        assert_eq!(regs.len(), 1);
        assert!(regs[0].1.attributes);
        doc.set_attribute(target, "a", "1");
        doc.set_attribute(target, "a", "2");
        let records = doc.take_mutation_records(mo);
        assert_eq!(records.len(), 2);
        assert_eq!(records[1].old_value.as_deref(), Some("1"));
        assert!(doc.take_mutation_observer_microtask());
        assert!(!doc.take_mutation_observer_microtask());
        doc.disconnect_mutation_observer(mo);
        doc.set_attribute(target, "a", "3");
        assert!(doc.take_mutation_records(mo).is_empty());
    }
}
