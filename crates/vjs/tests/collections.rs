//! Arrays, iterators and keyed collections through scripts: stability of
//! `sort`, holes in sparse arrays across the prototype methods, closing of
//! the underlying iterator by the iterator helpers, insertion order of
//! `Map` and `Set` with deletions during iteration, and a `WeakRef` whose
//! target is freed by a collection.

use vjs::agent::Agent;
use vjs::ops;
use vjs::value::{PropertyKey, Value};
use vjs::{Throw, evaluate_script};

const PRELUDE: &str = r#"
function fail(msg) { throw new Error(msg); }
function str(v) { return typeof v === "symbol" ? v.toString() : "" + v; }
function same(actual, expected, what) {
  if (!Object.is(actual, expected)) {
    fail(what + ": expected " + str(expected) + " (" + typeof expected + "), got " +
         str(actual) + " (" + typeof actual + ")");
  }
}
function list(actual, expected, what) {
  same(actual.length, expected.length, what + " length");
  for (var i = 0; i < expected.length; i++) same(actual[i], expected[i], what + " [" + i + "]");
}
function holes(a, expected, what) {
  same(a.length, expected.length, what + " length");
  for (var i = 0; i < expected.length; i++) {
    same(i in a, expected[i], what + " has " + i);
  }
}
function throws(C, f, what) {
  try { f(); } catch (e) {
    if (!(e instanceof C)) fail(what + ": wrong error " + e);
    return;
  }
  fail(what + ": did not throw");
}
// An iterator over `values` that records how often `return` is called.
function tracked(values) {
  var it = {
    i: 0, returned: 0,
    next: function () {
      return this.i < values.length ? { value: values[this.i++], done: false }
                                    : { value: undefined, done: true };
    },
    return: function () { this.returned++; return {}; },
  };
  Object.setPrototypeOf(it, Iterator.prototype);
  return it;
}
"#;

fn show(agent: &mut Agent, v: Value) -> String {
    match ops::to_string(agent, v) {
        Ok(s) => agent.heap.string_to_rust(s),
        Err(_) => "<unprintable>".to_string(),
    }
}

fn show_throw(agent: &mut Agent, Throw(v): Throw) -> String {
    if let Value::Object(o) = v {
        let name: PropertyKey = agent.heap.key_from_str("name");
        let message: PropertyKey = agent.heap.key_from_str("message");
        if let (Ok(n), Ok(m)) = (ops::get(agent, o, name), ops::get(agent, o, message)) {
            return format!("throw {}: {}", show(agent, n), show(agent, m));
        }
    }
    format!("throw {}", show(agent, v))
}

fn eval(agent: &mut Agent, src: &str) -> String {
    match evaluate_script(agent, src) {
        Ok(v) => show(agent, v),
        Err(e) => show_throw(agent, e),
    }
}

fn ok(src: &str) {
    let mut agent = Agent::new();
    agent.initialize_host_defined_realm();
    let source = format!("{PRELUDE}\n{src}");
    assert_eq!(eval(&mut agent, &source), "ok", "script:\n{src}");
}

// ----- sort -----

