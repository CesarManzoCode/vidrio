//! Interpreter tests: fragments evaluated as scripts with the expected
//! completion value (or the name of the uncaught error), one group per
//! family: scopes and TDZ, closures, `with`, direct and indirect `eval`,
//! `arguments`, destructuring, iterators and their closing, exceptions and
//! `finally`, classes and private names, generators, `async`/`await` with
//! the job queue drained, completion values, `using`, templates, labels,
//! `switch`, `for-in` order, optional chains, operators and safe points.

use vjs::agent::Agent;
use vjs::ops;
use vjs::value::{PropertyKey, Value};
use vjs::{Throw, evaluate_script};

/// An agent with a realm and a few globals the tests need before the
/// built-ins exist: the well-known symbols as `SYM_*`.
fn setup() -> Agent {
    let mut agent = Agent::new();
    agent.initialize_host_defined_realm();
    let global = agent.global_object();
    let symbols = [
        ("SYM_ITERATOR", agent.symbols.iterator),
        ("SYM_ASYNC_ITERATOR", agent.symbols.async_iterator),
        ("SYM_DISPOSE", agent.symbols.dispose),
        ("SYM_ASYNC_DISPOSE", agent.symbols.async_dispose),
        ("SYM_UNSCOPABLES", agent.symbols.unscopables),
    ];
    for (name, sym) in symbols {
        let key = agent.heap.key_from_str(name);
        ops::create_data_property_or_throw(&mut agent, global, key, Value::Symbol(sym)).unwrap();
    }
    agent
}

fn show(agent: &mut Agent, v: Value) -> String {
    match ops::to_string(agent, v) {
        Ok(s) => agent.heap.string_to_rust(s),
        Err(_) => "<unprintable>".to_string(),
    }
}

/// `throw <name>` for errors, `throw <value>` otherwise.
fn show_throw(agent: &mut Agent, Throw(v): Throw) -> String {
    if let Value::Object(o) = v {
        let key: PropertyKey = agent.heap.key_from_str("name");
        if let Ok(name) = ops::get(agent, o, key) {
            return format!("throw {}", show(agent, name));
        }
    }
    format!("throw {}", show(agent, v))
}

fn eval_in(agent: &mut Agent, src: &str) -> String {
    match evaluate_script(agent, src) {
        Ok(v) => show(agent, v),
        Err(e) => show_throw(agent, e),
    }
}

/// Evaluates `src` in a fresh agent.
fn run(src: &str) -> String {
    let mut agent = setup();
    eval_in(&mut agent, src)
}

/// Evaluates `src`, drains the job queue, then evaluates `result`.
fn run_async(src: &str, result: &str) -> String {
    let mut agent = setup();
    let first = eval_in(&mut agent, src);
    assert!(!first.starts_with("throw"), "script threw: {first}");
    let errors = agent.run_jobs();
    assert!(errors.is_empty(), "jobs threw");
    eval_in(&mut agent, result)
}

fn check(cases: &[(&str, &str)]) {
    for (src, expected) in cases {
        assert_eq!(run(src), *expected, "source: {src}");
    }
}

#[test]
fn scopes_and_tdz() {
    check(&[
        ("let x = 1; { let x = 2; } x", "1"),
        (
            "var r; try { x; let x = 1; } catch (e) { r = e instanceof ReferenceError } r",
            "true",
        ),
        ("typeof undeclaredName", "undefined"),
        (
            "const c = 1; var r; try { c = 2 } catch (e) { r = e instanceof TypeError } r",
            "true",
        ),
        ("f(); function f() { return 1 }", "1"),
        (
            "var a = 1; function g() { return a; var a = 2; } g()",
            "undefined",
        ),
        (
            "var r; try { typeof h } catch (e) { r = e instanceof ReferenceError } let h; r",
            "true",
        ),
        (
            "var fs = []; for (let i = 0; i < 3; i++) fs[fs.length] = function () { return i }; '' + fs[0]() + fs[1]() + fs[2]()",
            "012",
        ),
        (
            "var fs = []; for (var i = 0; i < 3; i++) fs[fs.length] = function () { return i }; '' + fs[0]() + fs[1]() + fs[2]()",
            "333",
        ),
        ("function f(a = () => b, b = 2) { return a() } f()", "2"),
        ("function f(a = 1) { var a; return a } f()", "1"),
        ("{ function inner() { return 'block' } inner() }", "block"),
        ("let q = 1; q++; q", "2"),
    ]);
}

