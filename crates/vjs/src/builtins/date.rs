//! `Date` (ECMA-262 §21.4): time value arithmetic, local time through the
//! TZif reader (`crate::tz`), `Date.parse` of the Date Time String Format
//! and of the formats `toString` and `toUTCString` produce, the getters
//! and setters, the string forms and `@@toPrimitive`, plus
//! `toTemporalInstant` (Temporal proposal). `getYear`, `setYear` and
//! `toGMTString` (Annex B) are not here.

use crate::agent::Agent;
use crate::builtins::{
    constructor, intrinsic, method, plain_object, set_intrinsic, str_value, value,
};
use crate::completion::JsResult;
use crate::function::{NativeCall, create_builtin_function};
use crate::gc::{Object, Realm};
use crate::object::ObjectKind;
use crate::ops::{self, PreferredType};
use crate::realm::Intrinsic;
use crate::temporal::{epoch_days_from_iso_date, iso_date_from_epoch_days, iso_days_in_month};
use crate::tz;
use crate::value::{PropertyKey, Value};

const MS_PER_DAY: f64 = 86_400_000.0;
const MS_PER_HOUR: f64 = 3_600_000.0;
const MS_PER_MINUTE: f64 = 60_000.0;
const MS_PER_SECOND: f64 = 1000.0;

const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

pub(crate) fn install(agent: &mut Agent, realm: Realm) {
    let object_proto = intrinsic(agent, realm, Intrinsic::ObjectPrototype);
    let proto = plain_object(agent, object_proto);
    set_intrinsic(agent, realm, Intrinsic::DatePrototype, proto);
    let ctor = constructor(agent, realm, "Date", date_constructor, 7, proto, None);
    set_intrinsic(agent, realm, Intrinsic::Date, ctor);
    method(agent, realm, ctor, "now", now, 0);
    method(agent, realm, ctor, "parse", parse, 1);
    method(agent, realm, ctor, "UTC", utc, 7);

    let methods: [(&str, crate::function::NativeFn, u32); 44] = [
        ("getDate", get_date, 0),
        ("getDay", get_day, 0),
        ("getFullYear", get_full_year, 0),
        ("getHours", get_hours, 0),
        ("getMilliseconds", get_milliseconds, 0),
        ("getMinutes", get_minutes, 0),
        ("getMonth", get_month, 0),
        ("getSeconds", get_seconds, 0),
        ("getTime", get_time, 0),
        ("getTimezoneOffset", get_timezone_offset, 0),
        ("getUTCDate", get_utc_date, 0),
        ("getUTCDay", get_utc_day, 0),
        ("getUTCFullYear", get_utc_full_year, 0),
        ("getUTCHours", get_utc_hours, 0),
        ("getUTCMilliseconds", get_utc_milliseconds, 0),
        ("getUTCMinutes", get_utc_minutes, 0),
        ("getUTCMonth", get_utc_month, 0),
        ("getUTCSeconds", get_utc_seconds, 0),
        ("setDate", set_date, 1),
        ("setFullYear", set_full_year, 3),
        ("setHours", set_hours, 4),
        ("setMilliseconds", set_milliseconds, 1),
        ("setMinutes", set_minutes, 3),
        ("setMonth", set_month, 2),
        ("setSeconds", set_seconds, 2),
        ("setTime", set_time, 1),
        ("setUTCDate", set_utc_date, 1),
        ("setUTCFullYear", set_utc_full_year, 3),
        ("setUTCHours", set_utc_hours, 4),
        ("setUTCMilliseconds", set_utc_milliseconds, 1),
        ("setUTCMinutes", set_utc_minutes, 3),
        ("setUTCMonth", set_utc_month, 2),
        ("setUTCSeconds", set_utc_seconds, 2),
        ("toDateString", to_date_string, 0),
        ("toISOString", to_iso_string, 0),
        ("toJSON", to_json, 1),
        ("toLocaleDateString", to_date_string, 0),
        ("toLocaleString", to_string, 0),
        ("toLocaleTimeString", to_time_string, 0),
        ("toString", to_string, 0),
        ("toTemporalInstant", to_temporal_instant, 0),
        ("toTimeString", to_time_string, 0),
        ("toUTCString", to_utc_string, 0),
        ("valueOf", get_time, 0),
    ];
    for (name, f, length) in methods {
        method(agent, realm, proto, name, f, length);
    }
    // Date.prototype[@@toPrimitive] is { !writable, !enumerable,
    // configurable }.
    let key = PropertyKey::Symbol(agent.symbols.to_primitive);
    let f = create_builtin_function(agent, to_primitive, 1, key, Some(realm), None, None);
    value(agent, proto, key, Value::Object(f), false, false, true);
}

