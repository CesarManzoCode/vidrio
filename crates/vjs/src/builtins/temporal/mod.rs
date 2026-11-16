//! The `Temporal` namespace (Temporal proposal, ISO 8601 calendar only;
//! the other calendars belong to ECMA-402): `Temporal.Duration`,
//! `Temporal.Instant`, `Temporal.PlainDate`, `Temporal.PlainTime`,
//! `Temporal.PlainDateTime`, `Temporal.PlainYearMonth`,
//! `Temporal.PlainMonthDay`, `Temporal.ZonedDateTime` and `Temporal.Now`,
//! with the shared abstract operations of options, property bags and
//! conversions. The arithmetic is in `crate::temporal`.

pub mod duration;
pub mod instant;
pub mod now;
pub mod plain_date;
pub mod plain_date_time;
pub mod plain_month_day;
pub mod plain_time;
pub mod plain_year_month;
pub mod zoned_date_time;

use crate::agent::{Agent, ErrorKind};
use crate::bigint::BigInt;
use crate::builtins::{intrinsic, plain_object, set_intrinsic, to_string_tag};
use crate::completion::{JsResult, Throw};
use crate::gc::{Object, Realm};
use crate::object::ObjectKind;
use crate::ops::{self, PreferredType};
use crate::realm::Intrinsic;
use crate::temporal::parse::{self, Kind};
use crate::temporal::zone::{Disambiguation, OffsetBehaviour, OffsetOption};
use crate::temporal::{
    DateTime, Duration, IsoDate, Overflow, RoundingMode, ShowCalendar, TResult, TemporalError,
    Time, Unit,
};

/// A time zone identifier (an offset or an available named zone).
pub(crate) use crate::temporal::zone::TimeZone as TimeZoneId;
use crate::value::Value;

/// The internal slots of a Temporal object. The calendar is always
/// `iso8601`, so no slot stores it.
#[derive(Clone, Debug, PartialEq)]
pub enum TemporalSlots {
    Duration(Duration),
    Instant(i128),
    PlainDate(IsoDate),
    PlainTime(Time),
    PlainDateTime(DateTime),
    /// `[[ISODate]]` with the reference day.
    PlainYearMonth(IsoDate),
    /// `[[ISODate]]` with the reference year.
    PlainMonthDay(IsoDate),
    ZonedDateTime(ZonedSlots),
}

/// The internal slots of a ZonedDateTime.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ZonedSlots {
    pub ns: i128,
    pub tz: TimeZoneId,
}

pub(crate) fn install(agent: &mut Agent, realm: Realm) {
    let object_proto = intrinsic(agent, realm, Intrinsic::ObjectPrototype);
    let temporal = plain_object(agent, object_proto);
    to_string_tag(agent, temporal, "Temporal");
    set_intrinsic(agent, realm, Intrinsic::Temporal, temporal);
    duration::install(agent, realm, temporal);
    instant::install(agent, realm, temporal);
    plain_date::install(agent, realm, temporal);
    plain_time::install(agent, realm, temporal);
    plain_date_time::install(agent, realm, temporal);
    plain_year_month::install(agent, realm, temporal);
    plain_month_day::install(agent, realm, temporal);
    zoned_date_time::install(agent, realm, temporal);
    now::install(agent, realm, temporal);
}

// ----- Errors and slots -----

pub(crate) fn throw(agent: &mut Agent, e: TemporalError) -> Throw {
    match e {
        TemporalError::Range(m) => agent.throw_error(ErrorKind::Range, &m),
        TemporalError::Type(m) => agent.throw_error(ErrorKind::Type, &m),
    }
}

/// Turns a Temporal result into a JavaScript completion.
pub(crate) fn tr<T>(agent: &mut Agent, r: TResult<T>) -> JsResult<T> {
    r.map_err(|e| throw(agent, e))
}

pub(crate) fn slots(agent: &Agent, v: Value) -> Option<TemporalSlots> {
    match v {
        Value::Object(o) => match &agent.heap.object(o).kind {
            ObjectKind::Temporal(s) => Some((**s).clone()),
            _ => None,
        },
        _ => None,
    }
}

/// A new Temporal object with the prototype from `new_target` (or the
/// intrinsic `proto` of the current realm).
pub(crate) fn create(
    agent: &mut Agent,
    s: TemporalSlots,
    new_target: Option<Object>,
    proto: Intrinsic,
) -> JsResult<Object> {
    let kind = ObjectKind::Temporal(Box::new(s));
    match new_target {
        Some(nt) => ops::ordinary_create_from_constructor(agent, nt, proto, kind),
        None => {
            let p = agent.intrinsic(proto);
            Ok(agent.heap.create_object(Some(p), kind))
        }
    }
}

/// CreateTemporalInstant(epochNanoseconds, newTarget).
pub(crate) fn create_instant(
    agent: &mut Agent,
    ns: i128,
    new_target: Option<Object>,
) -> JsResult<Object> {
    if !crate::temporal::is_valid_epoch_ns(ns) {
        return agent.range_error("instant outside the supported range");
    }
    create(
        agent,
        TemporalSlots::Instant(ns),
        new_target,
        Intrinsic::TemporalInstantPrototype,
    )
}

/// CreateTemporalDate(isoDate, calendar, newTarget).
pub(crate) fn create_date(
    agent: &mut Agent,
    date: IsoDate,
    new_target: Option<Object>,
) -> JsResult<Value> {
    if !date.within_limits() {
        return agent.range_error("date outside the supported range");
    }
    let o = create(
        agent,
        TemporalSlots::PlainDate(date),
        new_target,
        Intrinsic::TemporalPlainDatePrototype,
    )?;
    Ok(Value::Object(o))
}

/// CreateTemporalTime(time, newTarget).
pub(crate) fn create_time(
    agent: &mut Agent,
    time: Time,
    new_target: Option<Object>,
) -> JsResult<Value> {
    let o = create(
        agent,
        TemporalSlots::PlainTime(time),
        new_target,
        Intrinsic::TemporalPlainTimePrototype,
    )?;
    Ok(Value::Object(o))
}

/// CreateTemporalDateTime(isoDateTime, calendar, newTarget).
pub(crate) fn create_date_time(
    agent: &mut Agent,
    dt: DateTime,
    new_target: Option<Object>,
) -> JsResult<Value> {
    if !dt.within_limits() {
        return agent.range_error("date-time outside the supported range");
    }
    let o = create(
        agent,
        TemporalSlots::PlainDateTime(dt),
        new_target,
        Intrinsic::TemporalPlainDateTimePrototype,
    )?;
    Ok(Value::Object(o))
}

/// CreateTemporalDuration(…, newTarget).
pub(crate) fn create_duration(
    agent: &mut Agent,
    d: Duration,
    new_target: Option<Object>,
) -> JsResult<Value> {
    if !d.is_valid() {
        return agent.range_error("duration out of range");
    }
    let o = create(
        agent,
        TemporalSlots::Duration(d),
        new_target,
        Intrinsic::TemporalDurationPrototype,
    )?;
    Ok(Value::Object(o))
}

