//! Strings and the Unicode tables: normalization of hand-picked lines of
//! Unicode 17.0.0 `NormalizationTest.txt`, special case mappings (ß, ﬁ,
//! final sigma), lone surrogates in `isWellFormed`/`toWellFormed`,
//! `split`/`replace`/`replaceAll` with string patterns, property lookups
//! of `vjs::unicode` and the `String` built-ins through scripts.

use vjs::agent::Agent;
use vjs::ops;
use vjs::unicode::{
    self, NormalizationForm, PropertyMatch, binary_property, general_category,
    general_category_set, in_ranges, normalize, property_escape, script, script_extensions,
    script_set, string_property,
};
use vjs::value::{PropertyKey, Value};
use vjs::{Throw, evaluate_script};

// ----- normalization -----

fn cps(field: &str) -> Vec<u32> {
    field
        .split_whitespace()
        .map(|h| u32::from_str_radix(h, 16).unwrap())
        .collect()
}

/// Lines of NormalizationTest.txt: `c1;c2;c3;c4;c5`, where c2 = NFC(c1),
/// c3 = NFD(c1), c4 = NFKC(c1) and c5 = NFKD(c1).
const NORMALIZATION_LINES: &[&str] = &[
    "1E0A;1E0A;0044 0307;1E0A;0044 0307",
    "1E0A 0323;1E0C 0307;0044 0323 0307;1E0C 0307;0044 0323 0307",
    "1E0C 0307;1E0C 0307;0044 0323 0307;1E0C 0307;0044 0323 0307",
    "1E14;1E14;0045 0304 0300;1E14;0045 0304 0300",
    "0344;0308 0301;0308 0301;0308 0301;0308 0301",
    "0958;0915 093C;0915 093C;0915 093C;0915 093C",
    "0F73;0F71 0F72;0F71 0F72;0F71 0F72;0F71 0F72",
    "2126;03A9;03A9;03A9;03A9",
    "212B;00C5;0041 030A;00C5;0041 030A",
    "AC00;AC00;1100 1161;AC00;1100 1161",
    "AC01;AC01;1100 1161 11A8;AC01;1100 1161 11A8",
    "FB01;FB01;FB01;0066 0069;0066 0069",
    "1D15E;1D157 1D165;1D157 1D165;1D157 1D165;1D157 1D165",
    "0061 0315 0300 05AE 0300 0062;00E0 05AE 0300 0315 0062;0061 05AE 0300 0300 0315 0062;00E0 05AE 0300 0315 0062;0061 05AE 0300 0300 0315 0062",
    "00C5 0301;01FA;0041 030A 0301;01FA;0041 030A 0301",
];

#[test]
fn normalization_test_lines() {
    let forms = [
        NormalizationForm::Nfc,
        NormalizationForm::Nfd,
        NormalizationForm::Nfkc,
        NormalizationForm::Nfkd,
    ];
    for line in NORMALIZATION_LINES {
        let c: Vec<Vec<u32>> = line.split(';').map(cps).collect();
        for (k, form) in forms.iter().enumerate() {
            assert_eq!(normalize(&c[0], *form), c[k + 1], "{form:?} of {line}");
        }
        // NFC and NFD are idempotent and agree on canonically equivalent
        // columns (the conformance invariants c2 = NFC(c2) = NFC(c3)).
        assert_eq!(normalize(&c[1], NormalizationForm::Nfc), c[1], "{line}");
        assert_eq!(normalize(&c[2], NormalizationForm::Nfc), c[1], "{line}");
        assert_eq!(normalize(&c[1], NormalizationForm::Nfd), c[2], "{line}");
        assert_eq!(normalize(&c[3], NormalizationForm::Nfkd), c[4], "{line}");
    }
}

#[test]
fn normalization_keeps_lone_surrogates() {
    let units = [0xD800u16, 0x0041, 0x030A, 0xDC00];
    let nfc = unicode::normalize_utf16(&units, NormalizationForm::Nfc);
    assert_eq!(nfc, vec![0xD800, 0x00C5, 0xDC00]);
    assert_eq!(unicode::combining_class(0x0301), 230);
    assert_eq!(unicode::combining_class(0x0041), 0);
}

// ----- case mapping -----