// ----- Time value arithmetic (§21.4.1) -----

fn day(t: f64) -> f64 {
    (t / MS_PER_DAY).floor()
}

fn time_within_day(t: f64) -> f64 {
    t.rem_euclid(MS_PER_DAY)
}

/// Year, month (0-based) and date of a finite time value.
fn ymd(t: f64) -> (f64, f64, f64) {
    let (y, m, d) = iso_date_from_epoch_days(day(t) as i64);
    (y as f64, (m - 1) as f64, d as f64)
}

fn week_day(t: f64) -> f64 {
    (day(t) + 4.0).rem_euclid(7.0)
}

fn hour_from_time(t: f64) -> f64 {
    (t / MS_PER_HOUR).floor().rem_euclid(24.0)
}

fn min_from_time(t: f64) -> f64 {
    (t / MS_PER_MINUTE).floor().rem_euclid(60.0)
}

fn sec_from_time(t: f64) -> f64 {
    (t / MS_PER_SECOND).floor().rem_euclid(60.0)
}

fn ms_from_time(t: f64) -> f64 {
    t.rem_euclid(MS_PER_SECOND)
}

/// MakeTime(hour, min, sec, ms).
fn make_time(h: f64, m: f64, s: f64, ms: f64) -> f64 {
    if !(h.is_finite() && m.is_finite() && s.is_finite() && ms.is_finite()) {
        return f64::NAN;
    }
    let i = ops::f64_to_integer_or_infinity;
    i(h) * MS_PER_HOUR + i(m) * MS_PER_MINUTE + i(s) * MS_PER_SECOND + i(ms)
}

/// MakeDay(year, month, date).
fn make_day(year: f64, month: f64, date: f64) -> f64 {
    if !(year.is_finite() && month.is_finite() && date.is_finite()) {
        return f64::NAN;
    }
    let i = ops::f64_to_integer_or_infinity;
    let (y, m, dt) = (i(year), i(month), i(date));
    let ym = y + (m / 12.0).floor();
    if !ym.is_finite() || ym.abs() > 400_000.0 {
        return f64::NAN;
    }
    let mn = m.rem_euclid(12.0);
    let days = epoch_days_from_iso_date(ym as i64, mn as i64 + 1, 1) as f64;
    days + dt - 1.0
}

/// MakeDate(day, time).
fn make_date(day: f64, time: f64) -> f64 {
    if !day.is_finite() || !time.is_finite() {
        return f64::NAN;
    }
    let tv = day * MS_PER_DAY + time;
    if tv.is_finite() { tv } else { f64::NAN }
}

/// MakeFullYear(year).
fn make_full_year(year: f64) -> f64 {
    if year.is_nan() {
        return f64::NAN;
    }
    let truncated = ops::f64_to_integer_or_infinity(year);
    if (0.0..=99.0).contains(&truncated) {
        1900.0 + truncated
    } else {
        truncated
    }
}

/// TimeClip(time).
fn time_clip(t: f64) -> f64 {
    if !t.is_finite() || t.abs() > 8.64e15 {
        return f64::NAN;
    }
    ops::f64_to_integer_or_infinity(t) + 0.0
}

