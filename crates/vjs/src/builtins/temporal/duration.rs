//! `Temporal.Duration` (Temporal proposal §7).

use std::cmp::Ordering;

use super::{
    TemporalSlots, UnitGroup, UnitValue, create_duration, fractional_digits_option,
    get_options_object, getter_index, indexed_getter, number, relative_to_option, round_to_options,
    rounding_increment_option, rounding_mode_option, slots, to_integer_if_integral,
    to_partial_duration, to_temporal_duration, tr, unit_option, validate_increment, validate_unit,
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
    DURATION_FIELDS, DateDuration, DateTime, Duration, InternalDuration, NS_PER_DAY, Overflow,
    Precision, RoundingMode, Time, Unit, add_24h_days, calendar_date_add, check_time_duration,
    date_duration_days, difference_plain_date_time_with_rounding,
    difference_plain_date_time_with_total, duration_from_internal, format_duration,
    round_time_duration, round_to_increment, seconds_precision, total_time_duration,
};
use crate::value::Value;

pub(crate) fn install(agent: &mut Agent, realm: Realm, temporal: Object) {
    let object_proto = intrinsic(agent, realm, Intrinsic::ObjectPrototype);
    let proto = plain_object(agent, object_proto);
    set_intrinsic(agent, realm, Intrinsic::TemporalDurationPrototype, proto);
    let ctor = constructor(
        agent,
        realm,
        "Duration",
        duration_constructor,
        0,
        proto,
        None,
    );
    set_intrinsic(agent, realm, Intrinsic::TemporalDuration, ctor);
    let key = agent.heap.key_from_str("Duration");
    value(agent, temporal, key, Value::Object(ctor), true, false, true);
    method(agent, realm, ctor, "compare", compare, 2);
    method(agent, realm, ctor, "from", from, 1);
    for (i, name) in DURATION_FIELDS.iter().enumerate() {
        indexed_getter(agent, realm, proto, name, field_getter, i);
    }
    indexed_getter(agent, realm, proto, "sign", field_getter, 10);
    indexed_getter(agent, realm, proto, "blank", field_getter, 11);
    method(agent, realm, proto, "abs", abs, 0);
    method(agent, realm, proto, "add", add, 1);
    method(agent, realm, proto, "negated", negated, 0);
    method(agent, realm, proto, "round", round, 1);
    method(agent, realm, proto, "subtract", subtract, 1);
    method(agent, realm, proto, "toJSON", to_json, 0);
    method(agent, realm, proto, "toLocaleString", to_json, 0);
    method(agent, realm, proto, "toString", to_string, 0);
    method(agent, realm, proto, "total", total, 1);
    method(agent, realm, proto, "valueOf", value_of, 0);
    method(agent, realm, proto, "with", with, 1);
    to_string_tag(agent, proto, "Temporal.Duration");
}

fn this_duration(agent: &mut Agent, v: Value) -> JsResult<Duration> {
    match slots(agent, v) {
        Some(TemporalSlots::Duration(d)) => Ok(d),
        _ => agent.type_error("this is not a Temporal.Duration"),
    }
}

/// `new Temporal.Duration(years, …, nanoseconds)`.
fn duration_constructor(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let Some(new_target) = call.new_target else {
        return agent.type_error("Temporal.Duration must be called with new");
    };
    let mut f = [0.0; 10];
    for (i, slot) in f.iter_mut().enumerate() {
        let v = call.arg(i);
        if !v.is_undefined() {
            *slot = to_integer_if_integral(agent, v)?;
        }
    }
    create_duration(agent, Duration::from_fields(f), Some(new_target))
}

fn from(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let d = to_temporal_duration(agent, call.arg(0))?;
    create_duration(agent, d, None)
}

/// `Temporal.Duration.compare(one, two [, options])`.
fn compare(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let one = to_temporal_duration(agent, call.arg(0))?;
    let two = to_temporal_duration(agent, call.arg(1))?;
    let options = get_options_object(agent, call.arg(2))?;
    let relative = relative_to_option(agent, options)?;
    if one.fields() == two.fields() {
        return Ok(Value::Number(0.0));
    }
    let (lu1, lu2) = (one.default_largest_unit(), two.default_largest_unit());
    let (d1, d2) = (one.to_internal(), two.to_internal());
    let (days1, days2) = if lu1.is_calendar() || lu2.is_calendar() {
        let Some(rel) = relative else {
            return agent.range_error("comparing calendar units requires relativeTo");
        };
        let a = tr(agent, date_duration_days(d1.date, rel))?;
        let b = tr(agent, date_duration_days(d2.date, rel))?;
        (a, b)
    } else {
        (one.days as i64, two.days as i64)
    };
    let t1 = tr(agent, add_24h_days(d1.time, days1))?;
    let t2 = tr(agent, add_24h_days(d2.time, days2))?;
    Ok(Value::Number(match t1.cmp(&t2) {
        Ordering::Less => -1.0,
        Ordering::Equal => 0.0,
        Ordering::Greater => 1.0,
    }))
}

