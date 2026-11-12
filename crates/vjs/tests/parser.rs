//! Parser tests: valid fragments for each syntactic construct and
//! fragments that must be a SyntaxError for each family of early errors,
//! as Script and Module, strict and sloppy.

use vjs::ast::{
    ClassElementKind, ExportDecl, ExprKind, FunctionBody, Goal, PatternKind, Program, StmtKind,
};
use vjs::{
    DynamicFunctionKind, EvalContext, ParseOptions, SyntaxError, parse_dynamic_function,
    parse_module, parse_script,
};

fn script(src: &str) -> Result<Program, SyntaxError> {
    parse_script(src, &ParseOptions::default())
}

fn strict(src: &str) -> Result<Program, SyntaxError> {
    parse_script(&format!("'use strict';\n{src}"), &ParseOptions::default())
}

fn module(src: &str) -> Result<Program, SyntaxError> {
    parse_module(src, &ParseOptions::default())
}

#[track_caller]
fn ok(result: Result<Program, SyntaxError>, src: &str, goal: &str) {
    if let Err(e) = result {
        panic!("{goal}: {src:?} should parse, got {e}");
    }
}

#[track_caller]
fn err(result: Result<Program, SyntaxError>, src: &str, goal: &str) {
    if result.is_ok() {
        panic!("{goal}: {src:?} should be a SyntaxError");
    }
}

/// Valid as sloppy script, strict script and module.
#[track_caller]
fn ok_all(src: &str) {
    ok(script(src), src, "script");
    ok(strict(src), src, "strict script");
    ok(module(src), src, "module");
}

/// Valid only as sloppy script.
#[track_caller]
fn ok_sloppy_only(src: &str) {
    ok(script(src), src, "script");
    err(strict(src), src, "strict script");
    err(module(src), src, "module");
}

#[track_caller]
fn ok_script(src: &str) {
    ok(script(src), src, "script");
}

#[track_caller]
fn ok_module(src: &str) {
    ok(module(src), src, "module");
}

#[track_caller]
fn err_all(src: &str) {
    err(script(src), src, "script");
    err(strict(src), src, "strict script");
    err(module(src), src, "module");
}

#[track_caller]
fn err_script(src: &str) {
    err(script(src), src, "script");
}

#[track_caller]
fn err_module(src: &str) {
    err(module(src), src, "module");
}

// ----- Valid constructs -----

#[test]
fn declarations() {
    for src in [
        "var a, b = 1, [c, , ...d] = e, {f, g: [h], ...i} = j;",
        "let a = 1, {b = 2} = {}; const c = 3, [d = 4] = [];",
        "{ let a; { let a; } } { const a = 1; }",
        "var a; var a; var b; function c() {}",
        "function f(a, b = a, ...[c]) { var a; }",
        "{ using a = null, b = undefined; }",
        "function f() { using x = null; }",
        "async function f() { await using x = null; for (await using y of []) {} }",
        "for (using x of []) {}",
        "for (using x = null; ;) break;",
        "class C { static { using x = null; } }",
    ] {
        ok_all(src);
    }
    ok_module("using x = null; await using y = null;");
    ok_script("var let = 1; let\n[a] = [1];");
    ok_script("var using = [], of = []; using[0] = 1; for (using of of []) ;");
    ok_script("{ using\nlet = 1; var using, let; }");
}

#[test]
fn functions_and_arrows() {
    for src in [
        "function f() { return; } function* g() { yield; yield 1; yield* g(); }",
        "async function f() { await 1; } async function* g() { yield await 1; for await (const x of y) ; }",
        "var f = function () {}, g = function* named() {}, h = async function () {};",
        "var a = () => 1, b = x => x * 2, c = (x, y = 1, ...z) => { return z; };",
        "var d = async () => {}, e = async x => x, f = async (x, { y }) => await y;",
        "var g = ({ a, b: [c] = [] }, [d, ...e]) => a;",
        "(a, b,) => a; f(a, b,); function h(a, b,) {}",
        "x = (a) => (b) => a + b;",
        "() => {}\n(1);",
        "var o = { f() {}, *g() {}, async h() {}, async *i() {}, get j() { return 1; }, set j(v) {} };",
        "var o = { get: 1, set: 2, async: 3, static: 4, get() {}, set() {}, async() {} };",
        "function f() { return new.target; } function g() { return () => new.target; }",
    ] {
        ok_all(src);
    }
    ok_script(
        "function f(a, a) {} var yield; function g(yield) {} var await; async function h() {} (await) => 1;",
    );
    ok_script(
        "function* g() { (function yield() {}); } async function f() { (function await() {}); }",
    );
}

