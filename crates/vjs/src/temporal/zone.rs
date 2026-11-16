//! Time zones of Temporal (Temporal proposal §11 and §6.5): offset zones
//! and the named zones of the TZif reader (`crate::tz`); the possible
//! instants of a wall-clock time, their disambiguation in gaps and folds,
//! the start of a day, the offset transitions, InterpretISODateTimeOffset
//! and the arithmetic of zoned date-times (AddZonedDateTime,
//! DifferenceZonedDateTime with rounding and totals).
//!
//! TZif offsets are whole seconds, so a named zone answers in seconds and
//! every instant here is in epoch nanoseconds.

use std::sync::Arc;

use super::parse::{self, TimeZoneString};
use super::{
    DateDuration, DateTime, InternalDuration, IsoDate, NS_MAX_INSTANT, NS_PER_DAY, Overflow,
    RoundingMode, TResult, Time, Unit, calendar_date_add, calendar_date_until, check_time_duration,
    is_valid_epoch_ns, range, round_relative_duration, round_time_duration, round_to_increment,
    total_relative_duration, total_time_duration,
};
use crate::tz::TimeZoneData;

const NS_PER_SECOND: i128 = 1_000_000_000;
const NS_PER_MINUTE: i128 = 60_000_000_000;

/// The `disambiguation` option.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Disambiguation {
    Compatible,
    Earlier,
    Later,
    Reject,
}

impl Disambiguation {
    pub fn from_name(s: &str) -> Option<Disambiguation> {
        Some(match s {
            "compatible" => Disambiguation::Compatible,
            "earlier" => Disambiguation::Earlier,
            "later" => Disambiguation::Later,
            "reject" => Disambiguation::Reject,
            _ => return None,
        })
    }
}

/// The `offset` option of ZonedDateTime parsing and `with`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OffsetOption {
    Prefer,
    Use,
    Ignore,
    Reject,
}

impl OffsetOption {
    pub fn from_name(s: &str) -> Option<OffsetOption> {
        Some(match s {
            "prefer" => OffsetOption::Prefer,
            "use" => OffsetOption::Use,
            "ignore" => OffsetOption::Ignore,
            "reject" => OffsetOption::Reject,
            _ => return None,
        })
    }
}

/// Where the offset of InterpretISODateTimeOffset comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OffsetBehaviour {
    /// An offset was given; the `offset` option decides.
    Option,
    /// `Z`: the date-time is UTC.
    Exact,
    /// No offset: the wall-clock time in the zone.
    Wall,
}

/// A time zone identifier: an offset in minutes or an available named
/// zone in its case-normalized form.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TimeZone {
    Offset(i64),
    Named(String),
}

/// FormatOffsetTimeZoneIdentifier(offsetMinutes).
pub fn format_offset_identifier(minutes: i64) -> String {
    let sign = if minutes < 0 { '-' } else { '+' };
    let m = minutes.abs();
    format!("{sign}{:02}:{:02}", m / 60, m % 60)
}

/// FormatUTCOffsetNanoseconds(offsetNanoseconds): `±HH:MM`, with seconds
/// and their fraction only when they are not zero.
pub fn format_utc_offset_ns(offset_ns: i128) -> String {
    let sign = if offset_ns < 0 { '-' } else { '+' };
    let (_, t) = Time::balance(offset_ns.abs());
    let precision =
        if t.second == 0 && t.millisecond == 0 && t.microsecond == 0 && t.nanosecond == 0 {
            super::Precision::Minute
        } else {
            super::Precision::Auto
        };
    format!("{sign}{}", super::format_time(t, precision))
}

/// CheckISODaysRange(isoDate).
pub fn check_iso_days_range(date: IsoDate) -> TResult<()> {
    if date.epoch_days().abs() > 100_000_000 {
        return range("date outside the supported range");
    }
    Ok(())
}