#[test]
fn global_declaration_conflicts() {
    let mut agent = setup();
    assert_eq!(eval_in(&mut agent, "let q = 1; q"), "1");
    assert_eq!(eval_in(&mut agent, "var q;"), "throw SyntaxError");
    assert_eq!(eval_in(&mut agent, "q + 1"), "2");
    assert_eq!(
        eval_in(&mut agent, "var v = 3; function fn() {}"),
        "undefined"
    );
    assert_eq!(eval_in(&mut agent, "let v;"), "throw SyntaxError");
    assert_eq!(eval_in(&mut agent, "typeof fn"), "function");
    assert_eq!(eval_in(&mut agent, "let let = 1"), "throw SyntaxError");
}

#[test]
fn closures() {
    check(&[
        (
            "function mk() { var n = 0; return function () { return ++n } } var c = mk(); c(); c(); c()",
            "3",
        ),
        (
            "var f = function g() { g = 1; return typeof g }; f()",
            "function",
        ),
        (
            "var r; var f = function g() { 'use strict'; try { g = 1 } catch (e) { r = e instanceof TypeError } }; f(); r",
            "true",
        ),
        (
            "function outer() { var x = 'o'; return () => () => x } outer()()()",
            "o",
        ),
        ("var f = function () {}; f.name", "f"),
        ("var o = { m() {}, get g() { return 1 } }; o.m.name", "m"),
        ("var k = 'key'; var o = { [k]: () => 0 }; o.key.name", "key"),
        ("(function (a, b, c = 1, d) {}).length", "2"),
        ("(class { constructor(a, b) {} }).length", "2"),
    ]);
}

#[test]
fn with_statement() {
    check(&[
        ("var o = { x: 1 }; with (o) { x = 2; } o.x", "2"),
        (
            "var x = 'outer'; var o = { x: 'inner' }; with (o) { x }",
            "inner",
        ),
        (
            "var o = { x: 1 }; o[SYM_UNSCOPABLES] = { x: true }; var x = 'g'; with (o) { x }",
            "g",
        ),
        (
            "var o = { f: function () { return this === o } }; with (o) { f() }",
            "true",
        ),
        (
            "function f() { var y = 'local'; with ({}) { return y } } f()",
            "local",
        ),
    ]);
}

#[test]
fn eval_direct_and_indirect() {
    check(&[
        ("function f() { var a = 5; return eval('a + 1') } f()", "6"),
        ("function f() { eval('var b = 7'); return b } f()", "7"),
        (
            "function f() { 'use strict'; eval('var b = 7'); return typeof b } f()",
            "undefined",
        ),
        (
            "var a = 'global'; function f() { var a = 'local'; return (0, eval)('a') } f()",
            "global",
        ),
        (
            "function f() { let a; eval('var a') } var r; try { f() } catch (e) { r = e instanceof SyntaxError } r",
            "true",
        ),
        (
            "var o = { m: function () { return eval('this') === o } }; o.m()",
            "true",
        ),
        (
            "function f() { eval('var d = 1'); delete d; return typeof d } f()",
            "undefined",
        ),
        ("eval('function ev() { return 3 }'); ev()", "3"),
        (
            "var r; try { eval('let z = 1; z +') } catch (e) { r = e instanceof SyntaxError } r",
            "true",
        ),
        ("eval(42)", "42"),
        (
            "class A { m() { return 'A' } } class B extends A { m() { return eval('super.m()') } } new B().m()",
            "A",
        ),
        (
            "function F() { return eval('new.target') === F } var r; function G() { r = eval('new.target') === G } new G(); r",
            "true",
        ),
    ]);
}