fn upper(s: &str) -> String {
    let c: Vec<u32> = s.chars().map(|c| c as u32).collect();
    unicode::to_uppercase(&c)
        .into_iter()
        .map(|c| char::from_u32(c).unwrap())
        .collect()
}

fn lower(s: &str) -> String {
    let c: Vec<u32> = s.chars().map(|c| c as u32).collect();
    unicode::to_lowercase(&c)
        .into_iter()
        .map(|c| char::from_u32(c).unwrap())
        .collect()
}

#[test]
fn special_casing() {
    assert_eq!(upper("straße"), "STRASSE");
    assert_eq!(upper("ﬁ"), "FI");
    assert_eq!(upper("ŉ"), "ʼN");
    assert_eq!(upper("\u{390}"), "\u{399}\u{308}\u{301}");
    assert_eq!(lower("İ"), "i\u{307}");
    // ẞ lowercases to ß; ß has no single uppercase.
    assert_eq!(lower("ẞ"), "ß");
    assert_eq!(unicode::simple_uppercase('ß' as u32), 'ß' as u32);
    let mut title = Vec::new();
    unicode::push_titlecase(&mut title, 'ﬁ' as u32);
    assert_eq!(title, vec!['F' as u32, 'i' as u32]);
    assert_eq!(unicode::simple_titlecase('ǆ' as u32), 'ǅ' as u32);
}

#[test]
fn final_sigma() {
    assert_eq!(lower("ΟΔΟΣ"), "οδος");
    assert_eq!(lower("ΟΔΟΣ ΟΔΟΣ"), "οδος οδος");
    // Alone, or followed by a letter, Σ is not final.
    assert_eq!(lower("Σ"), "σ");
    assert_eq!(lower("ΣΑ"), "σα");
    // Case-ignorable characters between are skipped on both sides.
    assert_eq!(lower("Α.Σ"), "α.ς");
    assert_eq!(lower("ΑΣ.Α"), "ασ.α");
    assert_eq!(lower("ΑΣ\u{301}"), "ας\u{301}");
}

#[test]
fn case_folding() {
    assert_eq!(unicode::simple_fold('A' as u32), 'a' as u32);
    assert_eq!(unicode::simple_fold('ẞ' as u32), 'ß' as u32);
    // KELVIN SIGN folds to k.
    assert_eq!(unicode::simple_fold(0x212A), 'k' as u32);
    let mut full = Vec::new();
    unicode::push_full_fold(&mut full, 'ß' as u32);
    assert_eq!(full, vec!['s' as u32, 's' as u32]);
    full.clear();
    unicode::push_full_fold(&mut full, 'Σ' as u32);
    assert_eq!(full, vec!['σ' as u32]);
}

// ----- properties -----