/// ISOYearMonthWithinLimits(isoDate).
pub(crate) fn year_month_within_limits(date: IsoDate) -> bool {
    !(date.year < -271_821
        || date.year > 275_760
        || (date.year == -271_821 && date.month < 4)
        || (date.year == 275_760 && date.month > 9))
}

/// CreateTemporalYearMonth(isoDate, calendar, newTarget).
pub(crate) fn create_year_month(
    agent: &mut Agent,
    date: IsoDate,
    new_target: Option<Object>,
) -> JsResult<Value> {
    if !year_month_within_limits(date) {
        return agent.range_error("year-month outside the supported range");
    }
    let o = create(
        agent,
        TemporalSlots::PlainYearMonth(date),
        new_target,
        Intrinsic::TemporalPlainYearMonthPrototype,
    )?;
    Ok(Value::Object(o))
}

/// CreateTemporalMonthDay(isoDate, calendar, newTarget).
pub(crate) fn create_month_day(
    agent: &mut Agent,
    date: IsoDate,
    new_target: Option<Object>,
) -> JsResult<Value> {
    if !date.within_limits() {
        return agent.range_error("month-day outside the supported range");
    }
    let o = create(
        agent,
        TemporalSlots::PlainMonthDay(date),
        new_target,
        Intrinsic::TemporalPlainMonthDayPrototype,
    )?;
    Ok(Value::Object(o))
}

/// CreateTemporalZonedDateTime(epochNanoseconds, timeZone, calendar,
/// newTarget).
pub(crate) fn create_zoned(
    agent: &mut Agent,
    ns: i128,
    tz: TimeZoneId,
    new_target: Option<Object>,
) -> JsResult<Value> {
    if !crate::temporal::is_valid_epoch_ns(ns) {
        return agent.range_error("instant outside the supported range");
    }
    let o = create(
        agent,
        TemporalSlots::ZonedDateTime(ZonedSlots { ns, tz }),
        new_target,
        Intrinsic::TemporalZonedDateTimePrototype,
    )?;
    Ok(Value::Object(o))
}

// ----- BigInt epoch nanoseconds -----

pub(crate) fn bigint_from_i128(agent: &mut Agent, n: i128) -> Value {
    let hi = (n >> 64) as i64;
    let lo = n as u64;
    let b = BigInt::from_i64(hi).shl(64).add(&BigInt::from_u64(lo));
    Value::BigInt(agent.heap.alloc_bigint(b))
}

/// The value of a BigInt if it fits 120 bits.
pub(crate) fn i128_from_bigint(agent: &Agent, h: crate::gc::HeapBigInt) -> Option<i128> {
    let b = agent.heap.bigint(h);
    if b.bit_length() > 120 {
        return None;
    }
    b.to_string_radix(10).parse().ok()
}

// ----- Property access -----

pub(crate) fn get_prop(agent: &mut Agent, o: Object, name: &str) -> JsResult<Value> {
    let key = agent.heap.key_from_str(name);
    ops::get(agent, o, key)
}

pub(crate) fn rust_string(agent: &mut Agent, v: Value) -> JsResult<String> {
    let s = ops::to_string(agent, v)?;
    Ok(agent.heap.string_to_rust(s))
}

// ----- Number conversions -----

/// ToIntegerWithTruncation(argument).
pub(crate) fn to_integer_with_truncation(agent: &mut Agent, v: Value) -> JsResult<f64> {
    let n = ops::to_number(agent, v)?;
    if !n.is_finite() {
        return agent.range_error("value must be a finite number");
    }
    Ok(n.trunc() + 0.0)
}

/// ToPositiveIntegerWithTruncation(argument).
pub(crate) fn to_positive_integer_with_truncation(agent: &mut Agent, v: Value) -> JsResult<f64> {
    let n = to_integer_with_truncation(agent, v)?;
    if n <= 0.0 {
        return agent.range_error("value must be a positive integer");
    }
    Ok(n)
}

/// ToIntegerIfIntegral(argument).
pub(crate) fn to_integer_if_integral(agent: &mut Agent, v: Value) -> JsResult<f64> {
    let n = ops::to_number(agent, v)?;
    if !n.is_finite() || n.trunc() != n {
        return agent.range_error("value must be an integer");
    }
    Ok(n + 0.0)
}

/// A field value already known to be an integer, as i64 (values past
/// the supported range saturate and are rejected later).
pub(crate) fn to_i64(n: f64) -> i64 {
    n.clamp(-1e15, 1e15) as i64
}

// ----- Options -----

/// GetOptionsObject(options).
pub(crate) fn get_options_object(agent: &mut Agent, v: Value) -> JsResult<Object> {
    match v {
        Value::Undefined => Ok(agent.heap.create_object(None, ObjectKind::Ordinary)),
        Value::Object(o) => Ok(o),
        _ => agent.type_error("options must be an object"),
    }
}

/// GetOption(options, property, string, values, default).
pub(crate) fn get_string_option(
    agent: &mut Agent,
    o: Object,
    name: &str,
    allowed: &[&str],
) -> JsResult<Option<String>> {
    let v = get_prop(agent, o, name)?;
    if v.is_undefined() {
        return Ok(None);
    }
    let s = rust_string(agent, v)?;
    if !allowed.is_empty() && !allowed.contains(&s.as_str()) {
        return agent.range_error(&format!("invalid value for option {name}: {s}"));
    }
    Ok(Some(s))
}

/// GetTemporalOverflowOption(options).
pub(crate) fn overflow_option(agent: &mut Agent, o: Object) -> JsResult<Overflow> {
    Ok(
        match get_string_option(agent, o, "overflow", &["constrain", "reject"])?.as_deref() {
            Some("reject") => Overflow::Reject,
            _ => Overflow::Constrain,
        },
    )
}

/// GetTemporalDisambiguationOption(options).
pub(crate) fn disambiguation_option(agent: &mut Agent, o: Object) -> JsResult<Disambiguation> {
    let allowed = ["compatible", "earlier", "later", "reject"];
    Ok(get_string_option(agent, o, "disambiguation", &allowed)?
        .and_then(|s| Disambiguation::from_name(&s))
        .unwrap_or(Disambiguation::Compatible))
}

/// GetTemporalOffsetOption(options, fallback).
pub(crate) fn offset_option(
    agent: &mut Agent,
    o: Object,
    fallback: OffsetOption,
) -> JsResult<OffsetOption> {
    let allowed = ["prefer", "use", "ignore", "reject"];
    Ok(get_string_option(agent, o, "offset", &allowed)?
        .and_then(|s| OffsetOption::from_name(&s))
        .unwrap_or(fallback))
}

/// GetTemporalShowOffsetOption(options): whether to show the offset.
pub(crate) fn show_offset_option(agent: &mut Agent, o: Object) -> JsResult<bool> {
    let v = get_string_option(agent, o, "offset", &["auto", "never"])?;
    Ok(v.as_deref() != Some("never"))
}

/// The `timeZoneName` option of ZonedDateTime strings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ShowTimeZone {
    Auto,
    Never,
    Critical,
}

