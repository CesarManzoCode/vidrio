//! Regressions of `test/language` scenarios of test262 recovered in K3:
//! references resolved before the right-hand side (`with`, direct
//! `eval`), `var` of a direct `eval` in the parameters of an arrow
//! function, GetSuperBase before ToPropertyKey in `super[key]`, a
//! parenthesized optional chain as callee, `eval?.()` as indirect eval,
//! and no NamedEvaluation for a parenthesized identifier.
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

fn run(src: &str) -> String {
    let mut agent = Agent::new();
    agent.initialize_host_defined_realm();
    let source = format!("{PRELUDE}\n{src}");
    match evaluate_script(&mut agent, &source) {
        Ok(v) => show(&mut agent, v),
        Err(e) => show_throw(&mut agent, e),
    }
}

fn ok(src: &str) {
    assert_eq!(run(src), "ok", "script:\n{src}");
}

#[test]
fn reference_is_resolved_before_the_right_hand_side() {
    // A direct eval in the right-hand side declares a closer `x`: the
    // store still goes to the binding resolved first.
    ok(r#"
var x = 3;
var inner = (function () {
  x *= (eval("var x = 2;"), 4);
  return x;
})();
same(inner, 2, "local x from eval");
same(x, 12, "outer x updated");
"ok"
"#);
    ok(r#"
var y = 0;
var inner = (function () {
  y = (eval("var y;"), 1);
  return y;
})();
same(inner, undefined, "local y untouched");
same(y, 1, "outer y assigned");
"ok"
"#);
    // The `with` object gains the property while the right-hand side
    // runs: the declarative binding resolved first is the target.
    ok(r#"
function f() {
  var z = 0;
  var scope = {};
  with (scope) {
    z = (scope.z = 2, 1);
  }
  same(scope.z, 2, "with object keeps its own value");
  same(z, 1, "declarative z assigned");
}
f();
"ok"
"#);
}

#[test]
fn eval_var_in_arrow_parameters_lands_in_the_function() {
    ok(r#"
var before = "arguments" in globalThis;
var f = (p = eval("var arguments = 'param'"), q = () => arguments) => {
  same(arguments, "param", "body sees the eval var");
  same(q(), "param", "parameter closure sees it");
};
f();
same("arguments" in globalThis, before, "no global binding");
var g = (p = eval("var w = 1")) => w;
same(g(), 1, "eval var visible in the body");
same("w" in globalThis, false, "w stays local");
"ok"
"#);
    // A `var` named like a parameter still conflicts.
    ok(r#"
var h = (p = eval("var r = 1"), r) => {};
throws(SyntaxError, h, "parameter conflict");
function k(p = eval("var arguments")) {}
throws(SyntaxError, k, "arguments of a function");
"ok"
"#);
}

#[test]
fn super_base_comes_before_to_property_key() {
    ok(r#"
var proto = { p: "ok", n: 1 };
var proto2 = { p: "bad", n: -1 };
var obj = {
  __proto__: proto,
  get(key) { return super[key]; },
  inc(key) { return super[key] += 1; },
};
var key = {
  toString() { Object.setPrototypeOf(obj, proto2); return "p"; }
};
same(obj.get(key), "ok", "get");
Object.setPrototypeOf(obj, proto);
var count = 0;
var nkey = {
  toString() { count++; Object.setPrototypeOf(obj, proto2); return "n"; }
};
same(obj.inc(nkey), 2, "compound");
same(count, 1, "key converted once");
"ok"
"#);
    // The key is converted after the right-hand side.
    ok(r#"
function E() {}
class C extends class {} {
  m(k, v) { super[k] = v(); }
}
var k = { toString() { fail("key converted first"); } };
throws(E, function () { new C().m(k, function () { throw new E(); }); }, "rhs first");
"ok"
"#);
}

#[test]
fn parenthesized_optional_chain_keeps_this() {
    ok(r#"
var a = { b() { return this._b; }, _b: { c: 42 } };
same((a?.b)().c, 42, "(a?.b)()");
same((a?.b)?.().c, 42, "(a?.b)?.()");
var n = null;
throws(TypeError, function () { (n?.b)(); }, "short-circuit then call");
"ok"
"#);
}

#[test]
fn optional_call_of_eval_is_indirect() {
    ok(r#"
var a = "global";
function fn() {
  var a = "local";
  return eval?.("a");
}
same(fn(), "global", "eval?.()");
same((function () { var a = "local"; return eval("a"); })(), "local", "direct");
"ok"
"#);
}

#[test]
fn parenthesized_target_is_not_named() {
    ok(r#"
var fn, g, h;
(fn) = function () {};
same(fn.name, "", "(fn) =");
g = function () {};
same(g.name, "g", "g =");
(h) ??= function () {};
same(h.name, "", "(h) ??=");
"ok"
"#);
}