/// AddInstant(epochNanoseconds, timeDuration).
pub fn add_instant(ns: i128, time: i128) -> TResult<i128> {
    let result = ns + time;
    if !is_valid_epoch_ns(result) {
        return range("instant outside the supported range");
    }
    Ok(result)
}

impl TimeZone {
    /// The zone of a ParseTimeZoneIdentifier or ParseTemporalTimeZoneString
    /// result: named zones must be available.
    pub fn resolve(parsed: TimeZoneString) -> TResult<TimeZone> {
        match parsed {
            TimeZoneString::Offset(minutes) => Ok(TimeZone::Offset(minutes)),
            TimeZoneString::Name(name) => match crate::tz::find_zone(&name) {
                Some(z) => Ok(TimeZone::Named(z.identifier)),
                None => range(&format!("unknown time zone: {name}")),
            },
        }
    }

    /// A time zone identifier (`±HH:MM` or a name), not an ISO string.
    pub fn from_identifier(s: &str) -> TResult<TimeZone> {
        TimeZone::resolve(parse::parse_time_zone_identifier(s)?)
    }

    /// The zone of ToTemporalTimeZoneIdentifier on a string: an identifier
    /// or the zone of an ISO string.
    pub fn from_string(s: &str) -> TResult<TimeZone> {
        TimeZone::resolve(parse::parse_time_zone_string(s)?)
    }

    /// The `timeZoneId` of the zone.
    pub fn identifier(&self) -> String {
        match self {
            TimeZone::Offset(minutes) => format_offset_identifier(*minutes),
            TimeZone::Named(id) => id.clone(),
        }
    }

    /// TimeZoneEquals(one, two): named zones compare their primary
    /// identifiers.
    pub fn equals(&self, other: &TimeZone) -> bool {
        match (self, other) {
            (TimeZone::Offset(a), TimeZone::Offset(b)) => a == b,
            (TimeZone::Named(a), TimeZone::Named(b)) => {
                if a == b {
                    return true;
                }
                match (crate::tz::find_zone(a), crate::tz::find_zone(b)) {
                    (Some(x), Some(y)) => x.primary == y.primary,
                    _ => false,
                }
            }
            _ => false,
        }
    }

    fn data(&self) -> Option<Arc<TimeZoneData>> {
        match self {
            TimeZone::Offset(_) => None,
            TimeZone::Named(id) => crate::tz::zone(id),
        }
    }

    /// GetOffsetNanosecondsFor(timeZone, epochNs).
    pub fn offset_ns(&self, epoch_ns: i128) -> i128 {
        match self {
            TimeZone::Offset(minutes) => i128::from(*minutes) * NS_PER_MINUTE,
            TimeZone::Named(_) => self.data().map_or(0, |z| offset_ns_in(&z, epoch_ns)),
        }
    }

    /// GetISODateTimeFor(timeZone, epochNs).
    pub fn date_time_for(&self, epoch_ns: i128) -> DateTime {
        DateTime::from_epoch_ns(epoch_ns + self.offset_ns(epoch_ns))
    }

    /// GetPossibleEpochNanoseconds(timeZone, isoDateTime), ascending.
    pub fn possible_epoch_ns(&self, dt: DateTime) -> TResult<Vec<i128>> {
        let out = match self {
            TimeZone::Offset(minutes) => {
                let balanced = DateTime::from_epoch_ns(
                    dt.utc_epoch_ns() - i128::from(*minutes) * NS_PER_MINUTE,
                );
                check_iso_days_range(balanced.date)?;
                vec![balanced.utc_epoch_ns()]
            }
            TimeZone::Named(_) => match self.data() {
                Some(z) => named_epoch_ns(&z, dt),
                None => vec![dt.utc_epoch_ns()],
            },
        };
        if out.iter().any(|&ns| !is_valid_epoch_ns(ns)) {
            return range("instant outside the supported range");
        }
        Ok(out)
    }

