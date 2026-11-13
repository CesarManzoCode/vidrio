//! Heap and object model tests: collection from roots, weak references,
//! ephemerons and finalizers, the order of [[OwnPropertyKeys]], shape
//! transitions and dictionary mode, exotic objects and the type
//! conversion abstract operations.

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use vjs::agent::Agent;
use vjs::function::{NativeCall, create_native_function, native_host_data};
use vjs::gc::Trace;
use vjs::internal;
use vjs::object::{ObjectKind, PropertyDescriptor};
use vjs::ops::{self, PreferredType};
use vjs::proxy::proxy_create;
use vjs::realm::Intrinsic;
use vjs::shape::MAX_SHAPE_PROPERTIES;
use vjs::value::{PropertyKey, Value, WeakKey};
use vjs::weak;
use vjs::{JsResult, Object};

fn setup() -> Agent {
    let mut agent = Agent::new();
    agent.initialize_host_defined_realm();
    agent
}

fn new_object(agent: &mut Agent) -> Object {
    ops::object_create(agent)
}

fn key(agent: &mut Agent, name: &str) -> PropertyKey {
    agent.heap.key_from_str(name)
}

fn put(agent: &mut Agent, o: Object, name: &str, v: Value) {
    let k = key(agent, name);
    ops::create_data_property_or_throw(agent, o, k, v).unwrap();
}

fn read(agent: &mut Agent, o: Object, name: &str) -> Value {
    let k = key(agent, name);
    ops::get(agent, o, k).unwrap()
}

fn string(agent: &mut Agent, s: &str) -> Value {
    Value::String(agent.heap.alloc_str(s))
}

fn text(agent: &mut Agent, v: Value) -> String {
    match v {
        Value::String(s) => agent.heap.string_to_rust(s),
        other => panic!("expected a string, got {other:?}"),
    }
}

/// Collects with only the agent's roots, after the end of the "job".
fn collect(agent: &mut Agent) {
    agent.clear_kept_objects();
    agent
        .collect_garbage(&[])
        .expect("no native call is in progress");
}

fn native_number(_agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    Ok(vjs::function::native_slots(_agent, call.callee)
        .first()
        .copied()
        .unwrap_or(Value::Undefined))
}

/// A native function that returns `value`.
fn constant_function(agent: &mut Agent, value: Value) -> Object {
    create_native_function(agent, native_number, 0, "constant", vec![value], None)
}

fn assert_type_error(agent: &mut Agent, thrown: vjs::Throw) {
    let Value::Object(e) = thrown.0 else {
        panic!("expected an error object, got {:?}", thrown.0);
    };
    let proto = agent.intrinsic(Intrinsic::TypeErrorPrototype);
    assert_eq!(agent.heap.object(e).proto(), Some(proto));
}

// ----- collection -----

#[test]
fn collection_frees_unreachable_and_keeps_what_roots_reach() {
    let mut agent = setup();
    let kept = new_object(&mut agent);
    let child = new_object(&mut agent);
    put(&mut agent, kept, "child", Value::Object(child));
    let garbage = new_object(&mut agent);
    let garbage_child = new_object(&mut agent);
    put(&mut agent, garbage, "child", Value::Object(garbage_child));
    put(&mut agent, garbage_child, "parent", Value::Object(garbage));

    let root = agent.heap.add_root(Value::Object(kept));
    collect(&mut agent);
    assert!(agent.heap.object_exists(kept));
    assert!(agent.heap.object_exists(child));
    assert!(
        !agent.heap.object_exists(garbage),
        "unreachable cycle survived"
    );
    assert!(!agent.heap.object_exists(garbage_child));
    assert_eq!(read(&mut agent, kept, "child"), Value::Object(child));

    agent.heap.remove_root(root);
    collect(&mut agent);
    assert!(!agent.heap.object_exists(kept));
    assert!(!agent.heap.object_exists(child));
}

#[test]
fn intrinsics_and_globals_survive_collection() {
    let mut agent = setup();
    let before = agent.heap.collections();
    collect(&mut agent);
    assert_eq!(agent.heap.collections(), before + 1);
    let global = agent.global_object();
    let error_ctor = agent.intrinsic(Intrinsic::TypeError);
    assert!(agent.heap.object_exists(global));
    assert_eq!(
        read(&mut agent, global, "TypeError"),
        Value::Object(error_ctor)
    );
    let nan = read(&mut agent, global, "NaN");
    assert!(matches!(nan, Value::Number(n) if n.is_nan()));
}

#[test]
fn extra_roots_and_root_sets_keep_values() {
    let mut agent = setup();
    let by_extra = new_object(&mut agent);
    let by_set = new_object(&mut agent);
    let s = agent.heap.alloc_str("a string only a root set holds");
    let set: Rc<RefCell<Vec<Value>>> =
        Rc::new(RefCell::new(vec![Value::Object(by_set), Value::String(s)]));
    let set_id = agent.heap.add_root_set(set.clone());
    let extra = Value::Object(by_extra);
    let roots: [&dyn Trace; 1] = [&extra];
    agent.clear_kept_objects();
    agent.collect_garbage(&roots).unwrap();
    assert!(agent.heap.object_exists(by_extra));
    assert!(agent.heap.object_exists(by_set));
    assert!(agent.heap.string_exists(s));

    agent.heap.remove_root_set(set_id);
    collect(&mut agent);
    assert!(!agent.heap.object_exists(by_extra));
    assert!(!agent.heap.object_exists(by_set));
    assert!(!agent.heap.string_exists(s));
}

