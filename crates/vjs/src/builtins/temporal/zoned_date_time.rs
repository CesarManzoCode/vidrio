//! `Temporal.ZonedDateTime` (Temporal proposal §6): an exact instant in a
//! time zone. Wall-clock times are resolved with the `disambiguation` and
//! `offset` options through `crate::temporal::zone`.

use super::plain_time::TIME_GETTERS;
use super::{
    DATE_GETTERS, ShowTimeZone, TemporalSlots, TimeZoneId, UnitGroup, UnitValue, ZONED_FIELDS,
    ZonedSlots, bigint_from_i128, create_date, create_date_time, create_duration, create_instant,
    create_time, create_zoned, date_from_fields, date_getter, difference_settings,
    disambiguation_option, fields_of, fractional_digits_option, get_options_object,
    get_string_option, getter_index, i128_from_bigint, indexed_getter, merge_fields, number,
    offset_option, overflow_option, prepare_named_fields, require_partial_temporal_object,
    round_to_options, rounding_increment_option, rounding_mode_option, show_calendar_option,
    show_offset_option, show_time_zone_option, slots, time_from_fields, to_calendar_identifier,
    to_temporal_duration, to_temporal_time, to_temporal_zoned_date_time, to_time_zone_identifier,
    tr, unit_option, validate_increment, validate_unit, value_of,
};
use crate::agent::Agent;
use crate::builtins::{
    constructor, intrinsic, method, plain_object, set_intrinsic, str_value, to_string_tag, value,
};
use crate::completion::JsResult;
use crate::function::NativeCall;
use crate::gc::{Object, Realm};
use crate::object::ObjectKind;
use crate::ops;
use crate::realm::Intrinsic;
use crate::temporal::zone::{
    Disambiguation, OffsetBehaviour, OffsetOption, add_zoned_date_time, difference_instant,
    difference_zoned_date_time_with_rounding, format_utc_offset_ns, hours_in_day,
    interpret_iso_date_time_offset, round_to_day,
};
use crate::temporal::{
    Precision, RoundingMode, ShowCalendar, Unit, duration_from_internal,
    format_calendar_annotation, format_date_time, format_offset_rounded, round_as_if_positive,
    seconds_precision,
};
use crate::value::Value;

/// The getters after the date and time getters.
const ZONED_GETTERS: [&str; 6] = [
    "timeZoneId",
    "epochMilliseconds",
    "epochNanoseconds",
    "hoursInDay",
    "offsetNanoseconds",
    "offset",
];

pub(crate) fn install(agent: &mut Agent, realm: Realm, temporal: Object) {
    let object_proto = intrinsic(agent, realm, Intrinsic::ObjectPrototype);
    let proto = plain_object(agent, object_proto);
    set_intrinsic(
        agent,
        realm,
        Intrinsic::TemporalZonedDateTimePrototype,
        proto,
    );
    let ctor = constructor(
        agent,
        realm,
        "ZonedDateTime",
        zoned_date_time_constructor,
        2,
        proto,
        None,
    );
    set_intrinsic(agent, realm, Intrinsic::TemporalZonedDateTime, ctor);
    let key = agent.heap.key_from_str("ZonedDateTime");
    value(agent, temporal, key, Value::Object(ctor), true, false, true);
    method(agent, realm, ctor, "compare", compare, 2);
    method(agent, realm, ctor, "from", from, 1);
    let names = DATE_GETTERS
        .iter()
        .chain(TIME_GETTERS.iter())
        .chain(ZONED_GETTERS.iter());
    for (i, name) in names.enumerate() {
        indexed_getter(agent, realm, proto, name, field_getter, i);
    }
    let methods: [(&str, crate::function::NativeFn, u32); 20] = [
        ("add", add, 1),
        ("equals", equals, 1),
        ("getTimeZoneTransition", get_time_zone_transition, 1),
        ("round", round, 1),
        ("since", since, 1),
        ("startOfDay", start_of_day, 0),
        ("subtract", subtract, 1),
        ("toInstant", to_instant, 0),
        ("toJSON", to_json, 0),
        ("toLocaleString", to_json, 0),
        ("toPlainDate", to_plain_date, 0),
        ("toPlainDateTime", to_plain_date_time, 0),
        ("toPlainTime", to_plain_time, 0),
        ("toString", to_string, 0),
        ("until", until, 1),
        ("valueOf", value_of, 0),
        ("with", with, 1),
        ("withCalendar", with_calendar, 1),
        ("withPlainTime", with_plain_time, 0),
        ("withTimeZone", with_time_zone, 1),
    ];
    for (name, f, length) in methods {
        method(agent, realm, proto, name, f, length);
    }
    to_string_tag(agent, proto, "Temporal.ZonedDateTime");
}