/// The local offset in milliseconds at a UTC time value.
fn offset_ms(t: f64) -> f64 {
    if t.abs() > 1e16 {
        return 0.0;
    }
    let seconds = (t / 1000.0).floor() as i64;
    f64::from(tz::local().data.offset_at(seconds)) * 1000.0
}

/// LocalTime(t).
fn local_time(t: f64) -> f64 {
    t + offset_ms(t)
}

/// UTC(t): the earlier instant in a fold; in a gap, the offset in effect
/// before the transition.
fn utc_time(t: f64) -> f64 {
    if !t.is_finite() {
        return f64::NAN;
    }
    if t.abs() > 1e16 {
        // Far outside the time value range: TimeClip makes it NaN.
        return t;
    }
    let data = &tz::local().data;
    let local_seconds = (t / 1000.0).floor() as i64;
    let possible = data.possible_instants(local_seconds);
    let offset = match possible.first() {
        Some(&first) => local_seconds - first,
        None => i64::from(data.offset_at(local_seconds - 86_400)),
    };
    t - offset as f64 * 1000.0
}

fn now_ms() -> f64 {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_millis() as f64,
        Err(e) => -(e.duration().as_millis() as f64),
    }
}

// ----- The constructor -----

/// thisTimeValue(value).
fn this_time_value(agent: &mut Agent, v: Value) -> JsResult<f64> {
    if let Value::Object(o) = v
        && let ObjectKind::Date(t) = agent.heap.object(o).kind
    {
        return Ok(t);
    }
    agent.type_error("this is not a Date object")
}

fn this_date(agent: &mut Agent, v: Value) -> JsResult<(Object, f64)> {
    let t = this_time_value(agent, v)?;
    let Value::Object(o) = v else {
        unreachable!("thisTimeValue accepted an object");
    };
    Ok((o, t))
}

fn set_time_value(agent: &mut Agent, o: Object, t: f64) {
    agent.heap.object_mut(o).kind = ObjectKind::Date(t);
}

/// `Date(...values)` (§21.4.2.1).
fn date_constructor(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let Some(new_target) = call.new_target else {
        let s = to_date_string_of(now_ms());
        return Ok(str_value(agent, &s));
    };
    let tv = match call.args.len() {
        0 => now_ms(),
        1 => {
            let v = call.arg(0);
            let tv = if let Value::Object(o) = v
                && let ObjectKind::Date(t) = agent.heap.object(o).kind
            {
                t
            } else {
                let p = ops::to_primitive(agent, v, PreferredType::Default)?;
                if let Value::String(s) = p {
                    let text = agent.heap.string_to_rust(s);
                    parse_date(&text)
                } else {
                    ops::to_number(agent, p)?
                }
            };
            time_clip(tv)
        }
        n => {
            let mut f = [f64::NAN, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0];
            for (i, slot) in f.iter_mut().enumerate().take(n.min(7)) {
                *slot = ops::to_number(agent, call.args[i])?;
            }
            let year = make_full_year(f[0]);
            let final_date = make_date(
                make_day(year, f[1], f[2]),
                make_time(f[3], f[4], f[5], f[6]),
            );
            time_clip(utc_time(final_date))
        }
    };
    let o = ops::ordinary_create_from_constructor(
        agent,
        new_target,
        Intrinsic::DatePrototype,
        ObjectKind::Date(tv),
    )?;
    Ok(Value::Object(o))
}

fn now(_agent: &mut Agent, _call: NativeCall<'_>) -> JsResult<Value> {
    Ok(Value::Number(now_ms()))
}

/// `Date.parse(string)`.
fn parse(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let s = ops::to_string(agent, call.arg(0))?;
    let text = agent.heap.string_to_rust(s);
    Ok(Value::Number(parse_date(&text)))
}

