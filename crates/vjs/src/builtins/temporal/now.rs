//! `Temporal.Now` (Temporal proposal §2): the system clock and the
//! system time zone (`TZ` or `/etc/localtime`, through `crate::tz`).

use super::{
    TimeZoneId, create_date, create_date_time, create_instant, create_time, create_zoned,
    to_time_zone_identifier,
};
use crate::agent::Agent;
use crate::builtins::{intrinsic, method, plain_object, str_value, to_string_tag, value};
use crate::completion::JsResult;
use crate::function::NativeCall;
use crate::gc::{Object, Realm};
use crate::realm::Intrinsic;
use crate::temporal::DateTime;
use crate::value::Value;

pub(crate) fn install(agent: &mut Agent, realm: Realm, temporal: Object) {
    let object_proto = intrinsic(agent, realm, Intrinsic::ObjectPrototype);
    let now = plain_object(agent, object_proto);
    let key = agent.heap.key_from_str("Now");
    value(agent, temporal, key, Value::Object(now), true, false, true);
    method(agent, realm, now, "instant", instant, 0);
    method(agent, realm, now, "plainDateISO", plain_date_iso, 0);
    method(
        agent,
        realm,
        now,
        "plainDateTimeISO",
        plain_date_time_iso,
        0,
    );
    method(agent, realm, now, "plainTimeISO", plain_time_iso, 0);
    method(agent, realm, now, "timeZoneId", time_zone_id, 0);
    method(
        agent,
        realm,
        now,
        "zonedDateTimeISO",
        zoned_date_time_iso,
        0,
    );
    to_string_tag(agent, now, "Temporal.Now");
}

/// SystemUTCEpochNanoseconds.
fn system_epoch_ns() -> i128 {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_nanos() as i128,
        Err(e) => -(e.duration().as_nanos() as i128),
    }
}

/// SystemTimeZoneIdentifier: the local zone, UTC if it has no usable
/// identifier.
fn system_time_zone() -> TimeZoneId {
    TimeZoneId::from_identifier(&crate::tz::local().id)
        .unwrap_or_else(|_| TimeZoneId::Named("UTC".to_string()))
}

/// The time zone argument of the `Now` functions: the system zone when
/// absent.
fn zone_argument(agent: &mut Agent, v: Value) -> JsResult<TimeZoneId> {
    if v.is_undefined() {
        Ok(system_time_zone())
    } else {
        to_time_zone_identifier(agent, v)
    }
}

/// SystemDateTime(temporalTimeZoneLike).
fn system_date_time(agent: &mut Agent, v: Value) -> JsResult<DateTime> {
    let tz = zone_argument(agent, v)?;
    Ok(tz.date_time_for(system_epoch_ns()))
}

fn time_zone_id(agent: &mut Agent, _call: NativeCall<'_>) -> JsResult<Value> {
    let id = system_time_zone().identifier();
    Ok(str_value(agent, &id))
}

fn instant(agent: &mut Agent, _call: NativeCall<'_>) -> JsResult<Value> {
    Ok(Value::Object(create_instant(
        agent,
        system_epoch_ns(),
        None,
    )?))
}

fn plain_date_time_iso(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let dt = system_date_time(agent, call.arg(0))?;
    create_date_time(agent, dt, None)
}

fn plain_date_iso(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let dt = system_date_time(agent, call.arg(0))?;
    create_date(agent, dt.date, None)
}

fn plain_time_iso(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let dt = system_date_time(agent, call.arg(0))?;
    create_time(agent, dt.time, None)
}

fn zoned_date_time_iso(agent: &mut Agent, call: NativeCall<'_>) -> JsResult<Value> {
    let tz = zone_argument(agent, call.arg(0))?;
    create_zoned(agent, system_epoch_ns(), tz, None)
}
