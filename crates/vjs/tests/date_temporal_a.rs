//! Date and Temporal through the TZif reader and scripts: the daylight
//! saving transitions of a real system TZif (`America/New_York`), local
//! time across a gap and a fold, `Date.parse` of the Date Time String
//! Format and of the `toString`/`toUTCString` forms, and the arithmetic
//! and rounding of `Temporal.Duration` and `Temporal.PlainDateTime` at
//! month and year borders.

use std::sync::Once;

use vjs::agent::Agent;
use vjs::ops;
use vjs::tz;
use vjs::value::{PropertyKey, Value};
use vjs::{Throw, evaluate_script};

/// 2024-03-10T07:00:00Z: 02:00 EST becomes 03:00 EDT.
const NY_SPRING_2024: i64 = 1_710_054_000;
/// 2024-11-03T06:00:00Z: 02:00 EDT becomes 01:00 EST.
const NY_FALL_2024: i64 = 1_730_613_600;

/// Every test reads the local zone through `TZ`; the local zone is read
/// once per process, so it is set before any test touches it.
fn local_zone_new_york() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        // SAFETY: every test of this file goes through this `Once` before
        // reading the environment, so no other thread reads it meanwhile.
        unsafe { std::env::set_var("TZ", "America/New_York") };
    });
}

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

/// Runs `src` (which must evaluate to "ok") in a fresh realm with the
/// local zone `America/New_York`.
fn ok(src: &str) {
    local_zone_new_york();
    let mut agent = Agent::new();
    agent.initialize_host_defined_realm();
    let source = format!("{PRELUDE}\n{src}");
    let result = match evaluate_script(&mut agent, &source) {
        Ok(v) => show(&mut agent, v),
        Err(e) => show_throw(&mut agent, e),
    };
    assert_eq!(result, "ok", "script:\n{src}");
}

// ----- TZif reader -----

#[test]
fn tzif_new_york_transitions_2024() {
    local_zone_new_york();
    let ny = tz::zone("America/New_York").expect("America/New_York in /usr/share/zoneinfo");
    // Offsets on each side of both transitions.
    assert_eq!(ny.offset_at(NY_SPRING_2024 - 1), -5 * 3600);
    assert_eq!(ny.offset_at(NY_SPRING_2024), -4 * 3600);
    assert_eq!(ny.offset_at(NY_FALL_2024 - 1), -4 * 3600);
    assert_eq!(ny.offset_at(NY_FALL_2024), -5 * 3600);
    assert_eq!(ny.local_type_at(NY_SPRING_2024 - 1).abbreviation, "EST");
    assert_eq!(ny.local_type_at(NY_SPRING_2024).abbreviation, "EDT");
    // Transition search from inside the year.
    let jan = NY_SPRING_2024 - 60 * 86_400;
    assert_eq!(ny.next_transition(jan), Some(NY_SPRING_2024));
    assert_eq!(ny.next_transition(NY_SPRING_2024), Some(NY_FALL_2024));
    assert_eq!(ny.previous_transition(NY_FALL_2024), Some(NY_SPRING_2024));
    // 2024-03-10T02:30 local is in the gap; 2024-11-03T01:30 local is in
    // the fold (EDT first, then EST).
    let gap_local = NY_SPRING_2024 - 5 * 3600 + 30 * 60;
    assert!(ny.possible_instants(gap_local).is_empty());
    let fold_local = NY_FALL_2024 - 4 * 3600 - 30 * 60;
    assert_eq!(
        ny.possible_instants(fold_local),
        vec![NY_FALL_2024 - 30 * 60, NY_FALL_2024 + 30 * 60]
    );
    // Far beyond the explicit transitions the POSIX footer still applies:
    // 2100-07-01T00:00:00Z is EDT, 2100-01-01T00:00:00Z is EST.
    assert_eq!(ny.offset_at(4_118_083_200), -4 * 3600);
    assert_eq!(ny.offset_at(4_102_444_800), -5 * 3600);
}

#[test]
fn tzif_identifiers_case_links_and_local() {
    local_zone_new_york();
    let found = tz::find_zone("aMeRiCa/nEw_YoRk").expect("case-insensitive lookup");
    assert_eq!(found.identifier, "America/New_York");
    assert_eq!(found.primary, "America/New_York");
    let link = tz::find_zone("us/eastern").expect("backward link US/Eastern");
    assert_eq!(link.identifier, "US/Eastern");
    assert_eq!(link.primary, "America/New_York");
    for utc in ["UTC", "Etc/UTC", "Etc/GMT", "GMT"] {
        assert_eq!(tz::find_zone(utc).expect(utc).primary, "UTC", "{utc}");
    }
    assert!(tz::find_zone("Mars/Olympus_Mons").is_none());
    assert!(tz::find_zone("posixrules").is_none());
    assert_eq!(tz::local().id, "America/New_York");
}