/// `Date.UTC(year [, month [, date [, hours [, minutes [, seconds [, ms]]]]]])`.
fn utc(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let mut f = [f64::NAN, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0];
    f[0] = ops::to_number(agent, call.arg(0))?;
    for (i, slot) in f.iter_mut().enumerate().skip(1) {
        if i < call.args.len() {
            *slot = ops::to_number(agent, call.args[i])?;
        }
    }
    let year = make_full_year(f[0]);
    let tv = make_date(
        make_day(year, f[1], f[2]),
        make_time(f[3], f[4], f[5], f[6]),
    );
    Ok(Value::Number(time_clip(tv)))
}

// ----- Getters -----

macro_rules! getters {
    ($($name:ident => $local:expr, $f:expr;)*) => {
        $(
            fn $name(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
                let t = this_time_value(agent, call.this)?;
                if t.is_nan() {
                    return Ok(Value::Number(f64::NAN));
                }
                let t = if $local { local_time(t) } else { t };
                let f: fn(f64) -> f64 = $f;
                Ok(Value::Number(f(t)))
            }
        )*
    };
}

getters! {
    get_date => true, |t| ymd(t).2;
    get_day => true, week_day;
    get_full_year => true, |t| ymd(t).0;
    get_hours => true, hour_from_time;
    get_milliseconds => true, ms_from_time;
    get_minutes => true, min_from_time;
    get_month => true, |t| ymd(t).1;
    get_seconds => true, sec_from_time;
    get_utc_date => false, |t| ymd(t).2;
    get_utc_day => false, week_day;
    get_utc_full_year => false, |t| ymd(t).0;
    get_utc_hours => false, hour_from_time;
    get_utc_milliseconds => false, ms_from_time;
    get_utc_minutes => false, min_from_time;
    get_utc_month => false, |t| ymd(t).1;
    get_utc_seconds => false, sec_from_time;
}

/// `getTime` and `valueOf`.
fn get_time(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    Ok(Value::Number(this_time_value(agent, call.this)?))
}

fn get_timezone_offset(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let t = this_time_value(agent, call.this)?;
    if t.is_nan() {
        return Ok(Value::Number(f64::NAN));
    }
    Ok(Value::Number((t - local_time(t)) / MS_PER_MINUTE))
}

// ----- Setters -----

/// The components a setter writes: year, month, date, hours, minutes,
/// seconds, milliseconds.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Field {
    FullYear = 0,
    Month = 1,
    Date = 2,
    Hours = 3,
    Minutes = 4,
    Seconds = 5,
    Milliseconds = 6,
}

/// The common steps of the setters: thisTimeValue, the argument
/// conversions in order, then the new time value from the components of
/// `t` with the given ones replaced.
fn set_components(
    agent: &mut Agent,
    call: &NativeCall<'_>,
    first: Field,
    count: usize,
    local: bool,
) -> JsResult<Value> {
    let (o, t) = this_date(agent, call.this)?;
    let mut given = [None; 7];
    let first = first as usize;
    for i in 0..count {
        if i == 0 || i < call.args.len() {
            given[first + i] = Some(ops::to_number(agent, call.arg(i))?);
        }
    }
    let t = if t.is_nan() {
        if first != Field::FullYear as usize {
            return Ok(Value::Number(f64::NAN));
        }
        0.0
    } else if local {
        local_time(t)
    } else {
        t
    };
    let (y, m, d) = ymd(t);
    let current = [
        y,
        m,
        d,
        hour_from_time(t),
        min_from_time(t),
        sec_from_time(t),
        ms_from_time(t),
    ];
    let mut c = current;
    for i in 0..7 {
        if let Some(v) = given[i] {
            c[i] = v;
        }
    }
    let new_date = make_date(
        make_day(c[0], c[1], c[2]),
        make_time(c[3], c[4], c[5], c[6]),
    );
    let u = time_clip(if local { utc_time(new_date) } else { new_date });
    set_time_value(agent, o, u);
    Ok(Value::Number(u))
}