/// GetTemporalShowTimeZoneNameOption(options).
pub(crate) fn show_time_zone_option(agent: &mut Agent, o: Object) -> JsResult<ShowTimeZone> {
    let allowed = ["auto", "never", "critical"];
    Ok(
        match get_string_option(agent, o, "timeZoneName", &allowed)?.as_deref() {
            Some("never") => ShowTimeZone::Never,
            Some("critical") => ShowTimeZone::Critical,
            _ => ShowTimeZone::Auto,
        },
    )
}

/// GetRoundingModeOption(options, fallback).
pub(crate) fn rounding_mode_option(
    agent: &mut Agent,
    o: Object,
    fallback: RoundingMode,
) -> JsResult<RoundingMode> {
    let all = [
        "ceil",
        "floor",
        "expand",
        "trunc",
        "halfCeil",
        "halfFloor",
        "halfExpand",
        "halfTrunc",
        "halfEven",
    ];
    Ok(get_string_option(agent, o, "roundingMode", &all)?
        .and_then(|s| RoundingMode::from_name(&s))
        .unwrap_or(fallback))
}

/// GetRoundingIncrementOption(options).
pub(crate) fn rounding_increment_option(agent: &mut Agent, o: Object) -> JsResult<i64> {
    let v = get_prop(agent, o, "roundingIncrement")?;
    if v.is_undefined() {
        return Ok(1);
    }
    let n = to_integer_with_truncation(agent, v)?;
    if !(1.0..=1e9).contains(&n) {
        return agent.range_error("roundingIncrement must be between 1 and 10^9");
    }
    Ok(n as i64)
}

/// ValidateTemporalRoundingIncrement(increment, dividend, inclusive).
pub(crate) fn validate_increment(
    agent: &mut Agent,
    increment: i64,
    dividend: i128,
    inclusive: bool,
) -> JsResult<()> {
    let maximum = if inclusive { dividend } else { dividend - 1 };
    let inc = i128::from(increment);
    if inc > maximum || dividend % inc != 0 {
        return agent.range_error("invalid roundingIncrement for this unit");
    }
    Ok(())
}

/// GetTemporalFractionalSecondDigitsOption: `None` is `auto`.
pub(crate) fn fractional_digits_option(agent: &mut Agent, o: Object) -> JsResult<Option<u8>> {
    let v = get_prop(agent, o, "fractionalSecondDigits")?;
    match v {
        Value::Undefined => Ok(None),
        Value::Number(n) => {
            if !n.is_finite() {
                return agent.range_error("fractionalSecondDigits must be finite");
            }
            let d = n.floor();
            if !(0.0..=9.0).contains(&d) {
                return agent.range_error("fractionalSecondDigits must be between 0 and 9");
            }
            Ok(Some(d as u8))
        }
        _ => {
            let s = rust_string(agent, v)?;
            if s != "auto" {
                return agent.range_error("fractionalSecondDigits must be a number or 'auto'");
            }
            Ok(None)
        }
    }
}

/// The value of a unit-valued option.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UnitValue {
    Unset,
    Auto,
    Unit(Unit),
}

/// GetTemporalUnitValuedOption(options, key, default unset).
pub(crate) fn unit_option(agent: &mut Agent, o: Object, name: &str) -> JsResult<UnitValue> {
    let v = get_prop(agent, o, name)?;
    if v.is_undefined() {
        return Ok(UnitValue::Unset);
    }
    let s = rust_string(agent, v)?;
    if s == "auto" {
        return Ok(UnitValue::Auto);
    }
    match Unit::from_name(&s) {
        Some(u) => Ok(UnitValue::Unit(u)),
        None => agent.range_error(&format!("invalid unit for {name}: {s}")),
    }
}

/// The unit groups of ValidateTemporalUnitValue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UnitGroup {
    Date,
    Time,
    DateTime,
}

/// ValidateTemporalUnitValue(value, unitGroup, extraValues).
pub(crate) fn validate_unit(
    agent: &mut Agent,
    value: UnitValue,
    group: UnitGroup,
    extra: &[UnitValue],
) -> JsResult<()> {
    if value == UnitValue::Unset || extra.contains(&value) {
        return Ok(());
    }
    let ok = match value {
        UnitValue::Unit(u) => match group {
            UnitGroup::Date => u.is_date(),
            UnitGroup::Time => !u.is_date(),
            UnitGroup::DateTime => true,
        },
        _ => false,
    };
    if !ok {
        return agent.range_error("unit not allowed here");
    }
    Ok(())
}

/// GetTemporalShowCalendarNameOption(options).
pub(crate) fn show_calendar_option(agent: &mut Agent, o: Object) -> JsResult<ShowCalendar> {
    let allowed = ["auto", "always", "never", "critical"];
    Ok(
        match get_string_option(agent, o, "calendarName", &allowed)?.as_deref() {
            Some("always") => ShowCalendar::Always,
            Some("never") => ShowCalendar::Never,
            Some("critical") => ShowCalendar::Critical,
            _ => ShowCalendar::Auto,
        },
    )
}

/// The result of GetDifferenceSettings.
#[derive(Clone, Copy, Debug)]
pub(crate) struct DifferenceSettings {
    pub smallest: Unit,
    pub largest: Unit,
    pub mode: RoundingMode,
    pub increment: i64,
}

/// GetDifferenceSettings(operation, options, unitGroup, disallowedUnits,
/// fallbackSmallestUnit, smallestLargestDefaultUnit).
pub(crate) fn difference_settings(
    agent: &mut Agent,
    since: bool,
    options: Object,
    group: UnitGroup,
    fallback_smallest: Unit,
    smallest_largest_default: Unit,
) -> JsResult<DifferenceSettings> {
    difference_settings_with(
        agent,
        since,
        options,
        group,
        &[],
        fallback_smallest,
        smallest_largest_default,
    )
}

/// GetDifferenceSettings with disallowed units.
pub(crate) fn difference_settings_with(
    agent: &mut Agent,
    since: bool,
    options: Object,
    group: UnitGroup,
    disallowed: &[Unit],
    fallback_smallest: Unit,
    smallest_largest_default: Unit,
) -> JsResult<DifferenceSettings> {
    let largest = unit_option(agent, options, "largestUnit")?;
    let increment = rounding_increment_option(agent, options)?;
    let mode = rounding_mode_option(agent, options, RoundingMode::Trunc)?;
    let smallest = unit_option(agent, options, "smallestUnit")?;
    validate_unit(agent, largest, group, &[UnitValue::Auto])?;
    if let UnitValue::Unit(u) = largest
        && disallowed.contains(&u)
    {
        return agent.range_error("largestUnit not allowed here");
    }
    validate_unit(agent, smallest, group, &[])?;
    if let UnitValue::Unit(u) = smallest
        && disallowed.contains(&u)
    {
        return agent.range_error("smallestUnit not allowed here");
    }
    let smallest = match smallest {
        UnitValue::Unit(u) => u,
        _ => fallback_smallest,
    };
    let default_largest = smallest_largest_default.larger(smallest);
    let largest = match largest {
        UnitValue::Unit(u) => u,
        _ => default_largest,
    };
    if largest.larger(smallest) != largest {
        return agent.range_error("largestUnit must be larger than smallestUnit");
    }
    if let Some(maximum) = smallest.maximum_increment() {
        validate_increment(agent, increment, maximum, false)?;
    }
    let mode = if since { mode.negate() } else { mode };
    Ok(DifferenceSettings {
        smallest,
        largest,
        mode,
        increment,
    })
}