fn this_zoned(agent: &mut Agent, v: Value) -> JsResult<ZonedSlots> {
    match slots(agent, v) {
        Some(TemporalSlots::ZonedDateTime(z)) => Ok(z),
        _ => agent.type_error("this is not a Temporal.ZonedDateTime"),
    }
}

/// `new Temporal.ZonedDateTime(epochNanoseconds, timeZone [, calendar])`.
fn zoned_date_time_constructor(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let Some(new_target) = call.new_target else {
        return agent.type_error("Temporal.ZonedDateTime must be called with new");
    };
    let b = ops::to_bigint(agent, call.arg(0))?;
    let ns = match i128_from_bigint(agent, b) {
        Some(ns) if crate::temporal::is_valid_epoch_ns(ns) => ns,
        _ => return agent.range_error("epoch nanoseconds outside the supported range"),
    };
    let Value::String(s) = call.arg(1) else {
        return agent.type_error("time zone must be a string");
    };
    let text = agent.heap.string_to_rust(s);
    let tz = tr(agent, TimeZoneId::from_identifier(&text))?;
    super::plain_date::calendar_argument(agent, call.arg(2))?;
    create_zoned(agent, ns, tz, Some(new_target))
}

fn from(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let z = to_temporal_zoned_date_time(agent, call.arg(0), call.arg(1))?;
    create_zoned(agent, z.ns, z.tz, None)
}

fn compare(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let one = to_temporal_zoned_date_time(agent, call.arg(0), Value::Undefined)?;
    let two = to_temporal_zoned_date_time(agent, call.arg(1), Value::Undefined)?;
    Ok(Value::Number(match one.ns.cmp(&two.ns) {
        std::cmp::Ordering::Less => -1.0,
        std::cmp::Ordering::Equal => 0.0,
        std::cmp::Ordering::Greater => 1.0,
    }))
}

fn field_getter(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let z = this_zoned(agent, call.this)?;
    let i = getter_index(agent, call.callee);
    if i < DATE_GETTERS.len() {
        let dt = z.tz.date_time_for(z.ns);
        return Ok(date_getter(agent, dt.date, DATE_GETTERS[i]));
    }
    let i = i - DATE_GETTERS.len();
    if i < TIME_GETTERS.len() {
        let dt = z.tz.date_time_for(z.ns);
        return Ok(number(dt.time.fields()[i] as f64));
    }
    Ok(match ZONED_GETTERS[i - TIME_GETTERS.len()] {
        "timeZoneId" => str_value(agent, &z.tz.identifier()),
        "epochMilliseconds" => number(z.ns.div_euclid(1_000_000) as f64),
        "epochNanoseconds" => bigint_from_i128(agent, z.ns),
        "hoursInDay" => number(tr(agent, hours_in_day(z.ns, &z.tz))?),
        "offsetNanoseconds" => number(z.tz.offset_ns(z.ns) as f64),
        _ => str_value(agent, &format_utc_offset_ns(z.tz.offset_ns(z.ns))),
    })
}

/// `with(temporalZonedDateTimeLike [, options])`.
fn with(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let z = this_zoned(agent, call.this)?;
    let like = require_partial_temporal_object(agent, call.arg(0))?;
    let offset_ns = z.tz.offset_ns(z.ns);
    let dt = z.tz.date_time_for(z.ns);
    let names: Vec<&str> = ZONED_FIELDS
        .iter()
        .copied()
        .filter(|n| *n != "timeZone")
        .collect();
    let (partial, partial_zone) = prepare_named_fields(agent, like, &names, true)?;
    let fields = merge_fields(fields_of(dt.date, Some(dt.time)), partial);
    let new_offset = partial_zone.offset.map_or(offset_ns, i128::from);
    let options = get_options_object(agent, call.arg(1))?;
    let disambiguation = disambiguation_option(agent, options)?;
    let offset = offset_option(agent, options, OffsetOption::Prefer)?;
    let overflow = overflow_option(agent, options)?;
    let date = date_from_fields(agent, &fields, overflow)?;
    let time = time_from_fields(agent, &fields, overflow)?;
    let ns = tr(
        agent,
        interpret_iso_date_time_offset(
            date,
            Some(time),
            OffsetBehaviour::Option,
            new_offset,
            &z.tz,
            disambiguation,
            offset,
            false,
        ),
    )?;
    create_zoned(agent, ns, z.tz, None)
}

/// `withPlainTime([plainTimeLike])`: midnight is the start of the day.
fn with_plain_time(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let z = this_zoned(agent, call.this)?;
    let dt = z.tz.date_time_for(z.ns);
    let ns = if call.arg(0).is_undefined() {
        tr(agent, z.tz.start_of_day(dt.date))?
    } else {
        let time = to_temporal_time(agent, call.arg(0), Value::Undefined)?;
        let result = crate::temporal::DateTime::new(dt.date, time);
        tr(agent, z.tz.epoch_ns_for(result, Disambiguation::Compatible))?
    };
    create_zoned(agent, ns, z.tz, None)
}

