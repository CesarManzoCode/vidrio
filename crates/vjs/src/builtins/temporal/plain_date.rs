//! `Temporal.PlainDate` (Temporal proposal §3).

use super::{
    DATE_GETTERS, TemporalSlots, UnitGroup, canonicalize_calendar, create_date, create_date_time,
    create_duration, date_from_fields, date_getter, difference_settings, fields_of,
    get_options_object, getter_index, indexed_getter, merge_fields, overflow_option,
    prepare_fields, require_partial_temporal_object, show_calendar_option, slots,
    to_calendar_identifier, to_i64, to_integer_with_truncation, to_temporal_date,
    to_temporal_duration, to_time_or_midnight, tr, value_of,
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
    DateTime, InternalDuration, IsoDate, Time, Unit, calendar_date_add, calendar_date_until,
    duration_from_internal, format_calendar_annotation, format_date, iso_days_in_month,
    round_relative_duration,
};
use crate::value::Value;

pub(crate) fn install(agent: &mut Agent, realm: Realm, temporal: Object) {
    let object_proto = intrinsic(agent, realm, Intrinsic::ObjectPrototype);
    let proto = plain_object(agent, object_proto);
    set_intrinsic(agent, realm, Intrinsic::TemporalPlainDatePrototype, proto);
    let ctor = constructor(
        agent,
        realm,
        "PlainDate",
        plain_date_constructor,
        3,
        proto,
        None,
    );
    set_intrinsic(agent, realm, Intrinsic::TemporalPlainDate, ctor);
    let key = agent.heap.key_from_str("PlainDate");
    value(agent, temporal, key, Value::Object(ctor), true, false, true);
    method(agent, realm, ctor, "compare", compare, 2);
    method(agent, realm, ctor, "from", from, 1);
    for (i, name) in DATE_GETTERS.iter().enumerate() {
        indexed_getter(agent, realm, proto, name, field_getter, i);
    }
    method(agent, realm, proto, "add", add, 1);
    method(agent, realm, proto, "equals", equals, 1);
    method(agent, realm, proto, "since", since, 1);
    method(agent, realm, proto, "subtract", subtract, 1);
    method(agent, realm, proto, "toJSON", to_json, 0);
    method(agent, realm, proto, "toLocaleString", to_json, 0);
    method(
        agent,
        realm,
        proto,
        "toPlainDateTime",
        to_plain_date_time,
        0,
    );
    method(agent, realm, proto, "toString", to_string, 0);
    method(agent, realm, proto, "until", until, 1);
    method(agent, realm, proto, "valueOf", value_of, 0);
    method(agent, realm, proto, "with", with, 1);
    method(agent, realm, proto, "withCalendar", with_calendar, 1);
    to_string_tag(agent, proto, "Temporal.PlainDate");
}

fn this_date(agent: &mut Agent, v: Value) -> JsResult<IsoDate> {
    match slots(agent, v) {
        Some(TemporalSlots::PlainDate(d)) => Ok(d),
        _ => agent.type_error("this is not a Temporal.PlainDate"),
    }
}

/// The calendar argument of the constructors: a calendar identifier
/// string (not an ISO string).
pub(crate) fn calendar_argument(agent: &mut Agent, v: Value) -> JsResult<()> {
    match v {
        Value::Undefined => Ok(()),
        Value::String(s) => {
            let id = agent.heap.string_to_rust(s);
            canonicalize_calendar(agent, &id)
        }
        _ => agent.type_error("calendar must be a string"),
    }
}

/// The ISO date of constructor arguments, or a RangeError.
pub(crate) fn date_arguments(agent: &mut Agent, y: f64, m: f64, d: f64) -> JsResult<IsoDate> {
    let valid = y.abs() <= 1e9
        && (1.0..=12.0).contains(&m)
        && d >= 1.0
        && d <= iso_days_in_month(to_i64(y), to_i64(m)) as f64;
    if !valid {
        return agent.range_error("invalid ISO date");
    }
    Ok(IsoDate::new(to_i64(y), to_i64(m), to_i64(d)))
}

fn plain_date_constructor(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let Some(new_target) = call.new_target else {
        return agent.type_error("Temporal.PlainDate must be called with new");
    };
    let y = to_integer_with_truncation(agent, call.arg(0))?;
    let m = to_integer_with_truncation(agent, call.arg(1))?;
    let d = to_integer_with_truncation(agent, call.arg(2))?;
    calendar_argument(agent, call.arg(3))?;
    let date = date_arguments(agent, y, m, d)?;
    create_date(agent, date, Some(new_target))
}

