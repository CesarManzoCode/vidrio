//! The engine-independent part of Temporal (the ISO 8601 calendar only):
//! ISO date arithmetic, time records, units and rounding modes, duration
//! records and their balancing, difference and rounding relative to a
//! date or a zoned date-time (Temporal proposal §7–§13), and the string
//! forms of every type. `parse` reads ISO 8601 / RFC 9557 strings and
//! `zone` holds the time zone operations. The JavaScript objects are in
//! `builtins::temporal`.
//!
//! Epoch nanoseconds and time durations are `i128`; date fields are
//! `i64`, always validated before they could overflow.

pub mod parse;
pub mod zone;

use std::cmp::Ordering;

pub const NS_PER_DAY: i128 = 86_400_000_000_000;
/// nsMaxInstant: 10^8 days.
pub const NS_MAX_INSTANT: i128 = 8_640_000_000_000_000_000_000;
/// maxTimeDuration: 2^53 × 10^9 − 1 nanoseconds.
pub const MAX_TIME_DURATION: i128 = (1i128 << 53) * 1_000_000_000 - 1;

/// An abrupt completion of a Temporal operation, before it becomes a
/// JavaScript error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TemporalError {
    Range(String),
    Type(String),
}

pub type TResult<T> = Result<T, TemporalError>;

pub fn range<T>(msg: &str) -> TResult<T> {
    Err(TemporalError::Range(msg.to_string()))
}

pub fn type_err<T>(msg: &str) -> TResult<T> {
    Err(TemporalError::Type(msg.to_string()))
}

// ----- ISO calendar -----

pub fn is_leap_year(year: i64) -> bool {
    year.rem_euclid(4) == 0 && (year.rem_euclid(100) != 0 || year.rem_euclid(400) == 0)
}

/// ISODaysInMonth(year, month), `month` in 1..=12.
pub fn iso_days_in_month(year: i64, month: i64) -> i64 {
    match month {
        2 if is_leap_year(year) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Days since 1970-01-01 of a date; `day` may lie outside the month.
pub fn epoch_days_from_iso_date(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The ISO date of a day count since 1970-01-01.
pub fn iso_date_from_epoch_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// The overflow option of the operations that build dates and times.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Overflow {
    Constrain,
    Reject,
}

/// An ISO Date Record. The derived order is CompareISODate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct IsoDate {
    pub year: i64,
    pub month: i64,
    pub day: i64,
}

impl IsoDate {
    pub fn new(year: i64, month: i64, day: i64) -> IsoDate {
        IsoDate { year, month, day }
    }

    /// IsValidISODate.
    pub fn is_valid(year: i64, month: i64, day: i64) -> bool {
        (1..=12).contains(&month) && day >= 1 && day <= iso_days_in_month(year, month)
    }

    pub fn epoch_days(self) -> i64 {
        epoch_days_from_iso_date(self.year, self.month, self.day)
    }

    pub fn from_epoch_days(days: i64) -> IsoDate {
        let (year, month, day) = iso_date_from_epoch_days(days);
        IsoDate { year, month, day }
    }

    /// BalanceISODate(year, month, day).
    pub fn balance(year: i64, month: i64, day: i64) -> IsoDate {
        let (y, m) = balance_iso_year_month(year, month);
        IsoDate::from_epoch_days(epoch_days_from_iso_date(y, m, 1) + day - 1)
    }

    /// RegulateISODate(year, month, day, overflow).
    pub fn regulate(year: i64, month: i64, day: i64, overflow: Overflow) -> TResult<IsoDate> {
        match overflow {
            Overflow::Constrain => {
                let month = month.clamp(1, 12);
                let day = day.clamp(1, iso_days_in_month(year, month));
                Ok(IsoDate::new(year, month, day))
            }
            Overflow::Reject => {
                if !IsoDate::is_valid(year, month, day) {
                    return range("date outside the valid range of its fields");
                }
                Ok(IsoDate::new(year, month, day))
            }
        }
    }

    /// ISODateWithinLimits: the date at noon is within the limits of
    /// date-times.
    pub fn within_limits(self) -> bool {
        DateTime::new(self, Time::NOON).within_limits()
    }

    /// 1 = Monday … 7 = Sunday.
    pub fn day_of_week(self) -> i64 {
        (self.epoch_days() + 3).rem_euclid(7) + 1
    }

    pub fn day_of_year(self) -> i64 {
        self.epoch_days() - epoch_days_from_iso_date(self.year, 1, 1) + 1
    }

    /// The ISO week number and the year it belongs to.
    pub fn week_of_year(self) -> (i64, i64) {
        fn weeks_in_year(y: i64) -> i64 {
            let p = |y: i64| (y + y.div_euclid(4) - y.div_euclid(100) + y.div_euclid(400)) % 7;
            if p(y).rem_euclid(7) == 4 || p(y - 1).rem_euclid(7) == 3 {
                53
            } else {
                52
            }
        }
        let week = (self.day_of_year() - self.day_of_week() + 10).div_euclid(7);
        if week < 1 {
            (weeks_in_year(self.year - 1), self.year - 1)
        } else if week > weeks_in_year(self.year) {
            (1, self.year + 1)
        } else {
            (week, self.year)
        }
    }

    pub fn days_in_month(self) -> i64 {
        iso_days_in_month(self.year, self.month)
    }

    pub fn days_in_year(self) -> i64 {
        if is_leap_year(self.year) { 366 } else { 365 }
    }
}

/// BalanceISOYearMonth(year, month).
pub fn balance_iso_year_month(year: i64, month: i64) -> (i64, i64) {
    (
        year + (month - 1).div_euclid(12),
        (month - 1).rem_euclid(12) + 1,
    )
}

/// A Time Record without the `[[Days]]` field. The derived order is
/// CompareTimeRecord.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Time {
    pub hour: i64,
    pub minute: i64,
    pub second: i64,
    pub millisecond: i64,
    pub microsecond: i64,
    pub nanosecond: i64,
}

impl Time {
    pub const MIDNIGHT: Time = Time {
        hour: 0,
        minute: 0,
        second: 0,
        millisecond: 0,
        microsecond: 0,
        nanosecond: 0,
    };

    pub const NOON: Time = Time {
        hour: 12,
        minute: 0,
        second: 0,
        millisecond: 0,
        microsecond: 0,
        nanosecond: 0,
    };

    pub fn from_fields(f: [i64; 6]) -> Time {
        Time {
            hour: f[0],
            minute: f[1],
            second: f[2],
            millisecond: f[3],
            microsecond: f[4],
            nanosecond: f[5],
        }
    }

    pub fn fields(self) -> [i64; 6] {
        [
            self.hour,
            self.minute,
            self.second,
            self.millisecond,
            self.microsecond,
            self.nanosecond,
        ]
    }

