//! Core built-ins: property descriptors (attributes, `length`, `name`) of
//! every constructor and prototype of `Object`, `Function`, the errors,
//! `Symbol`, `Boolean`, `Reflect`, `Proxy` and the global functions; the
//! Proxy invariants; `Function.prototype.toString` of user and native
//! functions; CreateDynamicFunction; and the URI functions with invalid
//! sequences.
//!
//! Each case is a script that returns `"ok"` or throws; a failure shows
//! the thrown value.

use vjs::agent::Agent;
use vjs::ops;
use vjs::value::{PropertyKey, Value};
use vjs::{Throw, evaluate_script};

/// Assertion helpers available to every script.
const PRELUDE: &str = r#"
// Only the built-ins of this sprint exist: no Array, String, JSON or
// RegExp methods here.
function fail(msg) { throw new Error(msg); }
function str(v) { return typeof v === "symbol" ? v.toString() : "" + v; }
// Array join without Array.prototype.join (nested arrays joined by ",").
function j(a, sep) {
  if (sep === undefined) sep = ",";
  var out = "";
  for (var i = 0; i < a.length; i++) {
    if (i > 0) out += sep;
    var v = a[i];
    if (typeof v === "object" && v !== null && "length" in v) out += j(v, ",");
    else if (v !== undefined && v !== null) out += str(v);
  }
  return out;
}
function same(actual, expected, what) {
  if (!Object.is(actual, expected)) {
    fail(what + ": expected " + str(expected) + ", got " + str(actual));
  }
}
function throws(C, f, what) {
  try { f(); } catch (e) {
    if (!(e instanceof C)) fail(what + ": wrong error " + e);
    return;
  }
  fail(what + ": did not throw");
}
// A data property with the given attributes.
function data(o, key, w, e, c, what) {
  var d = Object.getOwnPropertyDescriptor(o, key);
  if (d === undefined) fail(what + ": missing " + str(key));
  same("value" in d, true, what + " is data");
  same(d.writable, w, what + " writable");
  same(d.enumerable, e, what + " enumerable");
  same(d.configurable, c, what + " configurable");
  return d.value;
}
// A built-in method: { writable, !enumerable, configurable } with its
// length and name.
function fn(o, key, length, name, what) {
  var f = data(o, key, true, false, true, what);
  same(typeof f, "function", what + " type");
  checkFn(f, length, name, what);
  return f;
}
function checkFn(f, length, name, what) {
  same(data(f, "length", false, false, true, what + ".length"), length, what + ".length");
  same(data(f, "name", false, false, true, what + ".name"), name, what + ".name");
}
// A constructor: its prototype link and prototype.constructor.
function ctor(C, name, length, protoOfC) {
  checkFn(C, length, name, name);
  same(Object.getPrototypeOf(C), protoOfC, name + " [[Prototype]]");
  var P = data(C, "prototype", false, false, false, name + ".prototype");
  return P;
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

/// Runs `src` after the prelude in a fresh agent; the completion value or
/// the thrown error.
fn run(src: &str) -> String {
    let mut agent = Agent::new();
    agent.initialize_host_defined_realm();
    let source = format!("{PRELUDE}\n{src}");
    let result = match evaluate_script(&mut agent, &source) {
        Ok(v) => show(&mut agent, v),
        Err(e) => show_throw(&mut agent, e),
    };
    let errors = agent.run_jobs();
    if let Some(e) = errors.into_iter().next() {
        return show_throw(&mut agent, e);
    }
    result
}

fn ok(src: &str) {
    assert_eq!(run(src), "ok", "script:\n{src}");
}

// ----- descriptors -----

#[test]
fn global_bindings() {
    ok(r#"
    var g = globalThis;
    same(data(g, "globalThis", true, false, true, "globalThis"), g, "globalThis value");
    for (var n of ["NaN", "Infinity", "undefined"]) data(g, n, false, false, false, n);
    for (var n of ["Object", "Function", "Error", "EvalError", "RangeError",
                   "ReferenceError", "SyntaxError", "TypeError", "URIError",
                   "AggregateError", "SuppressedError", "Symbol", "Boolean",
                   "Reflect", "Proxy", "eval", "isFinite", "isNaN",
                   "parseFloat", "parseInt", "encodeURI",
                   "encodeURIComponent", "decodeURI", "decodeURIComponent"]) {
      data(g, n, true, false, true, n);
    }
    fn(g, "eval", 1, "eval", "eval");
    fn(g, "isFinite", 1, "isFinite", "isFinite");
    fn(g, "isNaN", 1, "isNaN", "isNaN");
    fn(g, "parseFloat", 1, "parseFloat", "parseFloat");
    fn(g, "parseInt", 2, "parseInt", "parseInt");
    fn(g, "encodeURI", 1, "encodeURI", "encodeURI");
    fn(g, "encodeURIComponent", 1, "encodeURIComponent", "encodeURIComponent");
    fn(g, "decodeURI", 1, "decodeURI", "decodeURI");
    fn(g, "decodeURIComponent", 1, "decodeURIComponent", "decodeURIComponent");
    "ok";
    "#);
}

#[test]
fn object_descriptors() {
    ok(r#"
    var P = ctor(Object, "Object", 1, Function.prototype);
    same(P, Object.prototype, "Object.prototype");
    same(Object.getPrototypeOf(P), null, "Object.prototype [[Prototype]]");
    same(data(P, "constructor", true, false, true, "constructor"), Object, "constructor");
    var statics = { assign: 2, create: 2, defineProperties: 2, defineProperty: 3,
      entries: 1, freeze: 1, fromEntries: 1, getOwnPropertyDescriptor: 2,
      getOwnPropertyDescriptors: 1, getOwnPropertyNames: 1,
      getOwnPropertySymbols: 1, getPrototypeOf: 1, groupBy: 2, hasOwn: 2, is: 2,
      isExtensible: 1, isFrozen: 1, isSealed: 1, keys: 1, preventExtensions: 1,
      seal: 1, setPrototypeOf: 2, values: 1 };
    for (var k in statics) fn(Object, k, statics[k], k, "Object." + k);
    var methods = { hasOwnProperty: 1, isPrototypeOf: 1, propertyIsEnumerable: 1,
      toLocaleString: 0, toString: 0, valueOf: 0, __defineGetter__: 2,
      __defineSetter__: 2, __lookupGetter__: 1, __lookupSetter__: 1 };
    for (var k in methods) fn(P, k, methods[k], k, "Object.prototype." + k);
    var d = Object.getOwnPropertyDescriptor(P, "__proto__");
    same(d.enumerable, false, "__proto__ enumerable");
    same(d.configurable, true, "__proto__ configurable");
    checkFn(d.get, 0, "get __proto__", "__proto__ getter");
    checkFn(d.set, 1, "set __proto__", "__proto__ setter");
    // Object.prototype is an immutable prototype exotic object.
    throws(TypeError, function () { Object.setPrototypeOf(P, {}); }, "immutable proto");
    same(Reflect.setPrototypeOf(P, null), true, "same proto is fine");
    "ok";
    "#);
}

#[test]
fn function_descriptors() {
    ok(r#"
    var P = ctor(Function, "Function", 1, Function.prototype);
    same(P, Function.prototype, "Function.prototype");
    checkFn(P, 0, "", "Function.prototype");
    same(P(), undefined, "Function.prototype()");
    same(data(P, "constructor", true, false, true, "constructor"), Function, "constructor");
    fn(P, "apply", 2, "apply", "apply");
    fn(P, "bind", 1, "bind", "bind");
    fn(P, "call", 1, "call", "call");
    fn(P, "toString", 0, "toString", "toString");
    var hi = data(P, Symbol.hasInstance, false, false, false, "@@hasInstance");
    checkFn(hi, 1, "[Symbol.hasInstance]", "@@hasInstance");
    // caller and arguments: the same %ThrowTypeError% accessor.
    var c = Object.getOwnPropertyDescriptor(P, "caller");
    var a = Object.getOwnPropertyDescriptor(P, "arguments");
    same(c.enumerable, false, "caller enumerable");
    same(c.configurable, true, "caller configurable");
    same(c.get, c.set, "caller get === set");
    same(a.get, c.get, "arguments uses the same thrower");
    var T = c.get;
    same(Object.isExtensible(T), false, "%ThrowTypeError% is not extensible");
    same(Object.isFrozen(T), true, "%ThrowTypeError% is frozen");
    same(data(T, "length", false, false, false, "T.length"), 0, "T.length");
    same(data(T, "name", false, false, false, "T.name"), "", "T.name");
    throws(TypeError, function () { T(); }, "ThrowTypeError throws");
    // Strict, arrow and class functions have no own caller/arguments.
    function strict() { "use strict"; }
    for (var f of [strict, () => 0, class {}, function* () {}, async function () {}]) {
      same(Object.prototype.hasOwnProperty.call(f, "caller"), false, "own caller");
      same(Object.prototype.hasOwnProperty.call(f, "arguments"), false, "own arguments");
      throws(TypeError, function () { return f.caller; }, "f.caller");
      throws(TypeError, function () { f.arguments = 1; }, "f.arguments =");
    }
    // The other function constructors.
    var GF = Object.getPrototypeOf(function* () {}).constructor;
    var AF = Object.getPrototypeOf(async function () {}).constructor;
    var AGF = Object.getPrototypeOf(async function* () {}).constructor;
    var names = ["GeneratorFunction", "AsyncFunction", "AsyncGeneratorFunction"];
    var list = [GF, AF, AGF];
    for (var i = 0; i < 3; i++) {
      var Pk = ctor(list[i], names[i], 1, Function);
      same(data(Pk, "constructor", false, false, true, names[i] + " constructor"),
           list[i], names[i] + ".prototype.constructor");
      same(Object.getPrototypeOf(Pk), Function.prototype, names[i] + ".prototype proto");
    }
    "ok";
    "#);
}

#[test]
fn error_descriptors() {
    ok(r#"
    var EP = ctor(Error, "Error", 1, Function.prototype);
    same(Object.getPrototypeOf(EP), Object.prototype, "Error.prototype proto");
    same(data(EP, "constructor", true, false, true, "constructor"), Error, "constructor");
    same(data(EP, "name", true, false, true, "name"), "Error", "Error.prototype.name");
    same(data(EP, "message", true, false, true, "message"), "", "Error.prototype.message");
    fn(EP, "toString", 0, "toString", "Error.prototype.toString");
    fn(Error, "isError", 1, "isError", "Error.isError");
    same(Object.prototype.toString.call(EP), "[object Object]", "Error.prototype is ordinary");
    var natives = [EvalError, RangeError, ReferenceError, SyntaxError, TypeError, URIError];
    for (var N of natives) {
      var NP = ctor(N, N.name, 1, Error);
      same(Object.getPrototypeOf(NP), EP, N.name + ".prototype proto");
      same(data(NP, "name", true, false, true, "name"), N.name, N.name + " name");
      same(data(NP, "message", true, false, true, "message"), "", N.name + " message");
      same(data(NP, "constructor", true, false, true, "constructor"), N, N.name + " ctor");
      same(Object.prototype.hasOwnProperty.call(NP, "toString"), false, "own toString");
    }
    var AP = ctor(AggregateError, "AggregateError", 2, Error);
    same(Object.getPrototypeOf(AP), EP, "AggregateError.prototype proto");
    same(data(AP, "name", true, false, true, "name"), "AggregateError", "name");
    same(data(AP, "message", true, false, true, "message"), "", "message");
    var SP = ctor(SuppressedError, "SuppressedError", 3, Error);
    same(data(SP, "name", true, false, true, "name"), "SuppressedError", "name");
    "ok";
    "#);
}

#[test]
fn symbol_and_boolean_descriptors() {
    ok(r#"
    var SP = ctor(Symbol, "Symbol", 0, Function.prototype);
    same(data(SP, "constructor", true, false, true, "constructor"), Symbol, "constructor");
    fn(Symbol, "for", 1, "for", "Symbol.for");
    fn(Symbol, "keyFor", 1, "keyFor", "Symbol.keyFor");
    var wk = ["asyncDispose", "asyncIterator", "dispose", "hasInstance",
              "isConcatSpreadable", "iterator", "match", "matchAll", "replace",
              "search", "species", "split", "toPrimitive", "toStringTag",
              "unscopables"];
    for (var n of wk) {
      var s = data(Symbol, n, false, false, false, "Symbol." + n);
      same(typeof s, "symbol", n + " type");
      same(s.description, "Symbol." + n, n + " description");
    }
    fn(SP, "toString", 0, "toString", "toString");
    fn(SP, "valueOf", 0, "valueOf", "valueOf");
    var tp = data(SP, Symbol.toPrimitive, false, false, true, "@@toPrimitive");
    checkFn(tp, 1, "[Symbol.toPrimitive]", "@@toPrimitive");
    same(data(SP, Symbol.toStringTag, false, false, true, "@@toStringTag"), "Symbol", "tag");
    var d = Object.getOwnPropertyDescriptor(SP, "description");
    same(d.set, undefined, "description setter");
    same(d.configurable, true, "description configurable");
    checkFn(d.get, 0, "get description", "description getter");

    var BP = ctor(Boolean, "Boolean", 1, Function.prototype);
    same(Object.prototype.toString.call(BP), "[object Boolean]", "Boolean.prototype");
    same(BP.valueOf(), false, "Boolean.prototype value");
    fn(BP, "toString", 0, "toString", "Boolean toString");
    fn(BP, "valueOf", 0, "valueOf", "Boolean valueOf");
    same(data(BP, "constructor", true, false, true, "constructor"), Boolean, "constructor");
    "ok";
    "#);
}

#[test]
fn reflect_and_proxy_descriptors() {
    ok(r#"
    same(Object.getPrototypeOf(Reflect), Object.prototype, "Reflect proto");
    same(typeof Reflect, "object", "Reflect is an ordinary object");
    same(data(Reflect, Symbol.toStringTag, false, false, true, "tag"), "Reflect", "tag");
    var m = { apply: 3, construct: 2, defineProperty: 3, deleteProperty: 2,
      get: 2, getOwnPropertyDescriptor: 2, getPrototypeOf: 1, has: 2,
      isExtensible: 1, ownKeys: 1, preventExtensions: 1, set: 3,
      setPrototypeOf: 2 };
    for (var k in m) fn(Reflect, k, m[k], k, "Reflect." + k);
    checkFn(Proxy, 2, "Proxy", "Proxy");
    same(Object.prototype.hasOwnProperty.call(Proxy, "prototype"), false, "Proxy.prototype");
    fn(Proxy, "revocable", 2, "revocable", "Proxy.revocable");
    var r = Proxy.revocable({}, {});
    checkFn(r.revoke, 0, "", "revoke function");
    same(Object.prototype.hasOwnProperty.call(r.revoke, "prototype"), false, "revoke.prototype");
    same(j(Object.keys(r)), "proxy,revoke", "revocable result keys");
    "ok";
    "#);
}

// ----- behaviour -----

#[test]
fn object_statics() {
    ok(r#"
    var o = Object.assign({}, { a: 1 }, null, "xy", { b: 2 });
    same(o.a + o.b, 3, "assign");
    same(o[0] + o[1], "xy", "assign from string");
    var c = Object.create(null, { x: { value: 1, enumerable: true } });
    same(Object.getPrototypeOf(c), null, "create proto");
    same(j(Object.keys(c)), "x", "create props");
    throws(TypeError, function () { Object.create(1); }, "create non-object");
    var e = Object.fromEntries([["a", 1], ["b", 2]]);
    same(j(Object.entries(e), "|"), "a,1|b,2", "fromEntries/entries");
    same(j(Object.values(e)), "1,2", "values");
    var closed = false;
    var it = { [Symbol.iterator]() { return { next() { return { value: 1, done: false }; },
                                              return() { closed = true; return {}; } }; } };
    throws(TypeError, function () { Object.fromEntries(it); }, "non-object entry");
    same(closed, true, "fromEntries closes the iterator");
    var g = Object.groupBy([1, 2, 3, 4], function (x, i) { return x % 2 ? "odd" : "even"; });
    same(Object.getPrototypeOf(g), null, "groupBy null proto");
    same(j(Object.keys(g)), "odd,even", "groupBy order");
    same(j(g.odd) + "|" + j(g.even), "1,3|2,4", "groupBy groups");
    same(Object.hasOwn({ q: 1 }, "q"), true, "hasOwn");
    same(Object.hasOwn(Object.create({ q: 1 }), "q"), false, "hasOwn inherited");
    same(Object.is(NaN, NaN) && !Object.is(0, -0), true, "is");
    var f = Object.freeze({ a: 1 });
    same(Object.isFrozen(f) && Object.isSealed(f) && !Object.isExtensible(f), true, "freeze");
    same(Object.isFrozen(1), true, "isFrozen primitive");
    same(Object.isExtensible(1), false, "isExtensible primitive");
    same(Object.freeze(1), 1, "freeze primitive");
    var s = Object.seal({ a: 1 });
    same(Object.isSealed(s) && !Object.isFrozen(s), true, "seal");
    same(j(Object.getOwnPropertyNames("ab")), "0,1,length", "getOwnPropertyNames");
    var sym = Symbol("k");
    var so = { [sym]: 1, x: 2 };
    same(Object.getOwnPropertySymbols(so)[0], sym, "getOwnPropertySymbols");
    var ds = Object.getOwnPropertyDescriptors({ get z() { return 1; } });
    same(typeof ds.z.get, "function", "getOwnPropertyDescriptors");
    same(Object.setPrototypeOf(1, null), 1, "setPrototypeOf primitive");
    throws(TypeError, function () { Object.setPrototypeOf(undefined, null); }, "setPrototypeOf undefined");
    throws(TypeError, function () { Object.setPrototypeOf({}, 1); }, "setPrototypeOf bad proto");
    throws(TypeError, function () { Object.defineProperty(1, "x", {}); }, "defineProperty primitive");
    same(typeof Object(1), "object", "Object(1)");
    same(Object(null) instanceof Object, true, "Object(null)");
    class Sub extends Object {}
    same(Object.getPrototypeOf(new Sub()), Sub.prototype, "Object new.target");
    "ok";
    "#);
}

#[test]
fn object_prototype_methods() {
    ok(r#"
    var ts = Object.prototype.toString;
    same(ts.call(undefined), "[object Undefined]", "undefined");
    same(ts.call(null), "[object Null]", "null");
    same(ts.call([]), "[object Array]", "array");
    same(ts.call(new Proxy([], {})), "[object Array]", "proxy of array");
    same(ts.call(function () {}), "[object Function]", "function");
    same(ts.call(new Error()), "[object Error]", "error");
    same(ts.call(true), "[object Boolean]", "boolean");
    same(ts.call(1), "[object Number]", "number");
    same(ts.call(""), "[object String]", "string");
    same(ts.call(function () { return arguments; }()), "[object Arguments]", "arguments");
    same(ts.call({ [Symbol.toStringTag]: "X" }), "[object X]", "tag");
    same(ts.call(Symbol()), "[object Symbol]", "symbol");
    var r = Proxy.revocable([], {}); r.revoke();
    throws(TypeError, function () { ts.call(r.proxy); }, "revoked proxy");
    same({}.hasOwnProperty.call("ab", "length"), true, "hasOwnProperty");
    same(Object.prototype.isPrototypeOf.call(Object.getPrototypeOf([]), []), true, "isPrototypeOf");
    same(Object.prototype.isPrototypeOf(1), false, "isPrototypeOf primitive");
    same([].propertyIsEnumerable("length"), false, "propertyIsEnumerable");
    same({ toString() { return "T"; } }.toLocaleString(), "T", "toLocaleString");
    var o = {};
    o.__defineGetter__("g", function () { return 7; });
    same(o.g, 7, "__defineGetter__");
    var d = Object.getOwnPropertyDescriptor(o, "g");
    same(d.enumerable && d.configurable, true, "__defineGetter__ attributes");
    var setv;
    o.__defineSetter__("g", function (v) { setv = v; });
    o.g = 3;
    same(setv, 3, "__defineSetter__");
    var child = Object.create(o);
    same(child.__lookupGetter__("g")(), 7, "__lookupGetter__ walks the chain");
    same(typeof child.__lookupSetter__("g"), "function", "__lookupSetter__");
    same(child.__lookupGetter__("nope"), undefined, "__lookupGetter__ missing");
    throws(TypeError, function () { o.__defineGetter__("h", 1); }, "__defineGetter__ non-callable");
    var p = {};
    var q = { __proto__: p };
    same(q.__proto__, p, "__proto__ getter");
    q.__proto__ = null;
    same(Object.getPrototypeOf(q), null, "__proto__ setter");
    q.__proto__ = 5;
    same(Object.getPrototypeOf(q), null, "__proto__ ignores primitives");
    var ne = Object.preventExtensions({});
    throws(TypeError, function () { ne.__proto__ = {}; }, "__proto__ on non-extensible");
    "ok";
    "#);
}

#[test]
fn function_prototype_methods() {
    ok(r#"
    function f(a, b, c) { "use strict"; return j([this, a, b, c]); }
    same(f.call("t", 1, 2), "t,1,2,", "call");
    same(f.apply("t", [1, 2, 3]), "t,1,2,3", "apply");
    same(f.apply("t", null), "t,,,", "apply null");
    same(f.apply("t", { length: 2, 0: "x", 1: "y" }), "t,x,y,", "apply array-like");
    throws(TypeError, function () { f.apply(null, 1); }, "apply non-object");
    throws(TypeError, function () { Function.prototype.call.call(1); }, "call non-callable");
    var b = f.bind("t", 1);
    same(b(2, 3), "t,1,2,3", "bind");
    checkFn(b, 2, "bound f", "bound");
    same(Object.getPrototypeOf(b), Function.prototype, "bound proto");
    var bb = b.bind(null, 2);
    same(bb.name, "bound bound f", "double bind name");
    same(bb.length, 1, "double bind length");
    var g = function () {};
    Object.defineProperty(g, "length", { value: Infinity });
    same(g.bind().length, Infinity, "bind +Infinity length");
    Object.defineProperty(g, "length", { value: -Infinity });
    same(g.bind().length, 0, "bind -Infinity length");
    Object.defineProperty(g, "length", { value: 2.5 });
    same(g.bind(null, 1).length, 1, "bind fractional length");
    Object.defineProperty(g, "length", { value: "3" });
    same(g.bind().length, 0, "bind non-number length");
    Object.defineProperty(g, "name", { value: 42 });
    same(g.bind().name, "bound ", "bind non-string name");
    function C(x) { this.x = x; }
    var BC = C.bind(null, 5);
    var inst = new BC();
    same(inst.x, 5, "bound construct");
    same(inst instanceof C && inst instanceof BC, true, "bound instanceof");
    same(Function.prototype[Symbol.hasInstance].call(C, inst), true, "@@hasInstance");
    same(Function.prototype[Symbol.hasInstance].call({}, inst), false, "@@hasInstance non-callable");
    "ok";
    "#);
}

#[test]
fn function_to_string_user_code() {
    ok(r#"
    function /* a */ f /* b */ ( x , y ) /* c */ { return x; }
    same(f.toString(), "function /* a */ f /* b */ ( x , y ) /* c */ { return x; }", "declaration");
    var e = function  ( ) {   };
    same(e.toString(), "function  ( ) {   }", "expression");
    var a = ( x ) =>  x * 2;
    same(a.toString(), "( x ) =>  x * 2", "arrow");
    var o = { m ( ) { }, get  p ( ) { return 1; }, [ "c" + 1 ] ( ) {}, * gen () {}, async  am () {} };
    same(o.m.toString(), "m ( ) { }", "method");
    same(Object.getOwnPropertyDescriptor(o, "p").get.toString(), "get  p ( ) { return 1; }", "getter");
    same(o.c1.toString(), '[ "c" + 1 ] ( ) {}', "computed method");
    same(o.gen.toString(), "* gen () {}", "generator method");
    same(o.am.toString(), "async  am () {}", "async method");
    class  K  extends  Object { constructor ( ) { super(); } static  s ( ) {} }
    same(K.toString(), "class  K  extends  Object { constructor ( ) { super(); } static  s ( ) {} }", "class");
    same(K.s.toString(), "s ( ) {}", "static method");
    class Empty {}
    same(Empty.toString(), "class Empty {}", "default constructor");
    async function * ag ( ) { }
    same(ag.toString(), "async function * ag ( ) { }", "async generator");
    "ok";
    "#);
}

#[test]
fn function_to_string_native() {
    ok(r#"
    // NativeFunction: function <name>() { [native code] }
    function check(f, expected, what) {
      var s = Function.prototype.toString.call(f);
      same(s, expected, what);
    }
    check(Object, "function Object() { [native code] }", "Object");
    check(Object.prototype.hasOwnProperty, "function hasOwnProperty() { [native code] }", "method");
    check(Function.prototype, "function () { [native code] }", "Function.prototype");
    check(Object.getOwnPropertyDescriptor(Object.prototype, "__proto__").get,
          "function get __proto__() { [native code] }", "getter");
    check(Function.prototype[Symbol.hasInstance],
          "function [Symbol.hasInstance]() { [native code] }", "symbol-named");
    check(function () {}.bind(), "function () { [native code] }", "bound");
    check(new Proxy(function () {}, {}), "function () { [native code] }", "callable proxy");
    check(Proxy.revocable({}, {}).revoke, "function () { [native code] }", "revoker");
    throws(TypeError, function () { Function.prototype.toString.call({}); }, "non-callable");
    throws(TypeError, function () { Function.prototype.toString.call(new Proxy({}, {})); }, "non-callable proxy");
    "ok";
    "#);
}

#[test]
fn create_dynamic_function() {
    ok(r#"
    var f = Function("a", "b", "return a + b");
    same(f(1, 2), 3, "Function call");
    same(f.toString(), "function anonymous(a,b\n) {\nreturn a + b\n}", "source text");
    checkFn(f, 2, "anonymous", "dynamic function");
    same(Object.getPrototypeOf(f), Function.prototype, "proto");
    var P = data(f, "prototype", true, false, false, "prototype");
    same(P.constructor, f, "prototype.constructor");
    same(new Function().toString(), "function anonymous(\n) {\n\n}", "no arguments");
    same(Function("a, b", "c", "return c").length, 3, "joined parameters");
    // The global scope, not the caller's.
    var x = "global";
    function outer() { var x = "local"; return Function("return x")(); }
    same(outer(), "global", "global scope");
    // Sloppy unless the body says otherwise.
    same(Function("return this")(), globalThis, "sloppy this");
    same(Function("'use strict'; return this")(), undefined, "strict this");
    // Parameters and body are checked on their own.
    throws(SyntaxError, function () { Function("/*", "*/){"); }, "comment across parts");
    throws(SyntaxError, function () { Function("a){ return 1; }; (function(", ""); }, "params escape");
    throws(SyntaxError, function () { Function("}); (function(){"); }, "body escape");
    throws(SyntaxError, function () { Function("a", "a", "'use strict';"); }, "strict duplicate params");
    throws(SyntaxError, function () { Function("super()"); }, "super call");
    // Conversions run in order, parameters before the body.
    var log = [];
    Function({ toString() { log[log.length] = "p"; return "a"; } },
             { toString() { log[log.length] = "b"; return ""; } });
    same(j(log), "p,b", "conversion order");
    // new.target gives the prototype.
    class MyF extends Function {}
    var mf = new MyF("return 1");
    same(Object.getPrototypeOf(mf), MyF.prototype, "subclass proto");
    same(mf(), 1, "subclass call");

    var GF = Object.getPrototypeOf(function* () {}).constructor;
    var g = GF("a", "yield a; yield a * 2");
    same(j([...g(3)]), "3,6", "GeneratorFunction");
    same(g.toString(), "function* anonymous(a\n) {\nyield a; yield a * 2\n}", "generator source");
    same(Object.getPrototypeOf(g.prototype), GF.prototype.prototype, "generator prototype");
    same(Object.prototype.hasOwnProperty.call(g.prototype, "constructor"), false, "no constructor");
    throws(SyntaxError, function () { GF("yield", ""); }, "yield param");
    var AF = Object.getPrototypeOf(async function () {}).constructor;
    var af = AF("return await 5");
    same(af.toString(), "async function anonymous(\n) {\nreturn await 5\n}", "async source");
    same(Object.prototype.hasOwnProperty.call(af, "prototype"), false, "async has no prototype");
    same(af() instanceof Promise, true, "async returns a promise");
    var AGF = Object.getPrototypeOf(async function* () {}).constructor;
    var agf = AGF("yield 1");
    same(agf.toString(), "async function* anonymous(\n) {\nyield 1\n}", "async generator source");
    same(typeof agf().next, "function", "async generator object");
    "ok";
    "#);
}

#[test]
fn errors() {
    ok(r#"
    var e = new Error("m", { cause: 0 });
    same(data(e, "message", true, false, true, "message"), "m", "message");
    same(data(e, "cause", true, false, true, "cause"), 0, "cause");
    same(Object.prototype.hasOwnProperty.call(new Error("m", {}), "cause"), false, "no cause");
    same(Object.prototype.hasOwnProperty.call(new Error(), "message"), false, "no message");
    same(Error("x") instanceof Error, true, "call without new");
    same(new TypeError("t", { cause: 1 }).cause, 1, "native cause");
    same("" + new RangeError("r"), "RangeError: r", "toString");
    var tos = Error.prototype.toString;
    same(tos.call({}), "Error", "toString defaults");
    same(tos.call({ name: "", message: "m" }), "m", "empty name");
    same(tos.call({ name: "N", message: "" }), "N", "empty message");
    same(tos.call({ name: undefined, message: undefined }), "Error", "undefined fields");
    throws(TypeError, function () { tos.call(1); }, "toString non-object");
    same(Error.isError(new Error()), true, "isError error");
    same(Error.isError(new URIError()), true, "isError native");
    same(Error.isError(Object.create(Error.prototype)), false, "isError ordinary");
    same(Error.isError(new Proxy(new Error(), {})), false, "isError proxy");
    same(Error.isError(Error.prototype), false, "isError prototype");
    class MyErr extends Error {}
    same(Error.isError(new MyErr()), true, "isError subclass");
    var a = new AggregateError([1, 2], "agg", { cause: "c" });
    same(a.message + a.cause + j(a.errors), "aggc1,2", "AggregateError");
    var ed = Object.getOwnPropertyDescriptor(a, "errors");
    same(ed.enumerable === false && ed.writable && ed.configurable, true, "errors attributes");
    same(Object.prototype.toString.call(a.errors), "[object Array]", "errors is an array");
    throws(TypeError, function () { new AggregateError(1); }, "non-iterable errors");
    same(Object.prototype.hasOwnProperty.call(AggregateError([]), "message"), false, "agg no message");
    var s = new SuppressedError(1, 2, "s");
    same(s.error + "," + s.suppressed + "," + s.message, "1,2,s", "SuppressedError");
    same(Error.isError(s), true, "SuppressedError isError");
    same(s instanceof SuppressedError && s instanceof Error, true, "SuppressedError instanceof");
    "ok";
    "#);
}

#[test]
fn symbols_and_booleans() {
    ok(r#"
    var s = Symbol("d");
    same(s.description, "d", "description");
    same(Symbol().description, undefined, "no description");
    same(Symbol("").description, "", "empty description");
    same(s.toString(), "Symbol(d)", "toString");
    same(Object(s).valueOf(), s, "valueOf");
    same(Object(s)[Symbol.toPrimitive]("number"), s, "@@toPrimitive");
    throws(TypeError, function () { new Symbol(); }, "new Symbol");
    throws(TypeError, function () { Symbol.prototype.toString.call({}); }, "thisSymbolValue");
    same(Symbol.for("k"), Symbol.for("k"), "Symbol.for");
    same(Symbol.keyFor(Symbol.for("k")), "k", "keyFor");
    same(Symbol.keyFor(s), undefined, "keyFor unregistered");
    same(Symbol.keyFor(Symbol.iterator), undefined, "keyFor well-known");
    throws(TypeError, function () { Symbol.keyFor("k"); }, "keyFor non-symbol");
    same(Boolean(""), false, "Boolean()");
    var b = new Boolean(0);
    same(typeof b, "object", "new Boolean");
    same(b.valueOf(), false, "valueOf");
    same(new Boolean(1).toString(), "true", "toString");
    throws(TypeError, function () { Boolean.prototype.valueOf.call({}); }, "thisBooleanValue");
    class MyB extends Boolean {}
    same(new MyB(true) instanceof MyB, true, "Boolean subclass");
    "ok";
    "#);
}

#[test]
fn reflect_methods() {
    ok(r#"
    same(Reflect.apply(function (a) { "use strict"; return this + a; }, 1, [2]), 3, "apply");
    throws(TypeError, function () { Reflect.apply(1, null, []); }, "apply non-callable");
    throws(TypeError, function () { Reflect.apply(function () {}, null); }, "apply missing list");
    function C() { this.nt = new.target; }
    function D() {}
    var o = Reflect.construct(C, [], D);
    same(o.nt, D, "construct newTarget");
    same(Object.getPrototypeOf(o), D.prototype, "construct proto");
    throws(TypeError, function () { Reflect.construct(C, [], {}); }, "bad newTarget");
    throws(TypeError, function () { Reflect.construct(() => 0, []); }, "non-constructor");
    var t = {};
    same(Reflect.defineProperty(t, "x", { value: 1 }), true, "defineProperty");
    same(Reflect.defineProperty(t, "x", { value: 2 }), false, "defineProperty fails");
    same(Reflect.deleteProperty(t, "x"), false, "deleteProperty non-configurable");
    var recv = { y: 5 };
    same(Reflect.get({ get z() { return this.y; } }, "z", recv), 5, "get receiver");
    same(Reflect.getOwnPropertyDescriptor(t, "x").value, 1, "getOwnPropertyDescriptor");
    same(Reflect.getPrototypeOf([]), Object.getPrototypeOf([]), "getPrototypeOf");
    same(Reflect.has(Object.create({ q: 1 }), "q"), true, "has");
    same(Reflect.isExtensible(Object.preventExtensions({})), false, "isExtensible");
    same(Reflect.ownKeys({ b: 1, a: 2, 1: 0, [Symbol.iterator]: 0 }).length, 4, "ownKeys");
    same(j(Reflect.ownKeys({ b: 1, a: 2, 1: 0 })), "1,b,a", "ownKeys order");
    same(Reflect.preventExtensions({}), true, "preventExtensions");
    var r2 = {};
    same(Reflect.set({ set w(v) { this.v = v; } }, "w", 9, r2), true, "set receiver");
    same(r2.v, 9, "set wrote the receiver");
    same(Reflect.set(Object.freeze({ a: 1 }), "a", 2), false, "set frozen");
    same(Reflect.setPrototypeOf({}, null), true, "setPrototypeOf");
    same(Reflect.setPrototypeOf(Object.preventExtensions({}), {}), false, "setPrototypeOf non-extensible");
    throws(TypeError, function () { Reflect.setPrototypeOf({}, 1); }, "setPrototypeOf bad proto");
    for (var k of Reflect.ownKeys(Reflect)) {
      if (typeof Reflect[k] === "function")
        throws(TypeError, function () { Reflect[k](1, "x", []); }, "Reflect." + k + " non-object");
    }
    "ok";
    "#);
}

#[test]
fn proxy_traps_and_invariants() {
    ok(r#"
    throws(TypeError, function () { Proxy({}, {}); }, "call without new");
    throws(TypeError, function () { new Proxy(1, {}); }, "non-object target");
    throws(TypeError, function () { new Proxy({}, null); }, "non-object handler");
    var log = [];
    var handler = new Proxy({}, { get(t, name) { log[log.length] = name; return undefined; } });
    var p = new Proxy({}, handler);
    p.a; p.a = 1; "a" in p; delete p.a; Object.keys(p);
    Object.getPrototypeOf(p); Object.setPrototypeOf(p, null);
    Object.getOwnPropertyDescriptor(p, "a"); Object.defineProperty(p, "b", { value: 1, configurable: true });
    Object.isExtensible(p); Object.preventExtensions(p);
    same(j(log), "get,set,getOwnPropertyDescriptor,defineProperty,has,deleteProperty,ownKeys,getPrototypeOf,setPrototypeOf,getOwnPropertyDescriptor,defineProperty,isExtensible,preventExtensions", "trap lookups");

    // Traps that are not callable.
    throws(TypeError, function () { new Proxy({}, { get: 1 }).x; }, "non-callable trap");

    // get: non-configurable, non-writable data property must report its value.
    var t = {};
    Object.defineProperty(t, "k", { value: 1 });
    throws(TypeError, function () { new Proxy(t, { get() { return 2; } }).k; }, "get invariant");
    same(new Proxy(t, { get() { return 1; } }).k, 1, "get invariant satisfied");
    // set: cannot report success for a non-writable, non-configurable property.
    throws(TypeError, function () { Reflect.set(new Proxy(t, { set() { return true; } }), "k", 2); }, "set invariant");
    // has: cannot hide a non-configurable property.
    throws(TypeError, function () { "k" in new Proxy(t, { has() { return false; } }); }, "has invariant");
    // deleteProperty: cannot report deleting a non-configurable property.
    throws(TypeError, function () { Reflect.deleteProperty(new Proxy(t, { deleteProperty() { return true; } }), "k"); }, "delete invariant");
    // defineProperty: cannot add to a non-extensible target.
    var ne = Object.preventExtensions({});
    throws(TypeError, function () { Object.defineProperty(new Proxy(ne, { defineProperty() { return true; } }), "z", { value: 1 }); }, "defineProperty invariant");
    // getOwnPropertyDescriptor: cannot report non-configurable if it is not.
    throws(TypeError, function () {
      Object.getOwnPropertyDescriptor(new Proxy({ a: 1 }, { getOwnPropertyDescriptor() { return { value: 1, configurable: false }; } }), "a");
    }, "getOwnPropertyDescriptor invariant");
    throws(TypeError, function () {
      Object.getOwnPropertyDescriptor(new Proxy(t, { getOwnPropertyDescriptor() { return undefined; } }), "k");
    }, "cannot hide non-configurable");
    // ownKeys: must include non-configurable keys, no duplicates, and only
    // strings and symbols.
    throws(TypeError, function () { Reflect.ownKeys(new Proxy(t, { ownKeys() { return []; } })); }, "ownKeys missing key");
    throws(TypeError, function () { Reflect.ownKeys(new Proxy({}, { ownKeys() { return ["a", "a"]; } })); }, "ownKeys duplicates");
    throws(TypeError, function () { Reflect.ownKeys(new Proxy({}, { ownKeys() { return [1]; } })); }, "ownKeys non-key");
    throws(TypeError, function () { Reflect.ownKeys(new Proxy(ne, { ownKeys() { return ["extra"]; } })); }, "ownKeys extra on non-extensible");
    // getPrototypeOf: must agree on a non-extensible target.
    throws(TypeError, function () { Object.getPrototypeOf(new Proxy(ne, { getPrototypeOf() { return null; } })); }, "getPrototypeOf invariant");
    throws(TypeError, function () { Object.getPrototypeOf(new Proxy({}, { getPrototypeOf() { return 1; } })); }, "getPrototypeOf non-object");
    // setPrototypeOf: must agree on a non-extensible target.
    throws(TypeError, function () { Reflect.setPrototypeOf(new Proxy(ne, { setPrototypeOf() { return true; } }), null); }, "setPrototypeOf invariant");
    // isExtensible / preventExtensions must agree with the target.
    throws(TypeError, function () { Object.isExtensible(new Proxy({}, { isExtensible() { return false; } })); }, "isExtensible invariant");
    throws(TypeError, function () { Object.preventExtensions(new Proxy({}, { preventExtensions() { return true; } })); }, "preventExtensions invariant");
    // apply and construct.
    var fp = new Proxy(function () { return 1; }, { apply(t, self, args) { return args.length; } });
    same(fp(1, 2, 3), 3, "apply trap");
    var cp = new Proxy(function () {}, { construct(t, args, nt) { return { n: args[0] }; } });
    same(new cp(7).n, 7, "construct trap");
    throws(TypeError, function () { new (new Proxy(function () {}, { construct() { return 1; } }))(); }, "construct must return object");
    throws(TypeError, function () { new (new Proxy(() => 0, {}))(); }, "arrow proxy not a constructor");
    same(typeof new Proxy(function () {}, {}), "function", "callable proxy typeof");
    same(typeof new Proxy({}, {}), "object", "object proxy typeof");

    // Revocation.
    var r = Proxy.revocable({}, {});
    same(r.proxy.x, undefined, "before revoke");
    same(r.revoke(), undefined, "revoke returns undefined");
    same(r.revoke(), undefined, "second revoke is a no-op");
    throws(TypeError, function () { r.proxy.x; }, "revoked get");
    throws(TypeError, function () { Object.keys(r.proxy); }, "revoked ownKeys");
    var withRevokedHandler = new Proxy({}, r.proxy);
    throws(TypeError, function () { withRevokedHandler.x; }, "revoked handler");
    var rf = Proxy.revocable(function () {}, {});
    rf.revoke();
    same(typeof rf.proxy, "function", "revoked function proxy keeps its type");
    throws(TypeError, function () { rf.proxy(); }, "revoked call");
    "ok";
    "#);
}

#[test]
fn global_functions() {
    ok(r#"
    same(isFinite("12"), true, "isFinite");
    same(isFinite(Infinity), false, "isFinite Infinity");
    same(isNaN("x"), true, "isNaN");
    throws(TypeError, function () { isNaN(Symbol()); }, "isNaN symbol");
    same(parseFloat("  3.25abc"), 3.25, "parseFloat");
    same(parseFloat("-.5e1x"), -5, "parseFloat exponent");
    same(parseFloat("1e"), 1, "parseFloat dangling e");
    same(parseFloat("-0"), -0, "parseFloat -0");
    same(parseFloat("Infinityx"), Infinity, "parseFloat Infinity");
    same(parseFloat("-Infinity"), -Infinity, "parseFloat -Infinity");
    same(parseFloat("infinity"), NaN, "parseFloat case");
    same(parseFloat("0x10"), 0, "parseFloat hex");
    same(parseFloat("."), NaN, "parseFloat dot");
    same(parseFloat(" ﻿ 7"), 7, "parseFloat whitespace");
    same(parseInt("  42px"), 42, "parseInt");
    same(parseInt("-0x1F"), -31, "parseInt hex");
    same(parseInt("0x1F", 16), 31, "parseInt hex radix 16");
    same(parseInt("0x1F", 10), 0, "parseInt hex radix 10");
    same(parseInt("11", 2), 3, "parseInt binary");
    same(parseInt("z", 36), 35, "parseInt radix 36");
    same(parseInt("1", 37), NaN, "parseInt radix 37");
    same(parseInt("1", 1), NaN, "parseInt radix 1");
    same(parseInt("10", 4294967312), 16, "parseInt radix ToInt32");
    same(parseInt("-0"), -0, "parseInt -0");
    same(parseInt(""), NaN, "parseInt empty");
    same(parseInt("9007199254740993"), 9007199254740992, "parseInt rounding");
    same(parseInt("1e3"), 1, "parseInt stops at e");
    "ok";
    "#);
}

#[test]
fn uri_functions() {
    ok(r#"
    same(encodeURIComponent("a b&c/é€😀"), "a%20b%26c%2F%C3%A9%E2%82%AC%F0%9F%98%80", "encodeURIComponent");
    same(encodeURI("http://x.y/a b?q=1#f"), "http://x.y/a%20b?q=1#f", "encodeURI");
    same(encodeURIComponent("-_.!~*'()"), "-_.!~*'()", "unreserved marks");
    same(encodeURI(";/?:@&=+$,#"), ";/?:@&=+$,#", "reserved kept by encodeURI");
    same(encodeURIComponent(";/?:@&=+$,#"), "%3B%2F%3F%3A%40%26%3D%2B%24%2C%23", "reserved escaped");
    throws(URIError, function () { encodeURIComponent("\ud800"); }, "lone high surrogate");
    throws(URIError, function () { encodeURIComponent("\udc00"); }, "lone low surrogate");
    throws(URIError, function () { encodeURI("a\ud800b"); }, "high surrogate followed by non-low");
    same(decodeURIComponent("%C3%A9%e2%82%ac%F0%9F%98%80"), "é€😀", "decodeURIComponent");
    same(decodeURI("%3B%2f%23%20"), "%3B%2f%23 ", "decodeURI preserves reserved escapes");
    same(decodeURIComponent("%3B%2f%23"), ";/#", "decodeURIComponent decodes all");
    var bad = ["%", "%1", "%G0", "%80", "%C3", "%C3%", "%C3%2", "%C3%41", "%C0%80",
               "%E0%80%80", "%ED%A0%80", "%F4%90%80%80", "%F8%80%80%80%80", "%FF",
               "%C3%A9%"];
    for (var b of bad) {
      throws(URIError, function () { decodeURIComponent(b); }, "decodeURIComponent " + b);
      throws(URIError, function () { decodeURI(b); }, "decodeURI " + b);
    }
    var u = new URIError("x");
    same(u instanceof URIError && u.name === "URIError", true, "URIError");
    "ok";
    "#);
}

#[test]
fn indirect_eval() {
    ok(r#"
    var x = "global";
    function f() { var x = "local"; var e = eval; return e("x"); }
    same(f(), "global", "indirect eval uses the global scope");
    same((0, eval)("var declaredByEval = 1; declaredByEval"), 1, "indirect eval var");
    same(Object.getOwnPropertyDescriptor(globalThis, "declaredByEval").configurable, true, "eval var configurable");
    same(eval(5), 5, "non-string argument");
    throws(SyntaxError, function () { (0, eval)("("); }, "syntax error");
    "ok";
    "#);
}
