//! ISO 8601 / RFC 9557 strings of Temporal (the grammar of the Temporal
//! proposal §13.33): date-times with offsets and annotations, times,
//! year-months, month-days, time zone identifiers, calendar identifiers
//! and durations. Every failure is a RangeError.

use super::{Duration, IsoDate, TResult, Time, range};

/// What the string form must look like.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// TemporalDateTimeString[~Zoned]: no `Z`.
    DateTime,
    /// TemporalDateTimeString[+Zoned]: a time zone annotation is required.
    Zoned,
    /// TemporalInstantString: a time and `Z` or an offset are required.
    Instant,
    /// TemporalTimeString.
    Time,
    /// TemporalYearMonthString.
    YearMonth,
    /// TemporalMonthDayString.
    MonthDay,
}

/// The parse record of ParseISODateTime.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Parsed {
    pub year: Option<i64>,
    pub month: i64,
    pub day: i64,
    /// `None` is start-of-day.
    pub time: Option<Time>,
    pub z: bool,
    /// The offset in nanoseconds and whether it had seconds.
    pub offset: Option<(i64, bool)>,
    pub time_zone: Option<String>,
    pub calendar: Option<String>,
}

const MINUS: char = '\u{2212}';

struct P {
    s: Vec<char>,
    i: usize,
}

type R<T> = Result<T, ()>;

impl P {
    fn new(s: &str) -> P {
        P {
            s: s.chars().collect(),
            i: 0,
        }
    }

    fn peek(&self) -> Option<char> {
        self.s.get(self.i).copied()
    }

    fn peek_at(&self, k: usize) -> Option<char> {
        self.s.get(self.i + k).copied()
    }

    fn eat(&mut self, c: char) -> bool {
        if self.peek() == Some(c) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn eat_any(&mut self, set: &[char]) -> Option<char> {
        let c = self.peek()?;
        if set.contains(&c) {
            self.i += 1;
            Some(c)
        } else {
            None
        }
    }

    fn done(&self) -> bool {
        self.i == self.s.len()
    }

    fn digit(&mut self) -> R<i64> {
        match self.peek() {
            Some(c @ '0'..='9') => {
                self.i += 1;
                Ok(i64::from(c as u8 - b'0'))
            }
            _ => Err(()),
        }
    }

    fn digits(&mut self, n: usize) -> R<i64> {
        let mut v = 0;
        for _ in 0..n {
            v = v * 10 + self.digit()?;
        }
        Ok(v)
    }

    fn is_digit_at(&self, k: usize) -> bool {
        self.peek_at(k).is_some_and(|c| c.is_ascii_digit())
    }

    fn sign(&mut self) -> Option<i64> {
        match self.eat_any(&['+', '-', MINUS])? {
            '+' => Some(1),
            _ => Some(-1),
        }
    }

    /// DateYear.
    fn year(&mut self) -> R<i64> {
        if let Some(sign) = self.sign() {
            let y = self.digits(6)?;
            if y == 0 && sign < 0 {
                return Err(());
            }
            Ok(sign * y)
        } else {
            self.digits(4)
        }
    }

    fn month(&mut self) -> R<i64> {
        let m = self.digits(2)?;
        if (1..=12).contains(&m) {
            Ok(m)
        } else {
            Err(())
        }
    }

    fn day(&mut self) -> R<i64> {
        let d = self.digits(2)?;
        if (1..=31).contains(&d) {
            Ok(d)
        } else {
            Err(())
        }
    }

    /// Date: `YYYY-MM-DD` or `YYYYMMDD`.
    fn date(&mut self) -> R<(i64, i64, i64)> {
        let y = self.year()?;
        if self.eat('-') {
            let m = self.month()?;
            if !self.eat('-') {
                return Err(());
            }
            Ok((y, m, self.day()?))
        } else {
            let m = self.month()?;
            Ok((y, m, self.day()?))
        }
    }

    /// TemporalDecimalFraction: nanoseconds of 1 to 9 digits.
    fn fraction(&mut self) -> R<Option<i64>> {
        if !matches!(self.peek(), Some('.' | ',')) || !self.is_digit_at(1) {
            return Ok(None);
        }
        self.i += 1;
        let mut ns = 0;
        let mut n = 0;
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            if n == 9 {
                return Err(());
            }
            ns = ns * 10 + self.digit()?;
            n += 1;
        }
        for _ in n..9 {
            ns *= 10;
        }
        Ok(Some(ns))
    }

