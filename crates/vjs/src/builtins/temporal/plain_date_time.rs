//! `Temporal.PlainDateTime` (Temporal proposal §5). `toZonedDateTime`
//! needs `Temporal.ZonedDateTime` and is not installed yet.

use super::plain_date::{calendar_argument, date_arguments};
use super::plain_time::{TIME_GETTERS, time_arguments};
use super::{
    DATE_GETTERS, TemporalSlots, UnitGroup, UnitValue, create_date, create_date_time,
    create_duration, create_time, date_from_fields, date_getter, difference_settings, fields_of,
    fractional_digits_option, get_options_object, getter_index, indexed_getter, merge_fields,
    number, overflow_option, prepare_fields, require_partial_temporal_object, round_to_options,
    rounding_increment_option, rounding_mode_option, show_calendar_option, slots, time_from_fields,
    to_calendar_identifier, to_integer_with_truncation, to_temporal_date_time,
    to_temporal_duration, to_time_or_midnight, tr, unit_option, validate_increment, validate_unit,
    value_of,
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
    DateDuration, DateTime, Precision, RoundingMode, ShowCalendar, Time, Unit, calendar_date_add,
    difference_plain_date_time_with_rounding, duration_from_internal, format_calendar_annotation,
    format_date_time, seconds_precision,
};
use crate::value::Value;

pub(crate) fn install(agent: &mut Agent, realm: Realm, temporal: Object) {
    let object_proto = intrinsic(agent, realm, Intrinsic::ObjectPrototype);
    let proto = plain_object(agent, object_proto);
    set_intrinsic(
        agent,
        realm,
        Intrinsic::TemporalPlainDateTimePrototype,
        proto,
    );
    let ctor = constructor(
        agent,
        realm,
        "PlainDateTime",
        plain_date_time_constructor,
        3,
        proto,
        None,
    );
    set_intrinsic(agent, realm, Intrinsic::TemporalPlainDateTime, ctor);
    let key = agent.heap.key_from_str("PlainDateTime");
    value(agent, temporal, key, Value::Object(ctor), true, false, true);
    method(agent, realm, ctor, "compare", compare, 2);
    method(agent, realm, ctor, "from", from, 1);
    for (i, name) in DATE_GETTERS.iter().chain(TIME_GETTERS.iter()).enumerate() {
        indexed_getter(agent, realm, proto, name, field_getter, i);
    }
    method(agent, realm, proto, "add", add, 1);
    method(agent, realm, proto, "equals", equals, 1);
    method(agent, realm, proto, "round", round, 1);
    method(agent, realm, proto, "since", since, 1);
    method(agent, realm, proto, "subtract", subtract, 1);
    method(agent, realm, proto, "toJSON", to_json, 0);
    method(agent, realm, proto, "toLocaleString", to_json, 0);
    method(agent, realm, proto, "toPlainDate", to_plain_date, 0);
    method(agent, realm, proto, "toPlainTime", to_plain_time, 0);
    method(agent, realm, proto, "toString", to_string, 0);
    method(agent, realm, proto, "until", until, 1);
    method(agent, realm, proto, "valueOf", value_of, 0);
    method(agent, realm, proto, "with", with, 1);
    method(agent, realm, proto, "withCalendar", with_calendar, 1);
    method(agent, realm, proto, "withPlainTime", with_plain_time, 0);
    to_string_tag(agent, proto, "Temporal.PlainDateTime");
}

fn this_date_time(agent: &mut Agent, v: Value) -> JsResult<DateTime> {
    match slots(agent, v) {
        Some(TemporalSlots::PlainDateTime(dt)) => Ok(dt),
        _ => agent.type_error("this is not a Temporal.PlainDateTime"),
    }
}

fn plain_date_time_constructor(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let Some(new_target) = call.new_target else {
        return agent.type_error("Temporal.PlainDateTime must be called with new");
    };
    let y = to_integer_with_truncation(agent, call.arg(0))?;
    let m = to_integer_with_truncation(agent, call.arg(1))?;
    let d = to_integer_with_truncation(agent, call.arg(2))?;
    let f = time_arguments(agent, &call, 3)?;
    calendar_argument(agent, call.arg(9))?;
    let date = date_arguments(agent, y, m, d)?;
    if !Time::is_valid(f) {
        return agent.range_error("invalid time");
    }
    let time = Time::from_fields(f.map(|v| v as i64));
    create_date_time(agent, DateTime::new(date, time), Some(new_target))
}

fn from(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let dt = to_temporal_date_time(agent, call.arg(0), call.arg(1))?;
    create_date_time(agent, dt, None)
}

fn compare(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let one = to_temporal_date_time(agent, call.arg(0), Value::Undefined)?;
    let two = to_temporal_date_time(agent, call.arg(1), Value::Undefined)?;
    Ok(Value::Number(match one.cmp(&two) {
        std::cmp::Ordering::Less => -1.0,
        std::cmp::Ordering::Equal => 0.0,
        std::cmp::Ordering::Greater => 1.0,
    }))
}

fn field_getter(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let dt = this_date_time(agent, call.this)?;
    let i = getter_index(agent, call.callee);
    if i < DATE_GETTERS.len() {
        return Ok(date_getter(agent, dt.date, DATE_GETTERS[i]));
    }
    Ok(number(dt.time.fields()[i - DATE_GETTERS.len()] as f64))
}