// ----- Date local time -----

#[test]
fn date_local_time_across_transitions() {
    ok(r#"
      same(new Date(2024, 0, 15).getTimezoneOffset(), 300, "winter offset");
      same(new Date(2024, 6, 15).getTimezoneOffset(), 240, "summer offset");
      same(new Date(Date.UTC(2024, 2, 10, 6, 59, 59)).getHours(), 1, "before spring");
      same(new Date(Date.UTC(2024, 2, 10, 7)).getHours(), 3, "after spring");
      // In the gap the offset before the transition applies.
      var gap = new Date(2024, 2, 10, 2, 30);
      same(gap.getTime(), Date.UTC(2024, 2, 10, 7, 30), "gap time value");
      same(gap.getHours(), 3, "gap hours");
      same(gap.getMinutes(), 30, "gap minutes");
      // In the fold the earlier instant (EDT) wins.
      var fold = new Date(2024, 10, 3, 1, 30);
      same(fold.getTime(), Date.UTC(2024, 10, 3, 5, 30), "fold time value");
      same(fold.getTimezoneOffset(), 240, "fold offset");
      same(new Date(fold.getTime() + 3600000).getHours(), 1, "second 01:30");
      same(new Date(fold.getTime() + 3600000).getTimezoneOffset(), 300, "second offset");
      // Setters recompute through UTC(t).
      var d = new Date(2024, 2, 9, 2, 30);
      d.setDate(10);
      same(d.getHours(), 3, "setDate into the gap");
      d.setMonth(0);
      same(d.getTimezoneOffset(), 300, "setMonth back to winter");
      same(d.setHours(25), new Date(2024, 0, 11, 1, 30).getTime(), "setHours overflow");
      same(new Date(2024, 2, 10, 3).toString(),
           "Sun Mar 10 2024 03:00:00 GMT-0400 (EDT)", "toString");
      same(new Date(2024, 0, 1).toTimeString(), "00:00:00 GMT-0500 (EST)", "toTimeString");
      same(new Date(2024, 0, 1).toDateString(), "Mon Jan 01 2024", "toDateString");
      same(new Date(NaN).toString(), "Invalid Date", "invalid");
      same(new Date(2024, 0, 1)[Symbol.toPrimitive]("number"),
           Date.UTC(2024, 0, 1, 5), "toPrimitive number");
      same(typeof (new Date(0) + 1), "string", "default hint is string");
      same(Object.prototype.toString.call(new Date(0)), "[object Date]", "class");
      "ok"
    "#);
}

// ----- Date.parse -----

#[test]
fn date_parse_iso_format() {
    ok(r#"
      same(Date.parse("2024-03-10T07:00:00Z"), 1710054000000, "UTC date-time");
      same(Date.parse("2024-03-10T07:00:00.123Z"), 1710054000123, "milliseconds");
      same(Date.parse("2024-03-10T09:00:00+02:00"), 1710054000000, "offset");
      same(Date.parse("2024-03-10"), Date.UTC(2024, 2, 10), "date-only is UTC");
      same(Date.parse("2024-03"), Date.UTC(2024, 2, 1), "year-month");
      same(Date.parse("2024"), Date.UTC(2024, 0, 1), "year");
      same(Date.parse("2024-03-10T02:30"), new Date(2024, 2, 10, 2, 30).getTime(),
           "date-time without offset is local (gap)");
      same(Date.parse("2024-11-03T01:30:00"), Date.UTC(2024, 10, 3, 5, 30),
           "date-time without offset is local (fold)");
      same(Date.parse("2024-01-01T24:00:00Z"), Date.UTC(2024, 0, 2), "24:00");
      same(Date.parse("+275760-09-13T00:00:00.000Z"), 8.64e15, "maximum");
      same(Date.parse("-271821-04-20T00:00:00.000Z"), -8.64e15, "minimum");
      same(Date.parse("+275760-09-13T00:00:00.001Z"), NaN, "beyond maximum");
      same(Date.parse("-000000-01-01T00:00:00Z"), NaN, "minus zero year");
      same(Date.parse("2024-02-30"), NaN, "day out of month");
      same(Date.parse("2023-02-29"), NaN, "not a leap year");
      same(Date.parse("2024-13-01"), NaN, "month 13");
      same(Date.parse("2024-01-01T24:00:01Z"), NaN, "past 24:00");
      same(Date.parse("2024-01-01T12:60Z"), NaN, "minute 60");
      same(Date.parse("2024-01-01T12Z"), NaN, "hour without minutes");
      same(Date.parse("2024-01-01T12:00+0200"), NaN, "offset without colon");
      same(Date.parse(""), NaN, "empty");
      "ok"
    "#);
}

#[test]
fn date_parse_to_string_forms() {
    ok(r#"
      var d = new Date(Date.UTC(2024, 10, 3, 5, 30, 45, 678));
      var s = Math.floor(d.getTime() / 1000) * 1000;
      same(Date.parse(d.toString()), s, "toString round trip");
      same(Date.parse(d.toUTCString()), s, "toUTCString round trip");
      same(Date.parse(d.toISOString()), d.getTime(), "toISOString round trip");
      same(d.toUTCString(), "Sun, 03 Nov 2024 05:30:45 GMT", "toUTCString");
      same(d.toISOString(), "2024-11-03T05:30:45.678Z", "toISOString");
      same(Date.parse("Sun Mar 10 2024 03:00:00 GMT-0400 (EDT)"), 1710054000000,
           "toString form");
      same(Date.parse("Sun Mar 10 2024 03:00:00 GMT-0400"), 1710054000000,
           "toString form without name");
      same(Date.parse("Sun, 10 Mar 2024 07:00:00 GMT"), 1710054000000, "toUTCString form");
      same(Date.parse("Sun Mar 10 2024"), new Date(2024, 2, 10).getTime(),
           "toDateString form is local");
      // After the fold the second 01:30 still parses through its offset.
      var late = new Date(Date.UTC(2024, 10, 3, 6, 30));
      same(Date.parse(late.toString()), late.getTime(), "second 01:30 round trip");
      same(Date.parse("Mon, 32 Jan 2024 00:00:00 GMT"), NaN, "day 32");
      same(Date.parse("Sun Foo 10 2024"), NaN, "no month");
      "ok"
    "#);
}

// ----- Temporal.PlainDateTime -----

#[test]
fn plain_date_time_arithmetic_at_month_and_year_borders() {
    ok(r#"
      var P = Temporal.PlainDateTime;
      same(P.from("2024-01-31T12:00").add({ months: 1 }).toString(),
           "2024-02-29T12:00:00", "Jan 31 + 1 month constrains to Feb 29");
      same(P.from("2023-01-31T12:00").add({ months: 1 }).toString(),
           "2023-02-28T12:00:00", "non-leap February");
      throws(RangeError, function () {
        P.from("2024-01-31T12:00").add({ months: 1 }, { overflow: "reject" });
      }, "reject overflow");
      same(new P(2023, 12, 31, 23, 59, 59, 999, 999, 999).add({ nanoseconds: 1 }).toString(),
           "2024-01-01T00:00:00", "nanosecond into the new year");
      same(P.from("2024-03-01T00:00").subtract({ days: 1 }).toString(),
           "2024-02-29T00:00:00", "back over the leap day");
      same(P.from("2024-02-28T23:00").add({ hours: 25 }).toString(),
           "2024-03-01T00:00:00", "hours carry into days");
      same(P.from("2024-02-29T00:00").add({ years: 1 }).toString(),
           "2025-02-28T00:00:00", "leap day + 1 year");
      same(P.from("2024-12-31T00:00").add({ months: 2, days: 1 }).toString(),
           "2025-03-01T00:00:00", "months then days");
      same(P.from("2024-01-31T00:00").until("2024-03-01T00:00", { largestUnit: "months" })
             .toString(), "P1M1D", "until in months");
      same(P.from("2024-03-01T00:00").since("2024-01-31T00:00", { largestUnit: "months" })
             .toString(), "P1M1D", "since in months");
      same(P.from("2023-12-31T23:00").until("2024-01-01T01:30").toString(),
           "PT2H30M", "until across the year");
      same(P.from("2024-01-01T00:00").until("2024-12-31T12:00", {
             largestUnit: "years", smallestUnit: "months", roundingMode: "halfExpand"
           }).toString(), "P1Y", "rounding bubbles months into a year");
      same(P.from("2024-01-01T00:00").until("2024-12-31T12:00", {
             largestUnit: "years", smallestUnit: "months"
           }).toString(), "P11M", "trunc keeps 11 months");
      same(P.from("2024-02-28T12:00").with({ day: 31 }).toString(),
           "2024-02-29T12:00:00", "with constrains the day");
      same(P.from("2024-02-28T12:00").withPlainTime("23:59").toString(),
           "2024-02-28T23:59:00", "withPlainTime");
      same(P.compare("2024-01-01T00:00", "2023-12-31T23:59:59.999999999"), 1, "compare");
      same(P.from("2024-01-01").equals("2024-01-01T00:00"), true, "equals");
      same(P.from("2024-02-29T10:20").toPlainDate().toString(), "2024-02-29", "toPlainDate");
      same(P.from("2024-02-29T10:20").toPlainTime().toString(), "10:20:00", "toPlainTime");
      throws(RangeError, function () { new P(2023, 2, 29); }, "invalid date");
      throws(RangeError, function () { new P(2024, 1, 1, 24); }, "invalid hour");
      throws(TypeError, function () { P(2024, 1, 1); }, "call without new");
      "ok"
    "#);
}

#[test]
fn plain_date_time_rounding_and_to_string() {
    ok(r#"
      var P = Temporal.PlainDateTime;
      same(P.from("2023-12-31T23:59:59.5").round({ smallestUnit: "second" }).toString(),
           "2024-01-01T00:00:00", "halfExpand second into the new year");
      same(P.from("2024-02-29T12:00").round({ smallestUnit: "day" }).toString(),
           "2024-03-01T00:00:00", "half a day rounds up to March");
      same(P.from("2024-02-29T11:59:59").round("day").toString(),
           "2024-02-29T00:00:00", "string option");
      same(P.from("2024-01-01T00:00:30").round({ smallestUnit: "minute", roundingMode: "halfEven" })
             .toString(), "2024-01-01T00:00:00", "halfEven");
      same(P.from("2024-01-01T00:01:30").round({ smallestUnit: "minute", roundingMode: "halfEven" })
             .toString(), "2024-01-01T00:02:00", "halfEven to even");
      same(P.from("2024-01-31T23:50").round({ smallestUnit: "minute", roundingIncrement: 30,
             roundingMode: "ceil" }).toString(), "2024-02-01T00:00:00", "ceil with increment");
      same(P.from("2024-01-31T23:50").round({ smallestUnit: "hour", roundingMode: "floor" })
             .toString(), "2024-01-31T23:00:00", "floor");
      throws(RangeError, function () {
        P.from("2024-01-01").round({ smallestUnit: "minute", roundingIncrement: 7 });
      }, "increment must divide 60");
      throws(RangeError, function () {
        P.from("2024-01-01").round({ smallestUnit: "day", roundingIncrement: 2 });
      }, "day increment is 1");
      throws(RangeError, function () { P.from("2024-01-01").round({}); }, "smallestUnit required");
      var t = P.from("2024-12-31T23:59:59.987654321");
      same(t.toString(), "2024-12-31T23:59:59.987654321", "auto");
      same(t.toString({ fractionalSecondDigits: 2 }), "2024-12-31T23:59:59.98", "digits trunc");
      same(t.toString({ smallestUnit: "second", roundingMode: "halfExpand" }),
           "2025-01-01T00:00:00", "round into the new year");
      same(t.toString({ smallestUnit: "minute" }), "2024-12-31T23:59", "minute");
      same(P.from("2024-01-01").toString({ calendarName: "always" }),
           "2024-01-01T00:00:00[u-ca=iso8601]", "calendarName always");
      same(P.from("2024-01-01").toString({ calendarName: "critical" }),
           "2024-01-01T00:00:00[!u-ca=iso8601]", "calendarName critical");
      throws(RangeError, function () { t.toString({ smallestUnit: "hour" }); }, "hour");
      same(P.from("2024-01-01T00:00:00.100").toJSON(), "2024-01-01T00:00:00.1", "toJSON");
      throws(TypeError, function () { t.valueOf(); }, "valueOf");
      "ok"
    "#);
}

// ----- Temporal.Duration -----

#[test]
fn duration_arithmetic_and_balancing_at_borders() {
    ok(r#"
      var D = Temporal.Duration;
      same(D.from("PT23H").add("PT1H").toString(), "PT24H", "no balancing past hours");
      same(D.from({ hours: 25 }).round({ largestUnit: "days" }).toString(), "P1DT1H",
           "24-hour days without relativeTo");
      throws(RangeError, function () { D.from({ months: 1 }).add({ days: 1 }); },
             "calendar units need relativeTo");
      same(D.from({ months: 1 }).total({ unit: "days", relativeTo: "2024-01-31" }), 29,
           "one month from Jan 31 in a leap year");
      same(D.from({ months: 1 }).total({ unit: "days", relativeTo: "2023-01-31" }), 28,
           "one month from Jan 31");
      same(D.from({ days: 31 }).round({ largestUnit: "months", relativeTo: "2024-01-31" })
             .toString(), "P1M2D", "31 days from Jan 31");
      same(D.from({ days: 366 }).round({ largestUnit: "years", relativeTo: "2024-01-01" })
             .toString(), "P1Y", "leap year");
      same(D.from({ days: 366 }).round({ largestUnit: "years", relativeTo: "2023-01-01" })
             .toString(), "P1Y1D", "common year");
      same(D.from({ months: 13 }).round({ largestUnit: "years", relativeTo: "2024-12-15" })
             .toString(), "P1Y1M", "months into a year");
      var months = D.from({ days: 45 }).total({ unit: "months", relativeTo: "2024-02-01" });
      if (Math.abs(months - 47 / 31) > 1e-12) fail("fractional months: " + months);
      same(D.compare({ months: 1 }, { days: 30 }, { relativeTo: "2024-02-01" }), -1,
           "February is shorter");
      same(D.compare({ months: 1 }, { days: 30 }, { relativeTo: "2024-01-01" }), 1,
           "January is longer");
      throws(RangeError, function () { D.compare({ months: 1 }, { days: 30 }); },
             "compare with months needs relativeTo");
      throws(RangeError, function () { D.from({ days: 1, hours: -1 }); }, "mixed signs");
      same(D.from("-P1Y2M").negated().toString(), "P1Y2M", "negated");
      same(D.from("-P1Y2M").abs().toString(), "P1Y2M", "abs");
      same(D.from("P1Y").with({ months: 2 }).toString(), "P1Y2M", "with");
      "ok"
    "#);
}

#[test]
fn duration_rounding_modes() {
    ok(r#"
      var D = Temporal.Duration;
      function r(s, mode) {
        return D.from(s).round({ smallestUnit: "hours", roundingMode: mode }).toString();
      }
      same(r("PT1H30M", "halfEven"), "PT2H", "halfEven 1.5");
      same(r("PT2H30M", "halfEven"), "PT2H", "halfEven 2.5");
      same(r("PT2H30M", "halfExpand"), "PT3H", "halfExpand");
      same(r("PT2H30M", "halfTrunc"), "PT2H", "halfTrunc");
      same(r("PT2H30M", "trunc"), "PT2H", "trunc");
      same(r("PT2H1M", "expand"), "PT3H", "expand");
      same(r("-PT2H30M", "ceil"), "-PT2H", "ceil negative");
      same(r("-PT2H30M", "floor"), "-PT3H", "floor negative");
      same(r("-PT2H30M", "halfCeil"), "-PT2H", "halfCeil negative");
      same(r("-PT2H30M", "halfFloor"), "-PT3H", "halfFloor negative");
      same(D.from("PT59M59.5S").round({ smallestUnit: "seconds", largestUnit: "hours" })
             .toString(), "PT1H", "rounding balances up to hours");
      same(D.from("P1M15D").round({ smallestUnit: "months", relativeTo: "2024-01-01" })
             .toString(), "P2M", "15 of 29 February days rounds up");
      same(D.from("P1M15D").round({ smallestUnit: "months", relativeTo: "2024-02-01" })
             .toString(), "P1M", "15 of 31 March days rounds down");
      same(D.from({ seconds: 1, milliseconds: 500 }).toString({ fractionalSecondDigits: 0 }),
           "PT1S", "toString truncates");
      same(D.from({ seconds: 1, milliseconds: 500 })
             .toString({ smallestUnit: "seconds", roundingMode: "halfExpand" }),
           "PT2S", "toString rounds");
      same(D.from({ milliseconds: 1 }).toString({ fractionalSecondDigits: 6 }),
           "PT0.001000S", "fixed digits");
      throws(RangeError, function () {
        D.from("PT1H").round({ smallestUnit: "minutes", roundingIncrement: 7 });
      }, "increment must divide 60");
      "ok"
    "#);
}
