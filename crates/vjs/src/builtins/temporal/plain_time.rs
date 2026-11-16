//! `Temporal.PlainTime` (Temporal proposal §4).

use super::{
    TemporalSlots, UnitGroup, UnitValue, create_duration, create_time, difference_settings,
    fields_of, fractional_digits_option, get_options_object, getter_index, indexed_getter,
    merge_fields, number, overflow_option, prepare_fields, require_partial_temporal_object,
    round_to_options, rounding_increment_option, rounding_mode_option, slots, time_from_fields,
    to_integer_with_truncation, to_temporal_duration, to_temporal_time, tr, unit_option,
    validate_increment, validate_unit, value_of,
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
    DateDuration, InternalDuration, IsoDate, Precision, RoundingMode, Time, Unit,
    duration_from_internal, format_time, round_time_duration, seconds_precision,
};
use crate::value::Value;

pub(crate) const TIME_GETTERS: [&str; 6] = [
    "hour",
    "minute",
    "second",
    "millisecond",
    "microsecond",
    "nanosecond",
];

pub(crate) fn install(agent: &mut Agent, realm: Realm, temporal: Object) {
    let object_proto = intrinsic(agent, realm, Intrinsic::ObjectPrototype);
    let proto = plain_object(agent, object_proto);
    set_intrinsic(agent, realm, Intrinsic::TemporalPlainTimePrototype, proto);
    let ctor = constructor(
        agent,
        realm,
        "PlainTime",
        plain_time_constructor,
        0,
        proto,
        None,
    );
    set_intrinsic(agent, realm, Intrinsic::TemporalPlainTime, ctor);
    let key = agent.heap.key_from_str("PlainTime");
    value(agent, temporal, key, Value::Object(ctor), true, false, true);
    method(agent, realm, ctor, "compare", compare, 2);
    method(agent, realm, ctor, "from", from, 1);
    for (i, name) in TIME_GETTERS.iter().enumerate() {
        indexed_getter(agent, realm, proto, name, field_getter, i);
    }
    method(agent, realm, proto, "add", add, 1);
    method(agent, realm, proto, "equals", equals, 1);
    method(agent, realm, proto, "round", round, 1);
    method(agent, realm, proto, "since", since, 1);
    method(agent, realm, proto, "subtract", subtract, 1);
    method(agent, realm, proto, "toJSON", to_json, 0);
    method(agent, realm, proto, "toLocaleString", to_json, 0);
    method(agent, realm, proto, "toString", to_string, 0);
    method(agent, realm, proto, "until", until, 1);
    method(agent, realm, proto, "valueOf", value_of, 0);
    method(agent, realm, proto, "with", with, 1);
    to_string_tag(agent, proto, "Temporal.PlainTime");
}

fn this_time(agent: &mut Agent, v: Value) -> JsResult<Time> {
    match slots(agent, v) {
        Some(TemporalSlots::PlainTime(t)) => Ok(t),
        _ => agent.type_error("this is not a Temporal.PlainTime"),
    }
}

/// The time fields of constructor arguments starting at `first`
/// (`undefined` is 0), or a RangeError.
pub(crate) fn time_arguments(
    agent: &mut Agent,
    call: &NativeCall<'_>,
    first: usize,
) -> JsResult<[f64; 6]> {
    let mut f = [0.0; 6];
    for (i, slot) in f.iter_mut().enumerate() {
        let v = call.arg(first + i);
        if !v.is_undefined() {
            *slot = to_integer_with_truncation(agent, v)?;
        }
    }
    Ok(f)
}

fn plain_time_constructor(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let Some(new_target) = call.new_target else {
        return agent.type_error("Temporal.PlainTime must be called with new");
    };
    let f = time_arguments(agent, &call, 0)?;
    if !Time::is_valid(f) {
        return agent.range_error("invalid time");
    }
    let time = Time::from_fields(f.map(|v| v as i64));
    create_time(agent, time, Some(new_target))
}