    /// DisambiguatePossibleEpochNanoseconds(possible, timeZone,
    /// isoDateTime, disambiguation).
    pub fn disambiguate(
        &self,
        possible: &[i128],
        dt: DateTime,
        disambiguation: Disambiguation,
    ) -> TResult<i128> {
        match possible.len() {
            1 => return Ok(possible[0]),
            0 => {}
            n => {
                return match disambiguation {
                    Disambiguation::Earlier | Disambiguation::Compatible => Ok(possible[0]),
                    Disambiguation::Later => Ok(possible[n - 1]),
                    Disambiguation::Reject => {
                        range("the wall-clock time is ambiguous in the time zone")
                    }
                };
            }
        }
        if disambiguation == Disambiguation::Reject {
            return range("the wall-clock time does not exist in the time zone");
        }
        let epoch_ns = dt.utc_epoch_ns();
        let day_before = epoch_ns - NS_PER_DAY;
        if !is_valid_epoch_ns(day_before) {
            return range("instant outside the supported range");
        }
        let offset_before = self.offset_ns(day_before);
        let day_after = epoch_ns + NS_PER_DAY;
        if !is_valid_epoch_ns(day_after) {
            return range("instant outside the supported range");
        }
        let offset_after = self.offset_ns(day_after);
        let gap = offset_after - offset_before;
        let shift = if disambiguation == Disambiguation::Earlier {
            -gap
        } else {
            gap
        };
        let (days, time) = dt.time.add(shift);
        let date = IsoDate::balance(dt.date.year, dt.date.month, dt.date.day + days);
        let shifted = self.possible_epoch_ns(DateTime::new(date, time))?;
        let picked = if disambiguation == Disambiguation::Earlier {
            shifted.first()
        } else {
            shifted.last()
        };
        match picked {
            Some(&ns) => Ok(ns),
            None => range("the wall-clock time does not exist in the time zone"),
        }
    }

    /// GetEpochNanosecondsFor(timeZone, isoDateTime, disambiguation).
    pub fn epoch_ns_for(&self, dt: DateTime, disambiguation: Disambiguation) -> TResult<i128> {
        let possible = self.possible_epoch_ns(dt)?;
        self.disambiguate(&possible, dt, disambiguation)
    }

    /// GetStartOfDay(timeZone, isoDate).
    pub fn start_of_day(&self, date: IsoDate) -> TResult<i128> {
        let dt = DateTime::new(date, Time::MIDNIGHT);
        let possible = self.possible_epoch_ns(dt)?;
        if let Some(&first) = possible.first() {
            return Ok(first);
        }
        // Midnight is in a gap: the day starts at the transition.
        match self.next_transition(dt.utc_epoch_ns() - NS_PER_DAY) {
            Some(t) => Ok(t),
            None => range("the day has no start in the time zone"),
        }
    }

    /// GetNamedTimeZoneNextTransition: the first instant after `epoch_ns`
    /// where the offset changes, within the supported range.
    pub fn next_transition(&self, epoch_ns: i128) -> Option<i128> {
        let z = self.data()?;
        let mut t = epoch_ns.div_euclid(NS_PER_SECOND) as i64;
        for _ in 0..1000 {
            let next = z.next_transition(t)?;
            let ns = i128::from(next) * NS_PER_SECOND;
            if ns > NS_MAX_INSTANT {
                return None;
            }
            if z.offset_at(next - 1) != z.offset_at(next) {
                return Some(ns);
            }
            t = next;
        }
        None
    }

    /// GetNamedTimeZonePreviousTransition: the last instant before
    /// `epoch_ns` where the offset changes, within the supported range.
    pub fn previous_transition(&self, epoch_ns: i128) -> Option<i128> {
        let z = self.data()?;
        // The ceiling, so that a transition at `epoch_ns` itself is
        // excluded and one just before a fractional instant is not.
        let mut t = (epoch_ns + NS_PER_SECOND - 1).div_euclid(NS_PER_SECOND) as i64;
        for _ in 0..1000 {
            let previous = z.previous_transition(t)?;
            let ns = i128::from(previous) * NS_PER_SECOND;
            if ns < -NS_MAX_INSTANT {
                return None;
            }
            if z.offset_at(previous - 1) != z.offset_at(previous) {
                return Some(ns);
            }
            t = previous;
        }
        None
    }
}