#[test]
fn arguments_objects() {
    check(&[
        ("function f(a) { arguments[0] = 9; return a } f(1)", "9"),
        ("function f(a) { a = 3; return arguments[0] } f(1)", "3"),
        (
            "function f(a) { 'use strict'; arguments[0] = 9; return a } f(1)",
            "1",
        ),
        ("function f(a = 0) { a = 5; return arguments[0] } f(1)", "1"),
        ("function f() { return arguments.length } f(1, 2, 3)", "3"),
        (
            "function f() { 'use strict'; try { arguments.callee; return 'no' } catch (e) { return e instanceof TypeError } } f()",
            "true",
        ),
        ("function f() { return arguments.callee === f } f()", "true"),
        ("function f() { return (() => arguments[0])() } f(42)", "42"),
        (
            "function f() { var s = 0; for (var x of arguments) s += x; return s } f(1, 2, 3)",
            "6",
        ),
        (
            "function f(a) { arguments[1] = 2; return a + arguments.length } f(1)",
            "2",
        ),
    ]);
}

#[test]
fn destructuring() {
    check(&[
        (
            "var [a, , b = 3, ...c] = [1, 2, undefined, 4, 5]; '' + a + b + c.length + c[0]",
            "1324",
        ),
        (
            "var { p, q: { r } = { r: 2 }, ...rest } = { p: 1, s: 3, t: 4 }; '' + p + r + rest.s + rest.t + ('p' in rest)",
            "1234false",
        ),
        ("var a = 1, b = 2; [a, b] = [b, a]; '' + a + b", "21"),
        (
            "var r; try { var { x } = null } catch (e) { r = e instanceof TypeError } r",
            "true",
        ),
        ("var { f = function () {} } = {}; f.name", "f"),
        (
            "function g({ x, y = 2 }, [z]) { return x + y + z } g({ x: 1 }, [3])",
            "6",
        ),
        (
            "var o = {}; ({ a: o.x, b: o['y'] } = { a: 1, b: 2 }); o.x + o.y",
            "3",
        ),
        ("var k = 'dyn'; var { [k]: v } = { dyn: 'ok' }; v", "ok"),
        ("var [x = 1, y = x] = []; '' + x + y", "11"),
        ("var { length } = 'abc'; length", "3"),
    ]);
}

#[test]
fn iterators_and_closing() {
    let prelude = "var closed = 0; var it = {}; it[SYM_ITERATOR] = function () { var i = 0; return { next: function () { i++; return { value: i, done: i > 5 } }, return: function () { closed++; return {} } } };";
    check(&[
        (
            &format!("{prelude} for (var v of it) {{ if (v === 2) break; }} closed"),
            "1",
        ),
        (
            &format!("{prelude} var [a, b] = it; '' + a + b + closed"),
            "121",
        ),
        (
            &format!("{prelude} try {{ for (var w of it) {{ throw 1 }} }} catch (e) {{}} closed"),
            "1",
        ),
        (
            &format!("{prelude} var s = [...it]; '' + s.length + closed"),
            "50",
        ),
        (
            &format!(
                "{prelude} function f() {{ for (var v of it) {{ return v }} }} '' + f() + closed"
            ),
            "11",
        ),
        (
            &format!(
                "{prelude} outer: for (var x of [1]) {{ for (var y of it) {{ continue outer }} }} closed"
            ),
            "1",
        ),
        (
            "function* g() { yield 1; yield 2; } var t = 0; for (var x of g()) t += x; t",
            "3",
        ),
        (
            "var r; try { for (var x of 5) {} } catch (e) { r = e instanceof TypeError } r",
            "true",
        ),
        (
            "var log = ''; var it = {}; it[SYM_ITERATOR] = function () { return { next: function () { throw 'boom' }, return: function () { log += 'closed' } } }; try { for (var x of it) {} } catch (e) { log += e } log",
            "boom",
        ),
    ]);
}

