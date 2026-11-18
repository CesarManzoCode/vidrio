//! DOM algorithms across modules: live range updates on mutation, the
//! order of `MutationObserver` records and their delivery, event paths
//! through shadow roots with retargeting and `composedPath()`, and
//! selector matching with `:has` and `:nth-child(… of S)`.

use std::cell::RefCell;
use std::rc::Rc;

use vidrio::css::selectors::{Specificity, parse_selector_list};
use vidrio::dom::events::{
    Event, EventInit, EventRef, ListenerOptions, Phase, Target, add_event_listener, dispatch_event,
};
use vidrio::dom::host::DomHost;
use vidrio::dom::observer::{
    MutationRecord, ObserverId, ObserverInit, RecordType, notify_mutation_observers,
};
use vidrio::dom::range::BoundaryPoint;
use vidrio::dom::shadow::{ShadowRootInit, ShadowRootMode};
use vidrio::dom::traversal::filter::FILTER_ACCEPT;
use vidrio::dom::{DocRef, Document, DomException, HostRef, NodeId};
use vidrio::html::parse_document;

/// One listener call: callback, `currentTarget`, `target`, `eventPhase`
/// and `composedPath()` as the listener saw them.
type Call = (HostRef, Target, Option<Target>, Phase, Vec<Target>);

/// A host that records listener calls and mutation observer deliveries.
#[derive(Default)]
struct Recorder {
    calls: Vec<Call>,
    deliveries: Vec<(ObserverId, Vec<MutationRecord>)>,
}

impl DomHost for Recorder {
    fn call_listener(
        &mut self,
        callback: HostRef,
        current: Target,
        event: &EventRef,
        _: bool,
    ) -> Result<(), HostRef> {
        let e = event.borrow();
        self.calls
            .push((callback, current, e.target, e.phase, e.composed_path()));
        Ok(())
    }

    fn report_exception(&mut self, _: HostRef) {}

    fn call_mutation_observer(
        &mut self,
        observer: ObserverId,
        _: HostRef,
        records: Vec<MutationRecord>,
    ) -> Result<(), HostRef> {
        self.deliveries.push((observer, records));
        Ok(())
    }

    fn call_node_filter(&mut self, _: HostRef, _: NodeId) -> Result<u16, HostRef> {
        Ok(FILTER_ACCEPT)
    }

    fn create_exception(&mut self, _: DomException) -> HostRef {
        HostRef(0)
    }
}

fn by_id(doc: &Document, id: &str) -> NodeId {
    doc.get_element_by_id(NodeId::DOCUMENT, id)
        .unwrap_or_else(|| panic!("no #{id}"))
}

// ----- live ranges -----------------------------------------------------------

#[test]
fn live_ranges_follow_mutations() {
    let mut doc = Document::new_html();
    let p = doc.create_html_element("p");
    doc.append_child(NodeId::DOCUMENT, p);
    let a = doc.create_text("hello");
    let em = doc.create_html_element("em");
    let b = doc.create_text("world");
    doc.append_child(p, a);
    doc.append_child(p, em);
    doc.append_child(p, b);

    let r = doc.create_range();
    doc.range_set_start(r, a, 2).expect("start");
    doc.range_set_end(r, p, 3).expect("end");

    // Removing a child before the end offset shifts it back.
    doc.remove(em);
    assert_eq!(doc.range(r).start, BoundaryPoint::new(a, 2));
    assert_eq!(doc.range(r).end, BoundaryPoint::new(p, 2));

    // Inserting before it shifts it forward; the start in a Text stays.
    let c = doc.create_comment("c");
    doc.insert_before(p, c, Some(a));
    assert_eq!(doc.range(r).start, BoundaryPoint::new(a, 2));
    assert_eq!(doc.range(r).end, BoundaryPoint::new(p, 3));

    // Removing the start container moves the start to its old place.
    doc.remove(a);
    assert_eq!(doc.range(r).start, BoundaryPoint::new(p, 1));
    assert_eq!(doc.range(r).end, BoundaryPoint::new(p, 2));

    // Replacing data clamps offsets inside the removed run and shifts
    // those after it.
    let r2 = doc.create_range();
    doc.range_set_start(r2, b, 1).expect("start");
    doc.range_set_end(r2, b, 4).expect("end");
    doc.delete_data(b, 0, 2).expect("delete");
    assert_eq!(doc.text_content(b).as_deref(), Some("rld"));
    assert_eq!(doc.range(r2).start, BoundaryPoint::new(b, 0));
    assert_eq!(doc.range(r2).end, BoundaryPoint::new(b, 2));

    // Splitting a Text moves boundary points past the offset into the new
    // node, and a parent offset just after the split node gains one.
    let r3 = doc.create_range();
    doc.range_set_start(r3, b, 1).expect("start");
    doc.range_set_end(r3, b, 3).expect("end");
    let tail = doc.split_text(b, 1).expect("split");
    assert_eq!(doc.text_content(tail).as_deref(), Some("ld"));
    assert_eq!(doc.range(r3).start, BoundaryPoint::new(b, 1));
    assert_eq!(doc.range(r3).end, BoundaryPoint::new(tail, 2));
    assert_eq!(doc.range(r).start, BoundaryPoint::new(p, 1));
    assert_eq!(doc.range(r).end, BoundaryPoint::new(p, 3));

    // Normalizing merges `tail` back: the points in it move to `b`.
    doc.normalize(p);
    assert_eq!(doc.text_content(b).as_deref(), Some("rld"));
    assert_eq!(doc.range(r3).end, BoundaryPoint::new(b, 3));
    assert_eq!(doc.range(r).end, BoundaryPoint::new(p, 2));
    assert_eq!(doc.range_to_string(r), "rld");
}

