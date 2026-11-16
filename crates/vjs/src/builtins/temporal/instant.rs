//! `Temporal.Instant` (Temporal proposal §8).

use super::{
    TemporalSlots, UnitGroup, UnitValue, bigint_from_i128, create_duration, create_instant,
    create_zoned, difference_settings, fractional_digits_option, get_options_object, get_prop,
    getter_index, i128_from_bigint, indexed_getter, number, round_to_options,
    rounding_increment_option, rounding_mode_option, slots, to_temporal_duration,
    to_temporal_instant, to_time_zone_identifier, tr, unit_option, validate_increment,
    validate_unit, value_of,
};
use crate::agent::Agent;
use crate::builtins::{
    constructor, intrinsic, method, plain_object, set_intrinsic, str_value, to_string_tag, value,
};
use crate::completion::JsResult;
use crate::function::NativeCall;
use crate::gc::{Object, Realm};
use crate::ops;
use crate::realm::Intrinsic;
use crate::temporal::{
    DateDuration, DateTime, InternalDuration, NS_PER_DAY, Precision, RoundingMode, Unit,
    duration_from_internal, format_date_time, format_offset_rounded, round_as_if_positive,
    round_time_duration, seconds_precision,
};
use crate::value::Value;

pub(crate) fn install(agent: &mut Agent, realm: Realm, temporal: Object) {
    let object_proto = intrinsic(agent, realm, Intrinsic::ObjectPrototype);
    let proto = plain_object(agent, object_proto);
    set_intrinsic(agent, realm, Intrinsic::TemporalInstantPrototype, proto);
    let ctor = constructor(agent, realm, "Instant", instant_constructor, 1, proto, None);
    set_intrinsic(agent, realm, Intrinsic::TemporalInstant, ctor);
    let key = agent.heap.key_from_str("Instant");
    value(agent, temporal, key, Value::Object(ctor), true, false, true);
    method(agent, realm, ctor, "compare", compare, 2);
    method(agent, realm, ctor, "from", from, 1);
    method(
        agent,
        realm,
        ctor,
        "fromEpochMilliseconds",
        from_epoch_milliseconds,
        1,
    );
    method(
        agent,
        realm,
        ctor,
        "fromEpochNanoseconds",
        from_epoch_nanoseconds,
        1,
    );
    indexed_getter(agent, realm, proto, "epochMilliseconds", epoch_getter, 0);
    indexed_getter(agent, realm, proto, "epochNanoseconds", epoch_getter, 1);
    method(agent, realm, proto, "add", add, 1);
    method(agent, realm, proto, "equals", equals, 1);
    method(agent, realm, proto, "round", round, 1);
    method(agent, realm, proto, "since", since, 1);
    method(agent, realm, proto, "subtract", subtract, 1);
    method(agent, realm, proto, "toJSON", to_json, 0);
    method(agent, realm, proto, "toLocaleString", to_json, 0);
    method(agent, realm, proto, "toString", to_string, 0);
    method(
        agent,
        realm,
        proto,
        "toZonedDateTimeISO",
        to_zoned_date_time_iso,
        1,
    );
    method(agent, realm, proto, "until", until, 1);
    method(agent, realm, proto, "valueOf", value_of, 0);
    to_string_tag(agent, proto, "Temporal.Instant");
}

fn this_instant(agent: &mut Agent, v: Value) -> JsResult<i128> {
    match slots(agent, v) {
        Some(TemporalSlots::Instant(ns)) => Ok(ns),
        _ => agent.type_error("this is not a Temporal.Instant"),
    }
}

/// Epoch nanoseconds from a BigInt argument.
fn epoch_ns_arg(agent: &mut Agent, v: Value) -> JsResult<i128> {
    let b = ops::to_bigint(agent, v)?;
    match i128_from_bigint(agent, b) {
        Some(ns) if crate::temporal::is_valid_epoch_ns(ns) => Ok(ns),
        _ => agent.range_error("epoch nanoseconds outside the supported range"),
    }
}

fn instant_constructor(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let Some(new_target) = call.new_target else {
        return agent.type_error("Temporal.Instant must be called with new");
    };
    let ns = epoch_ns_arg(agent, call.arg(0))?;
    Ok(Value::Object(create_instant(agent, ns, Some(new_target))?))
}

fn from(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let ns = to_temporal_instant(agent, call.arg(0))?;
    Ok(Value::Object(create_instant(agent, ns, None)?))
}

fn from_epoch_milliseconds(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let n = ops::to_number(agent, call.arg(0))?;
    if !n.is_finite() || n.trunc() != n {
        return agent.range_error("epoch milliseconds must be an integer");
    }
    if n.abs() > 8.64e15 {
        return agent.range_error("epoch milliseconds outside the supported range");
    }
    let ns = n as i128 * 1_000_000;
    Ok(Value::Object(create_instant(agent, ns, None)?))
}

fn from_epoch_nanoseconds(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let ns = epoch_ns_arg(agent, call.arg(0))?;
    Ok(Value::Object(create_instant(agent, ns, None)?))
}