    fn sub_second(t: &mut Time, ns: i64) {
        t.millisecond = ns / 1_000_000;
        t.microsecond = ns / 1000 % 1000;
        t.nanosecond = ns % 1000;
    }

    /// Time: `HH[:MM[:SS[.fff]]]` or `HH[MM[SS[.fff]]]`.
    fn time(&mut self) -> R<Time> {
        let mut t = Time::MIDNIGHT;
        t.hour = self.digits(2)?;
        if t.hour > 23 {
            return Err(());
        }
        let extended = self.peek() == Some(':');
        let separator = |p: &mut P| -> bool { !extended || p.eat(':') };
        let start = self.i;
        if separator(self) && self.is_digit_at(0) {
            t.minute = self.digits(2)?;
            if t.minute > 59 {
                return Err(());
            }
            let before_seconds = self.i;
            if separator(self) && self.is_digit_at(0) {
                t.second = self.digits(2)?;
                if t.second > 60 {
                    return Err(());
                }
                if t.second == 60 {
                    t.second = 59;
                }
                if let Some(ns) = self.fraction()? {
                    P::sub_second(&mut t, ns);
                }
            } else {
                self.i = before_seconds;
            }
        } else {
            self.i = start;
        }
        Ok(t)
    }

    /// UTCOffset: nanoseconds and whether it had seconds. `sub_minute`
    /// allows seconds and fractions.
    fn offset(&mut self, sub_minute: bool) -> R<(i64, bool)> {
        let sign = self.sign().ok_or(())?;
        let hours = self.digits(2)?;
        if hours > 23 {
            return Err(());
        }
        let mut ns = hours * 3_600_000_000_000;
        let mut had_seconds = false;
        let extended = self.peek() == Some(':');
        if (extended && self.is_digit_at(1)) || (!extended && self.is_digit_at(0)) {
            if extended {
                self.i += 1;
            }
            let minutes = self.digits(2)?;
            if minutes > 59 {
                return Err(());
            }
            ns += minutes * 60_000_000_000;
            let more = if extended {
                self.peek() == Some(':') && self.is_digit_at(1)
            } else {
                self.is_digit_at(0)
            };
            if more {
                if !sub_minute {
                    return Err(());
                }
                if extended {
                    self.i += 1;
                }
                let seconds = self.digits(2)?;
                if seconds > 59 {
                    return Err(());
                }
                ns += seconds * 1_000_000_000;
                had_seconds = true;
                if let Some(f) = self.fraction()? {
                    ns += f;
                }
            }
        }
        Ok((sign * ns, had_seconds))
    }

    /// The date-time suffix: `Z`, an offset, or nothing.
    fn utc_offset(&mut self, out: &mut Parsed, allow_z: bool) -> R<()> {
        match self.peek() {
            Some('Z' | 'z') => {
                if !allow_z {
                    return Err(());
                }
                self.i += 1;
                out.z = true;
            }
            Some('+' | '-' | MINUS) => out.offset = Some(self.offset(true)?),
            _ => {}
        }
        Ok(())
    }