/// The options object of `round` methods: a string is the smallestUnit.
pub(crate) fn round_to_options(agent: &mut Agent, v: Value) -> JsResult<Object> {
    match v {
        Value::Undefined => agent.type_error("round requires options"),
        Value::String(_) => {
            let o = agent.heap.create_object(None, ObjectKind::Ordinary);
            let key = agent.heap.key_from_str("smallestUnit");
            ops::create_data_property_or_throw(agent, o, key, v)?;
            Ok(o)
        }
        _ => get_options_object(agent, v),
    }
}

// ----- Calendars and time zones -----

/// CanonicalizeCalendar: only `iso8601` exists without ECMA-402.
pub(crate) fn canonicalize_calendar(agent: &mut Agent, id: &str) -> JsResult<()> {
    if id.eq_ignore_ascii_case("iso8601") {
        Ok(())
    } else {
        agent.range_error(&format!("unknown calendar: {id}"))
    }
}

/// ToTemporalCalendarIdentifier(temporalCalendarLike).
pub(crate) fn to_calendar_identifier(agent: &mut Agent, v: Value) -> JsResult<()> {
    if matches!(
        slots(agent, v),
        Some(
            TemporalSlots::PlainDate(_)
                | TemporalSlots::PlainDateTime(_)
                | TemporalSlots::PlainYearMonth(_)
                | TemporalSlots::PlainMonthDay(_)
                | TemporalSlots::ZonedDateTime(_)
        )
    ) {
        return Ok(());
    }
    let Value::String(s) = v else {
        return agent.type_error("calendar must be a string");
    };
    let text = agent.heap.string_to_rust(s);
    let id = tr(agent, parse::parse_calendar_string(&text))?;
    canonicalize_calendar(agent, &id)
}

/// GetTemporalCalendarIdentifierWithISODefault(item).
pub(crate) fn calendar_with_iso_default(agent: &mut Agent, item: Object) -> JsResult<()> {
    if slots(agent, Value::Object(item)).is_some() {
        return Ok(());
    }
    let v = get_prop(agent, item, "calendar")?;
    if v.is_undefined() {
        return Ok(());
    }
    to_calendar_identifier(agent, v)
}

/// The calendar annotation of a parsed string, canonicalized.
pub(crate) fn parsed_calendar(agent: &mut Agent, parsed: &parse::Parsed) -> JsResult<()> {
    match &parsed.calendar {
        Some(id) => {
            let id = id.clone();
            canonicalize_calendar(agent, &id)
        }
        None => Ok(()),
    }
}

/// ToTemporalTimeZoneIdentifier(temporalTimeZoneLike).
pub(crate) fn to_time_zone_identifier(agent: &mut Agent, v: Value) -> JsResult<TimeZoneId> {
    if let Some(TemporalSlots::ZonedDateTime(z)) = slots(agent, v) {
        return Ok(z.tz);
    }
    let Value::String(s) = v else {
        return agent.type_error("time zone must be a string");
    };
    let text = agent.heap.string_to_rust(s);
    tr(agent, TimeZoneId::from_string(&text))
}

// ----- Property bags -----

/// The fields of PrepareCalendarFields for the ISO 8601 calendar.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Fields {
    pub year: Option<f64>,
    pub month: Option<f64>,
    /// The month number of `monthCode` and whether it had the leap `L`.
    pub month_code: Option<(i64, bool)>,
    pub day: Option<f64>,
    /// hour, minute, second, millisecond, microsecond, nanosecond.
    pub time: [Option<f64>; 6],
}

/// ToMonthCode(argument): the month number and the leap flag.
fn to_month_code(agent: &mut Agent, v: Value) -> JsResult<(i64, bool)> {
    let p = ops::to_primitive(agent, v, PreferredType::String)?;
    let Value::String(s) = p else {
        return agent.type_error("monthCode must be a string");
    };
    let text = agent.heap.string_to_rust(s);
    let b = text.as_bytes();
    let leap = b.len() == 4 && b[3] == b'L';
    let valid = (b.len() == 3 || leap)
        && b[0] == b'M'
        && b[1].is_ascii_digit()
        && b[2].is_ascii_digit()
        && !(b[1] == b'0' && b[2] == b'0' && !leap);
    if !valid {
        return agent.range_error(&format!("invalid monthCode: {text}"));
    }
    Ok((i64::from((b[1] - b'0') * 10 + (b[2] - b'0')), leap))
}

const TIME_FIELDS: [&str; 6] = [
    "hour",
    "minute",
    "second",
    "millisecond",
    "microsecond",
    "nanosecond",
];

/// PrepareCalendarFields(calendar, fields, «year, month, monthCode, day»
/// if `date`, the time fields if `time`, partial or not). Reads the
/// properties in code unit order and converts each as it goes.
pub(crate) fn prepare_fields(
    agent: &mut Agent,
    item: Object,
    date: bool,
    time: bool,
    partial: bool,
) -> JsResult<Fields> {
    let mut names: Vec<&str> = Vec::new();
    if date {
        names.extend(["year", "month", "monthCode", "day"]);
    }
    if time {
        names.extend(TIME_FIELDS);
    }
    Ok(prepare_named_fields(agent, item, &names, partial)?.0)
}

/// The offset and time zone fields of PrepareCalendarFields.
#[derive(Clone, Debug, Default)]
pub(crate) struct ZoneFields {
    /// The offset string, in nanoseconds.
    pub offset: Option<i64>,
    pub time_zone: Option<TimeZoneId>,
}

/// ToOffsetString(argument), in nanoseconds.
fn to_offset_string(agent: &mut Agent, v: Value) -> JsResult<i64> {
    let p = ops::to_primitive(agent, v, PreferredType::String)?;
    let Value::String(s) = p else {
        return agent.type_error("offset must be a string");
    };
    let text = agent.heap.string_to_rust(s);
    tr(agent, parse::parse_offset_string(&text))
}

/// PrepareCalendarFields over any of the names year, month, monthCode,
/// day, the time fields, offset and timeZone: reads them in code unit
/// order and converts each as it goes.
pub(crate) fn prepare_named_fields(
    agent: &mut Agent,
    item: Object,
    names: &[&str],
    partial: bool,
) -> JsResult<(Fields, ZoneFields)> {
    let mut names: Vec<&str> = names.to_vec();
    names.sort_unstable();
    let mut f = Fields::default();
    let mut z = ZoneFields::default();
    let mut any = false;
    for name in names {
        let v = get_prop(agent, item, name)?;
        if v.is_undefined() {
            continue;
        }
        any = true;
        match name {
            "year" => f.year = Some(to_integer_with_truncation(agent, v)?),
            "month" => f.month = Some(to_positive_integer_with_truncation(agent, v)?),
            "monthCode" => f.month_code = Some(to_month_code(agent, v)?),
            "day" => f.day = Some(to_positive_integer_with_truncation(agent, v)?),
            "offset" => z.offset = Some(to_offset_string(agent, v)?),
            "timeZone" => z.time_zone = Some(to_time_zone_identifier(agent, v)?),
            _ => {
                let i = TIME_FIELDS
                    .iter()
                    .position(|n| *n == name)
                    .expect("a time field");
                f.time[i] = Some(to_integer_with_truncation(agent, v)?);
            }
        }
    }
    if partial && !any {
        return agent.type_error("no recognized properties");
    }
    Ok((f, z))
}