macro_rules! setters {
    ($($name:ident => $field:expr, $count:expr, $local:expr;)*) => {
        $(
            fn $name(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
                set_components(agent, &call, $field, $count, $local)
            }
        )*
    };
}

setters! {
    set_date => Field::Date, 1, true;
    set_full_year => Field::FullYear, 3, true;
    set_hours => Field::Hours, 4, true;
    set_milliseconds => Field::Milliseconds, 1, true;
    set_minutes => Field::Minutes, 3, true;
    set_month => Field::Month, 2, true;
    set_seconds => Field::Seconds, 2, true;
    set_utc_date => Field::Date, 1, false;
    set_utc_full_year => Field::FullYear, 3, false;
    set_utc_hours => Field::Hours, 4, false;
    set_utc_milliseconds => Field::Milliseconds, 1, false;
    set_utc_minutes => Field::Minutes, 3, false;
    set_utc_month => Field::Month, 2, false;
    set_utc_seconds => Field::Seconds, 2, false;
}

/// `setTime(time)`.
fn set_time(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let (o, _) = this_date(agent, call.this)?;
    let t = ops::to_number(agent, call.arg(0))?;
    let v = time_clip(t);
    set_time_value(agent, o, v);
    Ok(Value::Number(v))
}

// ----- String forms -----

fn year_string(y: f64) -> String {
    let sign = if y < 0.0 { "-" } else { "" };
    format!("{sign}{:04}", y.abs() as i64)
}

/// DateString(tv) of a local time value.
fn date_string(t: f64) -> String {
    let (y, m, d) = ymd(t);
    format!(
        "{} {} {:02} {}",
        WEEKDAYS[week_day(t) as usize],
        MONTHS[m as usize],
        d as i64,
        year_string(y)
    )
}

/// TimeString(tv).
fn time_string(t: f64) -> String {
    format!(
        "{:02}:{:02}:{:02} GMT",
        hour_from_time(t) as i64,
        min_from_time(t) as i64,
        sec_from_time(t) as i64
    )
}

/// TimeZoneString(tv): the offset and the TZif abbreviation.
fn time_zone_string(tv: f64) -> String {
    let offset = offset_ms(tv);
    let sign = if offset >= 0.0 { '+' } else { '-' };
    let abs = offset.abs();
    let seconds = (tv / 1000.0).floor() as i64;
    let abbreviation = tz::local().data.local_type_at(seconds).abbreviation;
    let name = if abbreviation.is_empty() {
        String::new()
    } else {
        format!(" ({abbreviation})")
    };
    format!(
        "{sign}{:02}{:02}{name}",
        hour_from_time(abs) as i64,
        min_from_time(abs) as i64
    )
}

/// ToDateString(tv).
fn to_date_string_of(tv: f64) -> String {
    if tv.is_nan() {
        return "Invalid Date".to_string();
    }
    let t = local_time(tv);
    format!(
        "{} {}{}",
        date_string(t),
        time_string(t),
        time_zone_string(tv)
    )
}

fn to_string(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let tv = this_time_value(agent, call.this)?;
    Ok(str_value(agent, &to_date_string_of(tv)))
}

fn to_date_string(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let tv = this_time_value(agent, call.this)?;
    if tv.is_nan() {
        return Ok(str_value(agent, "Invalid Date"));
    }
    Ok(str_value(agent, &date_string(local_time(tv))))
}

fn to_time_string(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let tv = this_time_value(agent, call.this)?;
    if tv.is_nan() {
        return Ok(str_value(agent, "Invalid Date"));
    }
    let t = local_time(tv);
    let s = format!("{}{}", time_string(t), time_zone_string(tv));
    Ok(str_value(agent, &s))
}