    /// The bracketed annotations: an optional time zone annotation, then
    /// key-value annotations.
    fn annotations(&mut self, out: &mut Parsed) -> R<()> {
        let mut first = true;
        let mut calendar_critical = false;
        while self.eat('[') {
            let critical = self.eat('!');
            let start = self.i;
            while self.peek().is_some_and(|c| c != ']') {
                self.i += 1;
            }
            let body: String = self.s[start..self.i].iter().collect();
            if !self.eat(']') {
                return Err(());
            }
            match body.split_once('=') {
                None => {
                    if !first || !is_time_zone_identifier(&body) {
                        return Err(());
                    }
                    out.time_zone = Some(body);
                }
                Some((key, value)) => {
                    if !is_annotation_key(key) || !is_annotation_value(value) {
                        return Err(());
                    }
                    if key == "u-ca" {
                        if out.calendar.is_none() {
                            out.calendar = Some(value.to_string());
                            calendar_critical = critical;
                        } else if critical || calendar_critical {
                            return Err(());
                        }
                    } else if critical {
                        return Err(());
                    }
                }
            }
            first = false;
        }
        Ok(())
    }

    /// DateTime with an optional time, offset and annotations.
    fn date_time(&mut self, out: &mut Parsed, time_required: bool, allow_z: bool) -> R<()> {
        let (y, m, d) = self.date()?;
        out.year = Some(y);
        out.month = m;
        out.day = d;
        if self.eat_any(&['T', 't', ' ']).is_some() {
            out.time = Some(self.time()?);
            self.utc_offset(out, allow_z)?;
        } else if time_required {
            return Err(());
        }
        self.annotations(out)
    }
}

fn is_annotation_key(k: &str) -> bool {
    let mut chars = k.chars();
    matches!(chars.next(), Some('a'..='z' | '_'))
        && chars.all(|c| matches!(c, 'a'..='z' | '0'..='9' | '_' | '-'))
}

fn is_annotation_value(v: &str) -> bool {
    !v.is_empty()
        && v.split('-')
            .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_alphanumeric()))
}

/// TimeZoneIdentifier: an offset `±HH[[:]MM]` or an IANA name.
pub fn is_time_zone_identifier(s: &str) -> bool {
    parse_offset_identifier(s).is_some() || is_iana_name(s)
}

fn is_iana_name(s: &str) -> bool {
    !s.is_empty() && s.split('/').all(|part| {
        let mut chars = part.chars();
        part != "."
            && part != ".."
            && matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '.' || c == '_')
            && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '+'))
    })
}

/// An offset time zone identifier (`±HH`, `±HHMM`, `±HH:MM`) in minutes.
pub fn parse_offset_identifier(s: &str) -> Option<i64> {
    let mut p = P::new(s);
    let (ns, seconds) = p.offset(false).ok()?;
    if !p.done() || seconds {
        return None;
    }
    Some(ns / 60_000_000_000)
}

/// A `±HH:MM[:SS[.fff]]` offset string in nanoseconds (the syntax of
/// ToOffsetString and of offset options).
pub fn parse_offset_string(s: &str) -> TResult<i64> {
    let mut p = P::new(s);
    match p.offset(true) {
        Ok((ns, _)) if p.done() => Ok(ns),
        _ => range("invalid offset string"),
    }
}

fn finish(p: &P, out: Parsed) -> TResult<Parsed> {
    if !p.done() {
        return range("invalid ISO 8601 string");
    }
    if let Some(y) = out.year
        && !IsoDate::is_valid(y, out.month, out.day)
    {
        return range("invalid date in ISO 8601 string");
    }
    Ok(out)
}

fn fail<T>() -> TResult<T> {
    range("invalid ISO 8601 string")
}

/// ParseISODateTime(isoString, «goal»).
pub fn parse_iso_date_time(s: &str, kind: Kind) -> TResult<Parsed> {
    let mut p = P::new(s);
    let mut out = Parsed::default();
    let ok = match kind {
        Kind::DateTime => p.date_time(&mut out, false, false),
        Kind::Zoned => p
            .date_time(&mut out, false, true)
            .and_then(|_| out.time_zone.as_ref().map(|_| ()).ok_or(())),
        Kind::Instant => p.date_time(&mut out, true, true).and_then(|_| {
            if out.z || out.offset.is_some() {
                Ok(())
            } else {
                Err(())
            }
        }),
        Kind::Time => return parse_time_string(s),
        Kind::YearMonth => return parse_year_month(s),
        Kind::MonthDay => return parse_month_day(s),
    };
    if ok.is_err() {
        return fail();
    }
    finish(&p, out)
}