/// The field names of a date-time with an offset and a time zone.
pub(crate) const ZONED_FIELDS: [&str; 12] = [
    "year",
    "month",
    "monthCode",
    "day",
    "hour",
    "minute",
    "second",
    "millisecond",
    "microsecond",
    "nanosecond",
    "offset",
    "timeZone",
];

/// CalendarResolveFields for the month: the month number of `month` and
/// `monthCode`, which must agree.
fn resolve_month(agent: &mut Agent, f: &Fields) -> JsResult<f64> {
    match (f.month, f.month_code) {
        (None, None) => agent.type_error("month or monthCode is required"),
        (m, Some((code, leap))) => {
            if leap || !(1..=12).contains(&code) {
                return agent.range_error("monthCode not in the ISO 8601 calendar");
            }
            if let Some(m) = m
                && m != code as f64
            {
                return agent.range_error("month and monthCode disagree");
            }
            Ok(code as f64)
        }
        (Some(m), None) => Ok(m),
    }
}

/// CalendarYearMonthFromFields for the ISO 8601 calendar: the first day
/// of the month.
pub(crate) fn year_month_from_fields(
    agent: &mut Agent,
    f: &Fields,
    overflow: Overflow,
) -> JsResult<IsoDate> {
    let Some(year) = f.year else {
        return agent.type_error("year is required");
    };
    let month = resolve_month(agent, f)?;
    if year.abs() > 1e9 {
        return agent.range_error("year-month outside the supported range");
    }
    let date = tr(
        agent,
        IsoDate::regulate(year as i64, to_i64(month), 1, overflow),
    )?;
    if !year_month_within_limits(date) {
        return agent.range_error("year-month outside the supported range");
    }
    Ok(date)
}

/// CalendarMonthDayFromFields for the ISO 8601 calendar: the date in the
/// reference year 1972 (the year, if given, only regulates the day).
pub(crate) fn month_day_from_fields(
    agent: &mut Agent,
    f: &Fields,
    overflow: Overflow,
) -> JsResult<IsoDate> {
    let Some(day) = f.day else {
        return agent.type_error("day is required");
    };
    let month = resolve_month(agent, f)?;
    let year = match f.year {
        Some(y) if y.abs() > 1e9 => return agent.range_error("year outside the supported range"),
        Some(y) => y as i64,
        None => 1972,
    };
    let date = tr(
        agent,
        IsoDate::regulate(year, to_i64(month), to_i64(day), overflow),
    )?;
    Ok(IsoDate::new(1972, date.month, date.day))
}

/// CalendarDateFromFields for the ISO 8601 calendar (CalendarResolveFields
/// and RegulateISODate, then the limit check).
pub(crate) fn date_from_fields(
    agent: &mut Agent,
    f: &Fields,
    overflow: Overflow,
) -> JsResult<IsoDate> {
    let Some(year) = f.year else {
        return agent.type_error("year is required");
    };
    let Some(day) = f.day else {
        return agent.type_error("day is required");
    };
    let month = resolve_month(agent, f)?;
    if year.abs() > 1e9 {
        return agent.range_error("date outside the supported range");
    }
    let date = tr(
        agent,
        IsoDate::regulate(year as i64, to_i64(month), to_i64(day), overflow),
    )?;
    if !date.within_limits() {
        return agent.range_error("date outside the supported range");
    }
    Ok(date)
}

/// The time fields of a record with the missing ones zero, regulated.
pub(crate) fn time_from_fields(
    agent: &mut Agent,
    f: &Fields,
    overflow: Overflow,
) -> JsResult<Time> {
    let mut values = [0.0; 6];
    for (i, v) in f.time.iter().enumerate() {
        values[i] = v.unwrap_or(0.0);
    }
    tr(agent, Time::regulate(values, overflow))
}

/// IsPartialTemporalObject(value), as the TypeError checks of `with`.
pub(crate) fn require_partial_temporal_object(agent: &mut Agent, v: Value) -> JsResult<Object> {
    let Value::Object(o) = v else {
        return agent.type_error("argument must be an object");
    };
    if slots(agent, v).is_some() {
        return agent.type_error("argument must not be a Temporal object");
    }
    if !get_prop(agent, o, "calendar")?.is_undefined() {
        return agent.type_error("argument must not have a calendar property");
    }
    if !get_prop(agent, o, "timeZone")?.is_undefined() {
        return agent.type_error("argument must not have a timeZone property");
    }
    Ok(o)
}

/// CalendarMergeFields for ISO 8601: `month` and `monthCode` replace each
/// other.
pub(crate) fn merge_fields(base: Fields, partial: Fields) -> Fields {
    let mut out = base;
    if partial.year.is_some() {
        out.year = partial.year;
    }
    if partial.month.is_some() || partial.month_code.is_some() {
        out.month = partial.month;
        out.month_code = partial.month_code;
    }
    if partial.day.is_some() {
        out.day = partial.day;
    }
    for i in 0..6 {
        if partial.time[i].is_some() {
            out.time[i] = partial.time[i];
        }
    }
    out
}

/// ISODateToFields plus the time of a date-time.
pub(crate) fn fields_of(date: IsoDate, time: Option<Time>) -> Fields {
    let mut f = Fields {
        year: Some(date.year as f64),
        month: Some(date.month as f64),
        month_code: Some((date.month, false)),
        day: Some(date.day as f64),
        time: [None; 6],
    };
    if let Some(t) = time {
        for (i, v) in t.fields().iter().enumerate() {
            f.time[i] = Some(*v as f64);
        }
    }
    f
}

// ----- To* conversions -----

/// Parses a string argument; non-strings are a TypeError.
fn string_arg(agent: &mut Agent, v: Value) -> JsResult<String> {
    match v {
        Value::String(s) => Ok(agent.heap.string_to_rust(s)),
        _ => agent.type_error("argument must be a string or an object"),
    }
}