#[test]
fn exceptions_and_finally() {
    check(&[
        (
            "function f() { try { return 1 } finally { return 2 } } f()",
            "2",
        ),
        (
            "function f() { var log = ''; for (var i = 0; i < 3; i++) { try { if (i == 1) continue; log += i; } finally { log += 'f' } } return log } f()",
            "0ff2f",
        ),
        (
            "function f() { try { throw new TypeError('x') } catch (e) { return e.message } } f()",
            "x",
        ),
        (
            "var s = ''; outer: for (;;) { try { try { break outer; } finally { s += 'a' } } finally { s += 'b' } } s",
            "ab",
        ),
        (
            "function f() { try { throw 1 } finally { return 'ok' } } f()",
            "ok",
        ),
        (
            "var e = 'outer'; try { throw 'inner' } catch (e) { } e",
            "outer",
        ),
        ("throw new RangeError('r')", "throw RangeError"),
        ("throw 'plain'", "throw plain"),
        (
            "function f() { try { return 'try' } finally { log = 'fin' } } var log; f() + log",
            "tryfin",
        ),
        (
            "var r = ''; try { try { throw 1 } finally { r += 'f' } } catch (e) { r += e } r",
            "f1",
        ),
        ("try { throw { a: 1 } } catch ({ a }) { a }", "1"),
        (
            "function r(n) { return n === 0 ? 0 : 1 + r(n - 1) } r(5000)",
            "5000",
        ),
        (
            "function f() { return f() } var r; try { f() } catch (e) { r = e instanceof RangeError } r",
            "true",
        ),
        ("null.x", "throw TypeError"),
        ("undefined()", "throw TypeError"),
        ("notDefined", "throw ReferenceError"),
    ]);
}

#[test]
fn classes_and_private_names() {
    check(&[
        (
            "class A { constructor(x) { this.x = x } get double() { return this.x * 2 } static make() { return new A(3) } } A.make().double",
            "6",
        ),
        (
            "class A { m() { return 'A' } } class B extends A { m() { return super.m() + 'B' } } new B().m()",
            "AB",
        ),
        (
            "class A {} class B extends A { constructor() { this.x = 1; super() } } var r; try { new B() } catch (e) { r = e instanceof ReferenceError } r",
            "true",
        ),
        (
            "class C { a = 1; b = this.a + 1; static s = 'S'; } var c = new C(); '' + c.a + c.b + C.s",
            "12S",
        ),
        (
            "class C { #p = 5; #m() { return this.#p } get p() { return this.#m() } static has(o) { return #p in o } } '' + new C().p + C.has(new C()) + C.has({})",
            "5truefalse",
        ),
        (
            "class C { #x; static get(o) { return o.#x } } var r; try { C.get({}) } catch (e) { r = e instanceof TypeError } r",
            "true",
        ),
        (
            "class C { static x; static { this.x = 'init' } } C.x",
            "init",
        ),
        (
            "class C {} var r; try { C() } catch (e) { r = e instanceof TypeError } r",
            "true",
        ),
        (
            "var k = 'dyn'; class C { [k]() { return 1 } } new C().dyn()",
            "1",
        ),
        (
            "class A { constructor(a, b) { this.s = a + b } } class B extends A {} new B(1, 2).s",
            "3",
        ),
        (
            "var r; function F() { r = new.target === F } new F(); r",
            "true",
        ),
        (
            "var base = { greet() { return 'hi' } }; var o = { __proto__: base, greet() { return super.greet() + '!' } }; o.greet()",
            "hi!",
        ),
        (
            "class C { m() {} } var n = 0; for (var k in C.prototype) n++; n",
            "0",
        ),
        (
            "class A { constructor() { this.f = () => this } } class B extends A { constructor() { var g = () => super(); g(); } } var b = new B(); b.f() === b",
            "true",
        ),
        (
            "class C { #v = 1; get #a() { return this.#v } set #a(x) { this.#v = x } bump() { this.#a += 10; return this.#a } } new C().bump()",
            "11",
        ),
        (
            "class C { static #s = 'st'; static read() { return C.#s } } C.read()",
            "st",
        ),
        (
            "var C = class Named { who() { return Named.name } }; new C().who()",
            "Named",
        ),
        (
            "class A { constructor() { return { custom: 1 } } } class B extends A {} new B().custom",
            "1",
        ),
        (
            "class B extends null {} var r; try { new B() } catch (e) { r = e instanceof TypeError } r",
            "true",
        ),
        (
            "class C { x = () => this; } var c = new C(); c.x() === c",
            "true",
        ),
        ("class C { ['f' + 1] = 'computed' } new C().f1", "computed"),
        ("class C { static f = function () {} } C.f.name", "f"),
        ("var o = { get x() { return 'g' } }; o.x", "g"),
    ]);
}