    /// IsValidTime.
    pub fn is_valid(f: [f64; 6]) -> bool {
        let max = [23.0, 59.0, 59.0, 999.0, 999.0, 999.0];
        f.iter().zip(max).all(|(&v, m)| (0.0..=m).contains(&v))
    }

    /// RegulateTime(hour, …, overflow).
    pub fn regulate(f: [f64; 6], overflow: Overflow) -> TResult<Time> {
        let max = [23.0, 59.0, 59.0, 999.0, 999.0, 999.0];
        if overflow == Overflow::Reject && !Time::is_valid(f) {
            return range("time outside the valid range of its fields");
        }
        let mut out = [0i64; 6];
        for i in 0..6 {
            out[i] = f[i].clamp(0.0, max[i]) as i64;
        }
        Ok(Time::from_fields(out))
    }

    /// Nanoseconds since midnight.
    pub fn total_ns(self) -> i128 {
        let f = self.fields();
        ((((i128::from(f[0]) * 60 + i128::from(f[1])) * 60 + i128::from(f[2])) * 1000
            + i128::from(f[3]))
            * 1000
            + i128::from(f[4]))
            * 1000
            + i128::from(f[5])
    }

    /// BalanceTime of a nanosecond count since midnight: the days it
    /// spans and the time of day.
    pub fn balance(ns: i128) -> (i64, Time) {
        let days = ns.div_euclid(NS_PER_DAY);
        let mut rest = ns.rem_euclid(NS_PER_DAY);
        let nanosecond = (rest % 1000) as i64;
        rest /= 1000;
        let microsecond = (rest % 1000) as i64;
        rest /= 1000;
        let millisecond = (rest % 1000) as i64;
        rest /= 1000;
        let second = (rest % 60) as i64;
        rest /= 60;
        let minute = (rest % 60) as i64;
        rest /= 60;
        (
            days as i64,
            Time {
                hour: rest as i64,
                minute,
                second,
                millisecond,
                microsecond,
                nanosecond,
            },
        )
    }

    /// AddTime(time, timeDuration).
    pub fn add(self, duration: i128) -> (i64, Time) {
        Time::balance(self.total_ns() + duration)
    }

    /// DifferenceTime(time1, time2).
    pub fn difference(self, other: Time) -> i128 {
        other.total_ns() - self.total_ns()
    }

    /// RoundTime(time, increment, unit, roundingMode): the days carried
    /// and the rounded time.
    pub fn round(self, increment: i128, unit: Unit, mode: RoundingMode) -> (i64, Time) {
        let length = unit.nanoseconds();
        Time::balance(round_to_increment(
            self.total_ns(),
            increment * length,
            mode,
        ))
    }
}

/// An ISO Date-Time Record. The derived order is CompareISODateTime.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DateTime {
    pub date: IsoDate,
    pub time: Time,
}

impl DateTime {
    pub fn new(date: IsoDate, time: Time) -> DateTime {
        DateTime { date, time }
    }

    /// GetUTCEpochNanoseconds(isoDateTime).
    pub fn utc_epoch_ns(self) -> i128 {
        i128::from(self.date.epoch_days()) * NS_PER_DAY + self.time.total_ns()
    }

    /// The date-time of epoch nanoseconds plus an offset.
    pub fn from_epoch_ns(ns: i128) -> DateTime {
        let days = ns.div_euclid(NS_PER_DAY);
        let (_, time) = Time::balance(ns.rem_euclid(NS_PER_DAY));
        DateTime::new(IsoDate::from_epoch_days(days as i64), time)
    }

    /// ISODateTimeWithinLimits.
    pub fn within_limits(self) -> bool {
        if self.date.year.abs() > 300_000 {
            return false;
        }
        let ns = self.utc_epoch_ns();
        ns > -NS_MAX_INSTANT - NS_PER_DAY && ns < NS_MAX_INSTANT + NS_PER_DAY
    }

    /// RoundISODateTime(isoDateTime, increment, unit, roundingMode).
    pub fn round(self, increment: i128, unit: Unit, mode: RoundingMode) -> DateTime {
        let (days, time) = self.time.round(increment, unit, mode);
        let date = IsoDate::balance(self.date.year, self.date.month, self.date.day + days);
        DateTime::new(date, time)
    }
}

/// IsValidEpochNanoseconds.
pub fn is_valid_epoch_ns(ns: i128) -> bool {
    (-NS_MAX_INSTANT..=NS_MAX_INSTANT).contains(&ns)
}

// ----- Units and rounding -----

/// A Temporal unit, largest first; the derived order makes the larger
/// unit the smaller value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Unit {
    Year,
    Month,
    Week,
    Day,
    Hour,
    Minute,
    Second,
    Millisecond,
    Microsecond,
    Nanosecond,
}

pub const UNITS: [Unit; 10] = [
    Unit::Year,
    Unit::Month,
    Unit::Week,
    Unit::Day,
    Unit::Hour,
    Unit::Minute,
    Unit::Second,
    Unit::Millisecond,
    Unit::Microsecond,
    Unit::Nanosecond,
];

