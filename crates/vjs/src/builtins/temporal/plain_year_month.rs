//! `Temporal.PlainYearMonth` (Temporal proposal §9). The ISO date keeps
//! the reference day (1 unless the constructor says otherwise).

use super::plain_date::calendar_argument;
use super::{
    TemporalSlots, UnitGroup, YEAR_MONTH_FIELDS, create_date, create_duration, create_year_month,
    date_from_fields, date_getter, difference_settings_with, get_options_object, getter_index,
    indexed_getter, merge_fields, overflow_option, prepare_named_fields,
    require_partial_temporal_object, show_calendar_option, slots, to_integer_with_truncation,
    to_temporal_duration, to_temporal_year_month, tr, value_of, year_month_fields,
    year_month_from_fields,
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
    DateDuration, DateTime, InternalDuration, IsoDate, Overflow, ShowCalendar, Time, Unit,
    calendar_date_add, calendar_date_until, duration_from_internal, format_calendar_annotation,
    iso_days_in_month, pad_iso_year, round_relative_duration,
};
use crate::value::Value;

const GETTERS: [&str; 10] = [
    "calendarId",
    "era",
    "eraYear",
    "year",
    "month",
    "monthCode",
    "daysInYear",
    "daysInMonth",
    "monthsInYear",
    "inLeapYear",
];

pub(crate) fn install(agent: &mut Agent, realm: Realm, temporal: Object) {
    let object_proto = intrinsic(agent, realm, Intrinsic::ObjectPrototype);
    let proto = plain_object(agent, object_proto);
    set_intrinsic(
        agent,
        realm,
        Intrinsic::TemporalPlainYearMonthPrototype,
        proto,
    );
    let ctor = constructor(
        agent,
        realm,
        "PlainYearMonth",
        plain_year_month_constructor,
        2,
        proto,
        None,
    );
    set_intrinsic(agent, realm, Intrinsic::TemporalPlainYearMonth, ctor);
    let key = agent.heap.key_from_str("PlainYearMonth");
    value(agent, temporal, key, Value::Object(ctor), true, false, true);
    method(agent, realm, ctor, "compare", compare, 2);
    method(agent, realm, ctor, "from", from, 1);
    for (i, name) in GETTERS.iter().enumerate() {
        indexed_getter(agent, realm, proto, name, field_getter, i);
    }
    method(agent, realm, proto, "add", add, 1);
    method(agent, realm, proto, "equals", equals, 1);
    method(agent, realm, proto, "since", since, 1);
    method(agent, realm, proto, "subtract", subtract, 1);
    method(agent, realm, proto, "toJSON", to_json, 0);
    method(agent, realm, proto, "toLocaleString", to_json, 0);
    method(agent, realm, proto, "toPlainDate", to_plain_date, 1);
    method(agent, realm, proto, "toString", to_string, 0);
    method(agent, realm, proto, "until", until, 1);
    method(agent, realm, proto, "valueOf", value_of, 0);
    method(agent, realm, proto, "with", with, 1);
    to_string_tag(agent, proto, "Temporal.PlainYearMonth");
}

fn this_year_month(agent: &mut Agent, v: Value) -> JsResult<IsoDate> {
    match slots(agent, v) {
        Some(TemporalSlots::PlainYearMonth(d)) => Ok(d),
        _ => agent.type_error("this is not a Temporal.PlainYearMonth"),
    }
}