#[test]
fn classes() {
    for src in [
        "class A {} class B extends A { constructor() { super(); super.x; } }",
        "class C { a = 1; b; 'c' = 2; [d] = 3; static e = 4; #f = 5; static #g; }",
        "class C { #x; get #y() { return this.#x; } set #y(v) { this.#x = v; } static m(o) { return #x in o; } }",
        "class C { static { this.x = 1; var y; } static async *m() {} get [k]() {} static set s(v) {} }",
        "class C { static; get; set; async; static static() {} async\nm() {} 'constructor'() {} }",
        "var C = class Named extends (class {}) { m() { return super.m?.(); } };",
        "class C { x = () => this; y = function () { return arguments; }; }",
        "class C { #a; m() { class D { n(o) { return o.#a; } } } }",
        "class C { get #a() {} set #a(v) {} static get #b() {} static set #b(v) {} }",
        "class C extends D { constructor() { (() => super())(); } }",
    ] {
        ok_all(src);
    }
    ok_script("class C { static { (() => { class await {} }); } }");
    ok_script("class C { static { var await_; } } class await {}");
}

#[test]
fn expressions() {
    for src in [
        "a?.b?.[c]?.(d); a?.b.c(); (a?.b).c;",
        "a ?? b; (a || b) ?? c; a ?? (b && c);",
        "a ** b ** c; (-a) ** b; ++a ** 2; a-- ** 2;",
        "a &&= b; a ||= c; a ??= d; a.b **= 2; a[0] >>>= 1;",
        "[a, b] = [b, a]; ({ a, b: { c = 1 }, ...d } = e); [x.y, z[0], ...w.v] = q;",
        "({ a = 1 } = {}); [{ b = 2 }] = [{}]; f(({ c = 3 } = {}));",
        "[(a)] = b; [(a.b)] = c; ({ x: (y) } = z); (a) = 1; ((a.b)) = 2;",
        "({ __proto__: a, __proto__: b } = c);",
        "x = { __proto__: a, ['__proto__']: b, __proto__() {} };",
        "`a${b}c${`d${e}`}`; tag`x${y}`; String.raw`\\u{`;",
        "/a[/]b/gimsuy.test(x); x = a / b / c; x = /=/;",
        "new A; new A(); new A.b.c(); new new A()(); new (a())(); new a.b`t`;",
        "a in b; a instanceof b; typeof a; void 0; delete a.b; !a; ~a; -a; +a;",
        "a ? b : c ? d : e; a, b, c; this; null; true; false;",
        "0x1F; 0o17; 0b1010; 1_000_000; .5; 5.; 1e3; 1E-3; 10n; 0x1fn;",
        "'\\x41\\u0042\\u{43}\\0\\\n'; \"\u{2028}\";",
        "import('m'); import('m', { with: { type: 'json' } }); import('m',);",
        "x = { if: 1, class: 2, new: 3 }; x.if; x.class.new;",
    ] {
        ok_all(src);
    }
    ok_script("f() = 1; f()++; --f(); f() += 1; for (f() in x) ; for (f() of y) ;");
    ok_script("async(a, b); async\n(x); var async; async = 1;");
}

#[test]
fn statements() {
    for src in [
        "if (a) b; else c; while (a) break; do continue; while (a)",
        "for (;;) break; for (var i = 0; i < 1; i++) ; for (let i of a) ; for (const k in o) ;",
        "for (a in b) ; for ([a, b] of c) ; for ({ a } of b) ; for (x.y in z) ;",
        "switch (a) { case 1: let b; break; default: { let b; } }",
        "try {} catch {} try {} catch (e) {} finally {} try {} finally {}",
        "try {} catch ([a, { b }]) {} try {} catch (e) { var e; for (var e of []) ; }",
        "a: b: for (;;) { continue a; } c: { break c; } d: if (x) break d;",
        "throw a; debugger; ;",
        "for (lett in x) ;",
    ] {
        ok_all(src);
    }
    ok_script("for (let in x) ; for (let.x in y) ; for (var a = 1 in b) ;");
    ok_script("with (a) b; l: function f() {} if (a) function g() {} else function h() {}");
    ok_script(
        "{ function f() {} function f() {} } switch (1) { case 1: function g() {} default: function g() {} }",
    );
}

#[test]
fn directives_and_strictness() {
    let program = script("'use strict'; var x;").unwrap();
    assert!(program.strict);
    assert!(matches!(program.body[0].kind, StmtKind::Directive { .. }));
    assert!(!script("'use\\x20strict'; with (a) ;").unwrap().strict);
    assert!(!script("('use strict'); with (a) ;").unwrap().strict);
    assert!(!script("'a' + 'use strict'; with (a) ;").unwrap().strict);
    assert!(module("x;").unwrap().strict);
    err_script("'use strict'; with (a) b;");
    err_script("function f() { 'use strict'; 010; }");
    err_script("function f() { '\\01'; 'use strict'; }");
    err_script("'\\01'; 'use strict';");
    err_script("function f() { 'use strict'\n010 }");
    ok_script("function f() { 'use strict' + 1; with (a) ; }");
    ok_script("function f() { 'use strict' } with (a) ;");
    let program = script("function f() { 'use strict'; }").unwrap();
    let StmtKind::Function(f) = &program.body[0].kind else {
        panic!()
    };
    assert!(f.strict && f.use_strict_directive);
}

