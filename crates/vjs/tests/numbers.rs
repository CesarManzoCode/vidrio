//! Numbers, Math and BigInt: number ↔ string conversion edges (subnormals,
//! `5e-324`, `1e21`, ties to even), `toFixed`/`toExponential`/
//! `toPrecision`/`toString(radix)`, the `Math` functions, BigInt
//! arithmetic with signs and large sizes, and mixed BigInt/Number
//! comparisons, both on the Rust API and through scripts.

use vjs::agent::Agent;
use vjs::bigint::BigInt;
use vjs::numconv::{
    decimal_to_f64, number_to_string, number_to_string_radix, parse_decimal, to_exponential,
    to_fixed, to_precision,
};
use vjs::ops;
use vjs::string::utf16;
use vjs::value::{PropertyKey, Value};
use vjs::{Throw, evaluate_script};

// ----- Rust API -----

fn parse(s: &str) -> f64 {
    parse_decimal(s.as_bytes()).unwrap_or_else(|| panic!("not a decimal: {s}"))
}

fn big(s: &str) -> BigInt {
    let (neg, digits) = match s.strip_prefix('-') {
        Some(d) => (true, d),
        None => (false, s),
    };
    let b = BigInt::parse_radix(digits.as_bytes(), 10).unwrap();
    if neg { b.neg() } else { b }
}

#[test]
fn string_to_number_is_correctly_rounded() {
    // Subnormals and the smallest Number.
    assert_eq!(parse("5e-324").to_bits(), 1);
    assert_eq!(parse("4.9406564584124654e-324").to_bits(), 1);
    assert_eq!(parse("2.4703282292062327e-324"), 0.0);
    // Exactly half of 2^-1074 (2^-1075 = 5^1075 × 10^-1075) ties to even
    // (zero); a hair above rounds up, also past the exact-digit cut.
    let half = BigInt::from_i64(5).pow(1075).unwrap().to_string_radix(10);
    assert_eq!(decimal_to_f64(half.as_bytes(), -1075), 0.0);
    let above = format!("{half}1");
    assert_eq!(decimal_to_f64(above.as_bytes(), -1076).to_bits(), 1);
    let far_above = format!("{half}{}1", "0".repeat(200));
    assert_eq!(decimal_to_f64(far_above.as_bytes(), -1276).to_bits(), 1);
    let below = BigInt::from_i64(5)
        .pow(1075)
        .unwrap()
        .sub(&BigInt::one())
        .to_string_radix(10);
    let far_below = format!("{below}{}", "9".repeat(300));
    assert_eq!(decimal_to_f64(far_below.as_bytes(), -1375), 0.0);
    // Three halves of 2^-1074 tie to the even neighbour, 2 × 2^-1074.
    let three_halves = BigInt::from_i64(5)
        .pow(1075)
        .unwrap()
        .mul(&BigInt::from_i64(3))
        .to_string_radix(10);
    assert_eq!(decimal_to_f64(three_halves.as_bytes(), -1075).to_bits(), 2);
    // Largest subnormal and smallest normal.
    assert_eq!(
        parse("2.225073858507201e-308").to_bits(),
        0x000f_ffff_ffff_ffff
    );
    assert_eq!(
        parse("2.2250738585072014e-308").to_bits(),
        0x0010_0000_0000_0000
    );
    // Ties to even around 2^53: 2^53 + 1 → 2^53, 2^53 + 3 → 2^53 + 4.
    assert_eq!(parse("9007199254740993"), 9007199254740992.0);
    assert_eq!(parse("9007199254740995"), 9007199254740996.0);
    assert_eq!(
        parse("9007199254740993.000000000000000000001"),
        9007199254740994.0
    );
    // Overflow edge: the midpoint between MAX and 2^1024 rounds to even
    // (Infinity); just below stays MAX.
    assert_eq!(parse("1.7976931348623157e308"), f64::MAX);
    assert_eq!(parse("1.7976931348623158e308"), f64::MAX);
    assert_eq!(parse("1.79769313486231581e308"), f64::INFINITY);
    assert_eq!(parse("1e21"), 1e21);
    assert_eq!(parse("0.1"), 0.1);
    assert_eq!(parse("1e-400"), 0.0);
    assert_eq!(parse("1e400"), f64::INFINITY);
    assert_eq!(parse("000123.4500e2"), 12345.0);
    assert_eq!(parse_decimal(b"."), None);
    assert_eq!(parse_decimal(b"1e+"), None);
    assert_eq!(parse_decimal(b"1x"), None);
    // StringToNumber on top of it.
    let n = |s: &str| vjs::number::string_to_number(&utf16(s));
    assert_eq!(n("  5e-324\n").to_bits(), 1);
    assert_eq!(n("-1e21"), -1e21);
    assert!(n("+-1").is_nan());
}