#[test]
fn ropes_keep_their_parts_alive() {
    let mut agent = setup();
    let a = agent.heap.alloc_str(&"a".repeat(50));
    let b = agent.heap.alloc_str(&"b".repeat(50));
    let ab = agent.heap.concat(a, b).unwrap();
    let root = agent.heap.add_root(Value::String(ab));
    collect(&mut agent);
    assert!(agent.heap.string_exists(a));
    assert!(agent.heap.string_exists(b));
    assert_eq!(agent.heap.string_len(ab), 100);
    let flat = agent.heap.string_to_rust(ab);
    assert_eq!(flat, format!("{}{}", "a".repeat(50), "b".repeat(50)));
    agent.heap.remove_root(root);
    collect(&mut agent);
    assert!(!agent.heap.string_exists(ab));
}

#[test]
fn rust_finalizers_run_after_their_object_is_freed() {
    let mut agent = setup();
    let freed = Rc::new(Cell::new(false));
    let kept = Rc::new(Cell::new(false));
    let o = new_object(&mut agent);
    let p = new_object(&mut agent);
    let (f1, f2) = (freed.clone(), kept.clone());
    agent.heap.add_finalizer(o, Box::new(move || f1.set(true)));
    agent.heap.add_finalizer(p, Box::new(move || f2.set(true)));
    let root = agent.heap.add_root(Value::Object(p));
    collect(&mut agent);
    assert!(freed.get());
    assert!(!kept.get());
    agent.heap.remove_root(root);
    collect(&mut agent);
    assert!(kept.get());
}

#[test]
fn allocation_threshold_triggers_collection_at_safe_points() {
    let mut agent = setup();
    agent.heap.set_gc_threshold(1);
    new_object(&mut agent);
    assert!(agent.heap.should_collect());
    assert!(agent.safepoint(&[]).is_some());
    agent.request_gc();
    assert!(agent.safepoint(&[]).is_some());
}

// ----- weak references, ephemerons, finalization registries -----

#[test]
fn weak_refs_are_cleared_when_the_target_dies() {
    let mut agent = setup();
    let target = new_object(&mut agent);
    let live_target = new_object(&mut agent);
    let w = weak::weak_ref_create(&mut agent, WeakKey::Object(target), None);
    let w_live = weak::weak_ref_create(&mut agent, WeakKey::Object(live_target), None);
    let r1 = agent.heap.add_root(Value::Object(w));
    let r2 = agent.heap.add_root(Value::Object(w_live));
    let r3 = agent.heap.add_root(Value::Object(live_target));

    // Kept alive until ClearKeptObjects.
    agent.collect_garbage(&[]).unwrap();
    assert!(agent.heap.object_exists(target));

    collect(&mut agent);
    assert!(!agent.heap.object_exists(target));
    assert_eq!(weak::weak_ref_deref(&mut agent, w), Value::Undefined);
    assert_eq!(
        weak::weak_ref_deref(&mut agent, w_live),
        Value::Object(live_target)
    );

    // A symbol that is not registered can be held weakly; a registered one cannot.
    let desc = agent.heap.alloc_str("s");
    let sym = agent.heap.alloc_symbol(Some(desc));
    assert_eq!(
        weak::can_be_held_weakly(&agent, Value::Symbol(sym)),
        Some(WeakKey::Symbol(sym))
    );
    let registered = agent.symbol_for(desc);
    assert_eq!(
        weak::can_be_held_weakly(&agent, Value::Symbol(registered)),
        None
    );
    for r in [r1, r2, r3] {
        agent.heap.remove_root(r);
    }
}