#[test]
fn modules() {
    for src in [
        "import a from 'm'; import * as b from 'm'; import { c, d as e, 'f g' as h, default as i } from 'm';",
        "import j, { k } from 'm'; import l, * as n from 'm'; import 'm'; import {} from 'm';",
        "import json from './x.json' with { type: 'json' }; import 'n' with { 'type': 'json', };",
        "export var a; export let b; export const c = 1; export function d() {} export class E {}",
        "var a, b, c; export async function f() {} export function* g() {} export { a as aa, b as 'b b', c as default };",
        "export * from 'm'; export * as ns from 'm'; export * as 'string name' from 'm'; export { x, y as z } from 'm';",
        "export { default } from 'm'; export { 'a' as 'b' } from 'm' with { type: 'json' };",
        "var x, y; export { x, y as w };",
        "await 1; for await (const x of y) ; const m = import.meta;",
        "export default class {}",
    ] {
        ok_module(src);
    }
    ok_module("export default function () {}");
    ok_module("export default async function named() {}");
    ok_module("export default (1 + 2);");
    ok_module("export { f }; function f() {}");
    ok_module("export { v }; { var v; }");
    let program = module("export default function f() {} f();").unwrap();
    assert_eq!(program.goal, Goal::Module);
    let StmtKind::Export(export) = &program.body[0].kind else {
        panic!()
    };
    assert!(matches!(**export, ExportDecl::DefaultFunction(_)));
}

#[test]
fn annex_b_syntax() {
    ok_sloppy_only("var a = 010, b = 08, c = '\\07', d = '\\8';");
    ok_sloppy_only("if (a) function f() {}");
    ok_sloppy_only("l: function f() {}");
    ok_sloppy_only("for (var a = 1 in b) ;");
    ok_sloppy_only("{ function f() {} function f() {} }");
    // HTML-like comments are comments in scripts only.
    ok_script("x = 1 <!-- comment\n--> another comment\n");
    ok_script("--> comment at the start\n/*\n*/ --> after a multi-line comment\n");
    err_module("x = 1 <!-- not a comment");
    err_module("--> not a comment");
    ok_script("var x = 1\n-->[0];");
}

#[test]
fn hashbang() {
    ok_script("#!/usr/bin/env vjs\nvar x;");
    ok_module("#!/usr/bin/env vjs\nvar x;");
    err_all(" #!/usr/bin/env vjs\n");
    err_all("var x;\n#!/usr/bin/env vjs\n");
}

#[test]
fn identifiers_and_unicode_escapes() {
    ok_all("var \\u0061b, c\\u{64}, λ, 𝐀, ℘, _$, $\\u200C;");
    ok_all("var x = { \\u0069f: 1 }; x.\\u0069f;");
    err_all("var \\u0069f;");
    err_all("\\u0076ar x;");
    err_all("var a\\u002Db;");
    err_all("var \\u{110000};");
    err_all("var 1a;");
    ok_script("l\\u0065t\nx = 1;");
    ok_script("var l\\u0065t;");
    err_strict("var l\\u0065t;");
    err_all("x = { g\\u0065t y() {} };");
    err_all("\\u0061sync function f() {}");
    err_all("class C { st\\u0061tic m() {} }");
    err_all("for (x \\u006ff y) ;");
    err_all("n\\u0065w.target;");
}

#[test]
fn literals_errors() {
    for src in [
        "1__0",
        "1_",
        "0_1",
        "1._5",
        "1e_1",
        "0x",
        "0b2",
        "01n",
        "1.5n",
        "1e3n",
        "3in []",
        "0x_1",
        "'unterminated",
        "'line\nbreak'",
        "'\\x4'",
        "'\\u{110000}'",
        "`unterminated",
        "/unterminated",
        "/a\n/",
        "/a/gg",
        "/a/uv",
        "/a/x",
        "/a/\\u0067",
        "`\\unicode`",
        "`\\01`",
        "`\\8`",
        "`\\x`",
        "/* unterminated",
        "@",
        "#",
    ] {
        err_all(src);
    }
    err_strict("010;");
    err_strict("08;");
    err_strict("'\\01';");
    err_strict("'\\8';");
    err_strict("({ 010: 1 });");
    ok_all("'\\0'; `\\0`; tag`\\unicode and \\01 and \\x`;");
}