#[test]
fn sort_is_stable() {
    ok(r#"
      var a = [];
      for (var i = 0; i < 200; i++) a.push({ key: i % 7, order: i });
      a.sort(function (x, y) { return x.key - y.key; });
      for (var i = 1; i < a.length; i++) {
        if (a[i - 1].key === a[i].key && a[i - 1].order > a[i].order) fail("unstable at " + i);
        if (a[i - 1].key > a[i].key) fail("unsorted at " + i);
      }
      var b = [];
      for (var i = 0; i < 50; i++) b.push({ key: i % 3, order: i });
      var c = b.toSorted(function (x, y) { return x.key - y.key; });
      same(b[1].order, 1, "toSorted copies");
      for (var i = 1; i < c.length; i++) {
        if (c[i - 1].key === c[i].key && c[i - 1].order > c[i].order) fail("toSorted unstable");
      }
      "ok";
    "#);
}

#[test]
fn sort_default_order_and_undefined_and_holes() {
    ok(r#"
      var a = [10, 9, , 1, undefined, 2, , "b", "a"];
      a.sort();
      list(a.slice(0, 7), [1, 10, 2, 9, "a", "b", undefined], "default compare is by string");
      holes(a, [true, true, true, true, true, true, true, false, false], "holes go last");
      list([3, 1, 2].sort(function (x, y) { return y - x; }), [3, 2, 1], "comparator");
      throws(TypeError, function () { [].sort(1); }, "non-callable comparator");
      "ok";
    "#);
}

// ----- sparse arrays -----

#[test]
fn sparse_arrays_skip_holes_in_iteration_methods() {
    ok(r#"
      var a = [0, , 2, , 4];
      var seen = [];
      a.forEach(function (v, i) { seen.push(i); });
      list(seen, [0, 2, 4], "forEach");
      holes(a.map(function (v) { return v * 2; }), [true, false, true, false, true], "map keeps holes");
      list(a.filter(function () { return true; }), [0, 2, 4], "filter");
      same(a.reduce(function (s, v) { return s + v; }), 6, "reduce");
      same(a.reduceRight(function (s, v) { return s + "," + v; }), "4,2,0", "reduceRight");
      same(a.indexOf(undefined), -1, "indexOf skips holes");
      same(a.includes(undefined), true, "includes reads holes");
      same(a.findIndex(function (v) { return v === undefined; }), 1, "findIndex reads holes");
      same(a.findLast(function (v) { return v === undefined; }), undefined, "findLast");
      same(a.findLastIndex(function (v) { return v === undefined; }), 3, "findLastIndex");
      same(a.lastIndexOf(undefined), -1, "lastIndexOf skips holes");
      same(a.at(-1), 4, "at");
      same(a.join("-"), "0--2--4", "join");
      "ok";
    "#);
}

#[test]
fn sparse_arrays_in_mutating_and_copying_methods() {
    ok(r#"
      var a = [1, , 3];
      a.reverse();
      holes(a, [true, false, true], "reverse keeps holes");
      list(a, [3, undefined, 1], "reverse values");
      holes([1, , 3].slice(0), [true, false, true], "slice keeps holes");
      holes([1, , 3].concat([4, , 6]), [true, false, true, true, false, true], "concat");
      holes([1, , 3].toReversed(), [true, true, true], "toReversed fills holes");
      holes([1, , 3].toSpliced(0, 0), [true, true, true], "toSpliced fills holes");
      holes([1, , 3].with(0, 9), [true, true, true], "with fills holes");
      holes([, 1].toSorted(), [true, true], "toSorted fills holes");
      list([[1, , 3], , [4]].flat(), [1, 3, 4], "flat skips holes");
      list([1, , 2].flatMap(function (v) { return [v, v]; }), [1, 1, 2, 2], "flatMap");
      var b = [1, , 3, , 5];
      b.copyWithin(0, 1, 3);
      holes(b, [false, true, true, false, true], "copyWithin moves holes");
      var c = [];
      c[1e6] = 1;
      same(c.length, 1000001, "huge sparse length");
      var count = 0;
      c.forEach(function () { count++; });
      same(count, 1, "huge sparse forEach");
      same(c.indexOf(1), 1e6, "huge sparse indexOf");
      c.length = 10;
      same(c[1e6], undefined, "length truncates");
      "ok";
    "#);
}

#[test]
fn array_length_semantics_and_statics() {
    ok(r#"
      var a = [1, 2, 3];
      a[9] = 1;
      same(a.length, 10, "index write grows length");
      a.length = 1;
      list(a, [1], "length truncation");
      throws(RangeError, function () { a.length = -1; }, "invalid length");
      Object.defineProperty(a, "length", { writable: false });
      throws(TypeError, function () { "use strict"; a.push(1); }, "push on frozen length");
      same(Array.isArray([]), true, "isArray");
      same(Array.isArray(new Proxy([], {})), true, "isArray through proxy");
      list(Array.of(7, 8), [7, 8], "of");
      list(Array.from("ab"), ["a", "b"], "from iterable");
      list(Array.from({ length: 2, 0: "x" }), ["x", undefined], "from array-like");
      same(Array.prototype[Symbol.unscopables].toSorted, true, "unscopables");
      same(Object.getPrototypeOf(Array.prototype[Symbol.unscopables]), null, "unscopables proto");
      var it = [1][Symbol.iterator]();
      same(Object.prototype.toString.call(it), "[object Array Iterator]", "array iterator tag");
      same(Object.getPrototypeOf(Object.getPrototypeOf(it)), Iterator.prototype, "array iterator proto");
      "ok";
    "#);
}

// ----- iterator helpers -----

#[test]
fn helpers_close_the_underlying_iterator() {
    ok(r#"
      var it = tracked([1, 2, 3, 4]);
      list(it.take(2).toArray(), [1, 2], "take");
      same(it.returned, 1, "take closes when the limit is reached");

      it = tracked([1, 2, 3]);
      same(it.find(function (v) { return v === 2; }), 2, "find");
      same(it.returned, 1, "find closes on a match");

      it = tracked([1, 2, 3]);
      same(it.some(function (v) { return v > 1; }), true, "some");
      same(it.returned, 1, "some closes");

      it = tracked([1, 2, 3]);
      same(it.every(function (v) { return v < 2; }), false, "every");
      same(it.returned, 1, "every closes");

      it = tracked([1, 2, 3]);
      throws(Error, function () {
        it.map(function () { throw new Error("boom"); }).next();
      }, "map callback throws");
      same(it.returned, 1, "map closes when the callback throws");

      it = tracked([1, 2, 3]);
      var h = it.filter(function () { return true; });
      h.next();
      h.return();
      same(it.returned, 1, "helper.return closes");
      h.return();
      same(it.returned, 1, "a finished helper does not close again");

      it = tracked([1, 2, 3]);
      throws(RangeError, function () { it.take(-1); }, "take validates");
      same(it.returned, 1, "argument validation closes");

      it = tracked([1, 2, 3]);
      list(it.drop(1).toArray(), [2, 3], "drop runs to the end");
      same(it.returned, 0, "exhaustion does not close");

      it = tracked([1]);
      throws(TypeError, function () { it.flatMap(function () { return 1; }).next(); }, "flatMap rejects primitives");
      same(it.returned, 1, "flatMap closes on a bad inner value");
      "ok";
    "#);
}

#[test]
fn iterator_constructor_from_and_concat() {
    ok(r#"
      throws(TypeError, function () { new Iterator(); }, "Iterator is abstract");
      throws(TypeError, function () { Iterator(); }, "Iterator needs new");
      var w = Iterator.from({ next: function () { return { done: true }; } });
      same(Object.getPrototypeOf(Object.getPrototypeOf(w)), Iterator.prototype, "wrapped");
      list(Iterator.from("ab").toArray(), ["a", "b"], "from string");
      list(Iterator.concat([1], new Set([2, 3])).toArray(), [1, 2, 3], "concat");
      same([1, 2, 3].values().reduce(function (s, v) { return s + v; }), 6, "reduce");
      var sum = 0;
      [1, 2].values().forEach(function (v) { sum += v; });
      same(sum, 3, "forEach");
      same(Iterator.prototype[Symbol.toStringTag], "Iterator", "toStringTag");
      "ok";
    "#);
}

// ----- Map and Set -----

#[test]
fn map_order_with_deletions_during_iteration() {
    ok(r#"
      var m = new Map([[1, "a"], [2, "b"], [3, "c"], [4, "d"]]);
      var seen = [];
      m.forEach(function (v, k) {
        seen.push(k);
        if (k === 1) { m.delete(2); m.set(5, "e"); }
        if (k === 3 && v === "c") { m.delete(3); m.set(3, "again"); }
      });
      list(seen, [1, 3, 4, 5, 3], "forEach order");
      list(Array.from(m.keys()), [1, 4, 5, 3], "keys after");

      m = new Map([["x", 1], ["y", 2]]);
      var it = m.entries();
      same(it.next().value[0], "x", "first entry");
      m.clear();
      m.set("z", 3);
      same(it.next().value[0], "z", "iterator sees entries added after clear");
      same(it.next().done, true, "done");
      same(it.next().done, true, "stays done");

      m = new Map();
      m.set(-0, 1);
      same(Object.is(m.keys().next().value, 0), true, "-0 key normalized");
      m.set(NaN, 2);
      same(m.get(NaN), 2, "NaN key");
      "ok";
    "#);
}

#[test]
fn set_order_with_deletions_during_iteration() {
    ok(r#"
      var s = new Set([1, 2, 3]);
      var seen = [];
      for (var v of s) {
        seen.push(v);
        if (v === 1 && seen.length === 1) { s.delete(1); s.delete(2); s.add(4); s.add(1); }
      }
      list(seen, [1, 3, 4, 1], "for-of order");
      list([...s], [3, 4, 1], "final order");
      same(Set.prototype.keys, Set.prototype.values, "keys is values");
      same(Set.prototype[Symbol.iterator], Set.prototype.values, "@@iterator is values");
      "ok";
    "#);
}

#[test]
fn set_methods() {
    ok(r#"
      var a = new Set([1, 2, 3]);
      var b = new Set([3, 4]);
      list([...a.union(b)], [1, 2, 3, 4], "union");
      list([...a.intersection(b)], [3], "intersection");
      list([...a.difference(b)], [1, 2], "difference");
      list([...a.symmetricDifference(b)], [1, 2, 4], "symmetricDifference");
      same(a.isSubsetOf(new Set([1, 2, 3, 4])), true, "isSubsetOf");
      same(a.isSupersetOf(new Set([1])), true, "isSupersetOf");
      same(a.isDisjointFrom(new Set([9])), true, "isDisjointFrom");
      var setLike = { size: 1, has: function (v) { return v === 2; }, keys: function () { return [2].values(); } };
      list([...a.intersection(setLike)], [2], "set-like argument");
      throws(TypeError, function () { a.union([1]); }, "arrays are not set-like");
      throws(TypeError, function () { a.union({ size: NaN, has: function () {}, keys: function () {} }); }, "NaN size");
      throws(RangeError, function () { a.union({ size: -1, has: function () {}, keys: function () {} }); }, "negative size");
      "ok";
    "#);
}

#[test]
fn group_by_and_upsert() {
    ok(r#"
      var g = Map.groupBy([1, 2, 3, 4], function (v) { return v % 2 ? "odd" : "even"; });
      list([...g.keys()], ["odd", "even"], "groupBy key order");
      list(g.get("even"), [2, 4], "groupBy values");
      var m = new Map([["a", 1]]);
      same(m.getOrInsert("a", 2), 1, "getOrInsert existing");
      same(m.getOrInsert("b", 2), 2, "getOrInsert new");
      var calls = 0;
      same(m.getOrInsertComputed("c", function (k) { calls++; return k + "!"; }), "c!", "computed");
      same(m.getOrInsertComputed("c", function () { calls++; }), "c!", "computed existing");
      same(calls, 1, "callback runs once");
      var wm = new WeakMap();
      var k = {};
      same(wm.getOrInsert(k, 5), 5, "WeakMap getOrInsert");
      throws(TypeError, function () { wm.getOrInsert(1, 5); }, "WeakMap rejects primitives");
      "ok";
    "#);
}

// ----- weak collections and references -----

#[test]
fn weak_collections_accept_only_weakly_holdable_keys() {
    ok(r#"
      var ws = new WeakSet();
      var o = {};
      ws.add(o);
      same(ws.has(o), true, "WeakSet has");
      same(ws.delete(o), true, "WeakSet delete");
      throws(TypeError, function () { ws.add(1); }, "WeakSet rejects numbers");
      throws(TypeError, function () { ws.add(Symbol.for("r")); }, "registered symbols cannot be held");
      ws.add(Symbol("unregistered"));
      var wm = new WeakMap([[o, 1]]);
      same(wm.get(o), 1, "WeakMap get");
      throws(TypeError, function () { new WeakRef(1); }, "WeakRef rejects numbers");
      same(new WeakRef(o).deref(), o, "deref");
      throws(TypeError, function () { new FinalizationRegistry(1); }, "registry needs a callback");
      var r = new FinalizationRegistry(function () {});
      throws(TypeError, function () { r.register(o, o); }, "target and held value must differ");
      same(r.unregister(o), false, "unregister without registrations");
      "ok";
    "#);
}

#[test]
fn weak_ref_is_cleared_after_collection() {
    let mut agent = Agent::new();
    agent.initialize_host_defined_realm();
    let setup = r#"
      var kept = {};
      var refs = { live: new WeakRef(kept), dead: new WeakRef({}) };
      var wm = new WeakMap();
      wm.set(kept, 1);
      "ok";
    "#;
    assert_eq!(eval(&mut agent, setup), "ok");
    // The end of the job clears [[KeptAlive]]; then nothing else holds
    // the target of `refs.dead`.
    agent.clear_kept_objects();
    agent
        .collect_garbage(&[])
        .expect("no native call is in progress");
    let check = format!(
        "{PRELUDE}\n{}",
        r#"
      same(refs.live.deref(), kept, "a reachable target survives");
      same(refs.dead.deref(), undefined, "an unreachable target is freed");
      same(wm.get(kept), 1, "WeakMap entry of a live key survives");
      "ok";
    "#
    );
    assert_eq!(eval(&mut agent, &check), "ok");
}

#[test]
fn finalization_registry_cleanup_runs_after_collection() {
    let mut agent = Agent::new();
    agent.initialize_host_defined_realm();
    let setup = r#"
      var cleaned = [];
      var registry = new FinalizationRegistry(function (held) { cleaned.push(held); });
      registry.register({}, "gone");
      var token = {};
      registry.register({}, "unregistered", token);
      registry.unregister(token);
      "ok";
    "#;
    assert_eq!(eval(&mut agent, setup), "ok");
    agent.clear_kept_objects();
    agent
        .collect_garbage(&[])
        .expect("no native call is in progress");
    while let Some(result) = agent.run_host_task() {
        assert!(result.is_ok(), "cleanup threw");
    }
    let check = format!(
        "{PRELUDE}\n{}",
        r#"
      list(cleaned, ["gone"], "cleanup callbacks");
      "ok";
    "#
    );
    assert_eq!(eval(&mut agent, &check), "ok");
}