#[test]
fn ephemerons_do_not_retain_their_keys() {
    let mut agent = setup();
    let map = weak::weak_map_create(&mut agent, None);
    let root = agent.heap.add_root(Value::Object(map));

    // The value points back to its key: still collectable.
    let dead_key = new_object(&mut agent);
    let dead_value = new_object(&mut agent);
    put(&mut agent, dead_value, "key", Value::Object(dead_key));
    weak::weak_map_set(
        &mut agent,
        map,
        WeakKey::Object(dead_key),
        Value::Object(dead_value),
    );

    // A live key keeps a value reachable only through the map.
    let live_key = new_object(&mut agent);
    let live_value = new_object(&mut agent);
    let key_root = agent.heap.add_root(Value::Object(live_key));
    weak::weak_map_set(
        &mut agent,
        map,
        WeakKey::Object(live_key),
        Value::Object(live_value),
    );

    // A chain: live_key → k2 → v3, all reachable only through ephemerons.
    let k2 = new_object(&mut agent);
    let v3 = new_object(&mut agent);
    weak::weak_map_set(&mut agent, map, WeakKey::Object(k2), Value::Object(v3));
    put(&mut agent, live_value, "next", Value::Object(k2));

    collect(&mut agent);
    assert!(!agent.heap.object_exists(dead_key));
    assert!(!agent.heap.object_exists(dead_value));
    assert!(!weak::weak_map_has(&agent, map, WeakKey::Object(dead_key)));
    assert!(agent.heap.object_exists(live_value));
    assert!(agent.heap.object_exists(k2));
    assert!(agent.heap.object_exists(v3));
    assert_eq!(
        weak::weak_map_get(&agent, map, WeakKey::Object(live_key)),
        Value::Object(live_value)
    );
    assert_eq!(
        weak::weak_map_get(&agent, map, WeakKey::Object(k2)),
        Value::Object(v3)
    );

    // Dropping the live key frees the whole chain.
    agent.heap.remove_root(key_root);
    collect(&mut agent);
    assert!(!agent.heap.object_exists(live_key));
    assert!(!agent.heap.object_exists(live_value));
    assert!(!agent.heap.object_exists(k2));
    assert!(!agent.heap.object_exists(v3));

    // WeakSet entries go away with their keys too.
    let set = weak::weak_set_create(&mut agent, None);
    let set_root = agent.heap.add_root(Value::Object(set));
    let member = new_object(&mut agent);
    weak::weak_set_add(&mut agent, set, WeakKey::Object(member));
    assert!(weak::weak_set_has(&agent, set, WeakKey::Object(member)));
    collect(&mut agent);
    assert!(!agent.heap.object_exists(member));
    assert!(!weak::weak_set_has(&agent, set, WeakKey::Object(member)));
    agent.heap.remove_root(set_root);
    agent.heap.remove_root(root);
}

fn record_held_value(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let data = native_host_data(agent, call.callee).expect("host data");
    let log = data
        .downcast_ref::<RefCell<Vec<f64>>>()
        .expect("a log of numbers");
    if let Value::Number(n) = call.arg(0) {
        log.borrow_mut().push(n);
    }
    Ok(Value::Undefined)
}

#[test]
fn finalization_registry_queues_its_callback_as_a_host_task() {
    let mut agent = setup();
    let log: Rc<RefCell<Vec<f64>>> = Rc::new(RefCell::new(Vec::new()));
    let data: Rc<dyn Any> = log.clone();
    let callback = create_native_function(
        &mut agent,
        record_held_value,
        1,
        "record",
        Vec::new(),
        Some(data),
    );
    let registry = weak::finalization_registry_create(&mut agent, Value::Object(callback), None);
    let root = agent.heap.add_root(Value::Object(registry));

    let target = new_object(&mut agent);
    let survivor = new_object(&mut agent);
    let unregistered = new_object(&mut agent);
    let token = new_object(&mut agent);
    let survivor_root = agent.heap.add_root(Value::Object(survivor));
    let token_root = agent.heap.add_root(Value::Object(token));
    weak::finalization_registry_register(
        &mut agent,
        registry,
        WeakKey::Object(target),
        Value::Number(42.0),
        None,
    );
    weak::finalization_registry_register(
        &mut agent,
        registry,
        WeakKey::Object(survivor),
        Value::Number(7.0),
        None,
    );
    weak::finalization_registry_register(
        &mut agent,
        registry,
        WeakKey::Object(unregistered),
        Value::Number(9.0),
        Some(WeakKey::Object(token)),
    );
    assert!(weak::finalization_registry_unregister(
        &mut agent,
        registry,
        WeakKey::Object(token)
    ));

    assert!(!agent.has_pending_host_tasks());
    collect(&mut agent);
    assert!(!agent.heap.object_exists(target));
    assert!(
        agent.heap.object_exists(callback),
        "the registry holds its callback"
    );
    assert!(agent.has_pending_host_tasks());
    assert!(
        log.borrow().is_empty(),
        "callbacks never run inside the collector"
    );
    let result = agent.run_host_task().expect("a cleanup task");
    assert!(result.is_ok());
    assert_eq!(*log.borrow(), vec![42.0]);
    assert!(!agent.has_pending_host_tasks());

    for r in [root, survivor_root, token_root] {
        agent.heap.remove_root(r);
    }
}

// ----- [[OwnPropertyKeys]] order -----

#[test]
fn own_property_keys_order_indices_strings_symbols() {
    let mut agent = setup();
    let o = new_object(&mut agent);
    let d1 = agent.heap.alloc_str("first");
    let s1 = agent.heap.alloc_symbol(Some(d1));
    let s2 = agent.heap.alloc_symbol(None);
    put(&mut agent, o, "b", Value::Number(1.0));
    ops::create_data_property_or_throw(&mut agent, o, PropertyKey::Symbol(s1), Value::Null)
        .unwrap();
    put(&mut agent, o, "2", Value::Number(2.0));
    put(&mut agent, o, "a", Value::Number(3.0));
    put(&mut agent, o, "0", Value::Number(4.0));
    ops::create_data_property_or_throw(&mut agent, o, PropertyKey::Symbol(s2), Value::Null)
        .unwrap();
    put(&mut agent, o, "10", Value::Number(5.0));
    put(&mut agent, o, "4294967295", Value::Number(6.0));
    put(&mut agent, o, "-1", Value::Number(7.0));
    put(&mut agent, o, "01", Value::Number(8.0));

    let keys = internal::own_property_keys(&mut agent, o).unwrap();
    let expected = vec![
        PropertyKey::Index(0),
        PropertyKey::Index(2),
        PropertyKey::Index(10),
        key(&mut agent, "b"),
        key(&mut agent, "a"),
        key(&mut agent, "4294967295"),
        key(&mut agent, "-1"),
        key(&mut agent, "01"),
        PropertyKey::Symbol(s1),
        PropertyKey::Symbol(s2),
    ];
    assert_eq!(keys, expected);
    assert!(matches!(
        key(&mut agent, "4294967295"),
        PropertyKey::String(_)
    ));
    assert_eq!(
        key(&mut agent, "4294967294"),
        PropertyKey::Index(4294967294)
    );
}