fn to_utc_string(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let tv = this_time_value(agent, call.this)?;
    if tv.is_nan() {
        return Ok(str_value(agent, "Invalid Date"));
    }
    let (y, m, d) = ymd(tv);
    let s = format!(
        "{}, {:02} {} {} {}",
        WEEKDAYS[week_day(tv) as usize],
        d as i64,
        MONTHS[m as usize],
        year_string(y),
        time_string(tv)
    );
    Ok(str_value(agent, &s))
}

/// The Date Time String Format of a finite time value.
pub(crate) fn iso_string(tv: f64) -> String {
    let (y, m, d) = ymd(tv);
    let year = if (0.0..=9999.0).contains(&y) {
        format!("{:04}", y as i64)
    } else {
        let sign = if y < 0.0 { '-' } else { '+' };
        format!("{sign}{:06}", y.abs() as i64)
    };
    format!(
        "{year}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        m as i64 + 1,
        d as i64,
        hour_from_time(tv) as i64,
        min_from_time(tv) as i64,
        sec_from_time(tv) as i64,
        ms_from_time(tv) as i64
    )
}

fn to_iso_string(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let tv = this_time_value(agent, call.this)?;
    if !tv.is_finite() {
        return agent.range_error("invalid time value");
    }
    Ok(str_value(agent, &iso_string(tv)))
}

/// `toJSON(key)` (§21.4.4.37).
fn to_json(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let o = ops::to_object(agent, call.this)?;
    let tv = ops::to_primitive(agent, Value::Object(o), PreferredType::Number)?;
    if let Value::Number(n) = tv
        && !n.is_finite()
    {
        return Ok(Value::Null);
    }
    let key = agent.heap.key_from_str("toISOString");
    ops::invoke(agent, Value::Object(o), key, &[])
}

/// `Date.prototype[@@toPrimitive](hint)`.
fn to_primitive(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let Value::Object(o) = call.this else {
        return agent.type_error("Date.prototype[Symbol.toPrimitive] called on a non-object");
    };
    let hint = match call.arg(0) {
        Value::String(s) => agent.heap.string_to_rust(s),
        _ => String::new(),
    };
    let try_first = match hint.as_str() {
        "string" | "default" => PreferredType::String,
        "number" => PreferredType::Number,
        _ => return agent.type_error("invalid hint"),
    };
    ops::ordinary_to_primitive(agent, o, try_first)
}

/// `toTemporalInstant()`.
fn to_temporal_instant(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let tv = this_time_value(agent, call.this)?;
    if !tv.is_finite() {
        return agent.range_error("invalid time value");
    }
    let ns = tv as i128 * 1_000_000;
    let o = crate::builtins::temporal::create_instant(agent, ns, None)?;
    Ok(Value::Object(o))
}

// ----- Date.parse -----

/// The time value of a string in the Date Time String Format, or in the
/// formats of `toString` and `toUTCString`; NaN otherwise.
pub fn parse_date(s: &str) -> f64 {
    let s = s.trim();
    parse_iso(s)
        .or_else(|| parse_fallback(s))
        .map_or(f64::NAN, time_clip)
}

struct Scan<'a> {
    b: &'a [u8],
    i: usize,
}

impl Scan<'_> {
    fn eat(&mut self, c: u8) -> bool {
        if self.b.get(self.i) == Some(&c) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn digits(&mut self, n: usize) -> Option<i64> {
        let mut v = 0i64;
        for _ in 0..n {
            let c = *self.b.get(self.i)?;
            if !c.is_ascii_digit() {
                return None;
            }
            v = v * 10 + i64::from(c - b'0');
            self.i += 1;
        }
        Some(v)
    }

    fn done(&self) -> bool {
        self.i == self.b.len()
    }
}