/// ToTemporalDate(item, options).
pub(crate) fn to_temporal_date(
    agent: &mut Agent,
    item: Value,
    options: Value,
) -> JsResult<IsoDate> {
    if let Value::Object(o) = item {
        match slots(agent, item) {
            Some(TemporalSlots::PlainDate(d)) => {
                let opts = get_options_object(agent, options)?;
                overflow_option(agent, opts)?;
                return Ok(d);
            }
            Some(TemporalSlots::PlainDateTime(dt)) => {
                let opts = get_options_object(agent, options)?;
                overflow_option(agent, opts)?;
                return Ok(dt.date);
            }
            Some(TemporalSlots::ZonedDateTime(z)) => {
                let dt = z.tz.date_time_for(z.ns);
                let opts = get_options_object(agent, options)?;
                overflow_option(agent, opts)?;
                return Ok(dt.date);
            }
            _ => {}
        }
        calendar_with_iso_default(agent, o)?;
        let fields = prepare_fields(agent, o, true, false, false)?;
        let opts = get_options_object(agent, options)?;
        let overflow = overflow_option(agent, opts)?;
        return date_from_fields(agent, &fields, overflow);
    }
    let text = string_arg(agent, item)?;
    let parsed = tr(agent, parse::parse_iso_date_time(&text, Kind::DateTime))?;
    parsed_calendar(agent, &parsed)?;
    let date = IsoDate::new(parsed.year.unwrap_or(0), parsed.month, parsed.day);
    if !date.within_limits() {
        return agent.range_error("date outside the supported range");
    }
    let opts = get_options_object(agent, options)?;
    overflow_option(agent, opts)?;
    Ok(date)
}

/// ToTemporalDateTime(item, options).
pub(crate) fn to_temporal_date_time(
    agent: &mut Agent,
    item: Value,
    options: Value,
) -> JsResult<DateTime> {
    if let Value::Object(o) = item {
        match slots(agent, item) {
            Some(TemporalSlots::PlainDateTime(dt)) => {
                let opts = get_options_object(agent, options)?;
                overflow_option(agent, opts)?;
                return Ok(dt);
            }
            Some(TemporalSlots::PlainDate(d)) => {
                let opts = get_options_object(agent, options)?;
                overflow_option(agent, opts)?;
                return Ok(DateTime::new(d, Time::MIDNIGHT));
            }
            Some(TemporalSlots::ZonedDateTime(z)) => {
                let dt = z.tz.date_time_for(z.ns);
                let opts = get_options_object(agent, options)?;
                overflow_option(agent, opts)?;
                return Ok(dt);
            }
            _ => {}
        }
        calendar_with_iso_default(agent, o)?;
        let fields = prepare_fields(agent, o, true, true, false)?;
        let opts = get_options_object(agent, options)?;
        let overflow = overflow_option(agent, opts)?;
        let date = date_from_fields(agent, &fields, overflow)?;
        let time = time_from_fields(agent, &fields, overflow)?;
        return Ok(DateTime::new(date, time));
    }
    let text = string_arg(agent, item)?;
    let parsed = tr(agent, parse::parse_iso_date_time(&text, Kind::DateTime))?;
    parsed_calendar(agent, &parsed)?;
    let date = IsoDate::new(parsed.year.unwrap_or(0), parsed.month, parsed.day);
    let dt = DateTime::new(date, parsed.time.unwrap_or(Time::MIDNIGHT));
    if !dt.within_limits() {
        return agent.range_error("date-time outside the supported range");
    }
    let opts = get_options_object(agent, options)?;
    overflow_option(agent, opts)?;
    Ok(dt)
}

/// ToTemporalTimeRecord(temporalTimeLike, completeness).
pub(crate) fn to_time_record(agent: &mut Agent, item: Object, partial: bool) -> JsResult<Fields> {
    let f = prepare_fields(agent, item, false, true, true)?;
    let _ = partial;
    Ok(f)
}

/// ToTemporalTime(item, options).
pub(crate) fn to_temporal_time(agent: &mut Agent, item: Value, options: Value) -> JsResult<Time> {
    if let Value::Object(o) = item {
        match slots(agent, item) {
            Some(TemporalSlots::PlainTime(t)) => {
                let opts = get_options_object(agent, options)?;
                overflow_option(agent, opts)?;
                return Ok(t);
            }
            Some(TemporalSlots::PlainDateTime(dt)) => {
                let opts = get_options_object(agent, options)?;
                overflow_option(agent, opts)?;
                return Ok(dt.time);
            }
            Some(TemporalSlots::ZonedDateTime(z)) => {
                let dt = z.tz.date_time_for(z.ns);
                let opts = get_options_object(agent, options)?;
                overflow_option(agent, opts)?;
                return Ok(dt.time);
            }
            _ => {}
        }
        let fields = to_time_record(agent, o, false)?;
        let opts = get_options_object(agent, options)?;
        let overflow = overflow_option(agent, opts)?;
        return time_from_fields(agent, &fields, overflow);
    }
    let text = string_arg(agent, item)?;
    let parsed = tr(agent, parse::parse_iso_date_time(&text, Kind::Time))?;
    parsed_calendar(agent, &parsed)?;
    let Some(time) = parsed.time else {
        return agent.range_error("string has no time");
    };
    let opts = get_options_object(agent, options)?;
    overflow_option(agent, opts)?;
    Ok(time)
}

/// ToTimeRecordOrMidnight(item).
pub(crate) fn to_time_or_midnight(agent: &mut Agent, item: Value) -> JsResult<Time> {
    if item.is_undefined() {
        return Ok(Time::MIDNIGHT);
    }
    to_temporal_time(agent, item, Value::Undefined)
}

/// ToTemporalInstant(item): epoch nanoseconds.
pub(crate) fn to_temporal_instant(agent: &mut Agent, item: Value) -> JsResult<i128> {
    match slots(agent, item) {
        Some(TemporalSlots::Instant(ns)) => return Ok(ns),
        Some(TemporalSlots::ZonedDateTime(z)) => return Ok(z.ns),
        _ => {}
    }
    let p = ops::to_primitive(agent, item, PreferredType::String)?;
    let Value::String(s) = p else {
        return agent.type_error("instant must be a string or an Instant");
    };
    let text = agent.heap.string_to_rust(s);
    let parsed = tr(agent, parse::parse_iso_date_time(&text, Kind::Instant))?;
    parsed_calendar(agent, &parsed)?;
    let date = IsoDate::new(parsed.year.unwrap_or(0), parsed.month, parsed.day);
    let dt = DateTime::new(date, parsed.time.unwrap_or(Time::MIDNIGHT));
    let offset = if parsed.z {
        0
    } else {
        i128::from(parsed.offset.map_or(0, |(ns, _)| ns))
    };
    if !dt.within_limits() {
        return agent.range_error("instant outside the supported range");
    }
    let ns = dt.utc_epoch_ns() - offset;
    if !crate::temporal::is_valid_epoch_ns(ns) {
        return agent.range_error("instant outside the supported range");
    }
    Ok(ns)
}

/// ToTemporalPartialDurationRecord(temporalDurationLike): the fields in
/// alphabetical order, `None` when absent.
pub(crate) fn to_partial_duration(agent: &mut Agent, item: Value) -> JsResult<[Option<f64>; 10]> {
    let Value::Object(o) = item else {
        return agent.type_error("duration must be an object");
    };
    // Alphabetical order and the index of each field in a Duration.
    let order = [
        ("days", 3),
        ("hours", 4),
        ("microseconds", 8),
        ("milliseconds", 7),
        ("minutes", 5),
        ("months", 1),
        ("nanoseconds", 9),
        ("seconds", 6),
        ("weeks", 2),
        ("years", 0),
    ];
    let mut out = [None; 10];
    let mut any = false;
    for (name, index) in order {
        let v = get_prop(agent, o, name)?;
        if !v.is_undefined() {
            any = true;
            out[index] = Some(to_integer_if_integral(agent, v)?);
        }
    }
    if !any {
        return agent.type_error("duration object has no duration properties");
    }
    Ok(out)
}