#[test]
fn exotic_objects_order_their_keys() {
    let mut agent = setup();
    let array = ops::create_array_from_list(&mut agent, &[Value::Number(1.0), Value::Number(2.0)]);
    put(&mut agent, array, "x", Value::Null);
    let keys = internal::own_property_keys(&mut agent, array).unwrap();
    let length = key(&mut agent, "length");
    let x = key(&mut agent, "x");
    assert_eq!(
        keys,
        vec![PropertyKey::Index(0), PropertyKey::Index(1), length, x]
    );

    let ab = string(&mut agent, "ab");
    let wrapper = ops::to_object(&mut agent, ab).unwrap();
    put(&mut agent, wrapper, "5", Value::Null);
    put(&mut agent, wrapper, "z", Value::Null);
    let keys = internal::own_property_keys(&mut agent, wrapper).unwrap();
    let z = key(&mut agent, "z");
    assert_eq!(
        keys,
        vec![
            PropertyKey::Index(0),
            PropertyKey::Index(1),
            PropertyKey::Index(5),
            length,
            z
        ]
    );
    let b = ops::get(&mut agent, wrapper, PropertyKey::Index(1)).unwrap();
    assert_eq!(text(&mut agent, b), "b");
    // String indices are non-writable: assignment fails silently.
    let ok = internal::set(
        &mut agent,
        wrapper,
        PropertyKey::Index(0),
        Value::Null,
        Value::Object(wrapper),
    )
    .unwrap();
    assert!(!ok);
}

#[test]
fn array_length_tracks_indices_and_truncates() {
    let mut agent = setup();
    let a = ops::create_array_from_list(
        &mut agent,
        &[Value::Number(1.0), Value::Number(2.0), Value::Number(3.0)],
    );
    assert_eq!(read(&mut agent, a, "length"), Value::Number(3.0));
    ops::create_data_property_or_throw(&mut agent, a, PropertyKey::Index(9), Value::Null).unwrap();
    assert_eq!(read(&mut agent, a, "length"), Value::Number(10.0));
    let length = key(&mut agent, "length");
    ops::set(&mut agent, a, length, Value::Number(1.0), true).unwrap();
    let keys = internal::own_property_keys(&mut agent, a).unwrap();
    assert_eq!(keys, vec![PropertyKey::Index(0), length]);
    assert!(ops::set(&mut agent, a, length, Value::Number(1.5), true).is_err());
    // A non-configurable element stops the truncation.
    let desc = PropertyDescriptor::data(Value::Null, true, true, false);
    ops::define_property_or_throw(&mut agent, a, PropertyKey::Index(4), desc).unwrap();
    let ok = internal::set(&mut agent, a, length, Value::Number(0.0), Value::Object(a)).unwrap();
    assert!(!ok);
    assert_eq!(read(&mut agent, a, "length"), Value::Number(5.0));
}

// ----- shapes and dictionary mode -----

#[test]
fn objects_with_the_same_keys_share_a_shape() {
    let mut agent = setup();
    let a = new_object(&mut agent);
    let b = new_object(&mut agent);
    let c = new_object(&mut agent);
    for o in [a, b] {
        put(&mut agent, o, "x", Value::Number(1.0));
        put(&mut agent, o, "y", Value::Number(2.0));
    }
    put(&mut agent, c, "y", Value::Number(2.0));
    put(&mut agent, c, "x", Value::Number(1.0));
    let shape_a = agent.heap.object_shape(a).expect("shaped");
    assert_eq!(agent.heap.object_shape(b), Some(shape_a));
    assert_ne!(agent.heap.object_shape(c), Some(shape_a));
    assert_eq!(agent.heap.shape(shape_a).len(), 2);

    // Assigning an existing property keeps the shape.
    put(&mut agent, a, "x", Value::Number(10.0));
    assert_eq!(agent.heap.object_shape(a), Some(shape_a));
    assert_eq!(read(&mut agent, a, "x"), Value::Number(10.0));

    // Deleting the newest property returns to the parent shape.
    let y = key(&mut agent, "y");
    assert!(internal::delete(&mut agent, b, y).unwrap());
    let parent = agent.heap.shape(shape_a).parent();
    assert_eq!(agent.heap.object_shape(b), parent);
    assert!(!agent.heap.is_dictionary(b));

    // Shape transitions survive a collection while used.
    let ra = agent.heap.add_root(Value::Object(a));
    let d = new_object(&mut agent);
    let rd = agent.heap.add_root(Value::Object(d));
    collect(&mut agent);
    put(&mut agent, d, "x", Value::Number(0.0));
    put(&mut agent, d, "y", Value::Number(0.0));
    assert_eq!(agent.heap.object_shape(d), Some(shape_a));
    agent.heap.remove_root(ra);
    agent.heap.remove_root(rd);
}

