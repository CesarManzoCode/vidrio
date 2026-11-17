//! Temporal ISO strings, through scripts: the Unicode minus sign U+2212 is
//! not a sign anywhere (dates, offsets, offset time zones, durations), and
//! the calendar annotation of a string read as an `Instant` or a
//! `PlainTime` is ignored even when the calendar is unknown.

use vjs::agent::Agent;
use vjs::ops;
use vjs::value::{PropertyKey, Value};
use vjs::{Throw, evaluate_script};

const PRELUDE: &str = r#"
function fail(msg) { throw new Error(msg); }
function same(actual, expected, what) {
  if (!Object.is(actual, expected)) {
    fail(what + ": expected " + expected + ", got " + actual);
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

/// Runs `src` (which must evaluate to "ok") in a fresh realm.
fn ok(src: &str) {
    let mut agent = Agent::new();
    agent.initialize_host_defined_realm();
    let source = format!("{PRELUDE}\n{src}");
    let result = match evaluate_script(&mut agent, &source) {
        Ok(v) => show(&mut agent, v),
        Err(e) => show_throw(&mut agent, e),
    };
    assert_eq!(result, "ok", "script:\n{src}");
}

#[test]
fn unicode_minus_sign_is_not_a_sign() {
    ok(r#"
        const offset = "2001-02-03T04:05:06−01:00";
        const year = "−000042-02-03T04:05";
        throws(RangeError, () => Temporal.Instant.from(offset), "instant offset");
        throws(RangeError, () => Temporal.PlainDate.from(year), "date year");
        throws(RangeError, () => Temporal.PlainDateTime.from(offset), "date-time offset");
        throws(RangeError, () => Temporal.PlainTime.from(offset), "time offset");
        throws(RangeError, () => Temporal.PlainYearMonth.from(year), "year-month year");
        throws(RangeError, () => Temporal.PlainMonthDay.from(offset), "month-day offset");
        throws(RangeError, () => Temporal.ZonedDateTime.from("2001-02-03T04:05-01:00[−01:00]"),
               "offset time zone");
        throws(RangeError, () => Temporal.Duration.from("−P1D"), "duration sign");
        same(Temporal.PlainDate.from("-000042-02-03").year, -42, "ASCII minus still works");
        "ok"
    "#);
}

#[test]
fn instant_and_time_strings_ignore_the_calendar() {
    ok(r#"
        same(Temporal.Instant.from("1970-01-01T00:00Z[u-ca=madeup]").epochNanoseconds, 0n,
             "instant unknown calendar");
        same(Temporal.Instant.compare("1970-01-01T00:00Z[!u-ca=gregory]", new Temporal.Instant(0n)), 0,
             "instant critical calendar");
        same(Temporal.PlainTime.from("01:02:03[u-ca=madeup]").toString(), "01:02:03", "time");
        same(Temporal.PlainTime.from("1970-01-01T01:02[!u-ca=madeup]").toString(), "01:02:00",
             "time with a date and a critical calendar");
        const d = new Temporal.PlainDate(2000, 1, 2);
        same(d.toPlainDateTime("05:06[u-ca=madeup]").toString(), "2000-01-02T05:06:00",
             "toPlainDateTime");
        throws(RangeError, () => Temporal.PlainDate.from("2000-01-02[u-ca=madeup]"),
               "dates still check the calendar");
        "ok"
    "#);
}
