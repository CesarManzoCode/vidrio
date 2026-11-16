//! Promise, async iteration and JSON through scripts: the job order of
//! `Promise.all` against chained `then`s, `finally` with values, closing
//! of a sync iterator behind an async-from-sync iterator, `JSON.stringify`
//! with cycles, gaps and lone surrogates, and `JSON.parse` with a reviver
//! and the source text access.

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
  same(actual.length, expected.length, what + " length (" + actual + ")");
  for (var i = 0; i < expected.length; i++) same(actual[i], expected[i], what + " [" + i + "]");
}
function throws(C, f, what) {
  try { f(); } catch (e) {
    if (!(e instanceof C)) fail(what + ": wrong error " + e);
    return;
  }
  fail(what + ": did not throw");
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

/// Runs `src` (which must evaluate to "ok") in a fresh realm.
fn ok(src: &str) {
    let mut agent = Agent::new();
    agent.initialize_host_defined_realm();
    let source = format!("{PRELUDE}\n{src}");
    assert_eq!(eval(&mut agent, &source), "ok", "script:\n{src}");
}

/// Runs `src`, drains the job queue, then runs `check` (which must
/// evaluate to "ok").
fn ok_after_jobs(src: &str, check: &str) {
    let mut agent = Agent::new();
    agent.initialize_host_defined_realm();
    let source = format!("{PRELUDE}\n{src}");
    let first = eval(&mut agent, &source);
    assert!(!first.starts_with("throw"), "script threw: {first}\n{src}");
    let errors = agent.run_jobs();
    assert!(errors.is_empty(), "jobs threw:\n{src}");
    assert_eq!(
        eval(&mut agent, check),
        "ok",
        "check:\n{check}\nafter:\n{src}"
    );
}

// ----- Promise job order -----

#[test]
fn promise_all_settles_after_its_element_jobs() {
    ok_after_jobs(
        r#"
        var log = [];
        var p1 = Promise.resolve(1), p2 = Promise.resolve(2);
        Promise.all([p1, p2]).then(function (v) { log.push("all:" + v.join()); });
        p1.then(function () { log.push("a1"); })
          .then(function () { log.push("a2"); })
          .then(function () { log.push("a3"); });
        "#,
        r#"list(log, ["a1", "all:1,2", "a2", "a3"], "order"); "ok";"#,
    );
}

#[test]
fn chained_thens_interleave_and_returned_promises_cost_two_jobs() {
    ok_after_jobs(
        r#"
        var log = [];
        Promise.resolve().then(function () { log.push("x1"); })
                         .then(function () { log.push("x2"); });
        Promise.resolve().then(function () { log.push("y1"); })
                         .then(function () { log.push("y2"); });
        var log2 = [];
        Promise.resolve().then(function () { log2.push("r1"); return Promise.resolve(); })
                         .then(function () { log2.push("r2"); });
        Promise.resolve().then(function () { log2.push("s1"); })
                         .then(function () { log2.push("s2"); })
                         .then(function () { log2.push("s3"); })
                         .then(function () { log2.push("s4"); });
        "#,
        r#"
        list(log, ["x1", "y1", "x2", "y2"], "interleaved");
        list(log2, ["r1", "s1", "s2", "s3", "r2", "s4"], "thenable resolution");
        "ok";
        "#,
    );
}

#[test]
fn promise_combinators() {
    ok_after_jobs(
        r#"
        var r = {};
        Promise.all([]).then(function (v) { r.empty = Array.isArray(v) && v.length === 0; });
        Promise.all([1, Promise.reject("no"), 3]).catch(function (e) { r.allRejected = e; });
        Promise.allSettled([Promise.resolve(1), Promise.reject(2)]).then(function (v) {
          r.settled = v.map(function (o) { return o.status + ":" + (o.value || o.reason); }).join();
        });
        Promise.any([Promise.reject(1), Promise.reject(2)]).catch(function (e) {
          r.anyError = e instanceof AggregateError && e.errors.join() === "1,2";
        });
        Promise.any([Promise.reject(1), Promise.resolve(2)]).then(function (v) { r.any = v; });
        Promise.race([new Promise(function () {}), Promise.resolve("fast")]).then(function (v) { r.race = v; });
        Promise.try(function (a, b) { return a + b; }, 2, 3).then(function (v) { r.try = v; });
        Promise.try(function () { throw "thrown"; }).catch(function (e) { r.tryThrow = e; });
        var w = Promise.withResolvers();
        w.promise.then(function (v) { r.withResolvers = v; });
        w.resolve("wr");
        Promise.all(5).catch(function (e) { r.notIterable = e instanceof TypeError; });
        "#,
        r#"
        same(r.empty, true, "all of nothing");
        same(r.allRejected, "no", "all rejects with the first reason");
        same(r.settled, "fulfilled:1,rejected:2", "allSettled");
        same(r.anyError, true, "any rejects with an AggregateError");
        same(r.any, 2, "any");
        same(r.race, "fast", "race");
        same(r.try, 5, "try passes arguments");
        same(r.tryThrow, "thrown", "try catches");
        same(r.withResolvers, "wr", "withResolvers");
        same(r.notIterable, true, "all of a non-iterable rejects");
        "ok";
        "#,
    );
}

#[test]
fn promise_statics_use_this_and_species() {
    ok(r#"
      same(Promise[Symbol.species], Promise, "species getter");
      class MyPromise extends Promise {}
      same(MyPromise.resolve(1) instanceof MyPromise, true, "resolve uses this");
      same(MyPromise.all([]) instanceof MyPromise, true, "all uses this");
      same(MyPromise.resolve(1).finally(function () {}) instanceof MyPromise, true, "finally species");
      var p = Promise.resolve(1);
      same(Promise.resolve(p), p, "resolve returns a promise of the same constructor");
      throws(TypeError, function () { Promise.resolve.call(1, 2); }, "resolve on a primitive");
      throws(TypeError, function () { Promise.reject.call({}, 2); }, "reject on a non-constructor");
      throws(TypeError, function () { Promise.prototype.finally.call(1); }, "finally on a primitive");
      list([Promise.all.length, Promise.prototype.finally.length, Promise.withResolvers.length,
            Promise.try.length], [1, 1, 0, 1], "lengths");
      "ok";
    "#);
}

#[test]
fn finally_passes_values_through() {
    ok_after_jobs(
        r#"
        var r = {};
        Promise.resolve(5).finally(function () { r.args = arguments.length; return 9; })
          .then(function (v) { r.kept = v; });
        Promise.reject("e").finally(function () { return 9; })
          .catch(function (e) { r.reason = e; });
        Promise.resolve(5).finally(function () { throw 7; })
          .catch(function (e) { r.thrown = e; });
        Promise.resolve(1).finally(function () { return Promise.reject(8); })
          .catch(function (e) { r.rejected = e; });
        Promise.resolve(3).finally(42).then(function (v) { r.notCallable = v; });
        var order = [];
        Promise.resolve().finally(function () { order.push("f"); }).then(function () { order.push("after"); });
        Promise.resolve().then(function () { order.push("t1"); })
          .then(function () { order.push("t2"); })
          .then(function () { order.push("t3"); });
        "#,
        r#"
        same(r.args, 0, "onFinally gets no arguments");
        same(r.kept, 5, "fulfillment value kept");
        same(r.reason, "e", "rejection reason kept");
        same(r.thrown, 7, "a throw in onFinally wins");
        same(r.rejected, 8, "a rejected promise from onFinally wins");
        same(r.notCallable, 3, "non-callable onFinally passes through");
        list(order, ["f", "t1", "t2", "t3", "after"], "finally costs extra jobs");
        "ok";
        "#,
    );
}

// ----- async iteration over sync iterators -----

#[test]
fn async_from_sync_closes_the_sync_iterator() {
    ok_after_jobs(
        r#"
        function iterable(value, counter) {
          var it = {
            next: function () { return { value: value, done: false }; },
            return: function () { counter.closed++; return {}; },
          };
          var o = {};
          o[Symbol.iterator] = function () { return it; };
          return o;
        }
        var rejected = { closed: 0 }, broken = { closed: 0 };
        var r = {};
        (async function () {
          try { for await (var v of iterable(Promise.reject("boom"), rejected)) {} }
          catch (e) { r.caught = e; }
        })();
        (async function () {
          for await (var v of iterable(1, broken)) { r.first = v; break; }
        })();
        "#,
        r#"
        same(r.caught, "boom", "a rejected value rejects the loop");
        same(rejected.closed, 1, "rejected value closes the sync iterator");
        same(r.first, 1, "first value");
        same(broken.closed, 1, "break closes the sync iterator");
        "ok";
        "#,
    );
}

#[test]
fn async_from_sync_throw_without_throw_method_closes() {
    ok_after_jobs(
        r#"
        var counter = { closed: 0 };
        var inner = {
          next: function () { return { value: 1, done: false }; },
          return: function () { counter.closed++; return {}; },
        };
        var syncIterable = {};
        syncIterable[Symbol.iterator] = function () { return inner; };
        async function* g() { yield* syncIterable; }
        var it = g();
        var r = {};
        it.next().then(function (v) { r.first = v.value; });
        it.throw(new Error("x")).catch(function (e) { r.error = e instanceof TypeError; });
        "#,
        r#"
        same(r.first, 1, "delegated value");
        same(r.error, true, "missing throw rejects with a TypeError");
        same(counter.closed, 1, "missing throw closes the sync iterator");
        "ok";
        "#,
    );
}

#[test]
fn generator_function_constructors_and_prototypes() {
    ok(r#"
      var GeneratorFunction = Object.getPrototypeOf(function* () {}).constructor;
      var AsyncFunction = Object.getPrototypeOf(async function () {}).constructor;
      var AsyncGeneratorFunction = Object.getPrototypeOf(async function* () {}).constructor;
      same(Object.getPrototypeOf(GeneratorFunction), Function, "GeneratorFunction parent");
      same(GeneratorFunction.prototype[Symbol.toStringTag], "GeneratorFunction", "tag");
      same(AsyncFunction.prototype[Symbol.toStringTag], "AsyncFunction", "tag");
      same(AsyncGeneratorFunction.prototype.prototype[Symbol.toStringTag], "AsyncGenerator", "tag");
      var g = new GeneratorFunction("a", "yield a; yield a * 2;");
      var it = g(3);
      same(it.next().value, 3, "first");
      same(it.next().value, 6, "second");
      same(it.next().done, true, "completed");
      same(it.next().done, true, "stays completed");
      var running = function* () { it2.next(); };
      var it2 = running();
      throws(TypeError, function () { it2.next(); }, "re-entering a running generator");
      var fresh = (function* () { yield 1; })();
      same(fresh.return(4).value, 4, "return before start");
      same(fresh.next().done, true, "return completes");
      var AsyncIteratorPrototype = Object.getPrototypeOf(AsyncGeneratorFunction.prototype.prototype);
      same(AsyncIteratorPrototype[Symbol.asyncIterator].call(AsyncIteratorPrototype),
           AsyncIteratorPrototype, "%AsyncIteratorPrototype%[Symbol.asyncIterator] returns this");
      same(Object.getPrototypeOf(AsyncIteratorPrototype), Object.prototype, "AsyncIteratorPrototype parent");
      "ok";
    "#);
}

// ----- JSON.stringify -----

#[test]
fn stringify_detects_cycles() {
    ok(r#"
      var a = {}; a.self = a;
      throws(TypeError, function () { JSON.stringify(a); }, "object cycle");
      var b = []; b.push([b]);
      throws(TypeError, function () { JSON.stringify(b); }, "array cycle");
      var c = {};
      c.d = { toJSON: function () { return c; } };
      throws(TypeError, function () { JSON.stringify(c); }, "cycle through toJSON");
      var shared = {};
      same(JSON.stringify([shared, { s: shared }]), '[{},{"s":{}}]', "shared is not a cycle");
      "ok";
    "#);
}

#[test]
fn stringify_with_gaps() {
    ok(r#"
      same(JSON.stringify({ a: [1, { b: 2 }], c: {}, d: [] }, null, 2),
           '{\n  "a": [\n    1,\n    {\n      "b": 2\n    }\n  ],\n  "c": {},\n  "d": []\n}', "two spaces");
      same(JSON.stringify([1], null, "abcdefghijkl"), "[\nabcdefghij1\n]", "string gap clipped to 10");
      same(JSON.stringify([1], null, 20), "[\n          1\n]", "number gap clipped to 10");
      same(JSON.stringify([1], null, new Number(1)), "[\n 1\n]", "Number object gap");
      same(JSON.stringify([1], null, new String("--")), "[\n--1\n]", "String object gap");
      same(JSON.stringify([1, 2], null, 0.9), "[1,2]", "gap below one");
      same(JSON.stringify({ a: 1 }, null, "\t"), '{\n\t"a": 1\n}', "tab gap");
      "ok";
    "#);
}

#[test]
fn stringify_escapes_lone_surrogates() {
    ok(r#"
      same(JSON.stringify("\uD800"), '"\\ud800"', "lone high");
      same(JSON.stringify("\uDEAD"), '"\\udead"', "lone low");
      same(JSON.stringify("\uDF06\uD834"), '"\\udf06\\ud834"', "reversed pair");
      same(JSON.stringify("𝌆"), '"𝌆"', "pair kept");
      same(JSON.stringify("a\u0001\b\u001f\"\\/"), '"a\\u0001\\b\\u001f\\"\\\\/"', "controls");
      same(JSON.stringify({ "\uD800": 1 }), '{"\\ud800":1}', "keys too");
      "ok";
    "#);
}

#[test]
fn stringify_values_replacers_and_to_json() {
    ok(r#"
      same(JSON.stringify(undefined), undefined, "undefined");
      same(JSON.stringify(function () {}), undefined, "function");
      same(JSON.stringify({ a: undefined, f: function () {}, s: Symbol() }), "{}", "skipped members");
      same(JSON.stringify([undefined, function () {}, Symbol()]), "[null,null,null]", "null elements");
      same(JSON.stringify([NaN, -Infinity, -0, 1e21]), "[null,null,0,1e+21]", "numbers");
      same(JSON.stringify([new Number(3), new String("s"), new Boolean(false)]), '[3,"s",false]', "wrappers");
      throws(TypeError, function () { JSON.stringify(1n); }, "BigInt");
      same(JSON.stringify({ a: 1, b: 2, c: { a: 3, d: 4 } }, ["a", "c", "a"]), '{"a":1,"c":{"a":3}}', "property list");
      same(JSON.stringify({ 1: "x", b: 2 }, [1, new String("b")]), '{"1":"x","b":2}', "list of numbers and wrappers");
      var keys = [];
      JSON.stringify({ a: [1] }, function (k, v) { keys.push(k); return v; });
      list(keys, ["", "a", "0"], "replacer keys");
      same(JSON.stringify({ a: 1, b: "x" }, function (k, v) { return typeof v === "number" ? v * 2 : v; }),
           '{"a":2,"b":"x"}', "replacer values");
      same(JSON.stringify({ d: { toJSON: function (k) { return "k=" + k; } } }), '{"d":"k=d"}', "toJSON key");
      BigInt.prototype.toJSON = function () { return this.toString(); };
      same(JSON.stringify([5n]), '["5"]', "BigInt toJSON");
      delete BigInt.prototype.toJSON;
      "ok";
    "#);
}

#[test]
fn raw_json() {
    ok(r#"
      var raw = JSON.rawJSON("1e1000");
      same(JSON.isRawJSON(raw), true, "isRawJSON");
      same(JSON.isRawJSON({ rawJSON: "1" }), false, "plain object");
      same(Object.getPrototypeOf(raw), null, "null prototype");
      same(Object.isFrozen(raw), true, "frozen");
      same(raw.rawJSON, "1e1000", "rawJSON property");
      same(JSON.stringify({ n: raw, s: JSON.rawJSON('"x"') }), '{"n":1e1000,"s":"x"}', "serialized as is");
      throws(SyntaxError, function () { JSON.rawJSON("{}"); }, "object");
      throws(SyntaxError, function () { JSON.rawJSON("[]"); }, "array");
      throws(SyntaxError, function () { JSON.rawJSON(" 1"); }, "leading whitespace");
      throws(SyntaxError, function () { JSON.rawJSON(""); }, "empty");
      throws(SyntaxError, function () { JSON.rawJSON("01"); }, "invalid");
      "ok";
    "#);
}

// ----- JSON.parse -----

#[test]
fn parse_with_reviver() {
    ok(r#"
      var o = JSON.parse('{"a":[1,2],"b":{"c":3}}', function (k, v) {
        return typeof v === "number" ? v * 10 : v;
      });
      list(o.a, [10, 20], "array elements revived");
      same(o.b.c, 30, "nested");
      var d = JSON.parse('{"a":1,"b":2}', function (k, v) { return k === "a" ? undefined : v; });
      same("a" in d, false, "undefined deletes");
      same(d.b, 2, "kept");
      var keys = [];
      JSON.parse('{"a":{"b":1},"c":[2]}', function (k, v) { keys.push(k); return v; });
      list(keys, ["b", "a", "0", "c", ""], "post-order");
      var holders = [];
      var root = JSON.parse('{"x":1}', function (k, v) { holders.push(this); return v; });
      same(holders[0], root, "holder of x");
      same(Object.keys(holders[1]).join(), "", "root holder has the '' key");
      same(holders[1][""], root, "root holder");
      same(JSON.parse('{"__proto__": 1}').__proto__, 1, "__proto__ is an own property");
      throws(SyntaxError, function () { JSON.parse("{'a':1}"); }, "single quotes");
      throws(SyntaxError, function () { JSON.parse("[1,]"); }, "trailing comma");
      same(JSON.parse('"\\ud800"'), "\uD800", "escaped lone surrogate");
      "ok";
    "#);
}

#[test]
fn parse_reviver_sees_source_text() {
    ok(r#"
      var sources = [];
      var rootContext;
      JSON.parse('[1.0, "x", -0, 1e3, true, null]', function (k, v, ctx) {
        if (k === "") rootContext = ctx; else sources.push(ctx.source);
        return v;
      });
      list(sources, ["1.0", '"x"', "-0", "1e3", "true", "null"], "sources");
      same(rootContext.source, undefined, "no source for arrays");
      same(Object.getPrototypeOf(rootContext), Object.prototype, "context is ordinary");
      var big = JSON.parse('{"n": 12345678901234567890}', function (k, v, ctx) {
        return k === "n" ? BigInt(ctx.source) : v;
      });
      same(big.n, 12345678901234567890n, "exact digits from the source");
      var seen = "unset";
      JSON.parse('[1, 2]', function (k, v, ctx) {
        if (k === "0") this[1] = 3;
        if (k === "1") seen = ctx.source;
        return v;
      });
      same(seen, undefined, "no source once the value changed");
      var dup;
      JSON.parse('{"a": 1, "a": 2.50}', function (k, v, ctx) { if (k === "a") dup = ctx.source; return v; });
      same(dup, "2.50", "last duplicate key");
      "ok";
    "#);
}