#[test]
fn delete_and_reconfiguration_switch_to_dictionary_mode() {
    let mut agent = setup();
    let o = new_object(&mut agent);
    for name in ["a", "b", "c"] {
        put(&mut agent, o, name, Value::Number(1.0));
    }
    let a = key(&mut agent, "a");
    assert!(internal::delete(&mut agent, o, a).unwrap());
    assert!(agent.heap.is_dictionary(o));
    assert_eq!(read(&mut agent, o, "a"), Value::Undefined);
    assert_eq!(read(&mut agent, o, "c"), Value::Number(1.0));
    put(&mut agent, o, "a", Value::Number(2.0));
    let keys = internal::own_property_keys(&mut agent, o).unwrap();
    let expected = vec![key(&mut agent, "b"), key(&mut agent, "c"), a];
    assert_eq!(keys, expected);

    let p = new_object(&mut agent);
    put(&mut agent, p, "x", Value::Number(1.0));
    put(&mut agent, p, "y", Value::Number(1.0));
    let x = key(&mut agent, "x");
    let hidden = PropertyDescriptor {
        enumerable: Some(false),
        ..PropertyDescriptor::default()
    };
    ops::define_property_or_throw(&mut agent, p, x, hidden).unwrap();
    assert!(agent.heap.is_dictionary(p));
    let desc = internal::get_own_property(&mut agent, p, x)
        .unwrap()
        .unwrap();
    assert_eq!(
        desc,
        PropertyDescriptor::data(Value::Number(1.0), true, false, true)
    );
    let keys = internal::own_property_keys(&mut agent, p).unwrap();
    let y = key(&mut agent, "y");
    assert_eq!(keys, vec![x, y]);
}

#[test]
fn many_properties_switch_to_dictionary_mode() {
    let mut agent = setup();
    let o = new_object(&mut agent);
    for i in 0..MAX_SHAPE_PROPERTIES {
        put(&mut agent, o, &format!("p{i}"), Value::Number(i as f64));
    }
    assert!(!agent.heap.is_dictionary(o));
    put(&mut agent, o, "last", Value::Null);
    assert!(agent.heap.is_dictionary(o));
    for i in 0..MAX_SHAPE_PROPERTIES {
        assert_eq!(
            read(&mut agent, o, &format!("p{i}")),
            Value::Number(i as f64)
        );
    }
    let keys = internal::own_property_keys(&mut agent, o).unwrap();
    assert_eq!(keys.len(), MAX_SHAPE_PROPERTIES + 1);
    assert_eq!(keys[0], key(&mut agent, "p0"));
    assert_eq!(*keys.last().unwrap(), key(&mut agent, "last"));
}

#[test]
fn frozen_objects_reject_changes() {
    let mut agent = setup();
    let o = new_object(&mut agent);
    put(&mut agent, o, "x", Value::Number(1.0));
    assert!(ops::set_integrity_level(&mut agent, o, ops::IntegrityLevel::Frozen).unwrap());
    assert!(ops::test_integrity_level(&mut agent, o, ops::IntegrityLevel::Frozen).unwrap());
    let x = key(&mut agent, "x");
    let err = ops::set(&mut agent, o, x, Value::Number(2.0), true).unwrap_err();
    assert_type_error(&mut agent, err);
    assert_eq!(read(&mut agent, o, "x"), Value::Number(1.0));
    let y = key(&mut agent, "y");
    assert!(!ops::create_data_property(&mut agent, o, y, Value::Null).unwrap());
}

#[test]
fn prototype_chains_and_cycles() {
    let mut agent = setup();
    let parent = new_object(&mut agent);
    let child = ops::ordinary_object_create(&mut agent, Some(parent));
    put(&mut agent, parent, "inherited", Value::Number(1.0));
    assert_eq!(read(&mut agent, child, "inherited"), Value::Number(1.0));
    let inherited = key(&mut agent, "inherited");
    assert!(ops::has_property(&mut agent, child, inherited).unwrap());
    assert!(!ops::has_own_property(&mut agent, child, inherited).unwrap());
    // Assignment creates an own property on the receiver.
    ops::set(&mut agent, child, inherited, Value::Number(2.0), true).unwrap();
    assert_eq!(read(&mut agent, parent, "inherited"), Value::Number(1.0));
    assert_eq!(read(&mut agent, child, "inherited"), Value::Number(2.0));
    // No cycles.
    assert!(!internal::set_prototype_of(&mut agent, parent, Some(child)).unwrap());
    // %Object.prototype% is an immutable prototype exotic object.
    let object_proto = agent.intrinsic(Intrinsic::ObjectPrototype);
    assert!(!internal::set_prototype_of(&mut agent, object_proto, Some(parent)).unwrap());
    assert!(internal::set_prototype_of(&mut agent, object_proto, None).unwrap());
}