#[test]
fn generators() {
    check(&[
        (
            "function* g() { var x = yield 1; yield x * 2 } var it = g(); it.next(); it.next(5).value",
            "10",
        ),
        (
            "var log = ''; function* g() { try { yield 1; yield 2 } finally { log += 'f' } } var it = g(); it.next(); var r = it.return(9); log + r.value + r.done",
            "f9true",
        ),
        (
            "function* g() { try { yield 1 } catch (e) { yield 'caught ' + e } } var it = g(); it.next(); it.throw('x').value",
            "caught x",
        ),
        (
            "function* inner() { yield 1; return 'r' } function* outer() { var v = yield* inner(); yield v } var s = ''; for (var x of outer()) s += x; s",
            "1r",
        ),
        (
            "function* g() { yield 1 } var it = g(); var r = it.return(5); '' + r.value + r.done + it.next().done",
            "5truetrue",
        ),
        (
            "var closed = false; function* inner() { try { yield 1; yield 2 } finally { closed = true } } function* outer() { yield* inner() } var it = outer(); it.next(); it.return(); closed",
            "true",
        ),
        (
            "function* g() { yield 1 } var it = g(); it[SYM_ITERATOR]() === it",
            "true",
        ),
        ("function* g(a = 1) { yield a } g().next().value", "1"),
        (
            "var r; function* g() { r = typeof it.next; yield } var it = g(); it.next(); r",
            "function",
        ),
        (
            "function* h() { yield 1 } var hi = h(); hi.next(); '' + hi.next().done",
            "true",
        ),
        (
            "function* g() { yield* [1, 2]; } var s = ''; for (var x of g()) s += x; s",
            "12",
        ),
        (
            "var o = { *m() { yield 'method' } }; o.m().next().value",
            "method",
        ),
        (
            "function* g() { var x = 0; while (true) { x = yield x + 1 } } var it = g(); it.next(); it.next(10).value",
            "11",
        ),
    ]);
}

#[test]
fn async_functions_and_jobs() {
    assert_eq!(
        run_async(
            "var r = ''; async function f() { r += 'a'; await null; r += 'c'; return 'v' } f().then(function (v) { r += v }); r += 'b';",
            "r",
        ),
        "abcv"
    );
    assert_eq!(
        run_async(
            "var r; async function f() { try { await new Promise(function (_, rej) { rej('e') }) } catch (e) { r = 'caught ' + e } } f();",
            "r",
        ),
        "caught e"
    );
    assert_eq!(
        run_async(
            "var r; async function f() { throw new Error('boom') } f().then(null, function (e) { r = e.message });",
            "r",
        ),
        "boom"
    );
    assert_eq!(
        run_async(
            "var r; async function f() { return await { then: function (res) { res(7) } } } f().then(function (v) { r = v });",
            "r",
        ),
        "7"
    );
    assert_eq!(
        run_async(
            "var log = ''; new Promise(function (res) { res() }).then(function () { log += '1' }); (async function () { log += 'a'; await undefined; log += '2' })(); log += 'b';",
            "log",
        ),
        "ab12"
    );
    assert_eq!(
        run_async(
            "var r; var o = { m() { return (async () => this)() } }; o.m().then(function (v) { r = v === o });",
            "r",
        ),
        "true"
    );
    assert_eq!(
        run_async(
            "var r; async function f(a = missing) {} f().then(null, function (e) { r = e instanceof ReferenceError });",
            "r",
        ),
        "true"
    );
}