/// The Date Time String Format (§21.4.1.32).
fn parse_iso(s: &str) -> Option<f64> {
    let mut p = Scan {
        b: s.as_bytes(),
        i: 0,
    };
    let year = if p.eat(b'+') {
        p.digits(6)?
    } else if p.eat(b'-') {
        let y = p.digits(6)?;
        if y == 0 {
            return None;
        }
        -y
    } else {
        p.digits(4)?
    };
    let (mut month, mut day) = (1, 1);
    if p.eat(b'-') {
        month = p.digits(2)?;
        if p.eat(b'-') {
            day = p.digits(2)?;
        }
    }
    if !(1..=12).contains(&month) || day < 1 || day > iso_days_in_month(year, month) {
        return None;
    }
    let (mut h, mut mi, mut sec, mut ms) = (0, 0, 0, 0);
    let mut has_time = false;
    let mut offset: Option<i64> = None;
    if p.eat(b'T') || p.eat(b't') {
        has_time = true;
        h = p.digits(2)?;
        if !p.eat(b':') {
            return None;
        }
        mi = p.digits(2)?;
        if p.eat(b':') {
            sec = p.digits(2)?;
            if p.eat(b'.') {
                let start = p.i;
                let mut scale = 100;
                while let Some(c) = p.b.get(p.i).filter(|c| c.is_ascii_digit()) {
                    ms += i64::from(c - b'0') * scale;
                    scale /= 10;
                    p.i += 1;
                }
                if p.i == start {
                    return None;
                }
            }
        }
        if h > 24 || mi > 59 || sec > 59 || (h == 24 && (mi, sec, ms) != (0, 0, 0)) {
            return None;
        }
        if p.eat(b'Z') || p.eat(b'z') {
            offset = Some(0);
        } else if let Some(sign) = match p.b.get(p.i) {
            Some(b'+') => Some(1),
            Some(b'-') => Some(-1),
            _ => None,
        } {
            p.i += 1;
            let oh = p.digits(2)?;
            if !p.eat(b':') {
                return None;
            }
            let om = p.digits(2)?;
            if oh > 23 || om > 59 {
                return None;
            }
            offset = Some(sign * (oh * 60 + om));
        }
    }
    if !p.done() {
        return None;
    }
    let date = make_date(
        make_day(year as f64, (month - 1) as f64, day as f64),
        make_time(h as f64, mi as f64, sec as f64, ms as f64),
    );
    Some(match offset {
        Some(minutes) => date - minutes as f64 * MS_PER_MINUTE,
        None if has_time => utc_time(date),
        None => date,
    })
}

fn month_index(word: &str) -> Option<usize> {
    let w = word.get(..3)?;
    MONTHS.iter().position(|m| m.eq_ignore_ascii_case(w))
}

fn is_weekday(word: &str) -> bool {
    word.get(..3)
        .is_some_and(|w| WEEKDAYS.iter().any(|d| d.eq_ignore_ascii_case(w)))
}

/// `±hhmm` in minutes.
fn offset_minutes(s: &str) -> Option<i64> {
    let (sign, digits) = match s.as_bytes().first()? {
        b'+' => (1, &s[1..]),
        b'-' => (-1, &s[1..]),
        _ => return None,
    };
    if digits.len() != 4 || !digits.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let n: i64 = digits.parse().ok()?;
    let (h, m) = (n / 100, n % 100);
    if m > 59 {
        return None;
    }
    Some(sign * (h * 60 + m))
}