#[test]
fn number_to_string_is_shortest_round_trip() {
    let cases: &[(f64, &str)] = &[
        (5e-324, "5e-324"),
        (1e21, "1e+21"),
        (999999999999999900000.0, "999999999999999900000"),
        (1e-6, "0.000001"),
        (1e-7, "1e-7"),
        (0.1 + 0.2, "0.30000000000000004"),
        (f64::MAX, "1.7976931348623157e+308"),
        (2.2250738585072014e-308, "2.2250738585072014e-308"),
        (2.225073858507201e-308, "2.225073858507201e-308"),
        (1e23, "1e+23"),
        (9007199254740993.0, "9007199254740992"),
        (123e-20, "1.23e-18"),
        (-1.5, "-1.5"),
        (-0.0, "0"),
        (f64::NAN, "NaN"),
        (f64::NEG_INFINITY, "-Infinity"),
        // A power of two, where the gap below is half the gap above.
        (2f64.powi(-1020), "8.900295434028806e-308"),
        (2f64.powi(100), "1.2676506002282294e+30"),
    ];
    for &(x, expected) in cases {
        assert_eq!(number_to_string(x), expected, "{x:e}");
    }
    let mut x = 5e-324;
    for _ in 0..200 {
        let s = number_to_string(x);
        assert_eq!(parse(&s), x, "{s}");
        x = x * 7.3 + 1e-300;
    }
}

#[test]
fn to_fixed_exponential_precision_are_exact() {
    assert_eq!(to_fixed(1.005, 2), "1.00");
    assert_eq!(to_fixed(1.25, 1), "1.3");
    assert_eq!(to_fixed(1.35, 1), "1.4");
    assert_eq!(to_fixed(0.5, 0), "1");
    assert_eq!(to_fixed(2.5, 0), "3");
    assert_eq!(to_fixed(-2.5, 0), "-3");
    assert_eq!(to_fixed(0.000001, 2), "0.00");
    assert_eq!(to_fixed(-0.0000001, 2), "-0.00");
    assert_eq!(to_fixed(1e21, 2), "1e+21");
    assert_eq!(to_fixed(1e20, 2), "100000000000000000000.00");
    assert_eq!(to_fixed(0.1, 30), "0.100000000000000005551115123126");
    assert_eq!(to_fixed(5e-324, 100), format!("0.{}", "0".repeat(100)));
    assert_eq!(to_fixed(123.456, 0), "123");
    assert_eq!(to_exponential(123456.0, Some(2)), "1.23e+5");
    assert_eq!(to_exponential(0.00015, Some(1)), "1.5e-4");
    assert_eq!(to_exponential(9.995, Some(2)), "9.99e+0");
    // 99.95 is slightly above 99.95 in binary, so it rounds up to 100.
    assert_eq!(to_exponential(99.95, Some(2)), "1.00e+2");
    assert_eq!(to_exponential(5e-324, None), "5e-324");
    assert_eq!(to_exponential(0.0, None), "0e+0");
    assert_eq!(to_exponential(-1.5, Some(3)), "-1.500e+0");
    assert_eq!(to_precision(123.456, 4), "123.5");
    assert_eq!(to_precision(0.000001234, 2), "0.0000012");
    assert_eq!(to_precision(0.0000001234, 2), "1.2e-7");
    assert_eq!(to_precision(123456.0, 2), "1.2e+5");
    assert_eq!(to_precision(1e21, 1), "1e+21");
    assert_eq!(to_precision(0.0, 3), "0.00");
    assert_eq!(to_precision(1.45, 2), "1.4");
    assert_eq!(to_precision(25.0, 1), "3e+1");
    assert_eq!(
        to_precision(0.1, 100),
        "0.1000000000000000055511151231257827021181583404541015625000000000000000000000000000000000000000000000"
    );
}