#[test]
fn async_iteration() {
    assert_eq!(
        run_async(
            "var out = ''; async function* ag() { yield 1; await null; yield 2 } (async function () { for await (var v of ag()) out += v })();",
            "out",
        ),
        "12"
    );
    assert_eq!(
        run_async(
            "var out = ''; (async function () { for await (var v of [new Promise(function (r) { r('x') }), 'y']) out += v })();",
            "out",
        ),
        "xy"
    );
    assert_eq!(
        run_async(
            "var log = ''; async function* g() { yield 1; yield 2 } var it = g(); it.next().then(function (r) { log += r.value }); it.next().then(function (r) { log += r.value }); it.next().then(function (r) { log += r.done });",
            "log",
        ),
        "12true"
    );
    assert_eq!(
        run_async(
            "var log = ''; async function* g() { try { yield 1 } finally { log += 'f' } } var it = g(); it.next().then(function () { return it.return('r') }).then(function (r) { log += r.value + r.done });",
            "log",
        ),
        "frtrue"
    );
    assert_eq!(
        run_async(
            "var out = ''; async function* inner() { yield 'a'; yield 'b' } async function* outer() { yield* inner(); yield 'c' } (async function () { for await (var v of outer()) out += v })();",
            "out",
        ),
        "abc"
    );
    assert_eq!(
        run_async(
            "var closed = false; var it = {}; it[SYM_ASYNC_ITERATOR] = function () { return { next: function () { return { value: 1, done: false } }, return: function () { closed = true; return {} } } }; (async function () { for await (var v of it) break })();",
            "closed",
        ),
        "true"
    );
}

#[test]
fn completion_values() {
    check(&[
        ("1; var x = 2;", "1"),
        ("if (true) { 'a' } else { 'b' }", "a"),
        ("var i = 0; while (i < 3) { i++; }", "2"),
        ("2; for (var k in {}) {}", "undefined"),
        ("3; try { } catch (e) { }", "undefined"),
        ("eval('1; if (true) { 2; }')", "2"),
        ("eval('3; var z;')", "3"),
        ("eval('4; if (false) {}')", "undefined"),
        ("eval('5; do { 6; break; } while (false)')", "6"),
        ("eval('7; try { 8 } finally { 9 }')", "8"),
        ("eval('L: { 10; break L; }')", "10"),
        ("eval('11; switch (1) { case 1: }')", "undefined"),
        ("eval('12; function decl() {}')", "12"),
        ("eval('13; class K {}')", "13"),
        ("eval('try { throw 1 } catch (e) { 14 }')", "14"),
        ("eval('15; try { 16; throw 0 } catch (e) { }')", "undefined"),
        ("eval('17; with ({}) {}')", "undefined"),
        (
            "eval('do { 18; if (true) { break; } } while (false)')",
            "undefined",
        ),
        ("eval('\"use strict\"')", "use strict"),
        ("eval('')", "undefined"),
    ]);
}

#[test]
fn explicit_resource_management() {
    check(&[
        (
            "var log = ''; { using a = { [SYM_DISPOSE]: function () { log += 'a' } }; using b = { [SYM_DISPOSE]: function () { log += 'b' } }; log += 'body'; } log",
            "bodyba",
        ),
        (
            "var r; try { using a = { [SYM_DISPOSE]() { throw 'd' } }; throw 'body' } catch (e) { r = e.error + '/' + e.suppressed } r",
            "d/body",
        ),
        (
            "var log = ''; function f() { using x = { [SYM_DISPOSE]() { log += 'x' } }; return 'ret' } f() + log",
            "retx",
        ),
        (
            "var log = ''; for (var i = 0; i < 2; i++) { using r = { [SYM_DISPOSE]() { log += i } }; if (i == 0) continue; } log",
            "01",
        ),
        (
            "var r; try { using n = 5 } catch (e) { r = e instanceof TypeError } r",
            "true",
        ),
        ("{ using n = null; 'ok' }", "ok"),
    ]);
    assert_eq!(
        run_async(
            "var log = ''; (async function () { { await using a = { [SYM_ASYNC_DISPOSE]() { log += 'ad'; return new Promise(function (r) { r() }) } }; log += 'b'; } log += 'e' })();",
            "log",
        ),
        "bade"
    );
}

#[test]
fn templates() {
    check(&[
        ("var x = 2; `a${x}b${x + 1}`", "a2b3"),
        (
            "function tag(s) { return s } function f() { return tag`a${1}b` } f() === f()",
            "true",
        ),
        ("function tag(s) { return s } tag`x` === tag`x`", "false"),
        ("(function (s) { return s.raw[0] })`\\n`", "\\n"),
        (
            "(function (s) { return s[0] === undefined })`\\unicode`",
            "true",
        ),
        (
            "function tag(s, a, b) { return s.length + ':' + a + b } tag`1${'x'}2${'y'}3`",
            "3:xy",
        ),
        ("var o = { t(s) { return this === o } }; o.t`x`", "true"),
    ]);
}