fn with_time_zone(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let z = this_zoned(agent, call.this)?;
    let tz = to_time_zone_identifier(agent, call.arg(0))?;
    create_zoned(agent, z.ns, tz, None)
}

fn with_calendar(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let z = this_zoned(agent, call.this)?;
    to_calendar_identifier(agent, call.arg(0))?;
    create_zoned(agent, z.ns, z.tz, None)
}

/// AddDurationToZonedDateTime(operation, zonedDateTime,
/// temporalDurationLike, options).
fn add_duration(agent: &mut Agent, call: &NativeCall<'_>, subtract: bool) -> JsResult<Value> {
    let z = this_zoned(agent, call.this)?;
    let mut d = to_temporal_duration(agent, call.arg(0))?;
    if subtract {
        d = d.negated();
    }
    let options = get_options_object(agent, call.arg(1))?;
    let overflow = overflow_option(agent, options)?;
    let ns = tr(
        agent,
        add_zoned_date_time(z.ns, &z.tz, d.to_internal(), overflow),
    )?;
    create_zoned(agent, ns, z.tz, None)
}

fn add(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    add_duration(agent, &call, false)
}

fn subtract(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    add_duration(agent, &call, true)
}

/// DifferenceTemporalZonedDateTime(operation, zonedDateTime, other,
/// options).
fn difference(agent: &mut Agent, call: &NativeCall<'_>, since: bool) -> JsResult<Value> {
    let z = this_zoned(agent, call.this)?;
    let other = to_temporal_zoned_date_time(agent, call.arg(0), Value::Undefined)?;
    let options = get_options_object(agent, call.arg(1))?;
    let s = difference_settings(
        agent,
        since,
        options,
        UnitGroup::DateTime,
        Unit::Nanosecond,
        Unit::Hour,
    )?;
    if !s.largest.is_date() {
        let internal = tr(
            agent,
            difference_instant(z.ns, other.ns, s.increment, s.smallest, s.mode),
        )?;
        let mut result = duration_from_internal(internal, s.largest);
        if since {
            result = result.negated();
        }
        return create_duration(agent, result, None);
    }
    if !z.tz.equals(&other.tz) {
        return agent.range_error("differences in days or larger need the same time zone");
    }
    if z.ns == other.ns {
        return create_duration(agent, Default::default(), None);
    }
    let internal = tr(
        agent,
        difference_zoned_date_time_with_rounding(
            z.ns,
            other.ns,
            &z.tz,
            s.largest,
            s.increment,
            s.smallest,
            s.mode,
        ),
    )?;
    let mut result = duration_from_internal(internal, Unit::Hour);
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

/// `round(roundTo)`.
fn round(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let z = this_zoned(agent, call.this)?;
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
        return create_zoned(agent, z.ns, z.tz, None);
    }
    let ns = if smallest == Unit::Day {
        tr(agent, round_to_day(z.ns, &z.tz, mode))?
    } else {
        let dt = z.tz.date_time_for(z.ns);
        let rounded = dt.round(i128::from(increment), smallest, mode);
        let offset_ns = z.tz.offset_ns(z.ns);
        tr(
            agent,
            interpret_iso_date_time_offset(
                rounded.date,
                Some(rounded.time),
                OffsetBehaviour::Option,
                offset_ns,
                &z.tz,
                Disambiguation::Compatible,
                OffsetOption::Prefer,
                false,
            ),
        )?
    };
    create_zoned(agent, ns, z.tz, None)
}

fn equals(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let z = this_zoned(agent, call.this)?;
    let other = to_temporal_zoned_date_time(agent, call.arg(0), Value::Undefined)?;
    Ok(Value::Boolean(z.ns == other.ns && z.tz.equals(&other.tz)))
}

fn start_of_day(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let z = this_zoned(agent, call.this)?;
    let dt = z.tz.date_time_for(z.ns);
    let ns = tr(agent, z.tz.start_of_day(dt.date))?;
    create_zoned(agent, ns, z.tz, None)
}