#[test]
fn property_lookups() {
    assert_eq!(general_category('A' as u32), "Lu");
    assert_eq!(general_category(0x0378), "Cn");
    assert_eq!(general_category(0xE000), "Co");
    assert_eq!(general_category(0x4E00), "Lo");
    assert_eq!(script('A' as u32), "Latin");
    assert_eq!(script('α' as u32), "Greek");
    assert_eq!(script(0x0378), "Unknown");
    assert_eq!(script('1' as u32), "Common");
    assert_eq!(script_extensions('A' as u32), vec!["Latin"]);
    assert!(script_extensions(0x3001).contains(&"Hiragana"));
    assert!(!script_extensions(0x3001).contains(&"Common"));

    // Aliases resolve to the same table.
    let alpha = binary_property("Alphabetic").unwrap();
    assert_eq!(alpha, binary_property("Alpha").unwrap());
    assert!(in_ranges(alpha, 'é' as u32) && !in_ranges(alpha, '1' as u32));
    assert!(in_ranges(binary_property("space").unwrap(), 0x3000));
    assert!(in_ranges(binary_property("Any").unwrap(), 0x10FFFF));
    assert!(!in_ranges(binary_property("Assigned").unwrap(), 0x0378));
    assert!(in_ranges(binary_property("ASCII").unwrap(), 0x7F));
    assert!(!in_ranges(binary_property("ASCII").unwrap(), 0x80));
    assert!(in_ranges(binary_property("Bidi_M").unwrap(), '(' as u32));
    assert!(in_ranges(binary_property("Emoji").unwrap(), 0x1F600));
    assert!(in_ranges(binary_property("CWKCF").unwrap(), 'A' as u32));
    // Names are case-sensitive and loose matching is not allowed.
    assert!(binary_property("alphabetic").is_none());
    assert!(binary_property("Is_Alphabetic").is_none());

    let digit = general_category_set("digit").unwrap();
    assert_eq!(digit, general_category_set("Nd").unwrap());
    assert_eq!(digit, general_category_set("Decimal_Number").unwrap());
    let letters = general_category_set("L").unwrap();
    assert!(in_ranges(letters, 'z' as u32) && in_ranges(letters, 0x4E00));
    assert!(in_ranges(general_category_set("Cn").unwrap(), 0x10FFFF));
    let greek = script_set("Grek", false).unwrap();
    assert_eq!(greek, script_set("Greek", false).unwrap());
    assert!(in_ranges(greek, 'λ' as u32));
    assert!(in_ranges(script_set("Qaai", false).unwrap(), 0x0300));
    // U+0342 is Inherited but its Script_Extensions are {Greek}.
    assert!(in_ranges(script_set("Greek", true).unwrap(), 0x0342));
    assert!(!in_ranges(script_set("Greek", false).unwrap(), 0x0342));
    assert!(
        script_set("Katakana_Or_Hiragana", false)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn property_escapes() {
    let set = |name: Option<&str>, value: &str| match property_escape(name, value) {
        Some(PropertyMatch::CodePoints(r)) => Some(r),
        _ => None,
    };
    assert!(in_ranges(set(Some("gc"), "Lu").unwrap(), 'A' as u32));
    assert!(in_ranges(
        set(Some("General_Category"), "Lu").unwrap(),
        'A' as u32
    ));
    assert!(in_ranges(set(Some("sc"), "Latn").unwrap(), 'A' as u32));
    assert!(in_ranges(set(Some("scx"), "Hira").unwrap(), 0x3001));
    assert!(in_ranges(set(None, "Lu").unwrap(), 'A' as u32));
    assert!(in_ranges(set(None, "White_Space").unwrap(), ' ' as u32));
    // Script values need `sc=`; binary properties take no value.
    assert!(property_escape(None, "Latin").is_none());
    assert!(property_escape(Some("Alphabetic"), "Yes").is_none());
    assert!(property_escape(Some("Block"), "Basic_Latin").is_none());
    assert!(property_escape(Some("gc"), "Latin").is_none());

    let Some(PropertyMatch::Strings(rgi)) = property_escape(None, "RGI_Emoji") else {
        panic!("RGI_Emoji is a property of strings");
    };
    assert!(in_ranges(rgi.code_points, 0x1F600));
    let flag_us: &[u32] = &[0x1F1FA, 0x1F1F8];
    assert!(rgi.strings.binary_search(&flag_us).is_ok());
    let keycap = string_property("Emoji_Keycap_Sequence").unwrap();
    assert!(keycap.code_points.is_empty());
    let one: &[u32] = &[0x31, 0xFE0F, 0x20E3];
    assert!(keycap.strings.contains(&one));
    let zwj = string_property("RGI_Emoji_ZWJ_Sequence").unwrap();
    assert!(zwj.strings.iter().all(|s| s.contains(&0x200D)));
}

// ----- scripts -----

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
fn normalize_and_case_methods() {
    ok(r#"
    same("\u1E9B\u0323".normalize("NFC"), "\u1E9B\u0323", "NFC");
    same("\u1E9B\u0323".normalize("NFD"), "\u017F\u0323\u0307", "NFD");
    same("\u1E9B\u0323".normalize("NFKC"), "\u1E69", "NFKC");
    same("\u1E9B\u0323".normalize("NFKD"), "s\u0323\u0307", "NFKD");
    same("A\u030A".normalize(), "\u00C5", "default form is NFC");
    same("\u1100\u1161\u11A8".normalize(), "\uAC01", "Hangul composes");
    throws(RangeError, function () { "a".normalize("nfc"); }, "form names are exact");
    same("stra\u00DFe".toUpperCase(), "STRASSE", "sharp s");
    same("\uFB01".toUpperCase(), "FI", "fi ligature");
    same("\u039F\u0394\u039F\u03A3".toLowerCase(), "\u03BF\u03B4\u03BF\u03C2", "final sigma");
    same("\u03A3".toLowerCase(), "\u03C3", "lone sigma");
    same("\u0130".toLowerCase(), "i\u0307", "dotted I");
    same("\u0130".toLocaleLowerCase(), "i\u0307", "no locale tailoring");
    same("\uD801\uDC00".toLowerCase(), "\uD801\uDC28", "astral (Deseret)");
    same("\uD800a".toUpperCase(), "\uD800A", "lone surrogate kept");
    same("o\u0308".localeCompare("\u00F6"), 0, "canonical equivalence");
    same("a".localeCompare("b"), -1, "a < b");
    same("b".localeCompare("a"), 1, "b > a");
    "ok";
    "#);
}

#[test]
fn well_formed_strings() {
    ok(r#"
    same("abc".isWellFormed(), true, "ascii");
    same("\uD83D\uDE00".isWellFormed(), true, "pair");
    same("\uD83D".isWellFormed(), false, "lone lead");
    same("\uDE00".isWellFormed(), false, "lone trail");
    same("a\uDE00\uD83Db".isWellFormed(), false, "reversed pair");
    same("a\uDE00\uD83Db".toWellFormed(), "a\uFFFD\uFFFDb", "toWellFormed");
    same("\uD83D\uDE00".toWellFormed(), "\uD83D\uDE00", "pair kept");
    same(String.prototype.isWellFormed.call(5), true, "coerces this");
    throws(TypeError, function () { String.prototype.isWellFormed.call(null); }, "null this");
    "ok";
    "#);
}

#[test]
fn split_and_replace_with_strings() {
    ok(r#"
    list("a,b,,c".split(","), ["a", "b", "", "c"], "split");
    list("a,b,c".split(",", 2), ["a", "b"], "split limit");
    list("abc".split(""), ["a", "b", "c"], "split empty");
    list("".split(""), [], "empty split empty");
    list("".split(","), [""], "empty split");
    list("abc".split(), ["abc"], "no separator");
    list("abc".split(undefined, 0), [], "limit 0");
    list("a1b1c".split(1), ["a", "b", "c"], "number separator");
    list("\uD83D\uDE00".split(""), ["\uD83D", "\uDE00"], "split by code units");

    same("aXbX".replace("X", "-"), "a-bX", "replace first");
    same("aXbX".replaceAll("X", "-"), "a-b-", "replaceAll");
    same("abc".replace("b", "[$&|$`|$'|$$]"), "a[b|a|c|$]c", "substitutions");
    same("abc".replace("b", "$1$<x>"), "a$1$<x>c", "no captures");
    same("abc".replace("", "-"), "-abc", "empty pattern");
    same("abc".replaceAll("", "-"), "-a-b-c-", "replaceAll empty");
    same("aaa".replaceAll("aa", "b"), "ba", "non-overlapping");
    same("abc".replace("b", function (m, p, s) { return m + p + s; }), "ab1abcc", "fn");
    var seen = [];
    "xyx".replaceAll("x", function (m, p) { seen[seen.length] = p; return "z"; });
    list(seen, [0, 2], "replaceAll positions");
    var splitter = { [Symbol.split]: function (s, l) { return "custom " + s + l; } };
    same("abc".split(splitter, 3), "custom abc3", "Symbol.split");
    var replacer = { [Symbol.replace]: function (s, r) { return s + r; } };
    same("abc".replace(replacer, "!"), "abc!", "Symbol.replace");
    var noGlobal = { [Symbol.match]: true, flags: "i" };
    throws(TypeError, function () { "a".replaceAll(noGlobal, ""); }, "replaceAll needs g");
    throws(TypeError, function () { "a".matchAll(noGlobal); }, "matchAll needs g");
    "ok";
    "#);
}

#[test]
fn string_constructor_and_statics() {
    ok(r#"
    same(String(), "", "String()");
    same(String(Symbol("x")), "Symbol(x)", "String(symbol)");
    throws(TypeError, function () { new String(Symbol()); }, "new String(symbol)");
    var s = new String("ab");
    same(typeof s, "object", "new String");
    same(s.length, 2, "length");
    same(s[1], "b", "index");
    list(Object.getOwnPropertyNames(s), ["0", "1", "length"], "own keys");
    same(String.fromCharCode(0x10061, 98), "ab", "fromCharCode wraps");
    same(String.fromCodePoint(0x1F600), "\uD83D\uDE00", "fromCodePoint");
    throws(RangeError, function () { String.fromCodePoint(1.5); }, "fractional");
    throws(RangeError, function () { String.fromCodePoint(0x110000); }, "too large");
    same(String.raw`a\n${1}b`, "a\\n1b", "raw");
    same(String.raw({ raw: ["x", "y"] }, 1, 2), "x1y", "raw object");
    same(String.prototype.trimLeft, String.prototype.trimStart, "trimLeft alias");
    same(String.prototype.trimRight, String.prototype.trimEnd, "trimRight alias");
    same(String.length, 1, "String.length");
    same(String.prototype.padStart.length, 1, "padStart.length");
    same(String.prototype.replace.length, 2, "replace.length");
    "ok";
    "#);
}

#[test]
fn prototype_methods() {
    ok(r#"
    same("abc".at(-1), "c", "at");
    same("abc".at(3), undefined, "at out of range");
    same("abc".charAt(1), "b", "charAt");
    same("abc".charCodeAt(9), NaN, "charCodeAt");
    same("\uD83D\uDE00".codePointAt(0), 0x1F600, "codePointAt pair");
    same("\uD83D\uDE00".codePointAt(1), 0xDE00, "codePointAt trail");
    same("a".concat(1, null), "a1null", "concat");
    same("abc".endsWith("b", 2), true, "endsWith");
    same("abc".startsWith("b", 1), true, "startsWith");
    same("abc".includes("c", -5), true, "includes");
    throws(TypeError, function () { "a".includes({ [Symbol.match]: true }); }, "includes RegExp");
    same("abcabc".indexOf("c", 3), 5, "indexOf");
    same("abcabc".lastIndexOf("c"), 5, "lastIndexOf");
    same("abcabc".lastIndexOf("c", 4), 2, "lastIndexOf position");
    same("abc".lastIndexOf("", 1), 1, "lastIndexOf empty");
    same("5".padStart(3, "0"), "005", "padStart");
    same("5".padEnd(4, "ab"), "5aba", "padEnd");
    same("5".padEnd(4, ""), "5", "padEnd empty filler");
    same("ab".repeat(3), "ababab", "repeat");
    throws(RangeError, function () { "a".repeat(-1); }, "repeat negative");
    throws(RangeError, function () { "a".repeat(Infinity); }, "repeat infinite");
    same("".repeat(1e9), "", "empty repeat");
    same("abcdef".slice(-3, -1), "de", "slice");
    same("abcdef".substring(4, 1), "bcd", "substring swaps");
    same("abcdef".substr(-2), "ef", "substr");
    same("\u3000\u2028 a \uFEFF\n".trim(), "a", "trim Unicode spaces");
    same(" a ".trimStart(), "a ", "trimStart");
    same(" a ".trimEnd(), " a", "trimEnd");
    same("\u180Ea".trim(), "\u180Ea", "U+180E is not white space");
    same(new String("x").valueOf(), "x", "valueOf");
    throws(TypeError, function () { String.prototype.toString.call({}); }, "toString this");
    "ok";
    "#);
}

#[test]
fn string_iterator() {
    ok(r#"
    var it = "a\uD83D\uDE00\uD800"[Symbol.iterator]();
    same(Object.getPrototypeOf(Object.getPrototypeOf(it)),
         Object.getPrototypeOf(Object.getPrototypeOf([][Symbol.iterator]())),
         "inherits %IteratorPrototype%");
    same(Object.prototype.toString.call(it), "[object String Iterator]", "tag");
    same(it.next().value, "a", "first");
    same(it.next().value, "\uD83D\uDE00", "pair");
    same(it.next().value, "\uD800", "lone");
    var last = it.next();
    same(last.done, true, "done");
    same(last.value, undefined, "done value");
    var parts = [];
    for (var c of "x\uD83D\uDE00y") parts[parts.length] = c;
    list(parts, ["x", "\uD83D\uDE00", "y"], "for-of");
    throws(TypeError, function () { it.next.call({}); }, "next on non-iterator");
    "ok";
    "#);
}