fn compare(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let one = to_temporal_instant(agent, call.arg(0))?;
    let two = to_temporal_instant(agent, call.arg(1))?;
    Ok(Value::Number(match one.cmp(&two) {
        std::cmp::Ordering::Less => -1.0,
        std::cmp::Ordering::Equal => 0.0,
        std::cmp::Ordering::Greater => 1.0,
    }))
}

fn epoch_getter(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let ns = this_instant(agent, call.this)?;
    if getter_index(agent, call.callee) == 0 {
        Ok(number(ns.div_euclid(1_000_000) as f64))
    } else {
        Ok(bigint_from_i128(agent, ns))
    }
}

/// AddDurationToInstant(operation, instant, temporalDurationLike).
fn add_duration(agent: &mut Agent, call: &NativeCall<'_>, subtract: bool) -> JsResult<Value> {
    let ns = this_instant(agent, call.this)?;
    let mut d = to_temporal_duration(agent, call.arg(0))?;
    if subtract {
        d = d.negated();
    }
    if d.default_largest_unit().is_date() {
        return agent.range_error("instants cannot add years, months, weeks or days");
    }
    let result = ns + d.to_internal_24h().time;
    Ok(Value::Object(create_instant(agent, result, None)?))
}

fn add(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    add_duration(agent, &call, false)
}

fn subtract(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    add_duration(agent, &call, true)
}

/// DifferenceTemporalInstant(operation, instant, other, options).
fn difference(agent: &mut Agent, call: &NativeCall<'_>, since: bool) -> JsResult<Value> {
    let ns = this_instant(agent, call.this)?;
    let other = to_temporal_instant(agent, call.arg(0))?;
    let options = get_options_object(agent, call.arg(1))?;
    let settings = difference_settings(
        agent,
        since,
        options,
        UnitGroup::Time,
        Unit::Nanosecond,
        Unit::Second,
    )?;
    let time = tr(
        agent,
        round_time_duration(
            other - ns,
            i128::from(settings.increment),
            settings.smallest,
            settings.mode,
        ),
    )?;
    let internal = InternalDuration {
        date: DateDuration::default(),
        time,
    };
    let mut result = duration_from_internal(internal, settings.largest);
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
    let ns = this_instant(agent, call.this)?;
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
    let maximum = NS_PER_DAY / smallest.nanoseconds();
    validate_increment(agent, increment, maximum, true)?;
    let rounded = round_as_if_positive(ns, smallest.nanoseconds() * i128::from(increment), mode);
    Ok(Value::Object(create_instant(agent, rounded, None)?))
}

fn equals(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let ns = this_instant(agent, call.this)?;
    let other = to_temporal_instant(agent, call.arg(0))?;
    Ok(Value::Boolean(ns == other))
}

/// TemporalInstantToString(instant, timeZone, precision).
fn instant_string(ns: i128, tz: Option<&super::TimeZoneId>, precision: Precision) -> String {
    let offset = tz.map_or(0, |z| z.offset_ns(ns));
    let dt = DateTime::from_epoch_ns(ns + offset);
    let suffix = match tz {
        None => "Z".to_string(),
        Some(_) => format_offset_rounded(offset),
    };
    format!("{}{suffix}", format_date_time(dt, precision))
}

fn to_string(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let ns = this_instant(agent, call.this)?;
    let options = get_options_object(agent, call.arg(0))?;
    let digits = fractional_digits_option(agent, options)?;
    let mode = rounding_mode_option(agent, options, RoundingMode::Trunc)?;
    let smallest = unit_option(agent, options, "smallestUnit")?;
    let time_zone = get_prop(agent, options, "timeZone")?;
    validate_unit(agent, smallest, UnitGroup::Time, &[])?;
    let smallest = match smallest {
        UnitValue::Unit(Unit::Hour) => {
            return agent.range_error("smallestUnit must not be hour");
        }
        UnitValue::Unit(u) => Some(u),
        _ => None,
    };
    let tz = if time_zone.is_undefined() {
        None
    } else {
        Some(to_time_zone_identifier(agent, time_zone)?)
    };
    let precision = seconds_precision(smallest, digits);
    let rounded =
        round_as_if_positive(ns, precision.unit.nanoseconds() * precision.increment, mode);
    if !crate::temporal::is_valid_epoch_ns(rounded) {
        return agent.range_error("rounded instant outside the supported range");
    }
    let s = instant_string(rounded, tz.as_ref(), precision.precision);
    Ok(str_value(agent, &s))
}

/// `toZonedDateTimeISO(timeZone)`.
fn to_zoned_date_time_iso(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let ns = this_instant(agent, call.this)?;
    let tz = to_time_zone_identifier(agent, call.arg(0))?;
    create_zoned(agent, ns, tz, None)
}

fn to_json(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let ns = this_instant(agent, call.this)?;
    let s = instant_string(ns, None, Precision::Auto);
    Ok(str_value(agent, &s))
}