#[test]
fn proxy_traps_and_invariants() {
    let mut agent = setup();
    let target = new_object(&mut agent);
    let fixed = PropertyDescriptor::data(Value::Number(1.0), false, true, false);
    let c = key(&mut agent, "c");
    ops::define_property_or_throw(&mut agent, target, c, fixed).unwrap();
    put(&mut agent, target, "d", Value::Number(2.0));

    // Without traps the proxy forwards to its target.
    let empty_handler = new_object(&mut agent);
    let plain = proxy_create(
        &mut agent,
        Value::Object(target),
        Value::Object(empty_handler),
    )
    .unwrap();
    assert_eq!(read(&mut agent, plain, "d"), Value::Number(2.0));
    let keys = internal::own_property_keys(&mut agent, plain).unwrap();
    let d = key(&mut agent, "d");
    assert_eq!(keys, vec![c, d]);

    // A get trap answers for configurable properties...
    let handler = new_object(&mut agent);
    let trap = constant_function(&mut agent, Value::Number(7.0));
    put(&mut agent, handler, "get", Value::Object(trap));
    let proxy = proxy_create(&mut agent, Value::Object(target), Value::Object(handler)).unwrap();
    assert_eq!(read(&mut agent, proxy, "d"), Value::Number(7.0));
    assert_eq!(read(&mut agent, proxy, "missing"), Value::Number(7.0));
    // ...but may not lie about a non-writable, non-configurable one.
    let err = ops::get(&mut agent, proxy, c).unwrap_err();
    assert_type_error(&mut agent, err);
    assert!(ops::is_array(&mut agent, Value::Object(proxy)).is_ok_and(|a| !a));
}

// ----- conversions -----

#[test]
fn to_number_and_to_string() {
    let mut agent = setup();
    let cases: [(&str, f64); 9] = [
        ("  12  ", 12.0),
        ("0x1F", 31.0),
        ("0b101", 5.0),
        ("1e3", 1000.0),
        ("", 0.0),
        (" \t\n", 0.0),
        ("-Infinity", f64::NEG_INFINITY),
        (".5", 0.5),
        ("\u{FEFF}7\u{2028}", 7.0),
    ];
    for (input, expected) in cases {
        let v = string(&mut agent, input);
        assert_eq!(
            ops::to_number(&mut agent, v).unwrap(),
            expected,
            "{input:?}"
        );
    }
    for input in ["abc", "+0x1", "1_0", "0x", "1e", "Infinityx"] {
        let v = string(&mut agent, input);
        assert!(ops::to_number(&mut agent, v).unwrap().is_nan(), "{input:?}");
    }
    let minus_zero = string(&mut agent, "-0");
    assert!(
        ops::to_number(&mut agent, minus_zero)
            .unwrap()
            .is_sign_negative()
    );
    assert_eq!(ops::to_number(&mut agent, Value::Null).unwrap(), 0.0);
    assert!(
        ops::to_number(&mut agent, Value::Undefined)
            .unwrap()
            .is_nan()
    );
    assert_eq!(
        ops::to_number(&mut agent, Value::Boolean(true)).unwrap(),
        1.0
    );

    let numbers: [(f64, &str); 7] = [
        (1e21, "1e+21"),
        (123.456, "123.456"),
        (-0.0, "0"),
        (0.000001, "0.000001"),
        (1e-7, "1e-7"),
        (f64::NAN, "NaN"),
        (f64::NEG_INFINITY, "-Infinity"),
    ];
    for (n, expected) in numbers {
        let s = ops::to_string(&mut agent, Value::Number(n)).unwrap();
        assert_eq!(text(&mut agent, Value::String(s)), expected);
    }
    let big = vjs::bigint::BigInt::parse_radix(b"123456789012345678901234567890", 10).unwrap();
    let big = Value::BigInt(agent.heap.alloc_bigint(big.neg()));
    let s = ops::to_string(&mut agent, big).unwrap();
    assert_eq!(
        text(&mut agent, Value::String(s)),
        "-123456789012345678901234567890"
    );
    let s = ops::to_string(&mut agent, Value::Boolean(false)).unwrap();
    assert_eq!(text(&mut agent, Value::String(s)), "false");

    let sym = agent.heap.alloc_symbol(None);
    let err = ops::to_string(&mut agent, Value::Symbol(sym)).unwrap_err();
    assert_type_error(&mut agent, err);
    let err = ops::to_number(&mut agent, Value::Symbol(sym)).unwrap_err();
    assert_type_error(&mut agent, err);
}