impl Unit {
    pub fn name(self) -> &'static str {
        match self {
            Unit::Year => "year",
            Unit::Month => "month",
            Unit::Week => "week",
            Unit::Day => "day",
            Unit::Hour => "hour",
            Unit::Minute => "minute",
            Unit::Second => "second",
            Unit::Millisecond => "millisecond",
            Unit::Microsecond => "microsecond",
            Unit::Nanosecond => "nanosecond",
        }
    }

    /// A singular or plural unit name.
    pub fn from_name(s: &str) -> Option<Unit> {
        let singular = s.strip_suffix('s').unwrap_or(s);
        UNITS.iter().copied().find(|u| u.name() == singular)
    }

    /// IsCalendarUnit: year, month or week.
    pub fn is_calendar(self) -> bool {
        self <= Unit::Week
    }

    /// TemporalUnitCategory is date.
    pub fn is_date(self) -> bool {
        self <= Unit::Day
    }

    /// The length of a time unit (a day counts 24 hours).
    pub fn nanoseconds(self) -> i128 {
        match self {
            Unit::Day => NS_PER_DAY,
            Unit::Hour => 3_600_000_000_000,
            Unit::Minute => 60_000_000_000,
            Unit::Second => 1_000_000_000,
            Unit::Millisecond => 1_000_000,
            Unit::Microsecond => 1000,
            Unit::Nanosecond => 1,
            _ => unreachable!("calendar units have no fixed length"),
        }
    }

    /// LargerOfTwoTemporalUnits.
    pub fn larger(self, other: Unit) -> Unit {
        self.min(other)
    }

    /// MaximumTemporalDurationRoundingIncrement (`None` is unset).
    pub fn maximum_increment(self) -> Option<i128> {
        match self {
            Unit::Year | Unit::Month | Unit::Week | Unit::Day => None,
            Unit::Hour => Some(24),
            Unit::Minute | Unit::Second => Some(60),
            _ => Some(1000),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoundingMode {
    Ceil,
    Floor,
    Expand,
    Trunc,
    HalfCeil,
    HalfFloor,
    HalfExpand,
    HalfTrunc,
    HalfEven,
}

impl RoundingMode {
    pub fn from_name(s: &str) -> Option<RoundingMode> {
        Some(match s {
            "ceil" => RoundingMode::Ceil,
            "floor" => RoundingMode::Floor,
            "expand" => RoundingMode::Expand,
            "trunc" => RoundingMode::Trunc,
            "halfCeil" => RoundingMode::HalfCeil,
            "halfFloor" => RoundingMode::HalfFloor,
            "halfExpand" => RoundingMode::HalfExpand,
            "halfTrunc" => RoundingMode::HalfTrunc,
            "halfEven" => RoundingMode::HalfEven,
            _ => return None,
        })
    }

    /// NegateRoundingMode.
    pub fn negate(self) -> RoundingMode {
        match self {
            RoundingMode::Ceil => RoundingMode::Floor,
            RoundingMode::Floor => RoundingMode::Ceil,
            RoundingMode::HalfCeil => RoundingMode::HalfFloor,
            RoundingMode::HalfFloor => RoundingMode::HalfCeil,
            other => other,
        }
    }

    /// GetUnsignedRoundingMode for a value of the given sign.
    fn unsigned(self, negative: bool) -> Unsigned {
        use RoundingMode::*;
        match (self, negative) {
            (Ceil, false) | (Floor, true) | (Expand, _) => Unsigned::Infinity,
            (Ceil, true) | (Floor, false) | (Trunc, _) => Unsigned::Zero,
            (HalfCeil, false) | (HalfFloor, true) | (HalfExpand, _) => Unsigned::HalfInfinity,
            (HalfCeil, true) | (HalfFloor, false) | (HalfTrunc, _) => Unsigned::HalfZero,
            (HalfEven, _) => Unsigned::HalfEven,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Unsigned {
    Zero,
    Infinity,
    HalfZero,
    HalfInfinity,
    HalfEven,
}

/// ApplyUnsignedRoundingMode for a value `r1 + num/den × (r2 − r1)` with
/// `0 < num < den`; `r1_even` tells whether `r1 / (r2 − r1)` is even.
/// Returns whether the result is `r2`.
fn rounds_up(mode: Unsigned, num: u128, den: u128, r1_even: bool) -> bool {
    match mode {
        Unsigned::Zero => false,
        Unsigned::Infinity => true,
        _ => match (2 * num).cmp(&den) {
            Ordering::Less => false,
            Ordering::Greater => true,
            Ordering::Equal => match mode {
                Unsigned::HalfZero => false,
                Unsigned::HalfInfinity => true,
                _ => !r1_even,
            },
        },
    }
}

/// RoundNumberToIncrement(x, increment, roundingMode) on integers.
pub fn round_to_increment(x: i128, increment: i128, mode: RoundingMode) -> i128 {
    round_with(x, increment, mode.unsigned(x < 0))
}

/// RoundNumberToIncrementAsIfPositive: the unsigned mode applies to the
/// signed value, so `r1` is the floor even when `x` is negative.
pub fn round_as_if_positive(x: i128, increment: i128, mode: RoundingMode) -> i128 {
    let quotient = x.div_euclid(increment);
    let remainder = x.rem_euclid(increment);
    if remainder == 0 {
        return x;
    }
    let up = rounds_up(
        mode.unsigned(false),
        remainder as u128,
        increment as u128,
        quotient.rem_euclid(2) == 0,
    );
    (quotient + i128::from(up)) * increment
}

fn round_with(x: i128, increment: i128, mode: Unsigned) -> i128 {
    let quotient = x.div_euclid(increment);
    let remainder = x.rem_euclid(increment);
    if remainder == 0 {
        return x;
    }
    // Work on magnitudes: r1 is the candidate closer to zero.
    let negative = x < 0;
    let (r1_q, num) = if negative {
        (quotient + 1, increment - remainder)
    } else {
        (quotient, remainder)
    };
    let up = rounds_up(
        mode,
        num as u128,
        increment as u128,
        r1_q.rem_euclid(2) == 0,
    );
    let r1 = r1_q * increment;
    match (up, negative) {
        (false, _) => r1,
        (true, false) => r1 + increment,
        (true, true) => r1 - increment,
    }
}

/// The Number nearest to `num / den` (round half to even).
pub fn ratio_to_f64(num: i128, den: i128) -> f64 {
    if num == 0 {
        return 0.0;
    }
    let negative = (num < 0) != (den < 0);
    let n = num.unsigned_abs();
    let d = den.unsigned_abs();
    let mut m = n / d;
    let mut r = n % d;
    let mut exp: i32 = 0;
    while m < (1u128 << 55) && r != 0 {
        r <<= 1;
        m <<= 1;
        if r >= d {
            r -= d;
            m |= 1;
        }
        exp -= 1;
    }
    let sticky = r != 0;
    let bits = 128 - m.leading_zeros() as i32;
    if bits > 53 {
        let shift = bits - 53;
        let low = m & ((1u128 << shift) - 1);
        let half = 1u128 << (shift - 1);
        m >>= shift;
        exp += shift;
        let up = low > half || (low == half && (sticky || m & 1 == 1));
        if up {
            m += 1;
        }
    }
    let v = (m as f64) * 2f64.powi(exp);
    if negative { -v } else { v }
}

// ----- Durations -----

/// The fields of a Temporal.Duration, as Numbers.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Duration {
    pub years: f64,
    pub months: f64,
    pub weeks: f64,
    pub days: f64,
    pub hours: f64,
    pub minutes: f64,
    pub seconds: f64,
    pub milliseconds: f64,
    pub microseconds: f64,
    pub nanoseconds: f64,
}

pub const DURATION_FIELDS: [&str; 10] = [
    "years",
    "months",
    "weeks",
    "days",
    "hours",
    "minutes",
    "seconds",
    "milliseconds",
    "microseconds",
    "nanoseconds",
];

fn exact(x: f64) -> i128 {
    // Valid durations keep every field far below 2^127.
    x as i128
}

impl Duration {
    pub fn from_fields(f: [f64; 10]) -> Duration {
        Duration {
            years: f[0],
            months: f[1],
            weeks: f[2],
            days: f[3],
            hours: f[4],
            minutes: f[5],
            seconds: f[6],
            milliseconds: f[7],
            microseconds: f[8],
            nanoseconds: f[9],
        }
    }

    pub fn fields(&self) -> [f64; 10] {
        [
            self.years,
            self.months,
            self.weeks,
            self.days,
            self.hours,
            self.minutes,
            self.seconds,
            self.milliseconds,
            self.microseconds,
            self.nanoseconds,
        ]
    }

    /// DurationSign.
    pub fn sign(&self) -> i32 {
        for v in self.fields() {
            if v < 0.0 {
                return -1;
            }
            if v > 0.0 {
                return 1;
            }
        }
        0
    }

    /// IsValidDuration.
    pub fn is_valid(&self) -> bool {
        let f = self.fields();
        let sign = self.sign();
        for v in f {
            if !v.is_finite() || v.trunc() != v {
                return false;
            }
            if (v < 0.0 && sign > 0) || (v > 0.0 && sign < 0) {
                return false;
            }
        }
        if f[..3].iter().any(|v| v.abs() >= 4_294_967_296.0) {
            return false;
        }
        // Each time field alone past 2^53 seconds makes the sum too
        // large (they share a sign), and below these bounds the sum fits
        // an i128.
        let bounds = [1.1e11, 2.6e12, 1.6e14, 9.1e15, 9.1e18, 9.1e21, 9.1e24];
        if f[3..].iter().zip(bounds).any(|(v, b)| v.abs() > b) {
            return false;
        }
        let ns = exact(f[3]) * NS_PER_DAY + self.time_ns();
        ns.unsigned_abs() < (1u128 << 53) * 1_000_000_000
    }

    /// The hours to nanoseconds as one time duration.
    fn time_ns(&self) -> i128 {
        exact(self.hours) * 3_600_000_000_000
            + exact(self.minutes) * 60_000_000_000
            + exact(self.seconds) * 1_000_000_000
            + exact(self.milliseconds) * 1_000_000
            + exact(self.microseconds) * 1000
            + exact(self.nanoseconds)
    }

    /// CreateNegatedTemporalDuration (no negative zeros).
    pub fn negated(&self) -> Duration {
        let mut f = self.fields();
        for v in &mut f {
            *v = if *v == 0.0 { 0.0 } else { -*v };
        }
        Duration::from_fields(f)
    }

    pub fn abs(&self) -> Duration {
        let mut f = self.fields();
        for v in &mut f {
            *v = v.abs();
        }
        Duration::from_fields(f)
    }

    /// DefaultTemporalLargestUnit.
    pub fn default_largest_unit(&self) -> Unit {
        self.fields()
            .iter()
            .position(|&v| v != 0.0)
            .map_or(Unit::Nanosecond, |i| UNITS[i])
    }

    /// ToInternalDurationRecord.
    pub fn to_internal(&self) -> InternalDuration {
        InternalDuration {
            date: DateDuration {
                years: self.years as i64,
                months: self.months as i64,
                weeks: self.weeks as i64,
                days: self.days as i64,
            },
            time: self.time_ns(),
        }
    }

    /// ToInternalDurationRecordWith24HourDays.
    pub fn to_internal_24h(&self) -> InternalDuration {
        InternalDuration {
            date: DateDuration {
                years: self.years as i64,
                months: self.months as i64,
                weeks: self.weeks as i64,
                days: 0,
            },
            time: self.time_ns() + exact(self.days) * NS_PER_DAY,
        }
    }

    /// ToDateDurationRecordWithoutTime.
    pub fn to_date_duration_without_time(&self) -> TResult<DateDuration> {
        let internal = self.to_internal_24h();
        let days = internal.time / NS_PER_DAY;
        let d = DateDuration {
            days: days as i64,
            ..internal.date
        };
        d.validate()?;
        Ok(d)
    }
}

/// A Date Duration Record.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DateDuration {
    pub years: i64,
    pub months: i64,
    pub weeks: i64,
    pub days: i64,
}

impl DateDuration {
    pub fn sign(&self) -> i32 {
        for v in [self.years, self.months, self.weeks, self.days] {
            if v != 0 {
                return v.signum() as i32;
            }
        }
        0
    }

    /// The check of CreateDateDurationRecord.
    pub fn validate(&self) -> TResult<()> {
        let d = Duration {
            years: self.years as f64,
            months: self.months as f64,
            weeks: self.weeks as f64,
            days: self.days as f64,
            ..Duration::default()
        };
        if !d.is_valid() {
            return range("duration out of range");
        }
        Ok(())
    }
}

/// An Internal Duration Record: a date duration and a time duration in
/// nanoseconds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InternalDuration {
    pub date: DateDuration,
    pub time: i128,
}

impl InternalDuration {
    /// InternalDurationSign.
    pub fn sign(&self) -> i32 {
        match self.date.sign() {
            0 => self.time.signum() as i32,
            s => s,
        }
    }
}

/// Add24HourDaysToTimeDuration.
pub fn add_24h_days(time: i128, days: i64) -> TResult<i128> {
    check_time_duration(time + i128::from(days) * NS_PER_DAY)
}

/// The range check of time durations.
pub fn check_time_duration(ns: i128) -> TResult<i128> {
    if ns.abs() > MAX_TIME_DURATION {
        return range("time duration out of range");
    }
    Ok(ns)
}

/// RoundTimeDuration(timeDuration, increment, unit, roundingMode).
pub fn round_time_duration(
    time: i128,
    increment: i128,
    unit: Unit,
    mode: RoundingMode,
) -> TResult<i128> {
    check_time_duration(round_to_increment(
        time,
        unit.nanoseconds() * increment,
        mode,
    ))
}

/// TotalTimeDuration(timeDuration, unit).
pub fn total_time_duration(time: i128, unit: Unit) -> f64 {
    ratio_to_f64(time, unit.nanoseconds())
}

/// TemporalDurationFromInternal(internalDuration, largestUnit). The
/// caller validates the result (CreateTemporalDuration).
pub fn duration_from_internal(d: InternalDuration, largest: Unit) -> Duration {
    let sign: i128 = d.time.signum();
    let mut ns = d.time.unsigned_abs();
    let (mut days, mut hours, mut minutes, mut seconds, mut ms, mut us) = (0u128, 0, 0, 0, 0, 0);
    match largest {
        Unit::Year | Unit::Month | Unit::Week | Unit::Day | Unit::Hour => {
            us = ns / 1000;
            ns %= 1000;
            ms = us / 1000;
            us %= 1000;
            seconds = ms / 1000;
            ms %= 1000;
            minutes = seconds / 60;
            seconds %= 60;
            hours = minutes / 60;
            minutes %= 60;
            if largest != Unit::Hour {
                days = hours / 24;
                hours %= 24;
            }
        }
        Unit::Minute => {
            us = ns / 1000;
            ns %= 1000;
            ms = us / 1000;
            us %= 1000;
            seconds = ms / 1000;
            ms %= 1000;
            minutes = seconds / 60;
            seconds %= 60;
        }
        Unit::Second => {
            us = ns / 1000;
            ns %= 1000;
            ms = us / 1000;
            us %= 1000;
            seconds = ms / 1000;
            ms %= 1000;
        }
        Unit::Millisecond => {
            us = ns / 1000;
            ns %= 1000;
            ms = us / 1000;
            us %= 1000;
        }
        Unit::Microsecond => {
            us = ns / 1000;
            ns %= 1000;
        }
        Unit::Nanosecond => {}
    }
    let f = |v: u128| (v as i128 * sign) as f64;
    Duration {
        years: d.date.years as f64,
        months: d.date.months as f64,
        weeks: d.date.weeks as f64,
        days: (i128::from(d.date.days) + days as i128 * sign) as f64,
        hours: f(hours),
        minutes: f(minutes),
        seconds: f(seconds),
        milliseconds: f(ms),
        microseconds: f(us),
        nanoseconds: f(ns),
    }
}

// ----- Calendar operations (iso8601) -----

/// CalendarDateAdd for the ISO 8601 calendar.
pub fn calendar_date_add(date: IsoDate, d: DateDuration, overflow: Overflow) -> TResult<IsoDate> {
    let (y, m) = balance_iso_year_month(date.year + d.years, date.month + d.months);
    let regulated = IsoDate::regulate(y, m, date.day, overflow)?;
    let days = d.days + 7 * d.weeks;
    let result = IsoDate::from_epoch_days(regulated.epoch_days() + days);
    if !result.within_limits() {
        return range("date outside the supported range");
    }
    Ok(result)
}

/// ISODateSurpasses(sign, baseDate, isoDate2, years, months, weeks, days).
fn iso_date_surpasses(
    sign: i64,
    base: IsoDate,
    two: IsoDate,
    years: i64,
    months: i64,
    weeks: i64,
    days: i64,
) -> bool {
    let (y, m) = balance_iso_year_month(base.year + years, base.month + months);
    let (y0, m0, d0) = if weeks != 0 || days != 0 {
        let regulated = IsoDate::new(y, m, base.day.min(iso_days_in_month(y, m)));
        let balanced = IsoDate::from_epoch_days(regulated.epoch_days() + 7 * weeks + days);
        (balanced.year, balanced.month, balanced.day)
    } else {
        (y, m, base.day)
    };
    if y0 != two.year {
        sign * (y0 - two.year) > 0
    } else if m0 != two.month {
        sign * (m0 - two.month) > 0
    } else if d0 != two.day {
        sign * (d0 - two.day) > 0
    } else {
        false
    }
}

/// CalendarDateUntil for the ISO 8601 calendar.
pub fn calendar_date_until(one: IsoDate, two: IsoDate, largest: Unit) -> DateDuration {
    let sign: i64 = match two.cmp(&one) {
        Ordering::Equal => return DateDuration::default(),
        Ordering::Greater => 1,
        Ordering::Less => -1,
    };
    let (mut years, mut months) = (0i64, 0i64);
    if largest == Unit::Year || largest == Unit::Month {
        let mut candidate_years = two.year - one.year;
        if candidate_years != 0 {
            candidate_years -= sign;
        }
        while !iso_date_surpasses(sign, one, two, candidate_years, 0, 0, 0) {
            years = candidate_years;
            candidate_years += sign;
        }
        let mut candidate_months = sign;
        while !iso_date_surpasses(sign, one, two, years, candidate_months, 0, 0) {
            months = candidate_months;
            candidate_months += sign;
        }
        if largest == Unit::Month {
            months += years * 12;
            years = 0;
        }
    }
    let (y, m) = balance_iso_year_month(one.year + years, one.month + months);
    let constrained = IsoDate::new(y, m, one.day.min(iso_days_in_month(y, m)));
    let mut days = two.epoch_days() - constrained.epoch_days();
    let mut weeks = 0;
    if largest == Unit::Week {
        weeks = days / 7;
        days %= 7;
    }
    DateDuration {
        years,
        months,
        weeks,
        days,
    }
}

/// DifferenceISODateTime(isoDateTime1, isoDateTime2, calendar, largestUnit).
pub fn difference_iso_date_time(
    one: DateTime,
    two: DateTime,
    largest: Unit,
) -> TResult<InternalDuration> {
    let mut time = one.time.difference(two.time);
    let time_sign = time.signum() as i64;
    let date_sign = match two.date.cmp(&one.date) {
        Ordering::Less => -1,
        Ordering::Equal => 0,
        Ordering::Greater => 1,
    };
    let mut adjusted = two.date;
    if time_sign == -date_sign {
        adjusted = IsoDate::from_epoch_days(adjusted.epoch_days() + time_sign);
        time = add_24h_days(time, -time_sign)?;
    }
    let date_largest = Unit::Day.larger(largest);
    let mut date = calendar_date_until(one.date, adjusted, date_largest);
    if largest != date_largest {
        time = add_24h_days(time, date.days)?;
        date.days = 0;
    }
    Ok(InternalDuration { date, time })
}

struct Nudge {
    duration: InternalDuration,
    nudged_epoch_ns: i128,
    did_expand: bool,
}

/// The epoch nanoseconds of a date-time: UTC without a time zone, else
/// GetEpochNanosecondsFor(timeZone, isoDateTime, compatible).
fn epoch_ns_in(dt: DateTime, tz: Option<&zone::TimeZone>) -> TResult<i128> {
    match tz {
        None => Ok(dt.utc_epoch_ns()),
        Some(z) => z.epoch_ns_for(dt, zone::Disambiguation::Compatible),
    }
}

/// NudgeToCalendarUnit: the nudge record and the total.
#[allow(clippy::too_many_arguments)]
fn nudge_to_calendar_unit(
    sign: i64,
    duration: InternalDuration,
    origin_epoch_ns: i128,
    dest_epoch_ns: i128,
    origin: DateTime,
    tz: Option<&zone::TimeZone>,
    increment: i64,
    unit: Unit,
    mode: RoundingMode,
) -> TResult<(Nudge, f64)> {
    let trunc = |v: i64| {
        round_to_increment(i128::from(v), i128::from(increment), RoundingMode::Trunc) as i64
    };
    let date = duration.date;
    let (r1, r2, start, end) = match unit {
        Unit::Year => {
            let years = trunc(date.years);
            let r2 = years + increment * sign;
            let s = DateDuration {
                years,
                ..DateDuration::default()
            };
            let e = DateDuration {
                years: r2,
                ..DateDuration::default()
            };
            (years, r2, s, e)
        }
        Unit::Month => {
            let months = trunc(date.months);
            let r2 = months + increment * sign;
            let s = DateDuration {
                years: date.years,
                months,
                ..DateDuration::default()
            };
            let e = DateDuration { months: r2, ..s };
            (months, r2, s, e)
        }
        Unit::Week => {
            let years_months = DateDuration {
                years: date.years,
                months: date.months,
                ..DateDuration::default()
            };
            let weeks_start = calendar_date_add(origin.date, years_months, Overflow::Constrain)?;
            let weeks_end = IsoDate::from_epoch_days(weeks_start.epoch_days() + date.days);
            let until = calendar_date_until(weeks_start, weeks_end, Unit::Week);
            let weeks = trunc(date.weeks + until.weeks);
            let r2 = weeks + increment * sign;
            let s = DateDuration {
                weeks,
                ..years_months
            };
            let e = DateDuration { weeks: r2, ..s };
            (weeks, r2, s, e)
        }
        _ => {
            let days = trunc(date.days);
            let r2 = days + increment * sign;
            let s = DateDuration { days, ..date };
            let e = DateDuration { days: r2, ..date };
            (days, r2, s, e)
        }
    };
    let _ = r2;
    start.validate()?;
    end.validate()?;
    let start_date = calendar_date_add(origin.date, start, Overflow::Constrain)?;
    let end_date = calendar_date_add(origin.date, end, Overflow::Constrain)?;
    // The start of a zero duration is the origin itself, also when its
    // wall-clock time is ambiguous in the time zone.
    let start_ns = if start.sign() == 0 {
        origin_epoch_ns
    } else {
        epoch_ns_in(DateTime::new(start_date, origin.time), tz)?
    };
    let end_ns = epoch_ns_in(DateTime::new(end_date, origin.time), tz)?;
    let inside = if sign > 0 {
        start_ns <= dest_epoch_ns && dest_epoch_ns <= end_ns
    } else {
        end_ns <= dest_epoch_ns && dest_epoch_ns <= start_ns
    };
    if !inside {
        return range("the rounding interval does not contain the destination");
    }
    if end_ns == start_ns {
        return range("rounding over an empty interval");
    }
    let num = dest_epoch_ns - start_ns;
    let den = end_ns - start_ns;
    // total = r1 + num / den × increment × sign.
    let total = ratio_to_f64(
        i128::from(r1) * den + i128::from(sign) * i128::from(increment) * num,
        den,
    );
    let (n, d) = (num.unsigned_abs(), den.unsigned_abs());
    let expanded = if n == d {
        true
    } else if n == 0 {
        false
    } else {
        let r1_even = (r1.abs() / increment) % 2 == 0;
        rounds_up(mode.unsigned(sign < 0), n, d, r1_even)
    };
    let nudge = if expanded {
        Nudge {
            duration: InternalDuration { date: end, time: 0 },
            nudged_epoch_ns: end_ns,
            did_expand: true,
        }
    } else {
        Nudge {
            duration: InternalDuration {
                date: start,
                time: 0,
            },
            nudged_epoch_ns: start_ns,
            did_expand: false,
        }
    };
    Ok((nudge, total))
}

/// NudgeToZonedTime: rounds the time part within the day the date part
/// reaches, whose length the time zone decides.
fn nudge_to_zoned_time(
    sign: i64,
    duration: InternalDuration,
    origin: DateTime,
    tz: &zone::TimeZone,
    increment: i64,
    unit: Unit,
    mode: RoundingMode,
) -> TResult<Nudge> {
    let start = calendar_date_add(origin.date, duration.date, Overflow::Constrain)?;
    let end = IsoDate::balance(start.year, start.month, start.day + sign);
    let start_ns = tz.epoch_ns_for(
        DateTime::new(start, origin.time),
        zone::Disambiguation::Compatible,
    )?;
    let end_ns = tz.epoch_ns_for(
        DateTime::new(end, origin.time),
        zone::Disambiguation::Compatible,
    )?;
    let day_span = end_ns - start_ns;
    let step = unit.nanoseconds() * i128::from(increment);
    let mut rounded = check_time_duration(round_to_increment(duration.time, step, mode))?;
    let beyond = rounded - day_span;
    let (did_expand, day_delta, nudged_epoch_ns) = if beyond.signum() as i64 != -sign {
        rounded = check_time_duration(round_to_increment(beyond, step, mode))?;
        (true, sign, end_ns + rounded)
    } else {
        (false, 0, start_ns + rounded)
    };
    let date = DateDuration {
        days: duration.date.days + day_delta,
        ..duration.date
    };
    date.validate()?;
    Ok(Nudge {
        duration: InternalDuration {
            date,
            time: rounded,
        },
        nudged_epoch_ns,
        did_expand,
    })
}

/// NudgeToDayOrTime.
fn nudge_to_day_or_time(
    duration: InternalDuration,
    dest_epoch_ns: i128,
    largest: Unit,
    increment: i64,
    smallest: Unit,
    mode: RoundingMode,
) -> TResult<Nudge> {
    let time = add_24h_days(duration.time, duration.date.days)?;
    let rounded = check_time_duration(round_to_increment(
        time,
        smallest.nanoseconds() * i128::from(increment),
        mode,
    ))?;
    let diff = rounded - time;
    let whole_days = time / NS_PER_DAY;
    let rounded_whole_days = rounded / NS_PER_DAY;
    let day_delta = (rounded_whole_days - whole_days).signum();
    let did_expand = day_delta == time.signum();
    let nudged_epoch_ns = dest_epoch_ns + diff;
    let (days, remainder) = if largest.is_date() {
        (
            rounded_whole_days as i64,
            rounded - rounded_whole_days * NS_PER_DAY,
        )
    } else {
        (0, rounded)
    };
    let date = DateDuration {
        days,
        ..duration.date
    };
    date.validate()?;
    Ok(Nudge {
        duration: InternalDuration {
            date,
            time: remainder,
        },
        nudged_epoch_ns,
        did_expand,
    })
}

/// BubbleRelativeDuration.
fn bubble_relative_duration(
    sign: i64,
    mut duration: InternalDuration,
    nudged_epoch_ns: i128,
    origin: DateTime,
    tz: Option<&zone::TimeZone>,
    largest: Unit,
    smallest: Unit,
) -> TResult<InternalDuration> {
    if smallest == largest {
        return Ok(duration);
    }
    let mut index = smallest as usize;
    while index > largest as usize {
        index -= 1;
        let unit = UNITS[index];
        if unit == Unit::Week && largest != Unit::Week {
            continue;
        }
        let end = match unit {
            Unit::Year => DateDuration {
                years: duration.date.years + sign,
                ..DateDuration::default()
            },
            Unit::Month => DateDuration {
                months: duration.date.months + sign,
                weeks: 0,
                days: 0,
                ..duration.date
            },
            _ => DateDuration {
                weeks: duration.date.weeks + sign,
                days: 0,
                ..duration.date
            },
        };
        end.validate()?;
        let end_date = calendar_date_add(origin.date, end, Overflow::Constrain)?;
        let end_ns = epoch_ns_in(DateTime::new(end_date, origin.time), tz)?;
        let beyond = (nudged_epoch_ns - end_ns).signum() as i64;
        if beyond != -sign {
            duration = InternalDuration { date: end, time: 0 };
        } else {
            break;
        }
    }
    Ok(duration)
}

/// RoundRelativeDuration(duration, originEpochNs, destEpochNs,
/// isoDateTime, timeZone, calendar, largestUnit, increment, smallestUnit,
/// roundingMode).
#[allow(clippy::too_many_arguments)]
pub fn round_relative_duration(
    duration: InternalDuration,
    origin_epoch_ns: i128,
    dest_epoch_ns: i128,
    origin: DateTime,
    tz: Option<&zone::TimeZone>,
    largest: Unit,
    increment: i64,
    smallest: Unit,
    mode: RoundingMode,
) -> TResult<InternalDuration> {
    let irregular = smallest.is_calendar() || (tz.is_some() && smallest == Unit::Day);
    let sign = if duration.sign() < 0 { -1 } else { 1 };
    let nudge = if irregular {
        nudge_to_calendar_unit(
            sign,
            duration,
            origin_epoch_ns,
            dest_epoch_ns,
            origin,
            tz,
            increment,
            smallest,
            mode,
        )?
        .0
    } else if let Some(z) = tz {
        nudge_to_zoned_time(sign, duration, origin, z, increment, smallest, mode)?
    } else {
        nudge_to_day_or_time(duration, dest_epoch_ns, largest, increment, smallest, mode)?
    };
    let mut result = nudge.duration;
    if nudge.did_expand && smallest != Unit::Week {
        let start = smallest.larger(Unit::Day);
        result = bubble_relative_duration(
            sign,
            result,
            nudge.nudged_epoch_ns,
            origin,
            tz,
            largest,
            start,
        )?;
    }
    Ok(result)
}

/// TotalRelativeDuration(duration, originEpochNs, destEpochNs,
/// isoDateTime, timeZone, calendar, unit).
pub fn total_relative_duration(
    duration: InternalDuration,
    origin_epoch_ns: i128,
    dest_epoch_ns: i128,
    origin: DateTime,
    tz: Option<&zone::TimeZone>,
    unit: Unit,
) -> TResult<f64> {
    if unit.is_calendar() || (tz.is_some() && unit == Unit::Day) {
        let sign = if duration.sign() < 0 { -1 } else { 1 };
        let (_, total) = nudge_to_calendar_unit(
            sign,
            duration,
            origin_epoch_ns,
            dest_epoch_ns,
            origin,
            tz,
            1,
            unit,
            RoundingMode::Trunc,
        )?;
        return Ok(total);
    }
    let time = add_24h_days(duration.time, duration.date.days)?;
    Ok(total_time_duration(time, unit))
}

/// DifferencePlainDateTimeWithRounding.
pub fn difference_plain_date_time_with_rounding(
    one: DateTime,
    two: DateTime,
    largest: Unit,
    increment: i64,
    smallest: Unit,
    mode: RoundingMode,
) -> TResult<InternalDuration> {
    if one == two {
        return Ok(InternalDuration::default());
    }
    if !one.within_limits() || !two.within_limits() {
        return range("date-time outside the supported range");
    }
    let diff = difference_iso_date_time(one, two, largest)?;
    if smallest == Unit::Nanosecond && increment == 1 {
        return Ok(diff);
    }
    round_relative_duration(
        diff,
        one.utc_epoch_ns(),
        two.utc_epoch_ns(),
        one,
        None,
        largest,
        increment,
        smallest,
        mode,
    )
}

/// DifferencePlainDateTimeWithTotal.
pub fn difference_plain_date_time_with_total(
    one: DateTime,
    two: DateTime,
    unit: Unit,
) -> TResult<f64> {
    if one == two {
        return Ok(0.0);
    }
    if !one.within_limits() || !two.within_limits() {
        return range("date-time outside the supported range");
    }
    let diff = difference_iso_date_time(one, two, unit)?;
    if unit == Unit::Nanosecond {
        return Ok(diff.time as f64);
    }
    total_relative_duration(
        diff,
        one.utc_epoch_ns(),
        two.utc_epoch_ns(),
        one,
        None,
        unit,
    )
}

/// DateDurationDays(dateDuration, plainRelativeTo).
pub fn date_duration_days(d: DateDuration, relative_to: IsoDate) -> TResult<i64> {
    let ymw = DateDuration { days: 0, ..d };
    if ymw.sign() == 0 {
        return Ok(d.days);
    }
    let later = calendar_date_add(relative_to, ymw, Overflow::Constrain)?;
    Ok(d.days + later.epoch_days() - relative_to.epoch_days())
}

// ----- String forms -----

/// The `precision` of the string forms with seconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Precision {
    Minute,
    Auto,
    Digits(u8),
}

/// ToSecondsStringPrecisionRecord result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SecondsPrecision {
    pub precision: Precision,
    pub unit: Unit,
    pub increment: i128,
}