/// ToTemporalDuration(item).
pub(crate) fn to_temporal_duration(agent: &mut Agent, item: Value) -> JsResult<Duration> {
    if let Some(TemporalSlots::Duration(d)) = slots(agent, item) {
        return Ok(d);
    }
    if let Value::String(s) = item {
        let text = agent.heap.string_to_rust(s);
        return tr(agent, parse::parse_duration(&text));
    }
    if !item.is_object() {
        return agent.type_error("duration must be a string or an object");
    }
    let partial = to_partial_duration(agent, item)?;
    let mut f = [0.0; 10];
    for (i, v) in partial.iter().enumerate() {
        f[i] = v.unwrap_or(0.0);
    }
    let d = Duration::from_fields(f);
    if !d.is_valid() {
        return agent.range_error("duration out of range");
    }
    Ok(d)
}

/// The result of GetTemporalRelativeToOption.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RelativeTo {
    Plain(IsoDate),
    Zoned(ZonedSlots),
}

/// The offset string of a parsed ISO string: its offset behaviour, the
/// offset in nanoseconds and whether it matches to the minute only.
fn parsed_offset(parsed: &parse::Parsed) -> (OffsetBehaviour, i128, bool) {
    if parsed.z {
        return (OffsetBehaviour::Exact, 0, true);
    }
    match parsed.offset {
        None => (OffsetBehaviour::Wall, 0, true),
        Some((ns, seconds)) => (OffsetBehaviour::Option, i128::from(ns), !seconds),
    }
}

/// GetTemporalRelativeToOption(options).
pub(crate) fn relative_to_option(agent: &mut Agent, o: Object) -> JsResult<Option<RelativeTo>> {
    let v = get_prop(agent, o, "relativeTo")?;
    if v.is_undefined() {
        return Ok(None);
    }
    let (date, time, tz, behaviour, offset_ns, match_minutes) = if let Value::Object(item) = v {
        match slots(agent, v) {
            Some(TemporalSlots::ZonedDateTime(z)) => return Ok(Some(RelativeTo::Zoned(z))),
            Some(TemporalSlots::PlainDate(d)) => return Ok(Some(RelativeTo::Plain(d))),
            Some(TemporalSlots::PlainDateTime(dt)) => {
                return Ok(Some(RelativeTo::Plain(dt.date)));
            }
            _ => {}
        }
        calendar_with_iso_default(agent, item)?;
        let (f, z) = prepare_named_fields(agent, item, &ZONED_FIELDS, false)?;
        let date = date_from_fields(agent, &f, Overflow::Constrain)?;
        let time = time_from_fields(agent, &f, Overflow::Constrain)?;
        let (behaviour, offset) = match z.offset {
            Some(ns) => (OffsetBehaviour::Option, i128::from(ns)),
            None => (OffsetBehaviour::Wall, 0),
        };
        (date, Some(time), z.time_zone, behaviour, offset, false)
    } else {
        let Value::String(s) = v else {
            return agent.type_error("relativeTo must be a string or an object");
        };
        let text = agent.heap.string_to_rust(s);
        let parsed = match parse::parse_iso_date_time(&text, Kind::DateTime) {
            Ok(p) => p,
            Err(_) => tr(agent, parse::parse_iso_date_time(&text, Kind::Zoned))?,
        };
        let tz = match &parsed.time_zone {
            Some(annotation) => Some(tr(agent, TimeZoneId::from_identifier(annotation))?),
            None => None,
        };
        parsed_calendar(agent, &parsed)?;
        let (behaviour, offset, match_minutes) = parsed_offset(&parsed);
        let date = IsoDate::new(parsed.year.unwrap_or(0), parsed.month, parsed.day);
        (date, parsed.time, tz, behaviour, offset, match_minutes)
    };
    let Some(tz) = tz else {
        if !date.within_limits() {
            return agent.range_error("date outside the supported range");
        }
        return Ok(Some(RelativeTo::Plain(date)));
    };
    let ns = tr(
        agent,
        crate::temporal::zone::interpret_iso_date_time_offset(
            date,
            time,
            behaviour,
            offset_ns,
            &tz,
            Disambiguation::Compatible,
            OffsetOption::Reject,
            match_minutes,
        ),
    )?;
    if !crate::temporal::is_valid_epoch_ns(ns) {
        return agent.range_error("instant outside the supported range");
    }
    Ok(Some(RelativeTo::Zoned(ZonedSlots { ns, tz })))
}

/// ToTemporalZonedDateTime(item, options).
pub(crate) fn to_temporal_zoned_date_time(
    agent: &mut Agent,
    item: Value,
    options: Value,
) -> JsResult<ZonedSlots> {
    let (date, time, tz, behaviour, offset_ns, match_minutes, disambiguation, offset) =
        if let Value::Object(o) = item {
            if let Some(TemporalSlots::ZonedDateTime(z)) = slots(agent, item) {
                let opts = get_options_object(agent, options)?;
                disambiguation_option(agent, opts)?;
                offset_option(agent, opts, OffsetOption::Reject)?;
                overflow_option(agent, opts)?;
                return Ok(z);
            }
            calendar_with_iso_default(agent, o)?;
            let (f, z) = prepare_named_fields(agent, o, &ZONED_FIELDS, false)?;
            let Some(tz) = z.time_zone else {
                return agent.type_error("timeZone is required");
            };
            let (behaviour, offset_ns) = match z.offset {
                Some(ns) => (OffsetBehaviour::Option, i128::from(ns)),
                None => (OffsetBehaviour::Wall, 0),
            };
            let opts = get_options_object(agent, options)?;
            let disambiguation = disambiguation_option(agent, opts)?;
            let offset = offset_option(agent, opts, OffsetOption::Reject)?;
            let overflow = overflow_option(agent, opts)?;
            let date = date_from_fields(agent, &f, overflow)?;
            let time = time_from_fields(agent, &f, overflow)?;
            (
                date,
                Some(time),
                tz,
                behaviour,
                offset_ns,
                false,
                disambiguation,
                offset,
            )
        } else {
            let text = string_arg(agent, item)?;
            let parsed = tr(agent, parse::parse_iso_date_time(&text, Kind::Zoned))?;
            let annotation = parsed.time_zone.clone().unwrap_or_default();
            let tz = tr(agent, TimeZoneId::from_identifier(&annotation))?;
            parsed_calendar(agent, &parsed)?;
            let (behaviour, offset_ns, match_minutes) = parsed_offset(&parsed);
            let opts = get_options_object(agent, options)?;
            let disambiguation = disambiguation_option(agent, opts)?;
            let offset = offset_option(agent, opts, OffsetOption::Reject)?;
            overflow_option(agent, opts)?;
            let date = IsoDate::new(parsed.year.unwrap_or(0), parsed.month, parsed.day);
            (
                date,
                parsed.time,
                tz,
                behaviour,
                offset_ns,
                match_minutes,
                disambiguation,
                offset,
            )
        };
    let ns = tr(
        agent,
        crate::temporal::zone::interpret_iso_date_time_offset(
            date,
            time,
            behaviour,
            offset_ns,
            &tz,
            disambiguation,
            offset,
            match_minutes,
        ),
    )?;
    if !crate::temporal::is_valid_epoch_ns(ns) {
        return agent.range_error("instant outside the supported range");
    }
    Ok(ZonedSlots { ns, tz })
}