/// AddDurationToDateTime(operation, dateTime, temporalDurationLike,
/// options).
fn add_duration(agent: &mut Agent, call: &NativeCall<'_>, subtract: bool) -> JsResult<Value> {
    let dt = this_date_time(agent, call.this)?;
    let mut d = to_temporal_duration(agent, call.arg(0))?;
    if subtract {
        d = d.negated();
    }
    let options = get_options_object(agent, call.arg(1))?;
    let overflow = overflow_option(agent, options)?;
    let internal = d.to_internal_24h();
    let (days, time) = dt.time.add(internal.time);
    let date_duration = DateDuration {
        days,
        ..internal.date
    };
    tr(agent, date_duration.validate())?;
    let date = tr(agent, calendar_date_add(dt.date, date_duration, overflow))?;
    create_date_time(agent, DateTime::new(date, time), None)
}

fn add(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    add_duration(agent, &call, false)
}

fn subtract(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    add_duration(agent, &call, true)
}

fn with(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let dt = this_date_time(agent, call.this)?;
    let like = require_partial_temporal_object(agent, call.arg(0))?;
    let partial = prepare_fields(agent, like, true, true, true)?;
    let fields = merge_fields(fields_of(dt.date, Some(dt.time)), partial);
    let options = get_options_object(agent, call.arg(1))?;
    let overflow = overflow_option(agent, options)?;
    let date = date_from_fields(agent, &fields, overflow)?;
    let time = time_from_fields(agent, &fields, overflow)?;
    create_date_time(agent, DateTime::new(date, time), None)
}

fn with_plain_time(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let dt = this_date_time(agent, call.this)?;
    let time = to_time_or_midnight(agent, call.arg(0))?;
    create_date_time(agent, DateTime::new(dt.date, time), None)
}

fn with_calendar(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let dt = this_date_time(agent, call.this)?;
    to_calendar_identifier(agent, call.arg(0))?;
    create_date_time(agent, dt, None)
}

/// DifferenceTemporalPlainDateTime(operation, dateTime, other, options).
fn difference(agent: &mut Agent, call: &NativeCall<'_>, since: bool) -> JsResult<Value> {
    let dt = this_date_time(agent, call.this)?;
    let other = to_temporal_date_time(agent, call.arg(0), Value::Undefined)?;
    let options = get_options_object(agent, call.arg(1))?;
    let s = difference_settings(
        agent,
        since,
        options,
        UnitGroup::DateTime,
        Unit::Nanosecond,
        Unit::Day,
    )?;
    if dt == other {
        return create_duration(agent, Default::default(), None);
    }
    let internal = tr(
        agent,
        difference_plain_date_time_with_rounding(
            dt,
            other,
            s.largest,
            s.increment,
            s.smallest,
            s.mode,
        ),
    )?;
    let mut result = duration_from_internal(internal, s.largest);
    if since {
        result = result.negated();
    }
    create_duration(agent, result, None)
}

fn until(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    difference(agent, &call, false)
}

fn since(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    difference(agent, &call, true)
}

fn round(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let dt = this_date_time(agent, call.this)?;
    let options = round_to_options(agent, call.arg(0))?;
    let increment = rounding_increment_option(agent, options)?;
    let mode = rounding_mode_option(agent, options, RoundingMode::HalfExpand)?;
    let smallest = unit_option(agent, options, "smallestUnit")?;
    if smallest == UnitValue::Unset {
        return agent.range_error("smallestUnit is required");
    }
    validate_unit(
        agent,
        smallest,
        UnitGroup::Time,
        &[UnitValue::Unit(Unit::Day)],
    )?;
    let UnitValue::Unit(smallest) = smallest else {
        return agent.range_error("smallestUnit is required");
    };
    match smallest.maximum_increment() {
        Some(maximum) => validate_increment(agent, increment, maximum, false)?,
        None => validate_increment(agent, increment, 1, true)?,
    }
    if smallest == Unit::Nanosecond && increment == 1 {
        return create_date_time(agent, dt, None);
    }
    let result = dt.round(i128::from(increment), smallest, mode);
    create_date_time(agent, result, None)
}

fn equals(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let dt = this_date_time(agent, call.this)?;
    let other = to_temporal_date_time(agent, call.arg(0), Value::Undefined)?;
    Ok(Value::Boolean(dt == other))
}

fn to_plain_date(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let dt = this_date_time(agent, call.this)?;
    create_date(agent, dt.date, None)
}

fn to_plain_time(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let dt = this_date_time(agent, call.this)?;
    create_time(agent, dt.time, None)
}

fn to_string(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let dt = this_date_time(agent, call.this)?;
    let options = get_options_object(agent, call.arg(0))?;
    let show = show_calendar_option(agent, options)?;
    let digits = fractional_digits_option(agent, options)?;
    let mode = rounding_mode_option(agent, options, RoundingMode::Trunc)?;
    let smallest = unit_option(agent, options, "smallestUnit")?;
    validate_unit(agent, smallest, UnitGroup::Time, &[])?;
    let smallest = match smallest {
        UnitValue::Unit(Unit::Hour) => {
            return agent.range_error("smallestUnit must not be hour");
        }
        UnitValue::Unit(u) => Some(u),
        _ => None,
    };
    let precision = seconds_precision(smallest, digits);
    let rounded = dt.round(precision.increment, precision.unit, mode);
    if !rounded.within_limits() {
        return agent.range_error("date-time outside the supported range");
    }
    let s = format!(
        "{}{}",
        format_date_time(rounded, precision.precision),
        format_calendar_annotation(show)
    );
    Ok(str_value(agent, &s))
}

fn to_json(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let dt = this_date_time(agent, call.this)?;
    let s = format!(
        "{}{}",
        format_date_time(dt, Precision::Auto),
        format_calendar_annotation(ShowCalendar::Auto)
    );
    Ok(str_value(agent, &s))
}