/// `Www Mmm DD YYYY [HH:mm:ss [GMT±hhmm [(name)]]]` (toString and
/// toDateString) and `Www, DD Mmm YYYY HH:mm:ss GMT` (toUTCString).
fn parse_fallback(s: &str) -> Option<f64> {
    let s = match s.find('(') {
        Some(i) if s.ends_with(')') => s[..i].trim_end(),
        _ => s,
    };
    let words: Vec<&str> = s
        .split(|c: char| c.is_ascii_whitespace() || c == ',')
        .filter(|w| !w.is_empty())
        .collect();
    let mut i = 0;
    if words.first().is_some_and(|w| is_weekday(w)) {
        i += 1;
    }
    let number = |w: &str| -> Option<i64> {
        if w.is_empty() || !w.bytes().all(|c| c.is_ascii_digit()) {
            return None;
        }
        w.parse().ok()
    };
    let (month, day) = match (words.get(i), words.get(i + 1)) {
        (Some(a), Some(b)) if month_index(a).is_some() => (month_index(a)?, number(b)?),
        (Some(a), Some(b)) if month_index(b).is_some() => (month_index(b)?, number(a)?),
        _ => return None,
    };
    i += 2;
    let year_word = words.get(i)?;
    let year = match year_word.strip_prefix('-') {
        Some(rest) => -number(rest)?,
        None => number(year_word)?,
    };
    i += 1;
    if !(1..=31).contains(&day) {
        return None;
    }
    let (mut h, mut mi, mut sec) = (0, 0, 0);
    if let Some(w) = words.get(i)
        && w.contains(':')
    {
        let parts: Vec<&str> = w.split(':').collect();
        if parts.len() < 2 || parts.len() > 3 {
            return None;
        }
        h = number(parts[0])?;
        mi = number(parts[1])?;
        if parts.len() == 3 {
            sec = number(parts[2])?;
        }
        if h > 24 || mi > 59 || sec > 59 {
            return None;
        }
        i += 1;
    }
    let mut offset = None;
    if let Some(w) = words.get(i) {
        let rest = if let Some(r) = w.strip_prefix("GMT").or_else(|| w.strip_prefix("UTC")) {
            offset = Some(0);
            i += 1;
            if r.is_empty() {
                words.get(i).filter(|n| n.starts_with(['+', '-'])).map(|n| {
                    i += 1;
                    *n
                })
            } else {
                Some(r)
            }
        } else if w.starts_with(['+', '-']) {
            i += 1;
            Some(*w)
        } else {
            None
        };
        if let Some(r) = rest {
            offset = Some(offset_minutes(r)?);
        }
    }
    if i != words.len() {
        return None;
    }
    let date = make_date(
        make_day(year as f64, month as f64, day as f64),
        make_time(h as f64, mi as f64, sec as f64, 0.0),
    );
    Some(match offset {
        Some(minutes) => date - minutes as f64 * MS_PER_MINUTE,
        None => utc_time(date),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_strings() {
        assert_eq!(parse_date("1970-01-01T00:00:00.000Z"), 0.0);
        assert_eq!(parse_date("1970"), 0.0);
        assert_eq!(parse_date("1970-01-02"), 86_400_000.0);
        assert_eq!(parse_date("+002000-01-01T00:00Z"), 946_684_800_000.0);
        assert_eq!(parse_date("2000-01-01T24:00:00Z"), 946_771_200_000.0);
        assert_eq!(parse_date("2000-01-01T00:00+01:00"), 946_681_200_000.0);
        assert!(parse_date("-000000-01-01T00:00Z").is_nan());
        assert!(parse_date("2000-02-30").is_nan());
        assert!(parse_date("2000-01-01T24:00:01Z").is_nan());
    }

    #[test]
    fn utc_string_round_trip() {
        assert_eq!(parse_date("Thu, 01 Jan 1970 00:00:00 GMT"), 0.0);
        assert_eq!(
            parse_date("Sat Jan 01 2000 01:00:00 GMT+0100 (CET)"),
            946_684_800_000.0
        );
        assert_eq!(
            iso_string(-62_198_755_200_000.0),
            "-000001-01-01T00:00:00.000Z"
        );
    }

    #[test]
    fn make_day_wraps_months() {
        assert_eq!(make_day(1970.0, 12.0, 1.0), 365.0);
        assert_eq!(make_day(1970.0, -1.0, 1.0), -31.0);
        assert!(make_day(f64::INFINITY, 0.0, 1.0).is_nan());
    }
}