/// `getTimeZoneTransition(directionParam)`: the next or previous change
/// of offset, or null.
fn get_time_zone_transition(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let z = this_zoned(agent, call.this)?;
    let options = match call.arg(0) {
        Value::Undefined => return agent.type_error("getTimeZoneTransition requires a direction"),
        v @ Value::String(_) => {
            let o = agent.heap.create_object(None, ObjectKind::Ordinary);
            let key = agent.heap.key_from_str("direction");
            ops::create_data_property_or_throw(agent, o, key, v)?;
            o
        }
        v => get_options_object(agent, v)?,
    };
    let direction = get_string_option(agent, options, "direction", &["next", "previous"])?;
    let Some(direction) = direction else {
        return agent.range_error("direction is required");
    };
    let transition = if direction == "next" {
        z.tz.next_transition(z.ns)
    } else {
        z.tz.previous_transition(z.ns)
    };
    match transition {
        Some(ns) => create_zoned(agent, ns, z.tz, None),
        None => Ok(Value::Null),
    }
}

fn to_instant(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let z = this_zoned(agent, call.this)?;
    Ok(Value::Object(create_instant(agent, z.ns, None)?))
}

fn to_plain_date(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let z = this_zoned(agent, call.this)?;
    let dt = z.tz.date_time_for(z.ns);
    create_date(agent, dt.date, None)
}

fn to_plain_time(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let z = this_zoned(agent, call.this)?;
    let dt = z.tz.date_time_for(z.ns);
    create_time(agent, dt.time, None)
}

fn to_plain_date_time(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let z = this_zoned(agent, call.this)?;
    let dt = z.tz.date_time_for(z.ns);
    create_date_time(agent, dt, None)
}

/// TemporalZonedDateTimeToString(zonedDateTime, precision, showCalendar,
/// showTimeZone, showOffset, increment, unit, roundingMode).
#[allow(clippy::too_many_arguments)]
fn zoned_string(
    z: &ZonedSlots,
    precision: Precision,
    show_calendar: ShowCalendar,
    show_time_zone: ShowTimeZone,
    show_offset: bool,
    increment: i128,
    unit: Unit,
    mode: RoundingMode,
) -> String {
    let ns = round_as_if_positive(z.ns, increment * unit.nanoseconds(), mode);
    let offset_ns = z.tz.offset_ns(ns);
    let dt = crate::temporal::DateTime::from_epoch_ns(ns + offset_ns);
    let mut s = format_date_time(dt, precision);
    if show_offset {
        s.push_str(&format_offset_rounded(offset_ns));
    }
    match show_time_zone {
        ShowTimeZone::Never => {}
        ShowTimeZone::Auto => s.push_str(&format!("[{}]", z.tz.identifier())),
        ShowTimeZone::Critical => s.push_str(&format!("[!{}]", z.tz.identifier())),
    }
    s.push_str(format_calendar_annotation(show_calendar));
    s
}

fn to_string(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let z = this_zoned(agent, call.this)?;
    let options = get_options_object(agent, call.arg(0))?;
    let show_calendar = show_calendar_option(agent, options)?;
    let digits = fractional_digits_option(agent, options)?;
    let show_offset = show_offset_option(agent, options)?;
    let mode = rounding_mode_option(agent, options, RoundingMode::Trunc)?;
    let smallest = unit_option(agent, options, "smallestUnit")?;
    let show_time_zone = show_time_zone_option(agent, options)?;
    validate_unit(agent, smallest, UnitGroup::Time, &[])?;
    let smallest = match smallest {
        UnitValue::Unit(Unit::Hour) => {
            return agent.range_error("smallestUnit must not be hour");
        }
        UnitValue::Unit(u) => Some(u),
        _ => None,
    };
    let precision = seconds_precision(smallest, digits);
    let s = zoned_string(
        &z,
        precision.precision,
        show_calendar,
        show_time_zone,
        show_offset,
        precision.increment,
        precision.unit,
        mode,
    );
    Ok(str_value(agent, &s))
}

fn to_json(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let z = this_zoned(agent, call.this)?;
    let s = zoned_string(
        &z,
        Precision::Auto,
        ShowCalendar::Auto,
        ShowTimeZone::Auto,
        true,
        1,
        Unit::Nanosecond,
        RoundingMode::Trunc,
    );
    Ok(str_value(agent, &s))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strings_of_offset_zones() {
        let z = ZonedSlots {
            ns: 1_500_000_000,
            tz: TimeZoneId::Offset(-300),
        };
        let s = zoned_string(
            &z,
            Precision::Auto,
            ShowCalendar::Auto,
            ShowTimeZone::Auto,
            true,
            1,
            Unit::Nanosecond,
            RoundingMode::Trunc,
        );
        assert_eq!(s, "1969-12-31T19:00:01.5-05:00[-05:00]");
        let s = zoned_string(
            &z,
            Precision::Digits(0),
            ShowCalendar::Always,
            ShowTimeZone::Critical,
            false,
            1,
            Unit::Second,
            RoundingMode::Trunc,
        );
        assert_eq!(s, "1969-12-31T19:00:01[!-05:00][u-ca=iso8601]");
    }
}