/// `new Temporal.PlainYearMonth(isoYear, isoMonth [, calendar
/// [, referenceISODay]])`.
fn plain_year_month_constructor(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let Some(new_target) = call.new_target else {
        return agent.type_error("Temporal.PlainYearMonth must be called with new");
    };
    let y = to_integer_with_truncation(agent, call.arg(0))?;
    let m = to_integer_with_truncation(agent, call.arg(1))?;
    calendar_argument(agent, call.arg(2))?;
    let reference = call.arg(3);
    let d = if reference.is_undefined() {
        1.0
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
    create_year_month(agent, date, Some(new_target))
}

fn from(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let date = to_temporal_year_month(agent, call.arg(0), call.arg(1))?;
    create_year_month(agent, date, None)
}

fn compare(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let one = to_temporal_year_month(agent, call.arg(0), Value::Undefined)?;
    let two = to_temporal_year_month(agent, call.arg(1), Value::Undefined)?;
    Ok(Value::Number(match one.cmp(&two) {
        std::cmp::Ordering::Less => -1.0,
        std::cmp::Ordering::Equal => 0.0,
        std::cmp::Ordering::Greater => 1.0,
    }))
}

fn field_getter(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let date = this_year_month(agent, call.this)?;
    let name = GETTERS[getter_index(agent, call.callee)];
    Ok(date_getter(agent, date, name))
}

fn with(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let date = this_year_month(agent, call.this)?;
    let like = require_partial_temporal_object(agent, call.arg(0))?;
    let (partial, _) = prepare_named_fields(agent, like, &YEAR_MONTH_FIELDS, true)?;
    let fields = merge_fields(year_month_fields(date), partial);
    let options = get_options_object(agent, call.arg(1))?;
    let overflow = overflow_option(agent, options)?;
    let result = year_month_from_fields(agent, &fields, overflow)?;
    create_year_month(agent, result, None)
}

/// AddDurationToYearMonth(operation, yearMonth, temporalDurationLike,
/// options).
fn add_duration(agent: &mut Agent, call: &NativeCall<'_>, subtract: bool) -> JsResult<Value> {
    let ym = this_year_month(agent, call.this)?;
    let mut d = to_temporal_duration(agent, call.arg(0))?;
    if subtract {
        d = d.negated();
    }
    let options = get_options_object(agent, call.arg(1))?;
    let overflow = overflow_option(agent, options)?;
    let sign = d.sign();
    let mut fields = year_month_fields(ym);
    fields.day = Some(1.0);
    let first = date_from_fields(agent, &fields, Overflow::Constrain)?;
    let start = if sign < 0 {
        // Subtracting starts from the last day of the month.
        let one_month = DateDuration {
            months: 1,
            ..DateDuration::default()
        };
        let next = tr(
            agent,
            calendar_date_add(first, one_month, Overflow::Constrain),
        )?;
        IsoDate::balance(next.year, next.month, next.day - 1)
    } else {
        first
    };
    let to_add = tr(agent, d.to_date_duration_without_time())?;
    let added = tr(agent, calendar_date_add(start, to_add, overflow))?;
    let result = year_month_from_fields(agent, &year_month_fields(added), overflow)?;
    create_year_month(agent, result, None)
}

fn add(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    add_duration(agent, &call, false)
}

fn subtract(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    add_duration(agent, &call, true)
}

/// DifferenceTemporalPlainYearMonth(operation, yearMonth, other, options).
fn difference(agent: &mut Agent, call: &NativeCall<'_>, since: bool) -> JsResult<Value> {
    let ym = this_year_month(agent, call.this)?;
    let other = to_temporal_year_month(agent, call.arg(0), Value::Undefined)?;
    let options = get_options_object(agent, call.arg(1))?;
    let s = difference_settings_with(
        agent,
        since,
        options,
        UnitGroup::Date,
        &[Unit::Week, Unit::Day],
        Unit::Month,
        Unit::Year,
    )?;
    if ym == other {
        return create_duration(agent, Default::default(), None);
    }
    let mut this_fields = year_month_fields(ym);
    this_fields.day = Some(1.0);
    let this_date = date_from_fields(agent, &this_fields, Overflow::Constrain)?;
    let mut other_fields = year_month_fields(other);
    other_fields.day = Some(1.0);
    let other_date = date_from_fields(agent, &other_fields, Overflow::Constrain)?;
    let until = calendar_date_until(this_date, other_date, s.largest);
    let mut internal = InternalDuration {
        date: DateDuration {
            weeks: 0,
            days: 0,
            ..until
        },
        time: 0,
    };
    if s.smallest != Unit::Month || s.increment != 1 {
        let origin = DateTime::new(this_date, Time::MIDNIGHT);
        let dest = DateTime::new(other_date, Time::MIDNIGHT).utc_epoch_ns();
        internal = tr(
            agent,
            round_relative_duration(
                internal,
                origin.utc_epoch_ns(),
                dest,
                origin,
                None,
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
    let ym = this_year_month(agent, call.this)?;
    let other = to_temporal_year_month(agent, call.arg(0), Value::Undefined)?;
    Ok(Value::Boolean(ym == other))
}

/// `toPlainDate(item)`: the day comes from `item`.
fn to_plain_date(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let ym = this_year_month(agent, call.this)?;
    let Value::Object(item) = call.arg(0) else {
        return agent.type_error("toPlainDate requires an object with a day");
    };
    let (input, _) = prepare_named_fields(agent, item, &["day"], false)?;
    let fields = merge_fields(year_month_fields(ym), input);
    let date = date_from_fields(agent, &fields, Overflow::Constrain)?;
    create_date(agent, date, None)
}

/// TemporalYearMonthToString(yearMonth, showCalendar).
fn year_month_string(date: IsoDate, show: ShowCalendar) -> String {
    let mut s = format!("{}-{:02}", pad_iso_year(date.year), date.month);
    if matches!(show, ShowCalendar::Always | ShowCalendar::Critical) {
        s.push_str(&format!("-{:02}", date.day));
    }
    s.push_str(format_calendar_annotation(show));
    s
}

fn to_string(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let ym = this_year_month(agent, call.this)?;
    let options = get_options_object(agent, call.arg(0))?;
    let show = show_calendar_option(agent, options)?;
    let s = year_month_string(ym, show);
    Ok(str_value(agent, &s))
}

fn to_json(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let ym = this_year_month(agent, call.this)?;
    let s = year_month_string(ym, ShowCalendar::Auto);
    Ok(str_value(agent, &s))
}