// ----- mutation observers -----------------------------------------------------

#[test]
fn mutation_records_keep_order_and_transient_observers() {
    let doc: DocRef = Rc::new(RefCell::new(parse_document(
        "<div id=root><p id=a></p><p id=b></p></div>",
        false,
    )));
    let mut host = Recorder::default();
    let (root, a, b, t, i, mo1, mo2) = {
        let mut d = doc.borrow_mut();
        let root = by_id(&d, "root");
        let a = by_id(&d, "a");
        let b = by_id(&d, "b");
        let mo1 = d.create_mutation_observer(HostRef(1));
        let mo2 = d.create_mutation_observer(HostRef(2));
        let tree = ObserverInit {
            child_list: true,
            subtree: true,
            ..ObserverInit::default()
        };
        d.observe(mo1, root, tree).expect("observe tree");
        let attrs = ObserverInit {
            attributes: Some(true),
            attribute_old_value: Some(true),
            ..ObserverInit::default()
        };
        d.observe(mo2, a, attrs).expect("observe attributes");

        // mo2 gets the first record, so it is notified first.
        d.set_attribute(a, "x", "1");
        let t = d.create_text("t");
        d.append_child(root, t);
        d.remove(b);
        // `b` left the observed subtree: the transient registration still
        // reports its mutations until delivery.
        let i = d.create_html_element("i");
        d.append_child(b, i);
        d.set_attribute(a, "x", "2");
        assert!(d.take_mutation_observer_microtask());
        assert!(!d.take_mutation_observer_microtask());
        (root, a, b, t, i, mo1, mo2)
    };

    notify_mutation_observers(&doc, &mut host);
    assert_eq!(host.deliveries.len(), 2);

    let (first, attr_records) = &host.deliveries[0];
    assert_eq!(*first, mo2);
    assert_eq!(attr_records.len(), 2);
    assert!(
        attr_records
            .iter()
            .all(|r| r.kind == RecordType::Attributes && r.target == a)
    );
    assert_eq!(attr_records[0].attribute_name.as_deref(), Some("x"));
    assert_eq!(attr_records[0].old_value, None);
    assert_eq!(attr_records[1].old_value.as_deref(), Some("1"));

    let (second, tree_records) = &host.deliveries[1];
    assert_eq!(*second, mo1);
    assert_eq!(tree_records.len(), 3);
    assert!(tree_records.iter().all(|r| r.kind == RecordType::ChildList));
    assert_eq!(tree_records[0].target, root);
    assert_eq!(tree_records[0].added_nodes, vec![t]);
    assert_eq!(tree_records[0].previous_sibling, Some(b));
    assert_eq!(tree_records[1].target, root);
    assert_eq!(tree_records[1].removed_nodes, vec![b]);
    assert_eq!(tree_records[1].previous_sibling, Some(a));
    assert_eq!(tree_records[1].next_sibling, Some(t));
    assert_eq!(tree_records[2].target, b);
    assert_eq!(tree_records[2].added_nodes, vec![i]);

    // Delivery dropped the transient registration on `b`.
    let mut d = doc.borrow_mut();
    let u = d.create_html_element("u");
    d.append_child(b, u);
    assert!(d.take_mutation_records(mo1).is_empty());
    assert!(!d.take_mutation_observer_microtask());
}

// ----- event dispatch through shadow trees ------------------------------------