#[test]
fn regexp_validator_hook() {
    fn reject_x(pattern: &str, _flags: &str) -> Result<(), String> {
        if pattern.contains('x') {
            Err("no x".into())
        } else {
            Ok(())
        }
    }
    let options = ParseOptions {
        regexp_validator: Some(reject_x),
        ..ParseOptions::default()
    };
    assert!(parse_script("/abc/g;", &options).is_ok());
    let error = parse_script("var r = /axc/;", &options).unwrap_err();
    assert!(error.message.contains("no x"));
}

// ----- Early errors -----

#[test]
fn duplicate_lexical_declarations() {
    for src in [
        "let a; let a;",
        "let a; var a;",
        "var a; let a;",
        "const a = 1; let a;",
        "let a; function a() {}",
        "class A {} var A;",
        "{ let a; var a; }",
        "{ let a; { var a; } }",
        "{ let a; function a() {} }",
        "{ var a; let a; }",
        "switch (x) { case 1: let a; case 2: let a; }",
        "function f(a) { let a; }",
        "(a) => { const a = 1; };",
        "try {} catch (e) { let e; }",
        "try {} catch ([e]) { var e; }",
        "try {} catch (e) { function e() {} }",
        "try {} catch ([e, e]) {}",
        "for (let a of b) { var a; }",
        "for (let [a, a] of b) ;",
        "let [a, a] = b;",
        "let let = 1;",
        "const let = 1;",
        "for (let let of x) ;",
        "class C { static { let a; var a; } }",
        "{ async function f() {} function f() {} }",
        "{ function* f() {} function f() {} }",
    ] {
        err_all(src);
    }
    err_strict("{ function f() {} function f() {} }");
    err_module("function f() {} function f() {}");
    err_module("var f; function f() {}");
    err_module("import a from 'm'; let a;");
    err_module("import { a, b as a } from 'm';");
    ok_script("function f() {} function f() {} var f;");
}

#[test]
fn invalid_assignment_targets() {
    for src in [
        "1 = 2;",
        "a + b = c;",
        "a++ = 1;",
        "++1;",
        "1++;",
        "(a, b) = c;",
        "this = 1;",
        "new.target = 1;",
        "a?.b = 1;",
        "a?.b++;",
        "({a}) = 1;",
        "([a]) = 1;",
        "[...a,] = b;",
        "[...a = 1] = b;",
        "({a: 1} = b);",
        "({...{a}} = b);",
        "({ m() {} } = b);",
        "[(a = 1)] = b;",
        "[({a})] = b;",
        "({a = 1});",
        "f({a = 1});",
        "f() &&= 1;",
        "f() ??= 1;",
        "f()`` = 1;",
        "import('m') = 1;",
        "super() = 1;",
        "for ((a, b) in c) ;",
        "for (a + b of c) ;",
        "for (let.x of y) ;",
        "for (async of x) ;",
        "[a] += 1;",
        "({a} ||= 1);",
        "`a` = 1;",
        "(async () => {}) = 1;",
        "() => {} = 1;",
        "x => x = 1 = 2;",
    ] {
        err_all(src);
    }
    for src in [
        "f() = 1;",
        "f()++;",
        "++f();",
        "f() += 1;",
        "for (f() in x) ;",
        "for (f() of y) ;",
        "(f()) = 1;",
    ] {
        ok_script(src);
        err_strict(src);
        err_module(src);
    }
}

#[test]
fn strict_mode_restrictions() {
    for src in [
        "with (a) b;",
        "delete x;",
        "delete ((x));",
        "var eval;",
        "var arguments;",
        "eval = 1;",
        "arguments++;",
        "[eval] = a;",
        "({ arguments } = a);",
        "function eval() {}",
        "function f(eval) {}",
        "function f(a, a) {}",
        "(function arguments() {});",
        "try {} catch (eval) {}",
        "var implements, interface, let, package, private, protected, public, static, yield;",
        "var yield;",
        "function static() {}",
        "if (a) function f() {}",
        "l: function f() {}",
        "for (var a = 1 in b) ;",
        "eval => 1;",
        "(arguments) => 1;",
    ] {
        ok(script(src), src, "script");
        err(strict(src), src, "strict script");
        err(module(src), src, "module");
    }
    // A Use Strict Directive applies to the function's name and parameters.
    for src in [
        "function eval() { 'use strict'; }",
        "function f(eval) { 'use strict'; }",
        "function f(a, a) { 'use strict'; }",
        "function static() { 'use strict'; }",
        "(function arguments() { 'use strict'; });",
        "(eval) => { 'use strict'; };",
        "function f(a = 1) { 'use strict'; }",
        "function f({ a }) { 'use strict'; }",
        "function f(...a) { 'use strict'; }",
        "(a = 1) => { 'use strict'; };",
        "({ m(a = 1) { 'use strict'; } });",
    ] {
        err_all(src);
    }
    // Class bodies are strict.
    err_script("class C { m() { with (a) ; } }");
    err_script("class C extends (010) {}");
    err_script("class let {}");
}

