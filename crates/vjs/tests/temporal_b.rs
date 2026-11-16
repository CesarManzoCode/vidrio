//! Temporal with time zones and partial dates, through scripts: the
//! `disambiguation` and `offset` options in the gap and the fold of a
//! daylight saving year of `America/New_York` (a real system TZif),
//! days of 23 and 25 hours, `Duration.round`, `total` and `compare` with
//! a zoned `relativeTo`, and `PlainMonthDay`/`PlainYearMonth` at their
//! border days (February 29, month ends and the limits of the range).

use std::sync::Once;

use vjs::agent::Agent;
use vjs::ops;
use vjs::value::{PropertyKey, Value};
use vjs::{Throw, evaluate_script};

/// The local zone is read once per process, so `TZ` is set before any
/// test touches it.
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
const NY = "America/New_York";
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

// ----- Gaps and folds -----

#[test]
fn disambiguation_in_the_spring_gap() {
    // 2024-03-10T02:30 does not exist in New York: 02:00 EST jumps to
    // 03:00 EDT.
    ok(r#"
        const pdt = Temporal.PlainDateTime.from("2024-03-10T02:30");
        const at = (d) => pdt.toZonedDateTime(NY, { disambiguation: d });
        same(at("compatible").toString(), "2024-03-10T03:30:00-04:00[America/New_York]", "compatible");
        same(at("later").toString(), "2024-03-10T03:30:00-04:00[America/New_York]", "later");
        same(at("earlier").toString(), "2024-03-10T01:30:00-05:00[America/New_York]", "earlier");
        same(at("later").toInstant().toString(), "2024-03-10T07:30:00Z", "later instant");
        same(at("earlier").toInstant().toString(), "2024-03-10T06:30:00Z", "earlier instant");
        throws(RangeError, () => at("reject"), "reject in the gap");
        // The default is compatible.
        same(pdt.toZonedDateTime(NY).epochNanoseconds, at("later").epochNanoseconds, "default");
        // From a property bag, with the same options.
        const bag = { year: 2024, month: 3, day: 10, hour: 2, minute: 30, timeZone: NY };
        same(Temporal.ZonedDateTime.from(bag, { disambiguation: "earlier" }).hour, 1, "bag earlier");
        throws(RangeError, () => Temporal.ZonedDateTime.from(bag, { disambiguation: "reject" }), "bag reject");
        "ok"
    "#);
}

#[test]
fn disambiguation_in_the_autumn_fold() {
    // 2024-11-03T01:30 happens twice: first EDT, then EST.
    ok(r#"
        const pdt = Temporal.PlainDateTime.from("2024-11-03T01:30");
        const at = (d) => pdt.toZonedDateTime(NY, { disambiguation: d });
        same(at("compatible").offset, "-04:00", "compatible is the earlier");
        same(at("earlier").offset, "-04:00", "earlier");
        same(at("later").offset, "-05:00", "later");
        same(at("later").epochNanoseconds - at("earlier").epochNanoseconds, 3600000000000n, "an hour apart");
        throws(RangeError, () => at("reject"), "reject in the fold");
        "ok"
    "#);
}

#[test]
fn offset_option_in_gaps_and_folds() {
    ok(r#"
        // In the fold the offset picks the occurrence.
        const est = Temporal.ZonedDateTime.from("2024-11-03T01:30-05:00[America/New_York]");
        same(est.toInstant().toString(), "2024-11-03T06:30:00Z", "EST occurrence");
        const edt = Temporal.ZonedDateTime.from("2024-11-03T01:30-04:00[America/New_York]");
        same(edt.toInstant().toString(), "2024-11-03T05:30:00Z", "EDT occurrence");
        // An offset that does not exist at that wall-clock time.
        const s = "2024-03-10T02:30-05:00[America/New_York]";
        throws(RangeError, () => Temporal.ZonedDateTime.from(s), "reject is the default");
        throws(RangeError, () => Temporal.ZonedDateTime.from(s, { offset: "reject" }), "reject");
        same(Temporal.ZonedDateTime.from(s, { offset: "use" }).toString(),
             "2024-03-10T03:30:00-04:00[America/New_York]", "use keeps the instant");
        same(Temporal.ZonedDateTime.from(s, { offset: "prefer" }).toString(),
             "2024-03-10T03:30:00-04:00[America/New_York]", "prefer falls back to compatible");
        same(Temporal.ZonedDateTime.from(s, { offset: "ignore", disambiguation: "earlier" }).toString(),
             "2024-03-10T01:30:00-05:00[America/New_York]", "ignore uses the wall-clock time");
        // with() keeps the offset by default (prefer), so it stays in EST.
        same(est.with({ minute: 45 }).offset, "-05:00", "with in the fold");
        same(est.with({ minute: 45 }, { offset: "ignore" }).offset, "-04:00", "with ignoring the offset");
        // Z is exact.
        same(Temporal.ZonedDateTime.from("2024-03-10T07:30Z[America/New_York]").hour, 3, "Z");
        "ok"
    "#);
}

#[test]
fn days_of_23_and_25_hours() {
    ok(r#"
        const spring = Temporal.ZonedDateTime.from("2024-03-10T12:00[America/New_York]");
        const fall = Temporal.ZonedDateTime.from("2024-11-03T12:00[America/New_York]");
        same(spring.hoursInDay, 23, "spring day");
        same(fall.hoursInDay, 25, "autumn day");
        same(Temporal.ZonedDateTime.from("2024-07-01T12:00[America/New_York]").hoursInDay, 24, "summer day");
        // Adding a day keeps the wall-clock time; 24 hours does not.
        const before = Temporal.ZonedDateTime.from("2024-03-09T12:00[America/New_York]");
        same(before.add({ days: 1 }).toString(), "2024-03-10T12:00:00-04:00[America/New_York]", "P1D");
        same(before.add({ hours: 24 }).toString(), "2024-03-10T13:00:00-04:00[America/New_York]", "PT24H");
        same(before.until(spring).toString(), "PT23H", "until in hours");
        same(before.until(spring, { largestUnit: "days" }).toString(), "P1D", "until in days");
        // Rounding to the day uses the length of that day: noon of the
        // 23-hour day is 11 hours in (before half), 11:40 of the 25-hour
        // day is 12 h 40 min in (past half).
        same(Temporal.ZonedDateTime.from("2024-03-10T12:00[America/New_York]")
               .round({ smallestUnit: "day" }).toString(),
             "2024-03-10T00:00:00-05:00[America/New_York]", "noon of a 23-hour day");
        same(Temporal.ZonedDateTime.from("2024-11-03T11:40[America/New_York]")
               .round({ smallestUnit: "day" }).toString(),
             "2024-11-04T00:00:00-05:00[America/New_York]", "11:40 of a 25-hour day");
        // Transitions.
        const jan = Temporal.ZonedDateTime.from("2024-01-01T00:00[America/New_York]");
        const next = jan.getTimeZoneTransition("next");
        same(next.toString(), "2024-03-10T03:00:00-04:00[America/New_York]", "next transition");
        same(next.getTimeZoneTransition("previous").toString(),
             "2023-11-05T01:00:00-05:00[America/New_York]", "previous transition");
        same(Temporal.ZonedDateTime.from("2024-01-01T00:00[+05:30]").getTimeZoneTransition("next"),
             null, "offset zones have no transitions");
        throws(RangeError, () => jan.getTimeZoneTransition({}), "direction is required");
        // A day whose midnight is skipped starts at the transition.
        same(Temporal.PlainDate.from("2018-11-04").toZonedDateTime("America/Sao_Paulo").toString(),
             "2018-11-04T01:00:00-02:00[America/Sao_Paulo]", "start of a day without midnight");
        "ok"
    "#);
}

// ----- Duration with a zoned relativeTo -----

#[test]
fn duration_round_relative_to_a_zoned_date_time() {
    ok(r#"
        const relativeTo = Temporal.ZonedDateTime.from("2024-03-09T12:00[America/New_York]");
        const plain = Temporal.PlainDate.from("2024-03-09");
        // 24 hours from noon before the gap reach 13:00 of the next day.
        same(Temporal.Duration.from("PT24H").round({ largestUnit: "days", relativeTo }).toString(),
             "P1DT1H", "24 hours across the gap");
        same(Temporal.Duration.from("PT23H").round({ largestUnit: "days", relativeTo }).toString(),
             "P1D", "23 hours are the day of the gap");
        same(Temporal.Duration.from("PT24H").round({ largestUnit: "days", relativeTo: plain }).toString(),
             "P1D", "a plain relativeTo has 24-hour days");
        // 36 hours: one day and 13 of the next 24 hours, which rounds up.
        const d = Temporal.Duration.from("PT36H");
        same(d.round({ smallestUnit: "days", relativeTo }).toString(), "P2D", "round to days");
        same(d.round({ smallestUnit: "days", roundingMode: "trunc", relativeTo }).toString(), "P1D", "trunc");
        same(d.total({ unit: "days", relativeTo }), 37 / 24, "total days");
        same(d.total({ unit: "days", relativeTo: plain }), 1.5, "total days, plain");
        same(d.total({ unit: "hours", relativeTo }), 36, "total hours");
        // Days through the fold are 25 hours.
        const fall = Temporal.ZonedDateTime.from("2024-11-02T12:00[America/New_York]");
        same(Temporal.Duration.from({ days: 1 }).total({ unit: "hours", relativeTo: fall }), 25, "a 25-hour day");
        same(Temporal.Duration.from({ hours: 25 }).round({ largestUnit: "days", relativeTo: fall }).toString(),
             "P1D", "25 hours are one day");
        // compare adds both durations to relativeTo.
        same(Temporal.Duration.compare({ days: 1 }, { hours: 24 }, { relativeTo }), -1, "P1D < PT24H");
        same(Temporal.Duration.compare({ days: 1 }, { hours: 23 }, { relativeTo }), 0, "P1D = PT23H");
        same(Temporal.Duration.compare({ days: 1 }, { hours: 24 }), 0, "without relativeTo");
        // relativeTo as a string and as a property bag.
        same(Temporal.Duration.from("PT24H").round({
               largestUnit: "days", relativeTo: "2024-03-09T12:00-05:00[America/New_York]" }).toString(),
             "P1DT1H", "string relativeTo");
        same(Temporal.Duration.from("PT24H").round({
               largestUnit: "days",
               relativeTo: { year: 2024, month: 3, day: 9, hour: 12, timeZone: NY } }).toString(),
             "P1DT1H", "bag relativeTo");
        throws(RangeError, () => Temporal.Duration.from("PT1H").round({
               largestUnit: "days", relativeTo: "2024-03-09T12:00-04:00[America/New_York]" }),
             "a wrong offset in relativeTo");
        "ok"
    "#);
}

// ----- PlainMonthDay and PlainYearMonth -----

#[test]
fn plain_month_day_border_days() {
    ok(r#"
        const leap = new Temporal.PlainMonthDay(2, 29);
        same(leap.toString(), "02-29", "February 29 exists");
        same(leap.monthCode, "M02", "monthCode");
        same(leap.day, 29, "day");
        same(leap.toString({ calendarName: "always" }), "1972-02-29[u-ca=iso8601]", "reference year");
        same(leap.toPlainDate({ year: 2024 }).toString(), "2024-02-29", "in a leap year");
        same(leap.toPlainDate({ year: 2023 }).toString(), "2023-02-28", "constrained in a common year");
        throws(RangeError, () => new Temporal.PlainMonthDay(2, 30), "February 30");
        throws(RangeError, () => new Temporal.PlainMonthDay(4, 31), "April 31");
        same(Temporal.PlainMonthDay.from({ monthCode: "M02", day: 30 }).toString(), "02-29", "constrain");
        throws(RangeError, () => Temporal.PlainMonthDay.from({ monthCode: "M02", day: 30 }, { overflow: "reject" }),
               "reject");
        // A year only regulates the day.
        same(Temporal.PlainMonthDay.from({ year: 2023, month: 2, day: 29 }).toString(), "02-28", "year 2023");
        same(Temporal.PlainMonthDay.from({ year: 2024, month: 2, day: 29 }).toString(), "02-29", "year 2024");
        same(Temporal.PlainMonthDay.from("--12-31").day, 31, "--MM-DD");
        same(Temporal.PlainMonthDay.from("2023-12-31T10:00").toString(), "12-31", "from a date-time");
        same(leap.with({ day: 1 }).toString(), "02-01", "with");
        same(leap.equals("02-29"), true, "equals");
        same(Temporal.PlainDate.from("2024-02-29").toPlainMonthDay().toString(), "02-29", "toPlainMonthDay");
        throws(TypeError, () => Temporal.PlainMonthDay.from({ month: 2 }), "day is required");
        "ok"
    "#);
}

#[test]
fn plain_year_month_border_days() {
    ok(r#"
        same(new Temporal.PlainYearMonth(-271821, 4).toString(), "-271821-04", "lower limit");
        throws(RangeError, () => new Temporal.PlainYearMonth(-271821, 3), "below the lower limit");
        same(new Temporal.PlainYearMonth(275760, 9).toString(), "+275760-09", "upper limit");
        throws(RangeError, () => new Temporal.PlainYearMonth(275760, 10), "beyond the upper limit");
        throws(RangeError, () => new Temporal.PlainYearMonth(275760, 9).add({ months: 1 }), "add past the limit");
        const feb = new Temporal.PlainYearMonth(2024, 2);
        same(feb.daysInMonth, 29, "leap February");
        same(new Temporal.PlainYearMonth(2023, 2).daysInMonth, 28, "common February");
        same(feb.toPlainDate({ day: 31 }).toString(), "2024-02-29", "toPlainDate constrains");
        same(feb.toString({ calendarName: "always" }), "2024-02-01[u-ca=iso8601]", "reference day");
        // Days are added from the first day, and subtracted from the last.
        const jan = Temporal.PlainYearMonth.from("2020-01");
        same(jan.add({ days: 30 }).toString(), "2020-01", "30 days from January 1");
        same(jan.add({ days: 31 }).toString(), "2020-02", "31 days from January 1");
        same(jan.subtract({ days: 30 }).toString(), "2020-01", "30 days before January 31");
        same(jan.subtract({ days: 31 }).toString(), "2019-12", "31 days before January 31");
        same(Temporal.PlainYearMonth.from("2019-01").until("2020-03").toString(), "P1Y2M", "until");
        same(Temporal.PlainYearMonth.from("2019-01").until("2020-03", { largestUnit: "months" }).toString(),
             "P14M", "until in months");
        same(Temporal.PlainYearMonth.from("2019-01").until("2019-12",
               { smallestUnit: "years", roundingMode: "halfExpand" }).toString(), "P1Y", "rounded until");
        same(Temporal.PlainYearMonth.from("2020-03").since("2019-01").toString(), "P1Y2M", "since");
        throws(RangeError, () => jan.until("2020-03", { largestUnit: "weeks" }), "weeks are not allowed");
        same(Temporal.PlainYearMonth.compare("2020-01", "2019-12"), 1, "compare");
        same(Temporal.PlainDate.from("2024-02-29").toPlainYearMonth().toString(), "2024-02", "toPlainYearMonth");
        throws(RangeError, () => Temporal.PlainYearMonth.from({ year: 2024, monthCode: "M13" }),
               "M13 is not an ISO month");
        "ok"
    "#);
}

#[test]
fn now_and_date_to_temporal_instant() {
    ok(r#"
        same(Temporal.Now.timeZoneId(), NY, "system time zone");
        same(Temporal.Now.zonedDateTimeISO().timeZoneId, NY, "zonedDateTimeISO");
        same(Temporal.Now.zonedDateTimeISO("+01:00").timeZoneId, "+01:00", "with a zone");
        same(typeof Temporal.Now.instant().epochNanoseconds, "bigint", "instant");
        same(Object.prototype.toString.call(Temporal.Now), "[object Temporal.Now]", "toStringTag");
        same(new Date(0).toTemporalInstant().toString(), "1970-01-01T00:00:00Z", "toTemporalInstant");
        same(new Temporal.ZonedDateTime(0n, NY).toString(), "1969-12-31T19:00:00-05:00[America/New_York]",
             "epoch in New York");
        same(new Temporal.ZonedDateTime(0n, "america/new_york").timeZoneId, NY, "case-normalized");
        throws(RangeError, () => new Temporal.ZonedDateTime(0n, "+01:00:30"), "sub-minute offset zone");
        "ok"
    "#);
}