/// `<div id=host>` with a light `<span id=light>` assigned to a default
/// `<slot>` of a shadow root in `mode`, which also holds `<b>`.
struct ShadowFixture {
    doc: DocRef,
    host: NodeId,
    light: NodeId,
    shadow: NodeId,
    slot: NodeId,
    inner: NodeId,
    body: NodeId,
    html: NodeId,
}

fn shadow_fixture(mode: ShadowRootMode) -> ShadowFixture {
    let mut d = parse_document("<div id=host><span id=light></span></div>", false);
    let host = by_id(&d, "host");
    let light = by_id(&d, "light");
    let shadow = d
        .attach_shadow(host, ShadowRootInit::new(mode))
        .expect("attach");
    let slot = d.create_html_element("slot");
    let inner = d.create_html_element("b");
    d.append_child(shadow, inner);
    d.append_child(shadow, slot);
    assert_eq!(d.assigned_slot_internal(light), Some(slot));
    let body = d.body().expect("body");
    let html = d.document_element().expect("html");
    ShadowFixture {
        doc: Rc::new(RefCell::new(d)),
        host,
        light,
        shadow,
        slot,
        inner,
        body,
        html,
    }
}

fn listen(f: &ShadowFixture, host: &mut Recorder, node: NodeId, callback: u64) {
    add_event_listener(
        &mut f.doc.borrow_mut(),
        host,
        Target::Node(node),
        "x",
        Some(HostRef(callback)),
        ListenerOptions::default(),
    );
}

fn event(composed: bool) -> EventRef {
    let init = EventInit {
        bubbles: true,
        cancelable: false,
        composed,
    };
    Event::new("x", init, 0.0).into_ref()
}

fn nodes(ids: &[NodeId]) -> Vec<Target> {
    ids.iter().map(|&n| Target::Node(n)).collect()
}

#[test]
fn dispatch_retargets_across_shadow_boundary() {
    let f = shadow_fixture(ShadowRootMode::Open);
    let mut rec = Recorder::default();
    for (n, cb) in [
        (f.inner, 1),
        (f.shadow, 2),
        (f.host, 3),
        (NodeId::DOCUMENT, 4),
    ] {
        listen(&f, &mut rec, n, cb);
    }

    // Composed: the event leaves the shadow tree; outside it the target
    // is the host.
    let e = event(true);
    assert_eq!(
        dispatch_event(&f.doc, &mut rec, Target::Node(f.inner), &e),
        Ok(true)
    );
    let full = nodes(&[f.inner, f.shadow, f.host, f.body, f.html, NodeId::DOCUMENT]);
    let seen: Vec<(u64, Option<Target>, Phase)> =
        rec.calls.iter().map(|c| (c.0.0, c.2, c.3)).collect();
    assert_eq!(
        seen,
        vec![
            (1, Some(Target::Node(f.inner)), Phase::AtTarget),
            (2, Some(Target::Node(f.inner)), Phase::Bubbling),
            (3, Some(Target::Node(f.host)), Phase::AtTarget),
            (4, Some(Target::Node(f.host)), Phase::Bubbling),
        ]
    );
    assert!(rec.calls.iter().all(|c| c.4 == full));
    assert_eq!(e.borrow().target, Some(Target::Node(f.host)));

    // Not composed: the path stops at the shadow root and the target is
    // cleared afterwards because it is in a shadow tree.
    rec.calls.clear();
    let e = event(false);
    assert_eq!(
        dispatch_event(&f.doc, &mut rec, Target::Node(f.inner), &e),
        Ok(true)
    );
    let callbacks: Vec<u64> = rec.calls.iter().map(|c| c.0.0).collect();
    assert_eq!(callbacks, vec![1, 2]);
    assert_eq!(rec.calls[1].4, nodes(&[f.inner, f.shadow]));
    assert_eq!(e.borrow().target, None);
}

#[test]
fn slotted_events_pass_through_slot_and_closed_trees_hide_it() {
    let f = shadow_fixture(ShadowRootMode::Closed);
    let mut rec = Recorder::default();
    for (n, cb) in [
        (f.light, 1),
        (f.slot, 2),
        (f.shadow, 3),
        (f.host, 4),
        (NodeId::DOCUMENT, 5),
    ] {
        listen(&f, &mut rec, n, cb);
    }

    // Even an event that is not composed goes from the slotted light node
    // through its slot and the shadow root to the host; the target stays
    // the light node, which is not in a shadow tree.
    let e = event(false);
    assert_eq!(
        dispatch_event(&f.doc, &mut rec, Target::Node(f.light), &e),
        Ok(true)
    );
    let callbacks: Vec<u64> = rec.calls.iter().map(|c| c.0.0).collect();
    assert_eq!(callbacks, vec![1, 2, 3, 4, 5]);
    assert!(rec.calls.iter().all(|c| c.2 == Some(Target::Node(f.light))));

    let full = nodes(&[
        f.light,
        f.slot,
        f.shadow,
        f.host,
        f.body,
        f.html,
        NodeId::DOCUMENT,
    ]);
    // Inside the closed tree the whole path is visible.
    assert_eq!(rec.calls[1].4, full);
    assert_eq!(rec.calls[2].4, full);
    // Outside it, the slot and the shadow root are hidden.
    let outside = nodes(&[f.light, f.host, f.body, f.html, NodeId::DOCUMENT]);
    assert_eq!(rec.calls[0].4, outside);
    assert_eq!(rec.calls[3].4, outside);
    assert_eq!(rec.calls[4].4, outside);
    assert_eq!(e.borrow().target, Some(Target::Node(f.light)));
}