/// TemporalTimeString: an annotated time (`T` optional when the string
/// cannot be read as a year-month or month-day) or a date-time with a
/// time.
fn parse_time_string(s: &str) -> TResult<Parsed> {
    let mut p = P::new(s);
    let mut out = Parsed::default();
    if p.date_time(&mut out, true, false).is_ok()
        && let Ok(parsed) = finish(&p, out)
    {
        return Ok(parsed);
    }
    let mut p = P::new(s);
    let mut out = Parsed::default();
    let designator = p.eat_any(&['T', 't']).is_some();
    let ok = (|| -> R<()> {
        let time = p.time()?;
        out.time = Some(time);
        p.utc_offset(&mut out, false)?;
        let before_annotations = p.i;
        p.annotations(&mut out)?;
        if !designator {
            // Ambiguity with DateSpecMonthDay and DateSpecYearMonth.
            let head: String = p.s[..before_annotations].iter().collect();
            if is_date_spec_month_day(&head) || is_date_spec_year_month(&head) {
                return Err(());
            }
        }
        Ok(())
    })();
    if ok.is_err() {
        return fail();
    }
    finish(&p, out)
}

fn is_date_spec_year_month(s: &str) -> bool {
    let mut p = P::new(s);
    p.year().is_ok()
        && {
            p.eat('-');
            p.month().is_ok()
        }
        && p.done()
}

fn is_date_spec_month_day(s: &str) -> bool {
    let mut p = P::new(s);
    if p.eat('-') && !p.eat('-') {
        return false;
    }
    let Ok(m) = p.month() else {
        return false;
    };
    p.eat('-');
    let Ok(d) = p.day() else {
        return false;
    };
    p.done() && IsoDate::is_valid(1972, m, d)
}

/// TemporalYearMonthString: `YYYY-MM` with annotations, or a date-time.
fn parse_year_month(s: &str) -> TResult<Parsed> {
    let mut p = P::new(s);
    let mut out = Parsed::default();
    let ok = (|| -> R<()> {
        let y = p.year()?;
        p.eat('-');
        out.year = Some(y);
        out.month = p.month()?;
        out.day = 1;
        p.annotations(&mut out)
    })();
    if ok.is_ok() && p.done() {
        return Ok(out);
    }
    parse_iso_date_time(s, Kind::DateTime)
}

/// TemporalMonthDayString: `[--]MM[-]DD` with annotations, or a
/// date-time.
fn parse_month_day(s: &str) -> TResult<Parsed> {
    let mut p = P::new(s);
    let mut out = Parsed::default();
    let ok = (|| -> R<()> {
        if p.eat('-') && !p.eat('-') {
            return Err(());
        }
        out.month = p.month()?;
        p.eat('-');
        out.day = p.day()?;
        p.annotations(&mut out)
    })();
    if ok.is_ok() && p.done() {
        if !IsoDate::is_valid(1972, out.month, out.day) {
            return fail();
        }
        return Ok(out);
    }
    parse_iso_date_time(s, Kind::DateTime)
}

/// ParseTemporalCalendarString: the calendar of an ISO string
/// (`iso8601` if it has none), or the string itself if it is a calendar
/// identifier.
pub fn parse_calendar_string(s: &str) -> TResult<String> {
    for kind in [
        Kind::DateTime,
        Kind::Zoned,
        Kind::Instant,
        Kind::Time,
        Kind::MonthDay,
        Kind::YearMonth,
    ] {
        if let Ok(parsed) = parse_iso_date_time(s, kind) {
            return Ok(parsed.calendar.unwrap_or_else(|| "iso8601".to_string()));
        }
    }
    if is_annotation_value(s) {
        return Ok(s.to_string());
    }
    range("invalid calendar")
}