/// ToSecondsStringPrecisionRecord(smallestUnit, fractionalDigitCount);
/// `digits` is `None` for `auto`.
pub fn seconds_precision(smallest: Option<Unit>, digits: Option<u8>) -> SecondsPrecision {
    let p = |precision, unit, increment| SecondsPrecision {
        precision,
        unit,
        increment,
    };
    match smallest {
        Some(Unit::Minute) => p(Precision::Minute, Unit::Minute, 1),
        Some(Unit::Second) => p(Precision::Digits(0), Unit::Second, 1),
        Some(Unit::Millisecond) => p(Precision::Digits(3), Unit::Millisecond, 1),
        Some(Unit::Microsecond) => p(Precision::Digits(6), Unit::Microsecond, 1),
        Some(Unit::Nanosecond) => p(Precision::Digits(9), Unit::Nanosecond, 1),
        _ => match digits {
            None => p(Precision::Auto, Unit::Nanosecond, 1),
            Some(0) => p(Precision::Digits(0), Unit::Second, 1),
            Some(d @ 1..=3) => p(
                Precision::Digits(d),
                Unit::Millisecond,
                10i128.pow(3 - u32::from(d)),
            ),
            Some(d @ 4..=6) => p(
                Precision::Digits(d),
                Unit::Microsecond,
                10i128.pow(6 - u32::from(d)),
            ),
            Some(d) => p(
                Precision::Digits(d),
                Unit::Nanosecond,
                10i128.pow(9 - u32::from(d.min(9))),
            ),
        },
    }
}