// ----- selectors ------------------------------------------------------------

const LIST: &str = "<section id=s1><h2 id=h1></h2><p id=p1></p></section>\
<section id=s2><p id=p2></p><h2 id=h2></h2></section>\
<ul id=list><li id=a class=x></li><li id=b></li><li id=c class=x></li>\
<li id=d class=x><img id=img></li><li id=e></li></ul>";

fn select(doc: &Document, selectors: &str) -> Vec<String> {
    doc.query_selector_all(NodeId::DOCUMENT, selectors)
        .expect("valid selectors")
        .into_iter()
        .map(|n| doc.get_attribute(n, "id").unwrap_or("?").to_string())
        .collect()
}

#[test]
fn has_and_nth_child_of_selector() {
    let doc = parse_document(LIST, false);
    // `:has` with child, sibling and descendant relative selectors.
    assert_eq!(select(&doc, "section:has(> h2 + p)"), vec!["s1"]);
    assert_eq!(select(&doc, "h2:has(~ p)"), vec!["h1"]);
    assert_eq!(select(&doc, "p:has(+ h2)"), vec!["p2"]);
    assert_eq!(select(&doc, "ul:has(img)"), vec!["list"]);
    assert_eq!(select(&doc, "li:has(> img)"), vec!["d"]);
    assert_eq!(select(&doc, "li:not(:has(*))"), vec!["a", "b", "c", "e"]);
    assert_eq!(select(&doc, "section:has(p:first-child)"), vec!["s2"]);

    // `:nth-child(… of S)` counts only the siblings matching S.
    assert_eq!(select(&doc, "li:nth-child(2 of .x)"), vec!["c"]);
    assert_eq!(select(&doc, "li:nth-child(odd of .x)"), vec!["a", "d"]);
    assert_eq!(select(&doc, "li:nth-last-child(1 of .x)"), vec!["d"]);
    assert_eq!(select(&doc, ":nth-child(1 of li:not(.x))"), vec!["b"]);
    assert_eq!(select(&doc, "li:nth-child(2)"), vec!["b"]);
    // An element outside S never matches.
    assert_eq!(select(&doc, "#b:nth-child(1 of .x)"), Vec::<String>::new());
    // Both together.
    assert_eq!(
        select(&doc, "ul:has(> :nth-child(3 of .x) > img)"),
        vec!["list"]
    );
    assert_eq!(select(&doc, "li:nth-child(1 of :has(img))"), vec!["d"]);

    // `:scope` and `closest` over the same tree.
    let list = by_id(&doc, "list");
    let img = by_id(&doc, "img");
    let scoped = doc
        .query_selector_all(list, ":scope > li:nth-child(even of .x)")
        .expect("valid");
    assert_eq!(scoped, vec![by_id(&doc, "c")]);
    assert_eq!(
        doc.closest(img, "li:nth-child(3 of .x)"),
        Ok(Some(by_id(&doc, "d")))
    );
    assert_eq!(doc.matches(img, ":is(ul:has(> #e)) img"), Ok(true));
    assert_eq!(
        doc.query_selector(NodeId::DOCUMENT, "li:has(img) !"),
        Err(DomException::SyntaxError)
    );
}

#[test]
fn has_and_nth_child_of_specificity() {
    let spec = |s: &str| parse_selector_list(s).expect("valid").0[0].specificity;
    assert_eq!(spec(":nth-child(2 of .x)"), Specificity::new(0, 2, 0));
    assert_eq!(spec(":nth-child(2 of #a, .x)"), Specificity::new(1, 1, 0));
    assert_eq!(spec("li:has(> img, #a)"), Specificity::new(1, 0, 1));
    assert_eq!(spec(":where(:has(#a))"), Specificity::new(0, 0, 0));
}