#[test]
fn radix_strings_are_exact() {
    assert_eq!(number_to_string_radix(255.0, 16), "ff");
    assert_eq!(number_to_string_radix(-255.0, 2), "-11111111");
    assert_eq!(number_to_string_radix(0.5, 2), "0.1");
    assert!(number_to_string_radix(0.1, 3).starts_with("0.002200220022002200220022"));
    assert_eq!(number_to_string_radix(2f64.powi(-1074), 2).len(), 2 + 1074);
    assert_eq!(
        number_to_string_radix(2f64.powi(70), 16),
        "400000000000000000"
    );
    assert_eq!(number_to_string_radix(1e21, 10), "1e+21");
    assert_eq!(number_to_string_radix(f64::NAN, 2), "NaN");
    assert_eq!(number_to_string_radix(-0.0, 36), "0");
    assert_eq!(number_to_string_radix(35.0, 36), "z");
}

#[test]
fn bigint_arithmetic_with_signs_and_sizes() {
    let a = big("-340282366920938463463374607431768211457");
    let b = big("18446744073709551616");
    let (q, r) = a.div_rem(&b).unwrap();
    assert_eq!(q.to_string_radix(10), "-18446744073709551616");
    assert_eq!(r.to_string_radix(10), "-1");
    let (q, r) = big("7").div_rem(&big("-2")).unwrap();
    assert_eq!(
        (q.to_string_radix(10), r.to_string_radix(10)),
        ("-3".into(), "1".into())
    );
    assert!(big("1").div_rem(&BigInt::zero()).is_none());
    let p = big("-3").pow(101).unwrap();
    assert!(p.is_negative());
    assert_eq!(p.bit_length(), 161);
    let ten50 = big("10").pow(50).unwrap();
    assert_eq!(ten50.to_string_radix(10), format!("1{}", "0".repeat(50)));
    assert_eq!(
        ten50.mul(&ten50.neg()).to_string_radix(10),
        format!("-1{}", "0".repeat(100))
    );
    // Shifts round toward minus infinity.
    assert_eq!(big("-1").shr(100).to_string_radix(10), "-1");
    assert_eq!(big("-4294967297").shr(32).to_string_radix(10), "-2");
    assert_eq!(
        big("1").shl(100).to_string_radix(16),
        format!("1{}", "0".repeat(25))
    );
    // Two's complement bitwise operators on mixed signs and widths.
    let x = big("-18446744073709551617");
    assert_eq!(
        x.bitand(&big("4294967295")).to_string_radix(10),
        "4294967295"
    );
    assert_eq!(
        x.bitor(&big("1")).to_string_radix(10),
        "-18446744073709551617"
    );
    assert_eq!(
        x.bitxor(&big("-1")).to_string_radix(10),
        "18446744073709551616"
    );
    assert_eq!(big("-1").bitand(&big("-1")).to_string_radix(10), "-1");
    assert_eq!(BigInt::zero().bitnot().to_string_radix(10), "-1");
    // asIntN / asUintN.
    assert_eq!(
        big("-1").as_uint_n(64).unwrap().to_string_radix(10),
        "18446744073709551615"
    );
    assert_eq!(
        big("18446744073709551615")
            .as_int_n(64)
            .unwrap()
            .to_string_radix(10),
        "-1"
    );
    assert_eq!(
        big("9223372036854775808")
            .as_int_n(64)
            .unwrap()
            .to_string_radix(10),
        "-9223372036854775808"
    );
    assert_eq!(
        big("-9223372036854775808")
            .as_int_n(64)
            .unwrap()
            .to_string_radix(10),
        "-9223372036854775808"
    );
    assert_eq!(big("12345").as_uint_n(0).unwrap().to_string_radix(10), "0");
    assert_eq!(big("-5").as_int_n(1).unwrap().to_string_radix(10), "-1");
    assert_eq!(big("-36").to_string_radix(36), "-10");
    assert_eq!(
        BigInt::parse_radix(b"zzzzzzzzzzzzzz", 36)
            .unwrap()
            .to_string_radix(36),
        "zzzzzzzzzzzzzz"
    );
    // BigInt → Number rounds to nearest, ties to even, and overflows.
    assert_eq!(big("18014398509481985").to_f64(), 18014398509481984.0);
    assert_eq!(big("18014398509481987").to_f64(), 18014398509481988.0);
    assert_eq!(BigInt::one().shl(1024).to_f64(), f64::INFINITY);
    assert_eq!(BigInt::one().shl(1024).neg().to_f64(), f64::NEG_INFINITY);
}

