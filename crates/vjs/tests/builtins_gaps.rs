//! Regressions of `test/built-ins` scenarios of test262 recovered in K3:
//! the `caller` of sloppy functions, `byteLength`/`byteOffset` of an
//! out-of-bounds DataView, `toPrecision` just below a power of ten,
//! `copyWithin` after the buffer shrinks, the safe-integer bound of the
//! `take`/`drop` limit, `Promise.try` without wrapping, and the arguments
//! of `return` in `%AsyncIteratorPrototype%[@@asyncDispose]`.
//!
//! Each case is a sloppy script that returns `"ok"` or throws; a failure
//! shows the thrown value.

use vjs::agent::Agent;
use vjs::ops;
use vjs::value::{PropertyKey, Value};
use vjs::{Throw, evaluate_script};

const PRELUDE: &str = r#"
function fail(msg) { throw new Error(msg); }
function same(actual, expected, what) {
  if (!Object.is(actual, expected)) {
    fail(what + ": expected " + String(expected) + ", got " + String(actual));
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

#[test]
fn sloppy_function_caller_is_readable() {
    ok(r#"
function g() { return g.caller; }
function f() { return g(); }
same(f(), null, "caller from a sloppy function");
same(eval('"use strict"; g();'), null, "caller from strict eval code");
(function () { "use strict"; same(f(), null, "caller under a strict caller"); })();
var d = Object.getOwnPropertyDescriptor(g, "caller");
same(d.writable, false, "writable");
same(d.configurable, false, "configurable");
same(d.enumerable, false, "enumerable");
"ok"
"#);
    // Strict, arrow, method and generator functions keep reaching
    // %ThrowTypeError% on %Function.prototype%.
    ok(r#"
function s() { "use strict"; }
throws(TypeError, function () { return s.caller; }, "strict function");
same(Object.prototype.hasOwnProperty.call(() => 0, "caller"), false, "arrow");
same(Object.prototype.hasOwnProperty.call({ m() {} }.m, "caller"), false, "method");
same(Object.prototype.hasOwnProperty.call(function* () {}, "caller"), false, "generator");
same(Object.prototype.hasOwnProperty.call(class {}, "caller"), false, "class");
"ok"
"#);
}

#[test]
fn out_of_bounds_data_view_reports_zero() {
    ok(r#"
var ab = new ArrayBuffer(4);
var dv = new DataView(ab, 1);
ab.transfer();
same(dv.byteLength, 0, "detached byteLength");
same(dv.byteOffset, 0, "detached byteOffset");
var rab = new ArrayBuffer(4, { maxByteLength: 5 });
var fixed = new DataView(rab, 1, 2);
var auto = new DataView(rab, 1);
rab.resize(2);
same(fixed.byteLength, 0, "fixed out of bounds byteLength");
same(fixed.byteOffset, 0, "fixed out of bounds byteOffset");
same(auto.byteLength, 1, "length-tracking in bounds");
rab.resize(0);
same(auto.byteLength, 0, "length-tracking out of bounds byteLength");
same(auto.byteOffset, 0, "length-tracking out of bounds byteOffset");
rab.resize(3);
same(fixed.byteOffset, 1, "back in bounds");
"ok"
"#);
}

#[test]
fn to_precision_just_below_a_power_of_ten() {
    ok(r#"
var n = 1e-21;
same(n.toPrecision(7), "1.000000e-21", "rounds up to the power");
same(n.toPrecision(16), "9.999999999999999e-22", "16 digits");
same(n.toPrecision(21), "9.99999999999999907537e-22", "21 digits");
same((1000).toPrecision(1), "1e+3", "exact power");
same((99.99).toPrecision(2), "1.0e+2", "carry to the next decade");
same(n.toExponential(15), "9.999999999999999e-22", "toExponential");
"ok"
"#);
}

#[test]
fn copy_within_clamps_after_shrink() {
    ok(r#"
function run(target, start) {
  var rab = new ArrayBuffer(4, { maxByteLength: 8 });
  var ta = new Uint8Array(rab);
  for (var i = 0; i < 4; i++) ta[i] = i;
  var evil = { valueOf: function () { rab.resize(3); return 2; } };
  ta.copyWithin(target === "evil" ? evil : target, start === "evil" ? evil : start);
  return Array.from(ta);
}
list(run("evil", 0), [0, 1, 0], "forward");
list(run(0, "evil"), [2, 1, 2], "backward");
list(run("evil", 1), [0, 1, 1], "overlapping forward");
list(run(1, "evil"), [0, 2, 2], "overlapping backward");
"ok"
"#);
}

#[test]
fn take_and_drop_reject_limits_above_max_safe_integer() {
    ok(r#"
["take", "drop"].forEach(function (name) {
  var closed = 0;
  var it = {
    __proto__: Iterator.prototype,
    get next() { fail(name + ": next read"); },
    return: function () { closed++; return {}; }
  };
  throws(RangeError, function () { it[name](Number.MAX_SAFE_INTEGER + 1); }, name);
  same(closed, 1, name + " closes the receiver");
  var gen = (function* () {})();
  gen[name](Number.MAX_SAFE_INTEGER);
  gen[name](Infinity);
});
"ok"
"#);
}

#[test]
fn promise_try_does_not_wrap_a_promise_of_the_receiver() {
    ok(r#"
var p = Promise.resolve();
same(Promise.try(function () { return p; }), p, "Promise");
class Sub extends Promise {}
var s = Sub.resolve();
same(Sub.try(function () { return s; }), s, "subclass");
same(Sub.try(function () { return p; }) === p, false, "other constructor is wrapped");
var count = 0;
class Counted extends Promise { constructor(e) { super(e); count++; } }
Promise.try.call(Counted, function () {});
same(count, 1, "one construction");
throws(TypeError, function () { Promise.try.call(eval); }, "non-constructor");
"ok"
"#);
}

#[test]
fn async_iterator_async_dispose_calls_return_without_arguments() {
    ok_after_jobs(
        r#"
var proto = Object.getPrototypeOf(Object.getPrototypeOf((async function* () {}).prototype));
var it = Object.create(proto);
var argc = -1;
it.return = async function () { argc = arguments.length; return { done: true }; };
var settled = false;
it[Symbol.asyncDispose]().then(function (v) { same(v, undefined, "value"); settled = true; });
"#,
        r#"same(argc, 0, "argument count"); same(settled, true, "settled"); "ok""#,
    );
}