#[test]
fn duplicate_parameters() {
    for src in [
        "function f(a, a = 1) {}",
        "function f(a, [a]) {}",
        "function f(a, ...a) {}",
        "(a, a) => 1;",
        "async (a, a) => 1;",
        "({ m(a, a) {} });",
        "class C { m(a, a) {} }",
        "function* g(a, { a }) {}",
    ] {
        err_all(src);
    }
}

#[test]
fn yield_and_await_contexts() {
    for src in [
        "function* g() { var yield; }",
        "function* g() { yield: ; }",
        "function* g(x = yield) {}",
        "function* g() { (x = yield) => {}; }",
        "function* g() { function* yield() {} }",
        "(function* yield() {});",
        "function* g() { yi\\u0065ld; }",
        "async function f() { var await; }",
        "async function f(x = await 1) {}",
        "async function f() { (x = await 1) => {}; }",
        "async function f() { await: ; }",
        "async function f() { function await() {} }",
        "(async function await() {});",
        "async (await) => 1;",
        "async (x = await) => 1;",
        "async await => 1;",
        "async ({ await }) => 1;",
        "async () => { var await; };",
        "async function f() { aw\\u0061it 1; }",
        "function f() { await 1; }",
        "class C { static { await; } }",
        "class C { static { var await; } }",
        "class C { static { class await {} } }",
        "class C { static { (class await {}); } }",
        "async function f() { class C { static { await 0; } } }",
        "function* g() { class C { static { yield; } } }",
        "async function f() { class C { x = await 1; } }",
        "function f() { for await (x of y) ; }",
        "async function f() { for await (x in y) ; }",
        "async function f() { for await (;;) ; }",
        "async function* g() { -await 1 ** 2; }",
        "async function f() { await 1 ** 2; }",
    ] {
        err_all(src);
    }
    // `await` is an identifier in scripts outside async functions.
    ok_script("var await; await = 1; function await() {} await: ;");
    err_module("var await;");
    err_module("await: ;");
    err_module("function f() { var await; }");
    err_module("function f(await) {}");
    // Non-async arrows and functions reset [Await] and [Yield].
    ok_script("async function f() { (function () { var await; }); }");
    ok_script("function* g() { (function () { var yield; }); () => yield; }");
}

#[test]
fn super_and_new_target() {
    for src in [
        "super.x;",
        "super();",
        "function f() { super.x; }",
        "function f() { super(); }",
        "({ m() { super(); } });",
        "class C { constructor() { super(); } }",
        "class C extends D { m() { super(); } }",
        "class C extends D { x = super(); }",
        "class C extends D { static { super(); } }",
        "class C extends D { constructor() { function f() { super(); } } }",
        "new.target;",
        "() => new.target;",
        "new.targ;",
        "super?.x;",
        "class C extends D { m() { new super(); } }",
        "class C { m() { super.#x; } }",
    ] {
        err_all(src);
    }
    ok_all(
        "class C extends D { constructor() { super(); () => super(); } x = super.y; static { super.z; } }",
    );
    ok_all("({ m() { return () => super.x; }, get g() { return super.g; } });");
    ok_all("class C { x = new.target; static { new.target; } }");
}

#[test]
fn private_names() {
    for src in [
        "this.#x;",
        "class C { m() { this.#y; } }",
        "class C { #x; #x; }",
        "class C { #x; get #x() {} }",
        "class C { get #x() {} static set #x(v) {} }",
        "class C { get #x() {} get #x() {} }",
        "class C { #constructor; }",
        "class C { #constructor() {} }",
        "class C { #x; m() { delete this.#x; } }",
        "class C { #x; m() { delete (this.#x); } }",
        "class C { #x; m() { delete this?.#x; } }",
        "class C { #x; m() { return 1 + #x in this; } }",
        "class C { #x; m() { return #x; } }",
        "class C { #x; m() { return !#x in this; } }",
        "#x in this;",
        "({ #x: 1 });",
    ] {
        err_all(src);
    }
    ok_all("class C { #x; m() { return #x in this && this.#x == this?.#x; } }");
    ok_all("class C { m() { return this.#z; } #z = 1; }");
    let options = ParseOptions {
        eval: Some(EvalContext {
            private_names: vec!["x".into()],
            ..EvalContext::default()
        }),
        ..ParseOptions::default()
    };
    assert!(parse_script("this.#x;", &options).is_ok());
    assert!(parse_script("this.#y;", &options).is_err());
}