/// PadISOYear.
pub fn pad_iso_year(y: i64) -> String {
    if (0..=9999).contains(&y) {
        format!("{y:04}")
    } else {
        let sign = if y < 0 { '-' } else { '+' };
        format!("{sign}{:06}", y.unsigned_abs())
    }
}

/// FormatFractionalSeconds(subSecondNanoseconds, precision).
pub fn format_fractional_seconds(sub_ns: i64, precision: Precision) -> String {
    let digits = format!("{sub_ns:09}");
    match precision {
        Precision::Auto => {
            let trimmed = digits.trim_end_matches('0');
            if trimmed.is_empty() {
                String::new()
            } else {
                format!(".{trimmed}")
            }
        }
        Precision::Digits(0) | Precision::Minute => String::new(),
        Precision::Digits(n) => format!(".{}", &digits[..usize::from(n.min(9))]),
    }
}

/// FormatTimeString / TimeRecordToString.
pub fn format_time(t: Time, precision: Precision) -> String {
    if precision == Precision::Minute {
        return format!("{:02}:{:02}", t.hour, t.minute);
    }
    let sub = t.millisecond * 1_000_000 + t.microsecond * 1000 + t.nanosecond;
    format!(
        "{:02}:{:02}:{:02}{}",
        t.hour,
        t.minute,
        t.second,
        format_fractional_seconds(sub, precision)
    )
}