#[test]
fn integer_and_boolean_conversions() {
    let mut agent = setup();
    assert_eq!(
        ops::to_int32(&mut agent, Value::Number(4294967301.0)).unwrap(),
        5
    );
    assert_eq!(ops::to_int32(&mut agent, Value::Number(-1.5)).unwrap(), -1);
    assert_eq!(
        ops::to_uint32(&mut agent, Value::Number(-1.0)).unwrap(),
        4294967295
    );
    assert_eq!(ops::to_int8(&mut agent, Value::Number(200.0)).unwrap(), -56);
    assert_eq!(
        ops::to_uint8_clamp(&mut agent, Value::Number(2.5)).unwrap(),
        2
    );
    assert_eq!(
        ops::to_uint8_clamp(&mut agent, Value::Number(3.5)).unwrap(),
        4
    );
    assert_eq!(
        ops::to_int32(&mut agent, Value::Number(f64::NAN)).unwrap(),
        0
    );
    assert_eq!(
        ops::to_length(&mut agent, Value::Number(-5.0)).unwrap(),
        0.0
    );
    assert_eq!(
        ops::to_length(&mut agent, Value::Number(1e300)).unwrap(),
        9007199254740991.0
    );
    assert_eq!(
        ops::to_integer_or_infinity(&mut agent, Value::Number(-0.7)).unwrap(),
        0.0
    );
    assert!(ops::to_index(&mut agent, Value::Number(-1.0)).is_err());

    let empty = string(&mut agent, "");
    let zero = string(&mut agent, "0");
    let o = new_object(&mut agent);
    let sym = agent.heap.alloc_symbol(None);
    let falsy = [
        Value::Undefined,
        Value::Null,
        Value::Number(0.0),
        Value::Number(-0.0),
        Value::Number(f64::NAN),
        empty,
    ];
    for v in falsy {
        assert!(!ops::to_boolean(&agent, v), "{v:?}");
    }
    let truthy = [
        zero,
        Value::Object(o),
        Value::Symbol(sym),
        Value::Number(-1.0),
    ];
    for v in truthy {
        assert!(ops::to_boolean(&agent, v), "{v:?}");
    }
}

fn to_string_five(agent: &mut Agent, _call: NativeCall<'_>) -> JsResult<Value> {
    Ok(Value::String(agent.heap.alloc_str("five")))
}

#[test]
fn to_primitive_uses_value_of_and_to_string() {
    let mut agent = setup();
    let o = new_object(&mut agent);
    let value_of = constant_function(&mut agent, Value::Number(5.0));
    let to_string =
        create_native_function(&mut agent, to_string_five, 0, "toString", Vec::new(), None);
    put(&mut agent, o, "valueOf", Value::Object(value_of));
    put(&mut agent, o, "toString", Value::Object(to_string));
    let v = Value::Object(o);
    assert_eq!(
        ops::to_primitive(&mut agent, v, PreferredType::Number).unwrap(),
        Value::Number(5.0)
    );
    assert_eq!(
        ops::to_primitive(&mut agent, v, PreferredType::Default).unwrap(),
        Value::Number(5.0)
    );
    let s = ops::to_primitive(&mut agent, v, PreferredType::String).unwrap();
    assert_eq!(text(&mut agent, s), "five");
    assert_eq!(ops::to_number(&mut agent, v).unwrap(), 5.0);
    let s = ops::to_string(&mut agent, v).unwrap();
    assert_eq!(text(&mut agent, Value::String(s)), "five");
    assert!(ops::is_loosely_equal(&mut agent, v, Value::Number(5.0)).unwrap());

    // Symbol.toPrimitive wins, and must not return an object.
    let exotic = constant_function(&mut agent, Value::Object(o));
    let to_prim = PropertyKey::Symbol(agent.symbols.to_primitive);
    ops::create_data_property_or_throw(&mut agent, o, to_prim, Value::Object(exotic)).unwrap();
    let err = ops::to_primitive(&mut agent, v, PreferredType::Number).unwrap_err();
    assert_type_error(&mut agent, err);
}

#[test]
fn property_keys_from_values() {
    let mut agent = setup();
    let k = |agent: &mut Agent, v: Value| ops::to_property_key(agent, v).unwrap();
    assert_eq!(k(&mut agent, Value::Number(1.0)), PropertyKey::Index(1));
    assert_eq!(k(&mut agent, Value::Number(-0.0)), PropertyKey::Index(0));
    let one_and_half = key(&mut agent, "1.5");
    assert_eq!(k(&mut agent, Value::Number(1.5)), one_and_half);
    let big = key(&mut agent, "4294967295");
    assert_eq!(k(&mut agent, Value::Number(4294967295.0)), big);
    let s = string(&mut agent, "007");
    let leading_zeros = key(&mut agent, "007");
    assert_eq!(k(&mut agent, s), leading_zeros);
    let t = k(&mut agent, Value::Boolean(true));
    assert_eq!(t, key(&mut agent, "true"));
    assert_eq!(
        ops::canonical_numeric_index_string(&vjs::string::utf16("-0")).map(f64::is_sign_negative),
        Some(true)
    );
    assert_eq!(
        ops::canonical_numeric_index_string(&vjs::string::utf16("1.50")),
        None
    );
    assert_eq!(
        ops::canonical_numeric_index_string(&vjs::string::utf16("Infinity")),
        Some(f64::INFINITY)
    );
}

#[test]
fn to_object_wraps_primitives() {
    let mut agent = setup();
    let n = ops::to_object(&mut agent, Value::Number(3.0)).unwrap();
    assert!(matches!(agent.heap.object(n).kind, ObjectKind::Number(x) if x == 3.0));
    let number_proto = agent.intrinsic(Intrinsic::NumberPrototype);
    assert_eq!(agent.heap.object(n).proto(), Some(number_proto));
    let err = ops::to_object(&mut agent, Value::Undefined).unwrap_err();
    assert_type_error(&mut agent, err);
    assert!(ops::require_object_coercible(&mut agent, Value::Null).is_err());
}