fn from(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let time = to_temporal_time(agent, call.arg(0), call.arg(1))?;
    create_time(agent, time, None)
}

fn compare(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let one = to_temporal_time(agent, call.arg(0), Value::Undefined)?;
    let two = to_temporal_time(agent, call.arg(1), Value::Undefined)?;
    Ok(Value::Number(match one.cmp(&two) {
        std::cmp::Ordering::Less => -1.0,
        std::cmp::Ordering::Equal => 0.0,
        std::cmp::Ordering::Greater => 1.0,
    }))
}

fn field_getter(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let time = this_time(agent, call.this)?;
    let i = getter_index(agent, call.callee);
    Ok(number(time.fields()[i] as f64))
}

/// AddDurationToTime(operation, temporalTime, temporalDurationLike).
fn add_duration(agent: &mut Agent, call: &NativeCall<'_>, subtract: bool) -> JsResult<Value> {
    let time = this_time(agent, call.this)?;
    let mut d = to_temporal_duration(agent, call.arg(0))?;
    if subtract {
        d = d.negated();
    }
    let (_, result) = time.add(d.to_internal_24h().time);
    create_time(agent, result, None)
}

fn add(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    add_duration(agent, &call, false)
}

fn subtract(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    add_duration(agent, &call, true)
}

fn with(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let time = this_time(agent, call.this)?;
    let like = require_partial_temporal_object(agent, call.arg(0))?;
    let partial = prepare_fields(agent, like, false, true, true)?;
    let fields = merge_fields(fields_of(IsoDate::new(1970, 1, 1), Some(time)), partial);
    let options = get_options_object(agent, call.arg(1))?;
    let overflow = overflow_option(agent, options)?;
    let result = time_from_fields(agent, &fields, overflow)?;
    create_time(agent, result, None)
}

/// DifferenceTemporalPlainTime(operation, temporalTime, other, options).
fn difference(agent: &mut Agent, call: &NativeCall<'_>, since: bool) -> JsResult<Value> {
    let time = this_time(agent, call.this)?;
    let other = to_temporal_time(agent, call.arg(0), Value::Undefined)?;
    let options = get_options_object(agent, call.arg(1))?;
    let s = difference_settings(
        agent,
        since,
        options,
        UnitGroup::Time,
        Unit::Nanosecond,
        Unit::Hour,
    )?;
    let diff = time.difference(other);
    let rounded = tr(
        agent,
        round_time_duration(diff, i128::from(s.increment), s.smallest, s.mode),
    )?;
    let internal = InternalDuration {
        date: DateDuration::default(),
        time: rounded,
    };
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
    let time = this_time(agent, call.this)?;
    let options = round_to_options(agent, call.arg(0))?;
    let increment = rounding_increment_option(agent, options)?;
    let mode = rounding_mode_option(agent, options, RoundingMode::HalfExpand)?;
    let smallest = unit_option(agent, options, "smallestUnit")?;
    if smallest == UnitValue::Unset {
        return agent.range_error("smallestUnit is required");
    }
    validate_unit(agent, smallest, UnitGroup::Time, &[])?;
    let UnitValue::Unit(smallest) = smallest else {
        return agent.range_error("smallestUnit is required");
    };
    let maximum = smallest.maximum_increment().unwrap_or(1);
    validate_increment(agent, increment, maximum, false)?;
    let (_, result) = time.round(i128::from(increment), smallest, mode);
    create_time(agent, result, None)
}

fn equals(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let time = this_time(agent, call.this)?;
    let other = to_temporal_time(agent, call.arg(0), Value::Undefined)?;
    Ok(Value::Boolean(time == other))
}

fn to_string(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let time = this_time(agent, call.this)?;
    let options = get_options_object(agent, call.arg(0))?;
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
    let (_, rounded) = time.round(precision.increment, precision.unit, mode);
    let s = format_time(rounded, precision.precision);
    Ok(str_value(agent, &s))
}

fn to_json(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let time = this_time(agent, call.this)?;
    Ok(str_value(agent, &format_time(time, Precision::Auto)))
}