/// The field getters, `sign` (10) and `blank` (11).
fn field_getter(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let d = this_duration(agent, call.this)?;
    Ok(match getter_index(agent, call.callee) {
        i @ 0..=9 => number(d.fields()[i]),
        10 => number(f64::from(d.sign())),
        _ => Value::Boolean(d.sign() == 0),
    })
}

fn with(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let d = this_duration(agent, call.this)?;
    let partial = to_partial_duration(agent, call.arg(0))?;
    let mut f = d.fields();
    for (i, v) in partial.iter().enumerate() {
        if let Some(v) = v {
            f[i] = *v;
        }
    }
    create_duration(agent, Duration::from_fields(f), None)
}

fn negated(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let d = this_duration(agent, call.this)?;
    create_duration(agent, d.negated(), None)
}

fn abs(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let d = this_duration(agent, call.this)?;
    create_duration(agent, d.abs(), None)
}

/// AddDurations(operation, duration, other).
fn add_durations(agent: &mut Agent, call: &NativeCall<'_>, subtract: bool) -> JsResult<Value> {
    let d = this_duration(agent, call.this)?;
    let mut other = to_temporal_duration(agent, call.arg(0))?;
    if subtract {
        other = other.negated();
    }
    let largest = d
        .default_largest_unit()
        .larger(other.default_largest_unit());
    if largest.is_calendar() {
        return agent.range_error("adding durations with years, months or weeks needs a date");
    }
    let sum = d.to_internal_24h().time + other.to_internal_24h().time;
    let time = tr(agent, check_time_duration(sum))?;
    let internal = InternalDuration {
        date: DateDuration::default(),
        time,
    };
    create_duration(agent, duration_from_internal(internal, largest), None)
}

fn add(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    add_durations(agent, &call, false)
}

fn subtract(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    add_durations(agent, &call, true)
}

/// The steps of `round` and `total` with a plain relativeTo: the
/// date-time at midnight of the date and the date-time the duration
/// reaches from it.
fn relative_target(
    agent: &mut Agent,
    d: &Duration,
    relative: crate::temporal::IsoDate,
) -> JsResult<(DateTime, DateTime)> {
    let internal = d.to_internal_24h();
    let (days, target_time) = Time::MIDNIGHT.add(internal.time);
    let date = DateDuration {
        days,
        ..internal.date
    };
    tr(agent, date.validate())?;
    let target_date = tr(
        agent,
        calendar_date_add(relative, date, Overflow::Constrain),
    )?;
    Ok((
        DateTime::new(relative, Time::MIDNIGHT),
        DateTime::new(target_date, target_time),
    ))
}