/// TemporalDateToString without the calendar annotation.
pub fn format_date(d: IsoDate) -> String {
    format!("{}-{:02}-{:02}", pad_iso_year(d.year), d.month, d.day)
}

/// ISODateTimeToString without the calendar annotation.
pub fn format_date_time(dt: DateTime, precision: Precision) -> String {
    format!(
        "{}T{}",
        format_date(dt.date),
        format_time(dt.time, precision)
    )
}

/// The `calendarName` option.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShowCalendar {
    Auto,
    Always,
    Never,
    Critical,
}

/// FormatCalendarAnnotation for `iso8601`.
pub fn format_calendar_annotation(show: ShowCalendar) -> &'static str {
    match show {
        ShowCalendar::Auto | ShowCalendar::Never => "",
        ShowCalendar::Always => "[u-ca=iso8601]",
        ShowCalendar::Critical => "[!u-ca=iso8601]",
    }
}

/// FormatOffsetTimeZoneIdentifier / FormatDateTimeUTCOffsetRounded:
/// `±HH:MM` of an offset in nanoseconds rounded to the minute.
pub fn format_offset_rounded(offset_ns: i128) -> String {
    let rounded = round_to_increment(offset_ns, 60_000_000_000, RoundingMode::HalfExpand);
    let minutes = rounded / 60_000_000_000;
    let sign = if minutes < 0 { '-' } else { '+' };
    let m = minutes.abs();
    format!("{sign}{:02}:{:02}", m / 60, m % 60)
}