/// The offset of a named zone at an instant in nanoseconds.
fn offset_ns_in(z: &TimeZoneData, epoch_ns: i128) -> i128 {
    let seconds = epoch_ns.div_euclid(NS_PER_SECOND) as i64;
    i128::from(z.offset_at(seconds)) * NS_PER_SECOND
}

/// GetNamedTimeZoneEpochNanoseconds: the instants whose wall-clock time
/// is `dt`, ascending (none in a gap, two in a fold).
fn named_epoch_ns(z: &TimeZoneData, dt: DateTime) -> Vec<i128> {
    let local = dt.utc_epoch_ns();
    let mut offsets = vec![
        offset_ns_in(z, local - NS_PER_DAY),
        offset_ns_in(z, local),
        offset_ns_in(z, local + NS_PER_DAY),
    ];
    offsets.sort_unstable();
    offsets.dedup();
    let mut out: Vec<i128> = offsets
        .into_iter()
        .map(|o| local - o)
        .filter(|&t| offset_ns_in(z, t) == local - t)
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// InterpretISODateTimeOffset(isoDate, time, offsetBehaviour,
/// offsetNanoseconds, timeZone, disambiguation, offsetOption,
/// matchBehaviour). `time` is `None` for start-of-day; `match_minutes`
/// accepts an offset that equals the candidate rounded to the minute.
#[allow(clippy::too_many_arguments)]
pub fn interpret_iso_date_time_offset(
    date: IsoDate,
    time: Option<Time>,
    behaviour: OffsetBehaviour,
    offset_ns: i128,
    tz: &TimeZone,
    disambiguation: Disambiguation,
    offset_option: OffsetOption,
    match_minutes: bool,
) -> TResult<i128> {
    let Some(time) = time else {
        return tz.start_of_day(date);
    };
    let dt = DateTime::new(date, time);
    if behaviour == OffsetBehaviour::Wall
        || (behaviour == OffsetBehaviour::Option && offset_option == OffsetOption::Ignore)
    {
        return tz.epoch_ns_for(dt, disambiguation);
    }
    if behaviour == OffsetBehaviour::Exact || offset_option == OffsetOption::Use {
        let balanced = DateTime::from_epoch_ns(dt.utc_epoch_ns() - offset_ns);
        check_iso_days_range(balanced.date)?;
        let ns = balanced.utc_epoch_ns();
        if !is_valid_epoch_ns(ns) {
            return range("instant outside the supported range");
        }
        return Ok(ns);
    }
    check_iso_days_range(date)?;
    let utc = dt.utc_epoch_ns();
    let possible = tz.possible_epoch_ns(dt)?;
    for &candidate in &possible {
        let candidate_offset = utc - candidate;
        if candidate_offset == offset_ns {
            return Ok(candidate);
        }
        if match_minutes
            && round_to_increment(candidate_offset, NS_PER_MINUTE, RoundingMode::HalfExpand)
                == offset_ns
        {
            return Ok(candidate);
        }
    }
    if offset_option == OffsetOption::Reject {
        return range("the offset does not match the time zone");
    }
    tz.disambiguate(&possible, dt, disambiguation)
}

/// AddZonedDateTime(epochNanoseconds, timeZone, calendar, duration,
/// overflow).
pub fn add_zoned_date_time(
    epoch_ns: i128,
    tz: &TimeZone,
    duration: InternalDuration,
    overflow: Overflow,
) -> TResult<i128> {
    if duration.date.sign() == 0 {
        return add_instant(epoch_ns, duration.time);
    }
    let dt = tz.date_time_for(epoch_ns);
    let added = calendar_date_add(dt.date, duration.date, overflow)?;
    let intermediate = DateTime::new(added, dt.time);
    if !intermediate.within_limits() {
        return range("date-time outside the supported range");
    }
    let intermediate_ns = tz.epoch_ns_for(intermediate, Disambiguation::Compatible)?;
    add_instant(intermediate_ns, duration.time)
}

/// DifferenceInstant(ns1, ns2, roundingIncrement, smallestUnit,
/// roundingMode).
pub fn difference_instant(
    one: i128,
    two: i128,
    increment: i64,
    smallest: Unit,
    mode: RoundingMode,
) -> TResult<InternalDuration> {
    let time = round_time_duration(two - one, i128::from(increment), smallest, mode)?;
    Ok(InternalDuration {
        date: DateDuration::default(),
        time,
    })
}

/// DifferenceZonedDateTime(ns1, ns2, timeZone, calendar, largestUnit).
pub fn difference_zoned_date_time(
    one: i128,
    two: i128,
    tz: &TimeZone,
    largest: Unit,
) -> TResult<InternalDuration> {
    if one == two {
        return Ok(InternalDuration::default());
    }
    let start = tz.date_time_for(one);
    let end = tz.date_time_for(two);
    if start.date == end.date {
        return Ok(InternalDuration {
            date: DateDuration::default(),
            time: check_time_duration(two - one)?,
        });
    }
    let sign: i64 = if two < one { -1 } else { 1 };
    let max_correction = if sign == 1 { 2 } else { 1 };
    let mut correction = 0;
    let time = start.time.difference(end.time);
    if time.signum() as i64 == -sign {
        correction += 1;
    }
    let mut found = None;
    while correction <= max_correction {
        let date = IsoDate::balance(
            end.date.year,
            end.date.month,
            end.date.day - correction * sign,
        );
        let intermediate = DateTime::new(date, start.time);
        let intermediate_ns = tz.epoch_ns_for(intermediate, Disambiguation::Compatible)?;
        let time = two - intermediate_ns;
        if sign != -(time.signum() as i64) {
            found = Some((intermediate, time));
            break;
        }
        correction += 1;
    }
    let Some((intermediate, time)) = found else {
        return range("no date-time between the two instants");
    };
    let date_largest = largest.larger(Unit::Day);
    let date = calendar_date_until(start.date, intermediate.date, date_largest);
    Ok(InternalDuration {
        date,
        time: check_time_duration(time)?,
    })
}

/// DifferenceZonedDateTimeWithRounding(ns1, ns2, timeZone, calendar,
/// largestUnit, roundingIncrement, smallestUnit, roundingMode).
pub fn difference_zoned_date_time_with_rounding(
    one: i128,
    two: i128,
    tz: &TimeZone,
    largest: Unit,
    increment: i64,
    smallest: Unit,
    mode: RoundingMode,
) -> TResult<InternalDuration> {
    if !largest.is_date() {
        return difference_instant(one, two, increment, smallest, mode);
    }
    let difference = difference_zoned_date_time(one, two, tz, largest)?;
    if smallest == Unit::Nanosecond && increment == 1 {
        return Ok(difference);
    }
    let origin = tz.date_time_for(one);
    round_relative_duration(
        difference,
        one,
        two,
        origin,
        Some(tz),
        largest,
        increment,
        smallest,
        mode,
    )
}

/// DifferenceZonedDateTimeWithTotal(ns1, ns2, timeZone, calendar, unit).
pub fn difference_zoned_date_time_with_total(
    one: i128,
    two: i128,
    tz: &TimeZone,
    unit: Unit,
) -> TResult<f64> {
    if !unit.is_date() {
        return Ok(total_time_duration(check_time_duration(two - one)?, unit));
    }
    let difference = difference_zoned_date_time(one, two, tz, unit)?;
    let origin = tz.date_time_for(one);
    total_relative_duration(difference, one, two, origin, Some(tz), unit)
}

/// The rounding of ZonedDateTime.prototype.round to a day: the instant
/// rounded within the length of its own day.
pub fn round_to_day(epoch_ns: i128, tz: &TimeZone, mode: RoundingMode) -> TResult<i128> {
    let dt = tz.date_time_for(epoch_ns);
    let start = tz.start_of_day(dt.date)?;
    let next = IsoDate::balance(dt.date.year, dt.date.month, dt.date.day + 1);
    let end = tz.start_of_day(next)?;
    let length = end - start;
    if length <= 0 {
        return range("the day has no length in the time zone");
    }
    let progress = check_time_duration(epoch_ns - start)?;
    let rounded = check_time_duration(round_to_increment(progress, length, mode))?;
    Ok(start + rounded)
}

/// The `hoursInDay` of the day of an instant.
pub fn hours_in_day(epoch_ns: i128, tz: &TimeZone) -> TResult<f64> {
    let dt = tz.date_time_for(epoch_ns);
    let today = tz.start_of_day(dt.date)?;
    let next = IsoDate::balance(dt.date.year, dt.date.month, dt.date.day + 1);
    let tomorrow = tz.start_of_day(next)?;
    Ok(total_time_duration(tomorrow - today, Unit::Hour))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dt(y: i64, mo: i64, d: i64, h: i64, mi: i64) -> DateTime {
        DateTime::new(
            IsoDate::new(y, mo, d),
            Time {
                hour: h,
                minute: mi,
                ..Time::MIDNIGHT
            },
        )
    }

    #[test]
    fn offset_zones() {
        let tz = TimeZone::from_identifier("+05:30").unwrap();
        assert_eq!(tz, TimeZone::Offset(330));
        assert_eq!(tz.identifier(), "+05:30");
        assert_eq!(tz.offset_ns(0), 330 * NS_PER_MINUTE);
        let local = dt(1970, 1, 1, 5, 30);
        assert_eq!(tz.possible_epoch_ns(local).unwrap(), vec![0]);
        assert!(TimeZone::from_identifier("+05:30:01").is_err());
        assert_eq!(format_offset_identifier(-90), "-01:30");
    }

    #[test]
    fn utc_offsets_with_seconds() {
        assert_eq!(format_utc_offset_ns(0), "+00:00");
        assert_eq!(format_utc_offset_ns(-5 * 3_600_000_000_000), "-05:00");
        assert_eq!(
            format_utc_offset_ns(3_600_000_000_000 + 1_000_000_000 + 500),
            "+01:00:01.0000005"
        );
    }

    #[test]
    fn exact_and_use_offsets() {
        let tz = TimeZone::Offset(60);
        let date = IsoDate::new(2020, 1, 1);
        let ns = interpret_iso_date_time_offset(
            date,
            Some(Time::MIDNIGHT),
            OffsetBehaviour::Exact,
            0,
            &tz,
            Disambiguation::Compatible,
            OffsetOption::Reject,
            false,
        )
        .unwrap();
        assert_eq!(ns, DateTime::new(date, Time::MIDNIGHT).utc_epoch_ns());
        // A wrong offset with `reject` is a RangeError; with `use` it wins.
        let args = |option| {
            interpret_iso_date_time_offset(
                date,
                Some(Time::MIDNIGHT),
                OffsetBehaviour::Option,
                2 * 3_600_000_000_000,
                &tz,
                Disambiguation::Compatible,
                option,
                false,
            )
        };
        assert!(args(OffsetOption::Reject).is_err());
        assert_eq!(
            args(OffsetOption::Use).unwrap(),
            DateTime::new(date, Time::MIDNIGHT).utc_epoch_ns() - 2 * 3_600_000_000_000
        );
    }
}