#[test]
fn class_element_errors() {
    for src in [
        "class C { constructor() {} constructor() {} }",
        "class C { get constructor() {} }",
        "class C { *constructor() {} }",
        "class C { async constructor() {} }",
        "class C { constructor = 1; }",
        "class C { 'constructor' = 1; }",
        "class C { static constructor = 1; }",
        "class C { static prototype() {} }",
        "class C { static prototype = 1; }",
        "class C { static 'prototype' = 1; }",
        "class C { x = arguments; }",
        "class C { x = () => arguments; }",
        "class C { static { arguments; } }",
        "class C { static { () => arguments; } }",
        "class C { static { return; } }",
        "class C { static { l: ; break l2; } }",
        "class C { a b; }",
        "class C { get x(a) {} }",
        "class C { set x() {} }",
        "class C { set x(...a) {} }",
        "class",
        "class {}",
        "class C extends a + b {}",
    ] {
        err_all(src);
    }
    ok_all(
        "class C { x = function () { arguments; }; static { (function () { arguments; }); arguments: ; } }",
    );
    ok_all("class C { constructor() {} static constructor() {} ['constructor']() {} }");
}

#[test]
fn object_literal_errors() {
    for src in [
        "({ __proto__: 1, __proto__: 2 });",
        "({ __proto__: 1, '__proto__': 2 });",
        "({ a = 1 });",
        "({ get x(a) {} });",
        "({ set x() {} });",
        "({ async get x() {} });",
        "({ *get x() {} });",
        "({ async x: 1 });",
        "({ 'a' });",
        "({ 1 });",
        "({ [a] });",
        "({ this });",
        "({ if });",
        "({ a, b c });",
    ] {
        err_all(src);
    }
    ok_all("({ __proto__: 1, ['__proto__']: 2, __proto__() {}, get __proto__() {} });");
    ok_all("({ __proto__, __proto__: 1 });");
}

#[test]
fn break_continue_and_labels() {
    for src in [
        "break;",
        "continue;",
        "{ break; }",
        "l: { continue l; }",
        "l: if (a) continue l;",
        "switch (a) { case 1: continue; }",
        "while (a) { (function () { break; }); }",
        "l: while (a) { (function () { break l; }); }",
        "l: l: ;",
        "l: { l: ; }",
        "while (a) break l;",
        "l: while (a) () => { continue l; };",
        "class C { static { while (a) { } break; } }",
    ] {
        err_all(src);
    }
    ok_all(
        "a: b: while (x) { continue a; } c: { d: { break c; } } e: switch (x) { case 1: break e; }",
    );
    ok_all("l: ; l: ; m: { n: while (x) { break m; } }");
}

#[test]
fn single_statement_contexts() {
    for src in [
        "if (a) let [b] = c;",
        "if (a) const b = 1;",
        "while (a) let [b] = c;",
        "if (a) class C {}",
        "if (a) async function f() {}",
        "if (a) function* g() {}",
        "while (a) function f() {}",
        "do function f() {} while (a);",
        "for (;;) function f() {}",
        "while (a) l: function f() {}",
        "if (a) l: function f() {}",
        "l: function* g() {}",
        "l: async function f() {}",
        "l: class C {}",
        "l: let [a] = b;",
        "label: const a = 1;",
    ] {
        err_all(src);
    }
    ok_script("if (a) let\nb = 1;");
}

#[test]
fn using_declarations() {
    for src in [
        "using x = null;",
        "{ using [] = null; }",
        "{ using {a} = b; }",
        "{ using x; }",
        "for (using x in y) ;",
        "for (using x;;) ;",
        "switch (1) { case 1: using x = null; }",
        "switch (1) { default: using x = null; }",
        "{ using let = null; }",
        "function f() { await using x = null; }",
        "{ await using x = null; }",
    ] {
        err(script(src), src, "script");
    }
    err_module("switch (1) { case 1: using x = null; }");
    err_module("{ using x; }");
    ok_module("using x = null;");
    err_script("var f = function () { using x = y }; using z = 1;");
}

#[test]
fn coalesce_and_exponent_mixing() {
    for src in [
        "a ?? b || c;",
        "a || b ?? c;",
        "a && b ?? c;",
        "a ?? b && c;",
        "-a ** b;",
        "typeof a ** b;",
        "delete a.b ** c;",
        "!a ** b;",
    ] {
        err_all(src);
    }
}

#[test]
fn optional_chain_errors() {
    for src in [
        "new a?.b();",
        "a?.b`t`;",
        "a?.`t`;",
        "a?.b.c`t`;",
        "new a?.();",
    ] {
        err_all(src);
    }
}

#[test]
fn template_errors() {
    for src in [
        "`\\u{110000}`;",
        "`\\u{`;",
        "`\\x0`;",
        "`${a`;",
        "`${a}${`;",
    ] {
        err_all(src);
    }
    ok_all("tag`\\u{`; tag`\\u{110000}${x}\\x`;");
}