/// The result of ParseTemporalTimeZoneString.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TimeZoneString {
    /// An offset identifier in minutes.
    Offset(i64),
    /// A name, not yet looked up.
    Name(String),
}

/// ParseTimeZoneIdentifier.
pub fn parse_time_zone_identifier(s: &str) -> TResult<TimeZoneString> {
    if let Some(minutes) = parse_offset_identifier(s) {
        return Ok(TimeZoneString::Offset(minutes));
    }
    if is_iana_name(s) {
        return Ok(TimeZoneString::Name(s.to_string()));
    }
    range("invalid time zone identifier")
}

/// ParseTemporalTimeZoneString.
pub fn parse_time_zone_string(s: &str) -> TResult<TimeZoneString> {
    if let Ok(tz) = parse_time_zone_identifier(s) {
        return Ok(tz);
    }
    let parsed = {
        let mut p = P::new(s);
        let mut out = Parsed::default();
        if p.date_time(&mut out, false, true).is_ok() {
            finish(&p, out).ok()
        } else {
            None
        }
    }
    .or_else(|| parse_time_string(s).ok())
    .or_else(|| parse_year_month(s).ok())
    .or_else(|| parse_month_day(s).ok());
    let Some(parsed) = parsed else {
        return fail();
    };
    if let Some(annotation) = parsed.time_zone {
        return parse_time_zone_identifier(&annotation);
    }
    if parsed.z {
        return Ok(TimeZoneString::Name("UTC".to_string()));
    }
    if let Some((ns, seconds)) = parsed.offset {
        if seconds {
            return range("time zone offsets must not have seconds");
        }
        return Ok(TimeZoneString::Offset(ns / 60_000_000_000));
    }
    range("string has no time zone")
}

