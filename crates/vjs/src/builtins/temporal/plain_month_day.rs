//! `Temporal.PlainMonthDay` (Temporal proposal §10). The ISO date keeps
//! the reference year (1972, a leap year, unless the constructor says
//! otherwise), so February 29 is a valid month-day.

use super::plain_date::calendar_argument;
use super::{
    MONTH_DAY_FIELDS, TemporalSlots, create_date, create_month_day, date_from_fields,
    get_options_object, getter_index, indexed_getter, merge_fields, month_day_fields,
    month_day_from_fields, number, overflow_option, prepare_named_fields,
    require_partial_temporal_object, show_calendar_option, slots, to_integer_with_truncation,
    to_temporal_month_day, value_of,
};
use crate::agent::Agent;
use crate::builtins::{
    constructor, intrinsic, method, plain_object, set_intrinsic, str_value, to_string_tag, value,
};
use crate::completion::JsResult;
use crate::function::NativeCall;
use crate::gc::{Object, Realm};
use crate::realm::Intrinsic;
use crate::temporal::{
    IsoDate, Overflow, ShowCalendar, format_calendar_annotation, iso_days_in_month, pad_iso_year,
};
use crate::value::Value;

const GETTERS: [&str; 3] = ["calendarId", "monthCode", "day"];

pub(crate) fn install(agent: &mut Agent, realm: Realm, temporal: Object) {
    let object_proto = intrinsic(agent, realm, Intrinsic::ObjectPrototype);
    let proto = plain_object(agent, object_proto);
    set_intrinsic(
        agent,
        realm,
        Intrinsic::TemporalPlainMonthDayPrototype,
        proto,
    );
    let ctor = constructor(
        agent,
        realm,
        "PlainMonthDay",
        plain_month_day_constructor,
        2,
        proto,
        None,
    );
    set_intrinsic(agent, realm, Intrinsic::TemporalPlainMonthDay, ctor);
    let key = agent.heap.key_from_str("PlainMonthDay");
    value(agent, temporal, key, Value::Object(ctor), true, false, true);
    method(agent, realm, ctor, "from", from, 1);
    for (i, name) in GETTERS.iter().enumerate() {
        indexed_getter(agent, realm, proto, name, field_getter, i);
    }
    method(agent, realm, proto, "equals", equals, 1);
    method(agent, realm, proto, "toJSON", to_json, 0);
    method(agent, realm, proto, "toLocaleString", to_json, 0);
    method(agent, realm, proto, "toPlainDate", to_plain_date, 1);
    method(agent, realm, proto, "toString", to_string, 0);
    method(agent, realm, proto, "valueOf", value_of, 0);
    method(agent, realm, proto, "with", with, 1);
    to_string_tag(agent, proto, "Temporal.PlainMonthDay");
}

fn this_month_day(agent: &mut Agent, v: Value) -> JsResult<IsoDate> {
    match slots(agent, v) {
        Some(TemporalSlots::PlainMonthDay(d)) => Ok(d),
        _ => agent.type_error("this is not a Temporal.PlainMonthDay"),
    }
}

/// `new Temporal.PlainMonthDay(isoMonth, isoDay [, calendar
/// [, referenceISOYear]])`.
fn plain_month_day_constructor(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let Some(new_target) = call.new_target else {
        return agent.type_error("Temporal.PlainMonthDay must be called with new");
    };
    let m = to_integer_with_truncation(agent, call.arg(0))?;
    let d = to_integer_with_truncation(agent, call.arg(1))?;
    calendar_argument(agent, call.arg(2))?;
    let reference = call.arg(3);
    let y = if reference.is_undefined() {
        1972.0
    } else {
        to_integer_with_truncation(agent, reference)?
    };
    let valid = y.abs() <= 1e9
        && (1.0..=12.0).contains(&m)
        && d >= 1.0
        && d <= iso_days_in_month(y as i64, m as i64) as f64;
    if !valid {
        return agent.range_error("invalid ISO date");
    }
    let date = IsoDate::new(y as i64, m as i64, d as i64);
    create_month_day(agent, date, Some(new_target))
}

fn from(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let date = to_temporal_month_day(agent, call.arg(0), call.arg(1))?;
    create_month_day(agent, date, None)
}

fn field_getter(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let date = this_month_day(agent, call.this)?;
    Ok(match GETTERS[getter_index(agent, call.callee)] {
        "calendarId" => super::iso8601(agent),
        "monthCode" => str_value(agent, &format!("M{:02}", date.month)),
        _ => number(date.day as f64),
    })
}

fn with(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let date = this_month_day(agent, call.this)?;
    let like = require_partial_temporal_object(agent, call.arg(0))?;
    let (partial, _) = prepare_named_fields(agent, like, &MONTH_DAY_FIELDS, true)?;
    let fields = merge_fields(month_day_fields(date), partial);
    let options = get_options_object(agent, call.arg(1))?;
    let overflow = overflow_option(agent, options)?;
    let result = month_day_from_fields(agent, &fields, overflow)?;
    create_month_day(agent, result, None)
}

fn equals(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let md = this_month_day(agent, call.this)?;
    let other = to_temporal_month_day(agent, call.arg(0), Value::Undefined)?;
    Ok(Value::Boolean(md == other))
}

/// `toPlainDate(item)`: the year comes from `item`; February 29 becomes
/// February 28 in common years.
fn to_plain_date(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let md = this_month_day(agent, call.this)?;
    let Value::Object(item) = call.arg(0) else {
        return agent.type_error("toPlainDate requires an object with a year");
    };
    let (input, _) = prepare_named_fields(agent, item, &["year"], false)?;
    let fields = merge_fields(month_day_fields(md), input);
    let date = date_from_fields(agent, &fields, Overflow::Constrain)?;
    create_date(agent, date, None)
}

/// TemporalMonthDayToString(monthDay, showCalendar).
fn month_day_string(date: IsoDate, show: ShowCalendar) -> String {
    let mut s = String::new();
    if matches!(show, ShowCalendar::Always | ShowCalendar::Critical) {
        s.push_str(&pad_iso_year(date.year));
        s.push('-');
    }
    s.push_str(&format!("{:02}-{:02}", date.month, date.day));
    s.push_str(format_calendar_annotation(show));
    s
}

fn to_string(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let md = this_month_day(agent, call.this)?;
    let options = get_options_object(agent, call.arg(0))?;
    let show = show_calendar_option(agent, options)?;
    let s = month_day_string(md, show);
    Ok(str_value(agent, &s))
}

fn to_json(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let md = this_month_day(agent, call.this)?;
    let s = month_day_string(md, ShowCalendar::Auto);
    Ok(str_value(agent, &s))
}