#[test]
fn bigint_conversions() {
    let mut agent = setup();
    let hex = string(&mut agent, " 0x10 ");
    let b = ops::to_bigint(&mut agent, hex).unwrap();
    assert_eq!(agent.heap.bigint(b).to_string_radix(10), "16");
    let negative = string(&mut agent, "-12");
    assert_eq!(ops::to_bigint64(&mut agent, negative).unwrap(), -12);
    assert_eq!(
        ops::to_biguint64(&mut agent, negative).unwrap(),
        u64::MAX - 11
    );
    let bad = string(&mut agent, "1.5");
    let err = ops::to_bigint(&mut agent, bad).unwrap_err();
    let Value::Object(e) = err.0 else {
        panic!("expected an error object");
    };
    let syntax_proto = agent.intrinsic(Intrinsic::SyntaxErrorPrototype);
    assert_eq!(agent.heap.object(e).proto(), Some(syntax_proto));
    let err = ops::to_bigint(&mut agent, Value::Number(1.0)).unwrap_err();
    assert_type_error(&mut agent, err);
    let t = ops::to_bigint(&mut agent, Value::Boolean(true)).unwrap();
    assert_eq!(agent.heap.bigint(t).to_string_radix(10), "1");
}

#[test]
fn equality_and_relational_comparison() {
    let mut agent = setup();
    let one = string(&mut agent, "1");
    let hex = string(&mut agent, "0x10");
    let a = string(&mut agent, "a");
    let b = string(&mut agent, "b");
    let ten = string(&mut agent, "10");
    let nine = string(&mut agent, "9");
    let big_one = Value::BigInt(agent.heap.alloc_bigint(vjs::bigint::BigInt::from_i64(1)));
    let nan = Value::Number(f64::NAN);

    assert!(ops::is_loosely_equal(&mut agent, Value::Null, Value::Undefined).unwrap());
    assert!(!ops::is_loosely_equal(&mut agent, Value::Null, Value::Number(0.0)).unwrap());
    assert!(ops::is_loosely_equal(&mut agent, one, Value::Number(1.0)).unwrap());
    assert!(ops::is_loosely_equal(&mut agent, hex, Value::Number(16.0)).unwrap());
    assert!(ops::is_loosely_equal(&mut agent, big_one, one).unwrap());
    assert!(ops::is_loosely_equal(&mut agent, big_one, Value::Number(1.0)).unwrap());
    assert!(ops::is_loosely_equal(&mut agent, Value::Boolean(true), one).unwrap());
    assert!(!ops::is_loosely_equal(&mut agent, nan, nan).unwrap());
    assert!(!ops::is_strictly_equal(&mut agent, one, Value::Number(1.0)));

    let same_text = string(&mut agent, "a");
    assert!(ops::is_strictly_equal(&mut agent, a, same_text));
    assert!(!ops::same_value(
        &mut agent,
        Value::Number(0.0),
        Value::Number(-0.0)
    ));
    assert!(ops::same_value_zero(
        &mut agent,
        Value::Number(0.0),
        Value::Number(-0.0)
    ));
    assert!(ops::same_value(&mut agent, nan, nan));

    assert_eq!(
        ops::is_less_than(&mut agent, a, b, true).unwrap(),
        Some(true)
    );
    assert_eq!(
        ops::is_less_than(&mut agent, ten, nine, true).unwrap(),
        Some(true)
    );
    assert_eq!(
        ops::is_less_than(&mut agent, big_one, Value::Number(1.5), true).unwrap(),
        Some(true)
    );
    assert_eq!(
        ops::is_less_than(&mut agent, Value::Number(1.0), nan, true).unwrap(),
        None
    );
    let not_a_bigint = string(&mut agent, "x");
    assert_eq!(
        ops::is_less_than(&mut agent, big_one, not_a_bigint, true).unwrap(),
        None
    );
}

#[test]
fn errors_are_thrown_from_the_current_realm() {
    let mut agent = setup();
    let err = agent.type_error::<()>("boom").unwrap_err();
    let Value::Object(e) = err.0 else {
        panic!("expected an error object");
    };
    let message = read(&mut agent, e, "message");
    assert_eq!(text(&mut agent, message), "boom");
    let name = read(&mut agent, e, "name");
    assert_eq!(text(&mut agent, name), "TypeError");
    let type_error = agent.intrinsic(Intrinsic::TypeError);
    let error_proto = agent.intrinsic(Intrinsic::ErrorPrototype);
    let instance =
        ops::instanceof_operator(&mut agent, Value::Object(e), Value::Object(type_error)).unwrap();
    assert!(instance);
    let type_error_proto = agent.intrinsic(Intrinsic::TypeErrorPrototype);
    assert_eq!(
        agent.heap.object(type_error_proto).proto(),
        Some(error_proto)
    );

    // Constructing an error through its constructor.
    let msg = string(&mut agent, "made");
    let made = ops::construct(&mut agent, type_error, &[msg], None).unwrap();
    let message = read(&mut agent, made, "message");
    assert_eq!(text(&mut agent, message), "made");
    assert_eq!(agent.heap.object(made).proto(), Some(type_error_proto));
}