#[test]
fn module_errors() {
    for src in [
        "export { a };",
        "export { a as b, b as a }; var a;",
        "var a; export { a, a };",
        "export var a; export var a;",
        "export default 1; export default 2;",
        "var x; export { x as default }; export default 1;",
        "export function f() {} export { f };",
        "export { 'a' };",
        "export { 'a' as b };",
        "var x; export { x as '\\uD800' };",
        "import { 'a' } from 'm';",
        "import { if } from 'm';",
        "import a, from 'm';",
        "import * from 'm';",
        "import x from 'm' with { type: 'json', type: 'json' };",
        "import x from 'm' with { type: json };",
        "export { if };",
        "export { await };",
        "export * as a from 'm'; export * as a from 'n';",
        "{ import a from 'm'; }",
        "function f() { export var a; }",
        "export let;",
        "export using x = null;",
        "await: ;",
        "with (a) ;",
        "new import('m');",
        "import(...a);",
        "import();",
        "import('a', 'b', 'c');",
        "import.foo;",
        "let a; export { b as a };",
        "export { x }; { let x; }",
    ] {
        err_module(src);
    }
    err_script("import a from 'm';");
    err_script("export var a;");
    err_script("import.meta;");
    err_script("x = import.meta;");
    ok_module("var a; export { a as b, a as c };");
    ok_module("export { if } from 'm'; export { default as d, 'x' as y } from 'm';");
}

#[test]
fn return_and_misc_errors() {
    for src in [
        "return;",
        "() => { return; }; return 1;",
        "throw\n1;",
        "try {}",
        "switch (a) { default: default: }",
        "for (let a = 1 of b) ;",
        "for (const a in b) ; for (var [a] = 1 in b) ;",
        "for (var a, b of c) ;",
        "for (let a, b in c) ;",
        "const a;",
        "let [a];",
        "var {a};",
        "a\n=> 1;",
        "async a\n=> 1;",
        "async (a)\n=> 1;",
        "(a, ...b,) => 1;",
        "(...a = 1) => 1;",
        "(a, ...b);",
        "();",
        "(a,);",
        "((a)) => 1;",
        "({ a: (b) }) => 1;",
        "(a.b) => 1;",
        "([a.b]) => 1;",
        "!() => 1;",
        "a + () => 1;",
        "() => {} ? 2 : 3;",
        "function () {} ();",
        "var a = { b: 1 } c;",
        "a b;",
        "if (a) else b;",
        "do a while (b)",
        "new.target",
        "class C extends D, E {}",
        "...a;",
        "[...a, b] = c;",
        "({ ...a, b } = c);",
    ] {
        err_all(src);
    }
    ok_all("do a; while (b) c;");
    ok_all("a\n++b;");
    ok_all("x = a\n/b/g;");
}

// ----- Positions and source text -----

#[test]
fn positions_and_function_source_text() {
    let src = "var x = 1;\nfunction /* a */ f /* b */ (a, b) /* c */ { return a; }\n";
    let program = script(src).unwrap();
    let StmtKind::Function(f) = &program.body[1].kind else {
        panic!()
    };
    assert_eq!(
        f.span.text(src),
        "function /* a */ f /* b */ (a, b) /* c */ { return a; }"
    );
    assert_eq!(f.name.as_ref().unwrap().span.text(src), "f");
    assert_eq!(program.body[0].span.text(src), "var x = 1;");

    let src =
        "var o = { /* before */get /* a */ f /* b */ ( /* c */ ) /* d */ { /* e */ }/* after */ };";
    let program = script(src).unwrap();
    let StmtKind::Var(decl) = &program.body[0].kind else {
        panic!()
    };
    let ExprKind::Object(props) = &decl.declarations[0].init.as_ref().unwrap().kind else {
        panic!()
    };
    let vjs::ast::PropertyKind::Method { function, .. } = &props[0].kind else {
        panic!()
    };
    assert_eq!(
        function.span.text(src),
        "get /* a */ f /* b */ ( /* c */ ) /* d */ { /* e */ }"
    );

    let src = "let C = class { static /* before */f /* a */ ( /* b */ ) /* c */ { /* d */ }/* after */ };";
    let program = script(src).unwrap();
    let StmtKind::Var(decl) = &program.body[0].kind else {
        panic!()
    };
    let ExprKind::Class(class) = &decl.declarations[0].init.as_ref().unwrap().kind else {
        panic!()
    };
    assert_eq!(class.span.text(src), &src[8..src.len() - 1]);
    let ClassElementKind::Method { function, .. } = &class.elements[0].kind else {
        panic!()
    };
    assert_eq!(
        function.span.text(src),
        "f /* a */ ( /* b */ ) /* c */ { /* d */ }"
    );

    let src = "x = async (a) => a;";
    let program = script(src).unwrap();
    let StmtKind::Expr(e) = &program.body[0].kind else {
        panic!()
    };
    let ExprKind::Assign { value, .. } = &e.kind else {
        panic!()
    };
    let ExprKind::Arrow(arrow) = &value.kind else {
        panic!()
    };
    assert_eq!(arrow.span.text(src), "async (a) => a");
    assert!(arrow.is_async && matches!(arrow.body, FunctionBody::Expr(_)));
}