/// `round(roundTo)`.
fn round(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let d = this_duration(agent, call.this)?;
    let options = round_to_options(agent, call.arg(0))?;
    let largest = unit_option(agent, options, "largestUnit")?;
    let relative = relative_to_option(agent, options)?;
    let increment = rounding_increment_option(agent, options)?;
    let mode = rounding_mode_option(agent, options, RoundingMode::HalfExpand)?;
    let smallest = unit_option(agent, options, "smallestUnit")?;
    validate_unit(agent, smallest, UnitGroup::DateTime, &[])?;
    let smallest_present = smallest != UnitValue::Unset;
    let smallest = match smallest {
        UnitValue::Unit(u) => u,
        _ => Unit::Nanosecond,
    };
    let existing_largest = d.default_largest_unit();
    let default_largest = existing_largest.larger(smallest);
    let largest_present = largest != UnitValue::Unset;
    let largest = match largest {
        UnitValue::Unit(u) => u,
        _ => default_largest,
    };
    if !smallest_present && !largest_present {
        return agent.range_error("round requires smallestUnit or largestUnit");
    }
    if largest.larger(smallest) != largest {
        return agent.range_error("largestUnit must be larger than smallestUnit");
    }
    if let Some(maximum) = smallest.maximum_increment() {
        validate_increment(agent, increment, maximum, false)?;
    }
    if increment > 1 && largest != smallest && smallest.is_date() {
        return agent.range_error("roundingIncrement must be 1 when rounding to a date unit");
    }
    if let Some(rel) = relative {
        let (start, target) = relative_target(agent, &d, rel)?;
        let internal = tr(
            agent,
            difference_plain_date_time_with_rounding(
                start, target, largest, increment, smallest, mode,
            ),
        )?;
        return create_duration(agent, duration_from_internal(internal, largest), None);
    }
    if existing_largest.is_calendar() || largest.is_calendar() {
        return agent.range_error("rounding years, months or weeks requires relativeTo");
    }
    let internal = d.to_internal_24h();
    let internal = if smallest == Unit::Day {
        let unit = NS_PER_DAY * i128::from(increment);
        let days = round_to_increment(internal.time, unit, mode) / NS_PER_DAY;
        let date = DateDuration {
            days: days as i64,
            ..DateDuration::default()
        };
        tr(agent, date.validate())?;
        InternalDuration { date, time: 0 }
    } else {
        let time = tr(
            agent,
            round_time_duration(internal.time, i128::from(increment), smallest, mode),
        )?;
        InternalDuration {
            date: DateDuration::default(),
            time,
        }
    };
    create_duration(agent, duration_from_internal(internal, largest), None)
}

/// `total(totalOf)`.
fn total(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let d = this_duration(agent, call.this)?;
    let options = match call.arg(0) {
        Value::Undefined => return agent.type_error("total requires a unit"),
        v @ Value::String(_) => {
            let o = agent
                .heap
                .create_object(None, crate::object::ObjectKind::Ordinary);
            let key = agent.heap.key_from_str("unit");
            crate::ops::create_data_property_or_throw(agent, o, key, v)?;
            o
        }
        v => get_options_object(agent, v)?,
    };
    let relative = relative_to_option(agent, options)?;
    let unit = unit_option(agent, options, "unit")?;
    if unit == UnitValue::Unset {
        return agent.range_error("unit is required");
    }
    validate_unit(agent, unit, UnitGroup::DateTime, &[])?;
    let UnitValue::Unit(unit) = unit else {
        return agent.range_error("unit is required");
    };
    let total = if let Some(rel) = relative {
        let (start, target) = relative_target(agent, &d, rel)?;
        tr(
            agent,
            difference_plain_date_time_with_total(start, target, unit),
        )?
    } else {
        if d.default_largest_unit().is_calendar() || unit.is_calendar() {
            return agent.range_error("totals of years, months or weeks require relativeTo");
        }
        total_time_duration(d.to_internal_24h().time, unit)
    };
    Ok(Value::Number(total))
}

/// `toString([options])`.
fn to_string(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let d = this_duration(agent, call.this)?;
    let options = get_options_object(agent, call.arg(0))?;
    let digits = fractional_digits_option(agent, options)?;
    let mode = rounding_mode_option(agent, options, RoundingMode::Trunc)?;
    let smallest = unit_option(agent, options, "smallestUnit")?;
    validate_unit(agent, smallest, UnitGroup::Time, &[])?;
    let smallest = match smallest {
        UnitValue::Unit(Unit::Hour | Unit::Minute) => {
            return agent.range_error("smallestUnit must be seconds or smaller");
        }
        UnitValue::Unit(u) => Some(u),
        _ => None,
    };
    let precision = seconds_precision(smallest, digits);
    if precision.unit == Unit::Nanosecond && precision.increment == 1 {
        let s = format_duration(&d, precision.precision);
        return Ok(str_value(agent, &s));
    }
    let largest = d.default_largest_unit();
    let mut internal = d.to_internal();
    internal.time = tr(
        agent,
        round_time_duration(internal.time, precision.increment, precision.unit, mode),
    )?;
    let rounded = duration_from_internal(internal, largest.larger(Unit::Second));
    if !rounded.is_valid() {
        return agent.range_error("duration out of range");
    }
    let s = format_duration(&rounded, precision.precision);
    Ok(str_value(agent, &s))
}

fn to_json(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let d = this_duration(agent, call.this)?;
    let s = format_duration(&d, Precision::Auto);
    Ok(str_value(agent, &s))
}