#[test]
fn labels_and_switch() {
    check(&[
        (
            "var s = ''; switch (2) { case 1: s += 'a'; case 2: s += 'b'; case 3: s += 'c'; break; default: s += 'd' } s",
            "bc",
        ),
        (
            "var s = ''; switch (5) { case 1: s += '1'; default: s += 'd'; case 2: s += '2' } s",
            "d2",
        ),
        (
            "var s = ''; outer: for (var i = 0; i < 3; i++) { for (var j = 0; j < 3; j++) { if (j == 1) continue outer; s += i + '' + j + ';' } } s",
            "00;10;20;",
        ),
        (
            "var s = 'x'; block: { s += 'y'; break block; s += 'z' } s",
            "xy",
        ),
        ("switch (1) { case 1: let v = 'scoped'; v }", "scoped"),
        (
            "var n = 0; do { n++; if (n < 3) continue; break; } while (true); n",
            "3",
        ),
    ]);
}

#[test]
fn for_in_order() {
    check(&[
        (
            "var o = { b: 1, a: 2, 1: 3, 0: 4 }; var s = ''; for (var k in o) s += k; s",
            "01ba",
        ),
        (
            "var p = { x: 1, y: 2 }; var o = { __proto__: p, y: 3, z: 4 }; var s = ''; for (var k in o) s += k; s",
            "yzx",
        ),
        (
            "var o = { a: 1, b: 2, c: 3 }; var s = ''; for (var k in o) { s += k; delete o.b } s",
            "ac",
        ),
        (
            "var n = 0; for (var k in null) n++; for (var k in undefined) n++; n",
            "0",
        ),
        ("var s = ''; for (var k in 'ab') s += k; s", "01"),
        (
            "var fs = []; for (let k in { a: 1, b: 2 }) fs[fs.length] = () => k; fs[0]() + fs[1]()",
            "ab",
        ),
    ]);
}

#[test]
fn optional_chains_and_operators() {
    check(&[
        (
            "var o = null; '' + o?.x + o?.x.y.z + (o?.f())",
            "undefinedundefinedundefined",
        ),
        ("var o = { f() { return this === o } }; o?.f()", "true"),
        ("var o = null; delete o?.x", "true"),
        ("var o = { f: null }; o.f?.()", "undefined"),
        ("1 + '2'", "12"),
        ("2 ** 10", "1024"),
        ("10n ** 3n", "1000"),
        ("typeof 1n", "bigint"),
        ("1n + 1", "throw TypeError"),
        ("-7 % 3", "-1"),
        ("'b' > 'a'", "true"),
        ("1 / 0", "Infinity"),
        ("null ?? 'd'", "d"),
        ("0 ?? 'd'", "0"),
        ("5 >>> 1", "2"),
        ("-1 >>> 0", "4294967295"),
        ("var a = null; a ??= 'set'; a", "set"),
        ("var b = 1; b ||= 2; b", "1"),
        ("var c = 1; c &&= 3; c", "3"),
        ("var o = { n: 1 }; o.n += 4; o.n++; o.n", "6"),
        ("var x = 5; var y = x++; '' + x + y", "65"),
        ("typeof function () {}", "function"),
        ("typeof null", "object"),
        ("'x' in { x: 1 }", "true"),
        ("void 0", "undefined"),
        ("!!''", "false"),
        ("1n << 3n", "8"),
        ("-9n >> 1n", "-5"),
        ("7n / 2n", "3"),
        ("1n / 0n", "throw RangeError"),
        ("~5", "-6"),
        ("NaN == NaN", "false"),
        ("null == undefined", "true"),
        ("'1' == 1", "true"),
    ]);
}

#[test]
fn safe_points_collect_with_live_frames() {
    let mut agent = setup();
    agent.heap.set_gc_threshold(64 * 1024);
    let r = eval_in(
        &mut agent,
        "var keep = { v: 1 }; function make(i) { return { a: i, b: 'x' + i } } var t; for (var i = 0; i < 20000; i++) { t = make(i); } keep.v + t.a + t.b",
    );
    assert_eq!(r, "20000x19999");
    assert!(agent.heap.collections() > 0, "no collection happened");
}
