//! The RegExp engine: lookbehind, duplicate named groups, `v` mode set
//! operations and strings, Unicode properties, case folding under `i` with
//! and without `u`, modifiers, patterns with catastrophic or very deep
//! backtracking (which must finish on the explicit stack), early errors of
//! invalid literals, and the `RegExp` built-ins through scripts.

use vjs::agent::Agent;
use vjs::ops;
use vjs::regexp::{Flags, Regex, parse_options, validate_literal};
use vjs::string::utf16;
use vjs::value::{PropertyKey, Value};
use vjs::{Throw, evaluate_script, parse_module, parse_script};

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
  if (actual === null) fail(what + ": got null");
  same(actual.length, expected.length, what + " length");
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

fn regex(pattern: &str, flags: &str) -> Regex {
    Regex::new(
        &utf16(pattern),
        Flags::parse(&utf16(flags)).expect("valid flags"),
    )
    .unwrap_or_else(|e| panic!("/{pattern}/{flags}: {e}"))
}

// ----- lookbehind -----

#[test]
fn lookbehind() {
    ok(r#"
    same(/(?<=\$)\d+(\.\d*)?/.exec("cost $10.53")[0], "10.53", "positive");
    same(/(?<!\$)\b\d+/.exec("$10 or 20")[0], "20", "negative");
    same(/(?<!a)b/.exec("abcb").index, 3, "negative skips the first b");
    list(/(?<=(\d+)(\d+))$/.exec("1053"), ["", "1", "053"], "greedy right to left");
    list(/(?<=(\d+?)(\d+?))$/.exec("1053"), ["", "5", "3"], "lazy right to left");
    same("abcdef".replace(/(?<=c)/, "-"), "abc-def", "empty match after c");
    same(/(?<=a+)b/.test("aaab"), true, "variable length");
    same(/(?<=^a)b/.test("ab"), true, "anchor inside");
    same(/(?<=^a)b/.test("cab"), false, "anchor inside fails");
    list(/(?<=\1(.))x/.exec("aax"), ["x", "a"], "backreference read right to left");
    same(/(?<=\1(.))x/.exec("bax"), null, "backreference mismatch");
    list(/(?<=(.)\1)x/.exec("bax"), ["x", "a"], "backreference before its group is empty");
    same(/(?<=\u{1F600})!/u.exec("\u{1F600}!").index, 2, "astral lookbehind");
    same(/(?<=\uDE00)!/u.test("\u{1F600}!"), false, "no half pair with u");
    same(/(?<=\uDE00)!/.test("\u{1F600}!"), true, "half pair without u");
    same(/(?<![a-z])\d/.exec("a1 2").index, 3, "negative class");
    "ok";
    "#);
}

// ----- named groups -----

#[test]
fn duplicate_named_groups() {
    ok(r#"
    var re = /(?<y>\d{4})-\d{2}|\d{2}-(?<y>\d{4})/;
    same(re.exec("2024-05").groups.y, "2024", "first alternative");
    same(re.exec("05-2024").groups.y, "2024", "second alternative");
    list(Object.keys(re.exec("05-2024").groups), ["y"], "one property");
    var d = /(?<y>\d{4})-\d{2}|\d{2}-(?<y>\d{4})/d.exec("05-2024");
    list(d.indices.groups.y, [3, 7], "indices of the group that matched");
    var b = /^(?:(?<a>x)|(?<a>y))\k<a>$/;
    same(b.test("xx"), true, "backreference to the first");
    same(b.test("yy"), true, "backreference to the second");
    same(b.test("xy"), false, "backreference mismatch");
    var r = /(?:(?<a>x)|(?<a>y))+/.exec("xy");
    same(r.groups.a, "y", "last iteration wins");
    same("2024-05".replace(re, "$<y>!"), "2024!", "replace with $<name>");
    throws(SyntaxError, function () { new RegExp("(?<a>x)(?<a>y)"); }, "same alternative");
    throws(SyntaxError, function () { new RegExp("(?:(?<a>x)|(?<a>y))(?<a>z)"); }, "after the disjunction");
    var n = /(?<first>\w+)\s(?<last>\w+)/.exec("Ada Lovelace");
    same(n.groups.first + "," + n.groups.last, "Ada,Lovelace", "named groups");
    same(Object.getPrototypeOf(n.groups), null, "groups has a null prototype");
    same(/(a)/.exec("a").groups, undefined, "no names, no groups");
    same(/\k<a>(?<a>b)/.test("b"), true, "forward reference matches empty");
    same(/\k/.test("k"), true, "Annex B \\k without names");
    "ok";
    "#);
}

// ----- `v` mode -----

#[test]
fn unicode_sets_mode() {
    ok(r#"
    same(/[\p{L}--[a-z]]/v.test("a"), false, "difference removes a");
    same(/[\p{L}--[a-z]]/v.test("A"), true, "difference keeps A");
    same(/^[[a-z]&&[aeiou]]+$/v.test("aeiou"), true, "intersection");
    same(/[[a-z]&&[aeiou]]/v.test("b"), false, "intersection excludes b");
    same(/[\w--\d]/v.test("5"), false, "class escape difference");
    same(/[\w--_]/v.test("_"), false, "difference of a character");
    same(/[\q{abc|d}]/v.exec("abcd")[0], "abc", "longest string first");
    same(/[\q{abc|d}]/v.exec("xd")[0], "d", "single string");
    same(/^[\q{}]$/v.test(""), true, "empty string");
    same(/^[\p{RGI_Emoji}]$/v.test("\u{1F44D}\u{1F3FD}"), true, "emoji modifier sequence");
    same(/^\p{RGI_Emoji_Flag_Sequence}$/v.test("\u{1F1FA}\u{1F1F8}"), true, "flag sequence");
    same(/^\p{Emoji_Keycap_Sequence}$/v.test("1\uFE0F\u20E3"), true, "keycap");
    same(/[\q{KK}]/vi.test("kk"), true, "strings fold with i");
    same(/[[\q{ab|c}]--\q{ab}]/v.test("ab"), false, "string subtraction");
    same(/[[\q{ab|c}]&&\q{ab}]/v.exec("abc")[0], "ab", "string intersection");
    same(/[^\d]/v.test("5"), false, "negated");
    same(/[\-]/v.test("-"), true, "escaped reserved punctuator");
    same(/\P{Ll}/iv.test("a"), false, "v: complement of folded set");
    same(/\P{Ll}/iv.test("A"), false, "v: complement of folded set (upper)");
    same(/\P{Ll}/iu.test("a"), true, "u: complement then case closure");
    same(/[^\P{Ll}]/iv.test("A"), true, "v: double complement");
    throws(SyntaxError, function () { new RegExp("[^\\q{ab}]", "v"); }, "negated strings");
    throws(SyntaxError, function () { new RegExp("\\P{RGI_Emoji}", "v"); }, "\\P of strings");
    throws(SyntaxError, function () { new RegExp("[a-z&&b]", "v"); }, "range in intersection");
    throws(SyntaxError, function () { new RegExp("[a&&b--c]", "v"); }, "mixed operators");
    throws(SyntaxError, function () { new RegExp("[a&&&b]", "v"); }, "triple ampersand");
    throws(SyntaxError, function () { new RegExp("[(]", "v"); }, "syntax character");
    throws(SyntaxError, function () { new RegExp("[a!!b]", "v"); }, "reserved double punctuator");
    throws(SyntaxError, function () { new RegExp("\\p{RGI_Emoji}", "u"); }, "strings need v");
    throws(SyntaxError, function () { new RegExp(".", "uv"); }, "u with v");
    same(/[\q{abc}]/v.unicodeSets, true, "unicodeSets getter");
    same(/x/v.flags, "v", "flags");
    "ok";
    "#);
}

// ----- Unicode properties -----

#[test]
fn unicode_properties() {
    ok(r#"
    same(/^\p{Script=Greek}+$/u.test("\u03B1\u03B2\u03B3"), true, "Script=Greek");
    same(/\p{sc=Grek}/u.test("a"), false, "sc=Grek alias");
    same(/\p{Lu}/u.test("A") && !/\p{Lu}/u.test("a"), true, "Lu");
    same(/\p{General_Category=Decimal_Number}/u.test("\u0663"), true, "gc long name");
    same(/\P{L}/u.test("1"), true, "\\P");
    same(/[\P{L}]/u.test("a"), false, "\\P in a class");
    same(/\p{Script_Extensions=Latin}/u.test("\u0363"), true, "scx");
    same(/\p{ASCII_Hex_Digit}/u.test("f"), true, "binary property");
    same(/\p{Any}/u.test("\u{10FFFF}"), true, "Any");
    same(/^\p{Emoji_Presentation}$/u.test("\u{1F600}"), true, "astral emoji");
    same(/\p{L}/.test("p{L}"), true, "without u, \\p is a p");
    throws(SyntaxError, function () { new RegExp("\\p{Foo}", "u"); }, "unknown property");
    throws(SyntaxError, function () { new RegExp("\\p{Latin}", "u"); }, "script without sc=");
    throws(SyntaxError, function () { new RegExp("\\p{ascii}", "u"); }, "case-sensitive");
    throws(SyntaxError, function () { new RegExp("\\p{gc=L=x}", "u"); }, "two =");
    "ok";
    "#);
}

// ----- case folding -----

#[test]
fn case_folding_with_and_without_u() {
    ok(r#"
    same(/\u212A/i.test("k"), false, "Kelvin without u");
    same(/\u212A/iu.test("k"), true, "Kelvin with u");
    same(/\u017F/i.test("s"), false, "long s without u");
    same(/\u017F/iu.test("S"), true, "long s with u");
    same(/[a-z]/i.test("K"), true, "class with i");
    same(/[a-z]/i.test("\u212A"), false, "class: Kelvin without u");
    same(/[a-z]/iu.test("\u212A"), true, "class: Kelvin with u");
    same(/\w/iu.test("\u017F"), true, "\\w with iu has long s");
    same(/\W/iu.test("S"), false, "\\W with iu");
    same(/\w/i.test("\u017F"), false, "\\w with i");
    same(/\u00DF/i.test("\u1E9E"), false, "sharp s without u (toUppercase is SS)");
    same(/\u00DF/iu.test("\u1E9E"), true, "sharp s with u (simple folding)");
    same(/\u03C3/i.test("\u03C2"), true, "final sigma uppercases to sigma");
    same(/\u0131/i.test("i"), false, "dotless i never maps into ASCII");
    same(/(a)\1/i.test("aA"), true, "backreference with i");
    same(/^(\u{10400})\1$/iu.test("\u{10400}\u{10428}"), true, "astral backreference with iu");
    same(/[^a]/i.test("A"), false, "negated class with i");
    same(/\b\u017F/iu.test("\u017F"), true, "word boundary with iu");
    same(/(?i:a)b/.test("Ab"), true, "modifier adds i");
    same(/(?i:a)b/.test("AB"), false, "modifier is local");
    same(/(?-i:a)b/i.test("aB"), true, "modifier removes i");
    same(/(?-i:a)b/i.test("AB"), false, "modifier removes i (upper)");
    same(/(?m:^b)/.test("a\nb"), true, "modifier adds m");
    same(/(?s:.)/.test("\n"), true, "modifier adds s");
    same(/(?i:\u212A)/u.test("k"), true, "modifier with u folding");
    "ok";
    "#);
    // The engine directly.
    let re = regex("[\\u{10400}-\\u{10410}]", "iu");
    let input = utf16("\u{10430}");
    assert!(re.find(&input, 0).unwrap().is_some());
}

// ----- explicit stack -----

#[test]
fn catastrophic_patterns_terminate() {
    // Exponential paths, kept small enough to finish.
    ok(r#"
    same(/^(a+)+$/.test("aaaaaaaaaaaaaaaa!"), false, "nested plus");
    same(/^(a|a)*$/.test("aaaaaaaaaaaaaaa!"), false, "ambiguous alternation");
    same(/^(a*)*b/.test("aaaaaaaaaaaaaaaa"), false, "nested star");
    same(/(x+x+)+y/.test("xxxxxxxxxx"), false, "classic");
    "ok";
    "#);
}

#[test]
fn deep_backtracking_uses_the_explicit_stack() {
    // 100 000 iterations of a loop that is not a single character: a
    // recursive matcher would need a native frame per iteration.
    ok(r#"
    var s = "ab".repeat(50000);
    same(/^(?:a|b)*$/.test(s), true, "long loop matches");
    same(/^(?:a|b)*$/.test(s + "c"), false, "long loop backtracks all the way");
    same(/^(?:(a)|b)*$/.exec(s)[1], undefined, "captures cleared per iteration");
    same(/^(?:(a)|b)*$/.exec(s + "a")[1], "a", "capture of the last iteration");
    same(/^(?:(?=[ab])[ab])*$/.test(s), true, "lookahead in a long loop");
    same(/^(?:ab)+?$/.test(s), true, "lazy loop");
    same(/^(?:a|b)*?c/.test(s), false, "lazy loop fails");
    same(/^.*$/.exec(s)[0].length, 100000, "greedy single character");
    "ok";
    "#);
    let re = regex("^(?:a|b|cd)*e$", "");
    let input = utf16(&"cd".repeat(100_000));
    assert_eq!(re.find(&input, 0), Ok(None));
}

// ----- early errors -----

const INVALID_LITERALS: &[&str] = &[
    "/(/",
    "/)/",
    "/a**/",
    "/+a/",
    "/{1}/",
    "/a{2,1}/",
    "/[b-a]/",
    "/(?<a>.)(?<a>.)/",
    "/(?<1a>.)/",
    "/\\k<b>(?<a>.)/",
    "/(?<=a)+/",
    "/(?<=a){2}/",
    "/(?ii:a)/",
    "/(?i-i:a)/",
    "/(?-:a)/",
    "/(?x)/",
    "/\\p{Foo}/u",
    "/\\p{L}/vu",
    "/a/gg",
    "/a/x",
    "/{/u",
    "/]/u",
    "/\\-/u",
    "/[\\d-z]/u",
    "/\\2(a)/u",
    "/\\00/u",
    "/\\c1/u",
    "/\\u{110000}/u",
    "/(?=a)*/u",
    "/[^\\q{ab}]/v",
    "/[a-z&&b]/v",
    "/[|]/v",
    "/\\P{RGI_Emoji}/v",
];

const VALID_LITERALS: &[&str] = &[
    "/]/",
    "/{/",
    "/a{/",
    "/\\c/",
    "/[\\c_]/",
    "/\\8/",
    "/\\1/",
    "/(?=a)*/",
    "/[\\d-z]/",
    "/\\k/",
    "/(?<a>x)|(?<a>y)/",
    "/(?<a>x)\\k<a>/",
    "/\\1(a)/u",
    "/\\u{10FFFF}/u",
    "/(?<\\u{1d4d1}>.)/",
    "/(?i:a)(?-m:b)(?s-i:c)/",
    "/[\\p{L}--\\p{Lu}]/v",
    "/[\\q{abc|}]/v",
    "/\\p{RGI_Emoji}/v",
    "/a/dgimsy",
    "/a/v",
];

#[test]
fn early_errors_of_literals() {
    let options = parse_options();
    for src in INVALID_LITERALS {
        assert!(
            parse_script(src, &options).is_err(),
            "{src} should be an early error"
        );
        assert!(
            parse_module(src, &options).is_err(),
            "{src} should be an early error in a module"
        );
    }
    for src in VALID_LITERALS {
        if let Err(e) = parse_script(src, &options) {
            panic!("{src} should parse: {e:?}");
        }
    }
    // The validator hook on its own.
    assert!(validate_literal("(?<a>.)", "").is_ok());
    assert!(validate_literal("(", "").is_err());
    assert!(validate_literal("a", "uv").is_err());
    // Scripts, eval and Function report them as SyntaxErrors.
    assert!(run("/(/;").starts_with("throw SyntaxError"));
    ok(r#"
    throws(SyntaxError, function () { eval("/a**/"); }, "eval");
    throws(SyntaxError, function () { Function("return /[b-a]/;"); }, "Function");
    throws(SyntaxError, function () { eval("/(?<a>.)(?<a>.)/"); }, "duplicate names");
    same(eval("/(?<a>x)|(?<a>y)/").test("y"), true, "valid duplicate names");
    "ok";
    "#);
}

// ----- the built-ins -----

#[test]
fn exec_and_last_index() {
    ok(r#"
    var re = /a(b)?/g;
    var m = re.exec("xaab");
    list(m, ["a", undefined], "first match");
    same(m.index, 1, "index");
    same(m.input, "xaab", "input");
    same(re.lastIndex, 2, "lastIndex advanced");
    list(re.exec("xaab"), ["ab", "b"], "second match");
    same(re.exec("xaab"), null, "no more");
    same(re.lastIndex, 0, "lastIndex reset");
    var y = /a/y;
    same(y.test("ba"), false, "sticky does not scan");
    y.lastIndex = 1;
    same(y.test("ba"), true, "sticky at lastIndex");
    var d = /(?<x>b)(c)?/d.exec("abc");
    list(d.indices[0], [1, 3], "indices of the match");
    list(d.indices[1], [1, 2], "indices of group 1");
    list(d.indices.groups.x, [1, 2], "indices of a named group");
    var u = /./gu;
    u.lastIndex = 1;
    same(u.exec("\uD83D\uDE00").index, 1, "lastIndex inside a pair");
    same(u.lastIndex, 2, "end after the pair");
    same(/\uDE00/u.exec("\uD83D\uDE00"), null, "no trail surrogate match with u");
    var frozen = Object.freeze(/a/g);
    throws(TypeError, function () { frozen.exec("b"); }, "lastIndex not writable");
    "ok";
    "#);
}

#[test]
fn prototype_methods() {
    ok(r#"
    same(String(/a\/b/g), "/a\\/b/g", "toString");
    same(new RegExp("a/b").source, "a\\/b", "source escapes /");
    same(new RegExp("\n").source, "\\n", "source escapes line terminators");
    same(new RegExp("").source, "(?:)", "empty source");
    same(RegExp.prototype.source, "(?:)", "prototype source");
    same(RegExp.prototype.global, undefined, "prototype flag");
    same(RegExp.prototype.flags, "", "prototype flags");
    same(RegExp.prototype.toString(), "/(?:)/", "prototype toString");
    throws(TypeError, function () { RegExp.prototype.exec.call({}, "a"); }, "exec on a non-RegExp");
    same(/a/dgimsuy.flags, "dgimsuy", "flags order");
    same(Object.prototype.toString.call(/a/), "[object RegExp]", "builtin tag");
    var re = /a/g;
    same(RegExp(re), re, "RegExp(re) returns re");
    same(new RegExp(re) === re, false, "new RegExp(re) copies");
    same(new RegExp(re, "i").flags, "i", "new flags");
    same(RegExp[Symbol.species], RegExp, "species");
    var c = /a/;
    same(c.compile("b", "g"), c, "compile returns this");
    same(c.source + c.flags, "bg", "compile replaces the pattern");
    same(c.lastIndex, 0, "compile resets lastIndex");
    same(RegExp.escape("a.b"), "\\x61\\.b", "escape leading letter");
    same(RegExp.escape("1+1"), "\\x31\\+1", "escape leading digit");
    same(RegExp.escape("a b,c"), "\\x61\\x20b\\x2cc", "escape space and comma");
    same(RegExp.escape("\n"), "\\n", "escape control");
    throws(TypeError, function () { RegExp.escape(1); }, "escape needs a string");
    same(typeof RegExp.$1, "undefined", "no legacy statics");
    "ok";
    "#);
}

#[test]
fn string_methods_with_regexps() {
    ok(r#"
    list("a1b22c333".match(/\d+/g), ["1", "22", "333"], "match g");
    same("abc".match(/x/g), null, "match g fails");
    list("abc".match(/(b)/), ["b", "b"], "match without g");
    same("a-b_c".replace(/[-_]/g, "+"), "a+b+c", "replace g");
    same("John Smith".replace(/(\w+)\s(\w+)/, "$2, $1"), "Smith, John", "replace with $n");
    same("abc".replace(/b/, "[$&|$`|$']"), "a[b|a|c]c", "replace specials");
    same("aaa".replace(/a/g, function (m, i) { return i; }), "012", "functional replace");
    same("abc".replace(/(?<x>b)/, function () { return typeof arguments[arguments.length - 1]; }),
         "aobjectc", "groups passed last");
    same("xax".replace(/a*/g, "-"), "-x--x-", "empty matches");
    same("\uD83D\uDE00".replace(/(?:)/gu, "-"), "-\uD83D\uDE00-", "empty matches with u");
    same("abc".search(/c/), 2, "search");
    same("abc".search(/x/), -1, "search fails");
    list("a1b2c3".split(/\d/), ["a", "b", "c", ""], "split");
    list("a1b2c3".split(/(\d)/), ["a", "1", "b", "2", "c", "3", ""], "split with captures");
    list("abc".split(/(?:)/), ["a", "b", "c"], "split empty");
    list("a,b,c".split(/,/, 2), ["a", "b"], "split limit");
    list("".split(/x/), [""], "split empty string");
    list("".split(/(?:)/), [], "split empty string by empty");
    var all = [];
    for (var m of "a1b22".matchAll(/\d+/g)) all[all.length] = m[0] + "@" + m.index;
    list(all, ["1@1", "22@3"], "matchAll");
    throws(TypeError, function () { "a".matchAll(/a/); }, "matchAll needs g");
    var it = /a/g[Symbol.matchAll]("aa");
    same(Object.prototype.toString.call(it), "[object RegExp String Iterator]", "iterator tag");
    same(it.next().value[0], "a", "first");
    same(it.next().value.index, 1, "second");
    same(it.next().done, true, "done");
    same("aaa".replaceAll(/a/g, "b"), "bbb", "replaceAll");
    throws(TypeError, function () { "a".replaceAll(/a/, "b"); }, "replaceAll needs g");
    same("abc".startsWith("a"), true, "startsWith still works");
    throws(TypeError, function () { "a/b".startsWith(/a/); }, "startsWith rejects a RegExp");
    "ok";
    "#);
}

#[test]
fn species_and_exec_overrides() {
    ok(r#"
    var calls = 0;
    var re = /a/;
    re.exec = function (s) { calls++; return null; };
    same(re.test("a"), false, "test uses exec");
    same(calls, 1, "exec called");
    var bad = /a/;
    bad.exec = function () { return 1; };
    throws(TypeError, function () { bad.test("a"); }, "exec result must be an object");
    class R extends RegExp {}
    var r = new R("b", "g");
    same(r instanceof R, true, "subclass");
    same("abcb".split(r).length, 3, "split through species");
    "ok";
    "#);
}