fn from(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let date = to_temporal_date(agent, call.arg(0), call.arg(1))?;
    create_date(agent, date, None)
}

fn compare(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let one = to_temporal_date(agent, call.arg(0), Value::Undefined)?;
    let two = to_temporal_date(agent, call.arg(1), Value::Undefined)?;
    Ok(Value::Number(match one.cmp(&two) {
        std::cmp::Ordering::Less => -1.0,
        std::cmp::Ordering::Equal => 0.0,
        std::cmp::Ordering::Greater => 1.0,
    }))
}

fn field_getter(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let date = this_date(agent, call.this)?;
    let name = DATE_GETTERS[getter_index(agent, call.callee)];
    Ok(date_getter(agent, date, name))
}

/// AddDurationToDate(operation, temporalDate, temporalDurationLike,
/// options).
fn add_duration(agent: &mut Agent, call: &NativeCall<'_>, subtract: bool) -> JsResult<Value> {
    let date = this_date(agent, call.this)?;
    let mut d = to_temporal_duration(agent, call.arg(0))?;
    if subtract {
        d = d.negated();
    }
    let dd = tr(agent, d.to_date_duration_without_time())?;
    let options = get_options_object(agent, call.arg(1))?;
    let overflow = overflow_option(agent, options)?;
    let result = tr(agent, calendar_date_add(date, dd, overflow))?;
    create_date(agent, result, None)
}

fn add(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    add_duration(agent, &call, false)
}

fn subtract(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    add_duration(agent, &call, true)
}

fn with(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let date = this_date(agent, call.this)?;
    let like = require_partial_temporal_object(agent, call.arg(0))?;
    let partial = prepare_fields(agent, like, true, false, true)?;
    let fields = merge_fields(fields_of(date, None), partial);
    let options = get_options_object(agent, call.arg(1))?;
    let overflow = overflow_option(agent, options)?;
    let result = date_from_fields(agent, &fields, overflow)?;
    create_date(agent, result, None)
}

fn with_calendar(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let date = this_date(agent, call.this)?;
    to_calendar_identifier(agent, call.arg(0))?;
    create_date(agent, date, None)
}

/// DifferenceTemporalPlainDate(operation, temporalDate, other, options).
fn difference(agent: &mut Agent, call: &NativeCall<'_>, since: bool) -> JsResult<Value> {
    let date = this_date(agent, call.this)?;
    let other = to_temporal_date(agent, call.arg(0), Value::Undefined)?;
    let options = get_options_object(agent, call.arg(1))?;
    let s = difference_settings(agent, since, options, UnitGroup::Date, Unit::Day, Unit::Day)?;
    if date == other {
        return create_duration(agent, Default::default(), None);
    }
    let mut internal = InternalDuration {
        date: calendar_date_until(date, other, s.largest),
        time: 0,
    };
    if s.smallest != Unit::Day || s.increment != 1 {
        let origin = DateTime::new(date, Time::MIDNIGHT);
        let dest = DateTime::new(other, Time::MIDNIGHT).utc_epoch_ns();
        internal = tr(
            agent,
            round_relative_duration(
                internal,
                dest,
                origin,
                s.largest,
                s.increment,
                s.smallest,
                s.mode,
            ),
        )?;
    }
    let mut result = duration_from_internal(internal, Unit::Day);
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

fn equals(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let date = this_date(agent, call.this)?;
    let other = to_temporal_date(agent, call.arg(0), Value::Undefined)?;
    Ok(Value::Boolean(date == other))
}

fn to_plain_date_time(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let date = this_date(agent, call.this)?;
    let time = to_time_or_midnight(agent, call.arg(0))?;
    create_date_time(agent, DateTime::new(date, time), None)
}

fn to_string(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let date = this_date(agent, call.this)?;
    let options = get_options_object(agent, call.arg(0))?;
    let show = show_calendar_option(agent, options)?;
    let s = format!("{}{}", format_date(date), format_calendar_annotation(show));
    Ok(str_value(agent, &s))
}

fn to_json(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let date = this_date(agent, call.this)?;
    Ok(str_value(agent, &format_date(date)))
}
