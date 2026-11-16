//! Annex B and the remaining standard features through scripts: the web
//! legacy semantics of functions in blocks (B.3.2), `escape`/`unescape`,
//! the String HTML methods and the Date year methods, `using` and
//! `DisposableStack` with suppressed errors, `AsyncDisposableStack`, and
//! strict-mode tail calls that recurse 100 000 levels without overflowing
//! the frame stack.

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

#[test]
fn block_functions_in_function_code() {
    ok(r#"
function f() {
  same(g, undefined, "var binding before the block");
  { function g() { return 1; } }
  same(g(), 1, "copied when the declaration is evaluated");
  return "done";
}
same(f(), "done", "f");

function inner() {
  var log = [];
  {
    log.push(typeof h);
    function h() {}
    h = 2;
  }
  log.push(typeof h);
  return log.join();
}
same(inner(), "function,function", "assignment in the block stays in the block");

function shadowed() {
  let k = 0;
  { function k() {} }
  return k;
}
same(shadowed(), 0, "a let in the function body blocks the var binding");

function param(p) {
  { function p() {} }
  return p;
}
same(param(5), 5, "a parameter blocks the var binding");

function hanging(c) {
  if (c) function q() { return "q"; }
  return typeof q;
}
same(hanging(true), "function", "if clause, taken");
same(hanging(false), "undefined", "if clause, not taken");

function inSwitch(x) {
  switch (x) { case 1: function s() { return "s"; } }
  return typeof s;
}
same(inSwitch(1), "function", "case clause");

function strictBlock() {
  "use strict";
  { function t() {} }
  return typeof t;
}
same(strictBlock(), "undefined", "strict mode keeps block functions lexical");
"ok";
"#);
}

#[test]
fn block_functions_in_global_and_eval_code() {
    ok(r#"
same(typeof globalBlockFn, "undefined", "global var binding exists before the block");
same(Object.getOwnPropertyDescriptor(globalThis, "globalBlockFn").configurable, false,
     "global var binding is not deletable");
{ function globalBlockFn() { return 7; } }
same(globalBlockFn(), 7, "global copy");

function viaEval() {
  eval("{ function fromEval() { return 3; } }");
  return fromEval();
}
same(viaEval(), 3, "sloppy eval in a function");

function evalBlocked() {
  let fromEval2 = "let";
  { eval("{ function fromEval2() {} }"); }
  return fromEval2;
}
same(evalBlocked(), "let", "an enclosing let blocks the eval copy");

lbl: function labelled() { return "l"; }
same(labelled(), "l", "labelled function declaration");

var seen;
for (var x = (seen = "init") in {}) {}
same(seen, "init", "for (var x = init in obj)");
same(x, "init", "the initializer is assigned");
"ok";
"#);
}

#[test]
fn escape_and_unescape() {
    ok(r#"
same(escape("abc ABC 123 @*_+-./"), "abc%20ABC%20123%20@*_+-./", "kept characters");
same(escape("éĀ￿"), "%E9%u0100%uFFFF", "two and four digit escapes");
same(unescape("%41%u0042%"), "AB%", "decoded and trailing percent");
same(unescape("%u00zz%4"), "%u00zz%4", "malformed escapes stay");
var all = "";
for (var i = 0; i < 0x300; i++) all += String.fromCharCode(i);
same(unescape(escape(all)), all, "round trip");
same(escape.length, 1, "escape.length");
same(unescape.name, "unescape", "unescape.name");
"ok";
"#);
}

#[test]
fn string_html_and_date_year_methods() {
    ok(r#"
same("x".anchor('a"b'), '<a name="a&quot;b">x</a>', "anchor");
same("x".big(), "<big>x</big>", "big");
same("x".fontsize(7), '<font size="7">x</font>', "fontsize");
same("x".link("u"), '<a href="u">x</a>', "link");
same("x".sup(), "<sup>x</sup>", "sup");
throws(TypeError, function () { String.prototype.bold.call(undefined); }, "bold on undefined");

same(Date.prototype.toGMTString, Date.prototype.toUTCString, "toGMTString is toUTCString");
var d = new Date(2000, 5, 15);
same(d.getYear(), 100, "getYear");
d.setYear(95);
same(d.getFullYear(), 1995, "setYear two digits");
same(d.getMonth(), 5, "setYear keeps the month");
var n = new Date(NaN);
n.setYear(2001);
same(n.getFullYear(), 2001, "setYear on an invalid date starts from +0");
same(new Date(NaN).getYear(), NaN, "getYear of an invalid date");
"ok";
"#);
}

#[test]
fn using_with_suppressed_errors() {
    ok(r#"
var log = [];
function res(name, error) {
  return { [Symbol.dispose]() { log.push(name); if (error) throw error; } };
}
var e1 = new Error("one"), e2 = new Error("two"), body = new Error("body");
try {
  {
    using a = res("a", e1);
    using b = res("b", e2);
    throw body;
  }
} catch (e) {
  same(log.join(), "b,a", "disposal order");
  same(e instanceof SuppressedError, true, "outer is a SuppressedError");
  same(e.error, e1, "outer error is the last disposal error");
  same(e.suppressed instanceof SuppressedError, true, "inner is a SuppressedError");
  same(e.suppressed.error, e2, "inner error");
  same(e.suppressed.suppressed, body, "the body's error is suppressed");
}

var stack = new DisposableStack();
var order = [];
stack.defer(function () { order.push("defer"); });
same(stack.adopt(1, function (v) { order.push("adopt " + v); }), 1, "adopt returns the value");
stack.use({ [Symbol.dispose]() { order.push("use"); throw e1; } });
stack.use(null);
throws(Error, function () { stack.dispose(); }, "dispose rethrows");
same(order.join(), "use,adopt 1,defer", "stack order");
same(stack.disposed, true, "disposed");
same(stack.dispose(), undefined, "second dispose does nothing");
throws(ReferenceError, function () { stack.defer(function () {}); }, "defer after dispose");
throws(ReferenceError, function () { stack.use(1); }, "use after dispose");
throws(TypeError, function () { new DisposableStack().use(1); }, "use of a primitive");
throws(TypeError, function () { new DisposableStack().use({}); }, "use of a non-disposable");

var moved = new DisposableStack();
var hit = false;
moved.defer(function () { hit = true; });
var fresh = moved.move();
same(moved.disposed, true, "moved-from stack is disposed");
fresh[Symbol.dispose]();
same(hit, true, "moved resources dispose with the new stack");
same(Object.prototype.toString.call(fresh), "[object DisposableStack]", "toStringTag");
throws(TypeError, function () { DisposableStack(); }, "requires new");

var it = [1, 2].values();
var closed = false;
it.return = function () { closed = true; return {}; };
it[Symbol.dispose]();
same(closed, true, "Iterator.prototype[Symbol.dispose] calls return");
"ok";
"#);
}

#[test]
fn async_disposable_stack() {
    ok_after_jobs(
        r#"
var log = [];
var result;
var stack = new AsyncDisposableStack();
stack.use({ [Symbol.dispose]() { log.push("sync"); } });
stack.defer(async function () { log.push("deferred"); throw new Error("x"); });
stack.use({ async [Symbol.asyncDispose]() { log.push("async"); } });
stack.disposeAsync().then(
  function () { result = "fulfilled"; },
  function (e) { result = e.message; });
"#,
        r#"
same(log.join(), "async,deferred,sync", "async disposal order");
same(result, "x", "the error rejects disposeAsync");
same(stack.disposed, true, "disposed");
"ok";
"#,
    );
}

#[test]
fn strict_tail_calls_do_not_grow_the_stack() {
    ok(r#"
function count(n, acc) {
  "use strict";
  if (n === 0) return acc;
  return count(n - 1, acc + 1);
}
same(count(100000, 0), 100000, "direct tail recursion");

function even(n) { "use strict"; return n === 0 ? true : odd(n - 1); }
function odd(n) { "use strict"; return n === 0 ? false : even(n - 1); }
same(even(100001), false, "mutual recursion through ?:");

var arrow = (n) => { "use strict"; return n === 0 ? "done" : arrow(n - 1); };
same(arrow(100000), "done", "arrow function");

function spread(n) { "use strict"; return n === 0 ? 0 : spread(...[n - 1]); }
same(spread(100000), 0, "spread arguments");

function sloppy(n) { return n === 0 ? 0 : sloppy(n - 1); }
throws(RangeError, function () { sloppy(100000); }, "sloppy calls still use frames");

function guarded(n) {
  "use strict";
  try { return n === 0 ? 0 : guarded(n - 1); } finally {}
}
throws(RangeError, function () { guarded(100000); }, "calls inside try are not in tail position");
"ok";
"#);
}