fn format_count(x: f64) -> String {
    (x.abs() as u128).to_string()
}

/// TemporalDurationToString(duration, precision).
pub fn format_duration(d: &Duration, precision: Precision) -> String {
    let mut date = String::new();
    for (v, letter) in [
        (d.years, 'Y'),
        (d.months, 'M'),
        (d.weeks, 'W'),
        (d.days, 'D'),
    ] {
        if v != 0.0 {
            date.push_str(&format_count(v));
            date.push(letter);
        }
    }
    let mut time = String::new();
    for (v, letter) in [(d.hours, 'H'), (d.minutes, 'M')] {
        if v != 0.0 {
            time.push_str(&format_count(v));
            time.push(letter);
        }
    }
    let zero_minutes_and_higher = d.fields()[..6].iter().all(|&v| v == 0.0);
    let seconds = exact(d.seconds) * 1_000_000_000
        + exact(d.milliseconds) * 1_000_000
        + exact(d.microseconds) * 1000
        + exact(d.nanoseconds);
    if seconds != 0 || zero_minutes_and_higher || precision != Precision::Auto {
        let abs = seconds.unsigned_abs();
        time.push_str(&(abs / 1_000_000_000).to_string());
        time.push_str(&format_fractional_seconds(
            (abs % 1_000_000_000) as i64,
            precision,
        ));
        time.push('S');
    }
    let mut out = String::new();
    if d.sign() < 0 {
        out.push('-');
    }
    out.push('P');
    out.push_str(&date);
    if !time.is_empty() {
        out.push('T');
        out.push_str(&time);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_days_round_trip() {
        assert_eq!(epoch_days_from_iso_date(1970, 1, 1), 0);
        assert_eq!(epoch_days_from_iso_date(2000, 3, 1), 11_017);
        assert_eq!(epoch_days_from_iso_date(-271_821, 4, 20), -100_000_000);
        assert_eq!(epoch_days_from_iso_date(275_760, 9, 13), 100_000_000);
        for days in [-1_000_000, -1, 0, 59, 60, 11_016, 2_932_896] {
            let (y, m, d) = iso_date_from_epoch_days(days);
            assert_eq!(epoch_days_from_iso_date(y, m, d), days);
        }
    }

    #[test]
    fn limits_of_dates() {
        assert!(IsoDate::new(-271_821, 4, 19).within_limits());
        assert!(!IsoDate::new(-271_821, 4, 18).within_limits());
        assert!(IsoDate::new(275_760, 9, 13).within_limits());
        assert!(!IsoDate::new(275_760, 9, 14).within_limits());
    }

    #[test]
    fn rounding_modes() {
        use RoundingMode::*;
        let r = |x, m| round_to_increment(x, 10, m);
        assert_eq!(r(15, HalfEven), 20);
        assert_eq!(r(25, HalfEven), 20);
        assert_eq!(r(-15, HalfEven), -20);
        assert_eq!(r(-15, HalfExpand), -20);
        assert_eq!(r(-15, HalfCeil), -10);
        assert_eq!(r(-15, HalfFloor), -20);
        assert_eq!(r(-15, HalfTrunc), -10);
        assert_eq!(r(-11, Ceil), -10);
        assert_eq!(r(-11, Floor), -20);
        assert_eq!(r(-11, Expand), -20);
        assert_eq!(r(-11, Trunc), -10);
        assert_eq!(round_as_if_positive(-11, 10, Trunc), -20);
    }

    #[test]
    fn ratios_round_to_nearest() {
        assert_eq!(ratio_to_f64(1, 3), 1.0 / 3.0);
        assert_eq!(ratio_to_f64(-7, 2), -3.5);
        assert_eq!(ratio_to_f64(NS_PER_DAY * 3 / 2, NS_PER_DAY), 1.5);
    }

    #[test]
    fn date_until_at_month_ends() {
        let jan31 = IsoDate::new(2021, 1, 31);
        let feb28 = IsoDate::new(2021, 2, 28);
        assert_eq!(
            calendar_date_until(jan31, feb28, Unit::Month),
            DateDuration {
                days: 28,
                ..DateDuration::default()
            }
        );
        let mar31 = IsoDate::new(2021, 3, 31);
        assert_eq!(
            calendar_date_until(jan31, mar31, Unit::Year),
            DateDuration {
                months: 2,
                ..DateDuration::default()
            }
        );
    }
}