#[test]
fn error_positions() {
    let error = script("var a;\nvar b = ;").unwrap_err();
    assert_eq!((error.line, error.column), (2, 9));
    let error = script("let a;\r\n  let a;").unwrap_err();
    assert_eq!((error.line, error.column), (2, 7));
    assert!(error.to_string().starts_with("SyntaxError:"));
}

#[test]
fn ast_shapes() {
    let program = script("[a, b] = c; d ??= e; f?.g;").unwrap();
    let StmtKind::Expr(e) = &program.body[0].kind else {
        panic!()
    };
    let ExprKind::Assign { target, .. } = &e.kind else {
        panic!()
    };
    assert!(matches!(target.kind, PatternKind::Array { .. }));
    let StmtKind::Expr(e) = &program.body[2].kind else {
        panic!()
    };
    assert!(matches!(e.kind, ExprKind::OptionalChain(_)));
    let program = script("function f() { var a; let b; eval('x'); return arguments; }").unwrap();
    let StmtKind::Function(f) = &program.body[0].kind else {
        panic!()
    };
    assert_eq!(f.scope.var_names, ["a"]);
    assert_eq!(f.scope.lexical_names, ["b"]);
    assert!(f.scope.has_direct_eval && f.scope.uses_arguments);
    assert!(program.scope.has_direct_eval);
    assert_eq!(program.scope.var_names, ["f"]);
}

// ----- Eval and the Function constructor -----

#[test]
fn eval_contexts() {
    let plain = ParseOptions::default();
    assert!(parse_script("new.target", &plain).is_err());
    let in_function = ParseOptions {
        eval: Some(EvalContext {
            new_target: true,
            ..EvalContext::default()
        }),
        ..ParseOptions::default()
    };
    assert!(parse_script("new.target", &in_function).is_ok());
    assert!(parse_script("super.x", &in_function).is_err());
    let in_method = ParseOptions {
        eval: Some(EvalContext {
            new_target: true,
            super_property: true,
            ..EvalContext::default()
        }),
        ..ParseOptions::default()
    };
    assert!(parse_script("super.x", &in_method).is_ok());
    assert!(parse_script("super()", &in_method).is_err());
    let in_field = ParseOptions {
        eval: Some(EvalContext {
            class_field_initializer: true,
            ..EvalContext::default()
        }),
        ..ParseOptions::default()
    };
    assert!(parse_script("arguments", &in_field).is_err());
    let strict = ParseOptions {
        strict: true,
        ..ParseOptions::default()
    };
    assert!(parse_script("with (a) ;", &strict).is_err());
    assert!(parse_script("return 1;", &plain).is_err());
    assert!(parse_script("#!\n1", &plain).is_ok());
}

#[test]
fn function_constructor() {
    let options = ParseOptions::default();
    let (f, source) = parse_dynamic_function(
        DynamicFunctionKind::Normal,
        "a, b",
        "return a + b",
        &options,
    )
    .unwrap();
    assert_eq!(source, "function anonymous(a, b\n) {\nreturn a + b\n}");
    assert_eq!(f.span.text(&source), source);
    assert!(f.name.is_none());
    assert!(
        parse_dynamic_function(DynamicFunctionKind::Generator, "", "yield 1", &options).is_ok()
    );
    assert!(parse_dynamic_function(DynamicFunctionKind::Async, "", "await 1", &options).is_ok());
    assert!(
        parse_dynamic_function(
            DynamicFunctionKind::AsyncGenerator,
            "",
            "yield await 1",
            &options
        )
        .is_ok()
    );
    assert!(parse_dynamic_function(DynamicFunctionKind::Normal, "a /*", "*/){", &options).is_err());
    assert!(parse_dynamic_function(DynamicFunctionKind::Normal, "a){", "", &options).is_err());
    assert!(
        parse_dynamic_function(
            DynamicFunctionKind::Normal,
            "",
            "}); (function () {",
            &options
        )
        .is_err()
    );
    assert!(
        parse_dynamic_function(
            DynamicFunctionKind::Normal,
            "a = 1",
            "'use strict'",
            &options
        )
        .is_err()
    );
    assert!(
        parse_dynamic_function(DynamicFunctionKind::Generator, "a = yield", "", &options).is_err()
    );
    assert!(parse_dynamic_function(DynamicFunctionKind::Normal, "#!", "", &options).is_err());
    assert!(parse_dynamic_function(DynamicFunctionKind::Normal, "", "super.x", &options).is_err());
}

#[track_caller]
fn err_strict(src: &str) {
    err(strict(src), src, "strict script");
}