/// ParseTemporalDurationString.
pub fn parse_duration(s: &str) -> TResult<Duration> {
    let mut p = P::new(s);
    let sign = p.sign().unwrap_or(1) as f64;
    if p.eat_any(&['P', 'p']).is_none() {
        return fail();
    }
    // A run of digits with an optional fraction, then a designator.
    fn component(p: &mut P) -> R<Option<(String, Option<(i64, usize)>, char)>> {
        if !p.is_digit_at(0) {
            return Ok(None);
        }
        let start = p.i;
        while p.is_digit_at(0) {
            p.i += 1;
        }
        let whole: String = p.s[start..p.i].iter().collect();
        let mut fraction = None;
        if matches!(p.peek(), Some('.' | ',')) {
            p.i += 1;
            let fs = p.i;
            while p.is_digit_at(0) {
                p.i += 1;
            }
            let n = p.i - fs;
            if n == 0 || n > 9 {
                return Err(());
            }
            let digits: String = p.s[fs..p.i].iter().collect();
            fraction = Some((digits.parse::<i64>().map_err(|_| ())?, n));
        }
        let designator = p.peek().ok_or(())?.to_ascii_uppercase();
        p.i += 1;
        Ok(Some((whole, fraction, designator)))
    }
    let value = |w: &str| w.parse::<f64>().unwrap_or(f64::INFINITY);
    let mut f = [0.0f64; 10];
    let mut any = false;
    // Date part: Y, M, W, D in order.
    let mut next_date = 0;
    loop {
        let save = p.i;
        let Ok(c) = component(&mut p) else {
            return fail();
        };
        let Some((whole, fraction, designator)) = c else {
            break;
        };
        let index = match designator {
            'Y' => 0,
            'M' => 1,
            'W' => 2,
            'D' => 3,
            _ => {
                p.i = save;
                break;
            }
        };
        if index < next_date || fraction.is_some() {
            return fail();
        }
        f[index] = value(&whole);
        next_date = index + 1;
        any = true;
    }
    if p.eat_any(&['T', 't']).is_some() {
        let mut next_time = 0;
        let mut time_any = false;
        // The unit with a fraction (it must be the last one) and the
        // nanoseconds of that fraction.
        let mut fractional: Option<(usize, i128)> = None;
        loop {
            let Ok(c) = component(&mut p) else {
                return fail();
            };
            let Some((whole, fraction, designator)) = c else {
                break;
            };
            let (index, unit_ns) = match designator {
                'H' => (0, 3_600_000_000_000i128),
                'M' => (1, 60_000_000_000),
                'S' => (2, 1_000_000_000),
                _ => return fail(),
            };
            if index < next_time || fractional.is_some() {
                return fail();
            }
            f[4 + index] = value(&whole);
            if let Some((digits, n)) = fraction {
                let ns = i128::from(digits) * unit_ns / 10i128.pow(n as u32);
                fractional = Some((4 + index, ns));
            }
            next_time = index + 1;
            time_any = true;
        }
        if !time_any {
            return fail();
        }
        any = true;
        // Spread the fraction over the smaller units.
        if let Some((unit, ns)) = fractional {
            let mut rest = ns;
            let parts = [
                (5usize, 60_000_000_000i128),
                (6, 1_000_000_000),
                (7, 1_000_000),
                (8, 1000),
                (9, 1),
            ];
            for (i, size) in parts {
                if i > unit {
                    f[i] += (rest / size) as f64;
                    rest %= size;
                }
            }
        }
    }
    if !any || !p.done() {
        return fail();
    }
    for v in &mut f {
        *v = if *v == 0.0 { 0.0 } else { *v * sign };
    }
    let d = Duration::from_fields(f);
    if !d.is_valid() {
        return range("duration out of range");
    }
    Ok(d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn date_times_with_annotations() {
        let p = parse_iso_date_time(
            "2020-01-01T12:34:56.789+01:00[Europe/Paris][u-ca=iso8601]",
            Kind::DateTime,
        )
        .unwrap();
        assert_eq!((p.year, p.month, p.day), (Some(2020), 1, 1));
        assert_eq!(p.time.unwrap().millisecond, 789);
        assert_eq!(p.offset, Some((3_600_000_000_000, false)));
        assert_eq!(p.time_zone.as_deref(), Some("Europe/Paris"));
        assert_eq!(p.calendar.as_deref(), Some("iso8601"));
        assert!(parse_iso_date_time("2020-01-01T00:00Z", Kind::DateTime).is_err());
        assert!(parse_iso_date_time("2020-02-30", Kind::DateTime).is_err());
        assert!(parse_iso_date_time("-000000-01-01", Kind::DateTime).is_err());
        assert!(parse_iso_date_time("2020-01-01[!foo=bar]", Kind::DateTime).is_err());
        assert!(parse_iso_date_time("2020-01-01[foo=bar]", Kind::DateTime).is_ok());
        assert!(
            parse_iso_date_time("2020-01-01[u-ca=iso8601][!u-ca=gregory]", Kind::DateTime).is_err()
        );
    }

    #[test]
    fn durations() {
        let d = parse_duration("P1Y2M3W4DT5H6M7.008009010S").unwrap();
        assert_eq!(
            d.fields(),
            [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0]
        );
        let d = parse_duration("-PT1.5H").unwrap();
        assert_eq!((d.hours, d.minutes), (-1.0, -30.0));
        assert!(parse_duration("P").is_err());
        assert!(parse_duration("PT").is_err());
        assert!(parse_duration("PT1.5H2M").is_err());
        assert!(parse_duration("P1D2Y").is_err());
    }

    #[test]
    fn times_and_ambiguity() {
        assert!(parse_iso_date_time("12:34", Kind::Time).is_ok());
        assert!(parse_iso_date_time("T1214", Kind::Time).is_ok());
        assert!(parse_iso_date_time("1214", Kind::Time).is_err());
        assert!(parse_iso_date_time("12:34Z", Kind::Time).is_err());
    }
}