/// The field names of a year-month and of a month-day.
pub(crate) const YEAR_MONTH_FIELDS: [&str; 3] = ["year", "month", "monthCode"];
pub(crate) const MONTH_DAY_FIELDS: [&str; 4] = ["year", "month", "monthCode", "day"];

/// ToTemporalYearMonth(item, options).
pub(crate) fn to_temporal_year_month(
    agent: &mut Agent,
    item: Value,
    options: Value,
) -> JsResult<IsoDate> {
    if let Value::Object(o) = item {
        if let Some(TemporalSlots::PlainYearMonth(d)) = slots(agent, item) {
            let opts = get_options_object(agent, options)?;
            overflow_option(agent, opts)?;
            return Ok(d);
        }
        calendar_with_iso_default(agent, o)?;
        let (fields, _) = prepare_named_fields(agent, o, &YEAR_MONTH_FIELDS, false)?;
        let opts = get_options_object(agent, options)?;
        let overflow = overflow_option(agent, opts)?;
        return year_month_from_fields(agent, &fields, overflow);
    }
    let text = string_arg(agent, item)?;
    let parsed = tr(agent, parse::parse_iso_date_time(&text, Kind::YearMonth))?;
    parsed_calendar(agent, &parsed)?;
    let opts = get_options_object(agent, options)?;
    overflow_option(agent, opts)?;
    let date = IsoDate::new(parsed.year.unwrap_or(0), parsed.month, 1);
    if !year_month_within_limits(date) {
        return agent.range_error("year-month outside the supported range");
    }
    Ok(date)
}

/// ToTemporalMonthDay(item, options).
pub(crate) fn to_temporal_month_day(
    agent: &mut Agent,
    item: Value,
    options: Value,
) -> JsResult<IsoDate> {
    if let Value::Object(o) = item {
        if let Some(TemporalSlots::PlainMonthDay(d)) = slots(agent, item) {
            let opts = get_options_object(agent, options)?;
            overflow_option(agent, opts)?;
            return Ok(d);
        }
        calendar_with_iso_default(agent, o)?;
        let (fields, _) = prepare_named_fields(agent, o, &MONTH_DAY_FIELDS, false)?;
        let opts = get_options_object(agent, options)?;
        let overflow = overflow_option(agent, opts)?;
        return month_day_from_fields(agent, &fields, overflow);
    }
    let text = string_arg(agent, item)?;
    let parsed = tr(agent, parse::parse_iso_date_time(&text, Kind::MonthDay))?;
    parsed_calendar(agent, &parsed)?;
    let opts = get_options_object(agent, options)?;
    overflow_option(agent, opts)?;
    Ok(IsoDate::new(1972, parsed.month, parsed.day))
}

/// ISODateToFields(calendar, isoDate, year-month): the year and month.
pub(crate) fn year_month_fields(date: IsoDate) -> Fields {
    Fields {
        day: None,
        ..fields_of(date, None)
    }
}

/// ISODateToFields(calendar, isoDate, month-day): the month code and day.
pub(crate) fn month_day_fields(date: IsoDate) -> Fields {
    Fields {
        year: None,
        month: None,
        ..fields_of(date, None)
    }
}

// ----- Shared methods -----

/// `valueOf` of every Temporal type.
pub(crate) fn value_of(
    agent: &mut Agent,
    _call: crate::function::NativeCall<'_>,
) -> JsResult<Value> {
    agent.type_error("use compare() or equals() to compare Temporal values")
}

/// A Number value with no negative zero.
pub(crate) fn number(n: f64) -> Value {
    Value::Number(n + 0.0)
}

/// The calendarId getter value.
pub(crate) fn iso8601(agent: &mut Agent) -> Value {
    crate::builtins::str_value(agent, "iso8601")
}

/// The date getters shared by PlainDate and PlainDateTime.
pub(crate) fn date_getter(agent: &mut Agent, date: IsoDate, name: &str) -> Value {
    match name {
        "calendarId" => iso8601(agent),
        "era" | "eraYear" => Value::Undefined,
        "year" => number(date.year as f64),
        "month" => number(date.month as f64),
        "monthCode" => crate::builtins::str_value(agent, &format!("M{:02}", date.month)),
        "day" => number(date.day as f64),
        "dayOfWeek" => number(date.day_of_week() as f64),
        "dayOfYear" => number(date.day_of_year() as f64),
        "weekOfYear" => number(date.week_of_year().0 as f64),
        "yearOfWeek" => number(date.week_of_year().1 as f64),
        "daysInWeek" => number(7.0),
        "daysInMonth" => number(date.days_in_month() as f64),
        "daysInYear" => number(date.days_in_year() as f64),
        "monthsInYear" => number(12.0),
        "inLeapYear" => Value::Boolean(crate::temporal::is_leap_year(date.year)),
        _ => Value::Undefined,
    }
}

pub(crate) const DATE_GETTERS: [&str; 16] = [
    "calendarId",
    "era",
    "eraYear",
    "year",
    "month",
    "monthCode",
    "day",
    "dayOfWeek",
    "dayOfYear",
    "weekOfYear",
    "yearOfWeek",
    "daysInWeek",
    "daysInMonth",
    "daysInYear",
    "monthsInYear",
    "inLeapYear",
];

/// An accessor `get <name>` of `target` (`{ !enumerable, configurable }`)
/// whose function keeps `index` in its first slot.
pub(crate) fn indexed_getter(
    agent: &mut Agent,
    realm: Realm,
    target: Object,
    name: &str,
    f: crate::function::NativeFn,
    index: usize,
) {
    let key = agent.heap.key_from_str(name);
    let getter =
        crate::function::create_builtin_function(agent, f, 0, key, Some(realm), None, Some("get"));
    if let Some(s) = crate::function::native_slots_mut(agent, getter) {
        s.push(Value::Number(index as f64));
    }
    agent.heap.set_own(
        target,
        key,
        crate::object::Attributes::accessor(false, true),
        crate::object::Slot::Accessor {
            get: Value::Object(getter),
            set: Value::Undefined,
        },
    );
}

/// The index an [`indexed_getter`] keeps.
pub(crate) fn getter_index(agent: &Agent, callee: Object) -> usize {
    match crate::function::native_slots(agent, callee).first() {
        Some(Value::Number(n)) => *n as usize,
        _ => 0,
    }
}