#[test]
fn bigint_number_comparisons() {
    use std::cmp::Ordering::*;
    assert_eq!(
        big("9007199254740993").cmp_f64(9007199254740992.0),
        Some(Greater)
    );
    assert_eq!(
        big("9007199254740992").cmp_f64(9007199254740992.0),
        Some(Equal)
    );
    assert_eq!(big("-1").cmp_f64(-0.5), Some(Less));
    assert_eq!(big("0").cmp_f64(-0.0), Some(Equal));
    assert_eq!(big("0").cmp_f64(5e-324), Some(Less));
    assert_eq!(big("-0").cmp_f64(-5e-324), Some(Greater));
    assert_eq!(BigInt::one().shl(2000).cmp_f64(f64::MAX), Some(Greater));
    assert_eq!(BigInt::one().shl(2000).cmp_f64(f64::INFINITY), Some(Less));
    assert_eq!(big("1").cmp_f64(f64::NAN), None);
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
fn number_statics_and_prototype() {
    ok(r#"
    same(Number.MIN_VALUE, 5e-324, "MIN_VALUE");
    same(Number.MAX_SAFE_INTEGER, 9007199254740991, "MAX_SAFE_INTEGER");
    same(Number.EPSILON, Math.pow(2, -52), "EPSILON");
    same(Number.parseFloat, parseFloat, "parseFloat is shared");
    same(Number.parseInt, parseInt, "parseInt is shared");
    same(Number.isInteger(5.0), true, "isInteger");
    same(Number.isInteger("5"), false, "isInteger string");
    same(Number.isSafeInteger(9007199254740992), false, "isSafeInteger");
    same(Number.isNaN("NaN"), false, "isNaN no coercion");
    same(Number(), 0, "Number()");
    same(Number(" 0x10 "), 16, "Number hex");
    same(Number(12345678901234567890n), 12345678901234567000, "Number(BigInt)");
    same(typeof new Number(1), "object", "new Number");
    same(new Number(7).valueOf(), 7, "valueOf");
    same((25).toString(36), "p", "toString radix");
    same((-255).toString(16), "-ff", "toString negative radix");
    same((0.5).toString(2), "0.1", "binary fraction");
    same(1e21.toString(), "1e+21", "1e21");
    same(String(5e-324), "5e-324", "5e-324");
    same((1.005).toFixed(2), "1.00", "toFixed exact binary value");
    same((1e21).toFixed(2), "1e+21", "toFixed large");
    same((-1.5).toFixed(0), "-2", "toFixed tie away");
    same((123.456).toExponential(), "1.23456e+2", "toExponential shortest");
    same((0).toExponential(2), "0.00e+0", "toExponential zero");
    same((123.456).toPrecision(4), "123.5", "toPrecision");
    same((0.00000123).toPrecision(2), "0.0000012", "toPrecision small");
    same((1e-7).toPrecision(1), "1e-7", "toPrecision exponent");
    same(NaN.toFixed(2), "NaN", "toFixed NaN");
    same(Infinity.toExponential(1000), "Infinity", "toExponential Infinity first");
    throws(RangeError, function () { (1).toFixed(101); }, "toFixed range");
    throws(RangeError, function () { NaN.toFixed(Infinity); }, "toFixed infinite digits");
    throws(RangeError, function () { (1).toPrecision(0); }, "toPrecision range");
    throws(RangeError, function () { (1).toString(37); }, "radix range");
    throws(TypeError, function () { Number.prototype.toString.call("1"); }, "this check");
    "ok";
    "#);
}

#[test]
fn math_functions() {
    ok(r#"
    same(Object.prototype.toString.call(Math), "[object Math]", "tag");
    same(Math.fround(5.5), 5.5, "fround exact");
    same(Math.fround(5.05), 5.050000190734863, "fround");
    same(Math.fround(16777217), 16777216, "fround tie even");
    same(Math.f16round(1.337), 1.3369140625, "f16round");
    same(Math.f16round(65520), Infinity, "f16round overflow");
    same(Math.f16round(-0), -0, "f16round -0");
    same(Math.clz32(0), 32, "clz32 0");
    same(Math.clz32(1), 31, "clz32 1");
    same(Math.clz32(-1), 0, "clz32 -1");
    same(Math.imul(0xffffffff, 5), -5, "imul");
    same(Math.imul(0x7fffffff, 2), -2, "imul overflow");
    same(Math.hypot(), 0, "hypot()");
    same(Math.hypot(3, 4), 5, "hypot");
    same(Math.hypot(NaN, -Infinity), Infinity, "hypot inf beats NaN");
    same(Math.hypot(1e300, 1e300), 1.4142135623730952e300, "hypot no overflow");
    same(Math.max(), -Infinity, "max()");
    same(Math.max(-0, 0), 0, "max zeros");
    same(Math.min(0, -0), -0, "min zeros");
    same(Math.max(1, NaN, 3), NaN, "max NaN");
    same(Math.round(-0.5), -0, "round -0.5");
    same(Math.round(2.5), 3, "round 2.5");
    same(Math.round(0.49999999999999994), 0, "round below half");
    same(Math.sign(-0), -0, "sign -0");
    same(Math.trunc(-0.9), -0, "trunc");
    same(Math.cbrt(-27), -3, "cbrt");
    same(Math.pow(NaN, 0), 1, "pow NaN^0");
    same(Math.pow(1, Infinity), NaN, "pow 1^inf");
    same(Math.atan2(-0, -0), -Math.PI, "atan2");
    same(Math.asinh(-0), -0, "asinh -0");
    var r = Math.random();
    same(r >= 0 && r < 1, true, "random range");
    same(Math.sumPrecise([1e20, 0.1, -1e20]), 0.1, "sumPrecise exact");
    same(Math.sumPrecise([]), -0, "sumPrecise empty");
    same(Math.sumPrecise([-0, -0]), -0, "sumPrecise -0");
    same(Math.sumPrecise([-0, 0]), 0, "sumPrecise +0");
    same(Math.sumPrecise([1.7976931348623157e308, 1.7976931348623157e308, -1.7976931348623157e308]),
         1.7976931348623157e308, "sumPrecise intermediate overflow");
    same(Math.sumPrecise([1.7976931348623157e308, 1.7976931348623157e308]), Infinity, "sumPrecise overflow");
    same(Math.sumPrecise([Infinity, -Infinity]), NaN, "sumPrecise inf - inf");
    same(Math.sumPrecise([5e-324, 5e-324]), 1e-323, "sumPrecise subnormal");
    same(Math.sumPrecise([1, 1e-16, 1e-16]), 1.0000000000000002, "sumPrecise single rounding");
    throws(TypeError, function () { Math.sumPrecise([1, "2"]); }, "sumPrecise non-number");
    throws(TypeError, function () { Math.sumPrecise(); }, "sumPrecise undefined");
    "ok";
    "#);
}

#[test]
fn bigint_builtins_and_operators() {
    ok(r#"
    same(BigInt(10), 10n, "BigInt(Number)");
    same(BigInt("0x1f"), 31n, "BigInt(hex string)");
    same(BigInt("  -12  "), -12n, "BigInt(signed string)");
    same(BigInt(true), 1n, "BigInt(true)");
    throws(RangeError, function () { BigInt(1.5); }, "BigInt(1.5)");
    throws(SyntaxError, function () { BigInt("1.5"); }, "BigInt('1.5')");
    throws(SyntaxError, function () { BigInt("-0x1"); }, "BigInt('-0x1')");
    throws(TypeError, function () { BigInt(Symbol()); }, "BigInt(Symbol)");
    throws(TypeError, function () { new BigInt(1); }, "new BigInt");
    same(BigInt.asIntN(64, 2n ** 63n), -(2n ** 63n), "asIntN");
    same(BigInt.asUintN(64, -1n), 18446744073709551615n, "asUintN");
    same(BigInt.asIntN(0, 5n), 0n, "asIntN 0");
    throws(RangeError, function () { BigInt.asIntN(-1, 0n); }, "asIntN bits");
    same((255n).toString(16), "ff", "toString radix");
    same((-35n).toString(36), "-z", "toString negative");
    same(Object.prototype.toString.call(1n), "[object BigInt]", "tag");
    same(Object(1n).valueOf(), 1n, "valueOf wrapper");
    // Operators.
    same(7n / -2n, -3n, "division truncates");
    same(-7n % 2n, -1n, "remainder sign of dividend");
    same((-2n) ** 65n, -36893488147419103232n, "power");
    same(-5n >> 1n, -3n, "shift floors");
    same(1n << 100n >> 99n, 2n, "shift round trip");
    same(5n << -1n, 2n, "negative shift");
    same(-5n & 3n, 3n, "and");
    same(-5n | 2n, -5n, "or");
    same(-5n ^ -3n, 6n, "xor");
    same(~0n, -1n, "not");
    same(-(-0n), 0n, "negate zero");
    var x = 9007199254740993n; x++;
    same(x, 9007199254740994n, "increment");
    throws(RangeError, function () { 1n / 0n; }, "division by zero");
    throws(RangeError, function () { 1n % 0n; }, "remainder by zero");
    throws(RangeError, function () { 2n ** -1n; }, "negative exponent");
    throws(TypeError, function () { 1n >>> 0n; }, "unsigned shift");
    throws(TypeError, function () { 1n + 1; }, "mixed add");
    throws(TypeError, function () { +1n; }, "unary plus");
    same(1n + "", "1", "string concat");
    same(123456789012345678901234567890n * 987654321098765432109876543210n,
         121932631137021795226185032733622923332237463801111263526900n, "large product");
    // Mixed comparisons.
    same(1n < 1.5, true, "1n < 1.5");
    same(2n > 1.5, true, "2n > 1.5");
    same(9007199254740993n > 9007199254740992, true, "beyond 2^53");
    same(9007199254740992n == 9007199254740992, true, "== number");
    same(0n == -0, true, "0n == -0");
    same(1n == 1, true, "1n == 1");
    same(1n === 1, false, "1n === 1");
    same(1n < NaN, false, "NaN <");
    same(1n >= NaN, false, "NaN >=");
    same(2n ** 1100n > Number.MAX_VALUE, true, "huge > MAX_VALUE");
    same(2n ** 1100n < Infinity, true, "huge < Infinity");
    same(-(2n ** 1100n) > -Infinity, true, "huge negative > -Infinity");
    same(1n == "1", true, "== string");
    same(1n < "2", true, "< string");
    same(1n < "x", false, "< invalid string");
    same(10n > 9, true, "> number");
    "ok";
    "#);
}
