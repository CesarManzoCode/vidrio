//! Time zones read from the system's TZif files (RFC 8536) under
//! `/usr/share/zoneinfo`: the table of transitions and local time types,
//! the POSIX TZ string of the footer for instants after the last
//! transition, the index of IANA identifiers (case-insensitive lookup,
//! links from `tzdata.zi` and their primary identifiers) and the local
//! time zone from `TZ` or `/etc/localtime`.
//!
//! Leap seconds are ignored (the `right/` tree is not indexed), as
//! ECMA-262 time values do.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

/// The root of the system time zone database.
pub const ZONEINFO: &str = "/usr/share/zoneinfo";

const SECONDS_PER_DAY: i64 = 86_400;

/// A local time type: UT offset in seconds (east positive), DST flag and
/// abbreviation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalTimeType {
    pub offset: i32,
    pub is_dst: bool,
    pub abbreviation: String,
}

/// The day a POSIX TZ rule names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RuleDay {
    /// `Jn`: day 1..=365, February 29 never counted.
    Julian1(u16),
    /// `n`: zero-based day 0..=365, February 29 counted.
    Julian0(u16),
    /// `Mm.w.d`: day `d` (0 = Sunday) of week `w` (5 = last) of month `m`.
    MonthWeekDay(u8, u8, u8),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PosixDst {
    abbreviation: String,
    offset: i32,
    start: RuleDay,
    /// Seconds after local midnight (standard time), may be negative or
    /// beyond 24 hours (TZif version 3).
    start_time: i32,
    end: RuleDay,
    /// Seconds after local midnight (daylight saving time).
    end_time: i32,
}

/// A POSIX TZ string (the TZif footer).
#[derive(Clone, Debug, PartialEq, Eq)]
struct PosixTz {
    std_abbreviation: String,
    std_offset: i32,
    dst: Option<PosixDst>,
}

/// The rules of one time zone.
#[derive(Clone, Debug)]
pub struct TimeZoneData {
    /// Transition instants in epoch seconds, ascending.
    transitions: Vec<i64>,
    /// Index into `types` for each transition.
    transition_types: Vec<u8>,
    types: Vec<LocalTimeType>,
    footer: Option<PosixTz>,
}

fn be_u32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn be_i32(b: &[u8], at: usize) -> Option<i32> {
    Some(i32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn be_i64(b: &[u8], at: usize) -> Option<i64> {
    Some(i64::from_be_bytes(b.get(at..at + 8)?.try_into().ok()?))
}

struct Header {
    version: u8,
    isutcnt: usize,
    isstdcnt: usize,
    leapcnt: usize,
    timecnt: usize,
    typecnt: usize,
    charcnt: usize,
}

fn header(b: &[u8], at: usize) -> Option<Header> {
    if b.get(at..at + 4)? != b"TZif" {
        return None;
    }
    let count = |i: usize| be_u32(b, at + 20 + 4 * i).map(|n| n as usize);
    Some(Header {
        version: *b.get(at + 4)?,
        isutcnt: count(0)?,
        isstdcnt: count(1)?,
        leapcnt: count(2)?,
        timecnt: count(3)?,
        typecnt: count(4)?,
        charcnt: count(5)?,
    })
}

impl Header {
    /// Length of the data block that follows the header, with `time`-byte
    /// transition times.
    fn data_len(&self, time: usize) -> usize {
        self.timecnt * time
            + self.timecnt
            + self.typecnt * 6
            + self.charcnt
            + self.leapcnt * (time + 4)
            + self.isstdcnt
            + self.isutcnt
    }
}

impl TimeZoneData {
    /// The zone `UTC`.
    pub fn utc() -> TimeZoneData {
        TimeZoneData::fixed(0, "UTC")
    }

    /// A zone with one constant offset.
    pub fn fixed(offset: i32, abbreviation: &str) -> TimeZoneData {
        TimeZoneData {
            transitions: Vec::new(),
            transition_types: Vec::new(),
            types: vec![LocalTimeType {
                offset,
                is_dst: false,
                abbreviation: abbreviation.to_string(),
            }],
            footer: None,
        }
    }

    /// Parses a TZif file (versions 1 to 4). Uses the 64-bit block and
    /// the footer when the file has them.
    pub fn parse(b: &[u8]) -> Option<TimeZoneData> {
        let h1 = header(b, 0)?;
        let v1_end = 44 + h1.data_len(4);
        let (h, start, time_size) = if h1.version >= b'2' {
            (header(b, v1_end)?, v1_end + 44, 8)
        } else {
            (h1, 44, 4)
        };
        if h.typecnt == 0 {
            return None;
        }
        let mut at = start;
        let mut transitions = Vec::with_capacity(h.timecnt);
        for i in 0..h.timecnt {
            let t = if time_size == 8 {
                be_i64(b, at + 8 * i)?
            } else {
                i64::from(be_i32(b, at + 4 * i)?)
            };
            transitions.push(t);
        }
        at += h.timecnt * time_size;
        let transition_types = b.get(at..at + h.timecnt)?.to_vec();
        at += h.timecnt;
        let types_at = at;
        at += h.typecnt * 6;
        let chars = b.get(at..at + h.charcnt)?;
        let mut types = Vec::with_capacity(h.typecnt);
        for i in 0..h.typecnt {
            let r = types_at + 6 * i;
            let offset = be_i32(b, r)?;
            let is_dst = *b.get(r + 4)? != 0;
            let index = usize::from(*b.get(r + 5)?);
            let tail = chars.get(index..).unwrap_or(&[]);
            let end = tail.iter().position(|&c| c == 0).unwrap_or(tail.len());
            types.push(LocalTimeType {
                offset,
                is_dst,
                abbreviation: String::from_utf8_lossy(&tail[..end]).into_owned(),
            });
        }
        if transition_types
            .iter()
            .any(|&t| usize::from(t) >= types.len())
        {
            return None;
        }
        let mut footer = None;
        if time_size == 8 {
            let end = start + h.data_len(8);
            if let Some(rest) = b.get(end..)
                && rest.first() == Some(&b'\n')
            {
                let text = &rest[1..];
                let len = text.iter().position(|&c| c == b'\n').unwrap_or(text.len());
                let s = String::from_utf8_lossy(&text[..len]).into_owned();
                if !s.is_empty() {
                    footer = parse_posix_tz(&s);
                }
            }
        }
        Some(TimeZoneData {
            transitions,
            transition_types,
            types,
            footer,
        })
    }

    /// A zone defined only by a POSIX TZ string (`TZ=EST5EDT,M3.2.0,M11.1.0`).
    pub fn from_posix(s: &str) -> Option<TimeZoneData> {
        let footer = parse_posix_tz(s)?;
        let types = vec![LocalTimeType {
            offset: footer.std_offset,
            is_dst: false,
            abbreviation: footer.std_abbreviation.clone(),
        }];
        Some(TimeZoneData {
            transitions: Vec::new(),
            transition_types: Vec::new(),
            types,
            footer: Some(footer),
        })
    }

    /// The local time type in effect at `t` (epoch seconds).
    pub fn local_type_at(&self, t: i64) -> LocalTimeType {
        let after_table = self.transitions.last().is_none_or(|&last| t >= last);
        if after_table && let Some(footer) = &self.footer {
            return footer.local_type_at(t);
        }
        // Before the first transition the first type applies (RFC 8536
        // §3.2).
        let n = self.transitions.partition_point(|&tr| tr <= t);
        let index = if n == 0 {
            0
        } else {
            usize::from(self.transition_types[n - 1])
        };
        self.types[index].clone()
    }

    /// The UT offset in seconds at `t` (epoch seconds).
    pub fn offset_at(&self, t: i64) -> i32 {
        let after_table = self.transitions.last().is_none_or(|&last| t >= last);
        if after_table && let Some(footer) = &self.footer {
            return footer.offset_at(t);
        }
        let n = self.transitions.partition_point(|&tr| tr <= t);
        let index = if n == 0 {
            0
        } else {
            usize::from(self.transition_types[n - 1])
        };
        self.types[index].offset
    }

    /// The epoch seconds whose local wall clock reads `local` (seconds
    /// since the epoch as if the zone were UTC), ascending: none in a gap,
    /// two in a fold.
    pub fn possible_instants(&self, local: i64) -> Vec<i64> {
        let mut offsets = vec![
            self.offset_at(local - SECONDS_PER_DAY),
            self.offset_at(local),
            self.offset_at(local + SECONDS_PER_DAY),
        ];
        offsets.sort_unstable();
        offsets.dedup();
        let mut out: Vec<i64> = offsets
            .into_iter()
            .map(|o| local - i64::from(o))
            .filter(|&t| i64::from(self.offset_at(t)) == local - t)
            .collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    /// The first transition strictly after `t`, if any, within the
    /// supported range of time values.
    pub fn next_transition(&self, t: i64) -> Option<i64> {
        let n = self.transitions.partition_point(|&tr| tr <= t);
        if n < self.transitions.len() {
            return Some(self.transitions[n]);
        }
        let footer = self.footer.as_ref()?;
        footer.dst.as_ref()?;
        let (year, ..) = crate::temporal::iso_date_from_epoch_days(t.div_euclid(SECONDS_PER_DAY));
        for y in year - 1..year + 400 {
            for (when, _) in footer.transitions_in(y) {
                if when > t {
                    return Some(when);
                }
            }
        }
        None
    }

    /// The last transition strictly before `t`, if any.
    pub fn previous_transition(&self, t: i64) -> Option<i64> {
        if let Some(footer) = &self.footer
            && footer.dst.is_some()
            && self.transitions.last().is_none_or(|&last| t > last)
        {
            let (year, ..) =
                crate::temporal::iso_date_from_epoch_days(t.div_euclid(SECONDS_PER_DAY));
            let floor = self.transitions.last().copied();
            for y in (year - 400..=year + 1).rev() {
                let mut found = None;
                for (when, _) in footer.transitions_in(y) {
                    if when < t && floor.is_none_or(|f| when > f) {
                        found = Some(when);
                    }
                }
                if found.is_some() {
                    return found;
                }
                if floor.is_some_and(|f| {
                    crate::temporal::iso_date_from_epoch_days(f.div_euclid(SECONDS_PER_DAY)).0 > y
                }) {
                    break;
                }
            }
            return floor.filter(|&f| f < t);
        }
        let n = self.transitions.partition_point(|&tr| tr < t);
        if n == 0 {
            None
        } else {
            Some(self.transitions[n - 1])
        }
    }
}

// ----- POSIX TZ strings -----

struct Cursor<'a> {
    s: &'a [u8],
    i: usize,
}

impl Cursor<'_> {
    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }

    fn eat(&mut self, c: u8) -> bool {
        if self.peek() == Some(c) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn number(&mut self) -> Option<i32> {
        let start = self.i;
        let mut n: i32 = 0;
        while let Some(c @ b'0'..=b'9') = self.peek() {
            n = n.checked_mul(10)?.checked_add(i32::from(c - b'0'))?;
            self.i += 1;
        }
        if self.i == start { None } else { Some(n) }
    }

    fn abbreviation(&mut self) -> Option<String> {
        let start = self.i;
        if self.eat(b'<') {
            while self.peek().is_some_and(|c| c != b'>') {
                self.i += 1;
            }
            let text = &self.s[start + 1..self.i];
            if !self.eat(b'>') {
                return None;
            }
            return Some(String::from_utf8_lossy(text).into_owned());
        }
        while self.peek().is_some_and(|c| c.is_ascii_alphabetic()) {
            self.i += 1;
        }
        if self.i - start < 3 {
            return None;
        }
        Some(String::from_utf8_lossy(&self.s[start..self.i]).into_owned())
    }

    /// `[+-]hh[:mm[:ss]]` in seconds.
    fn hms(&mut self) -> Option<i32> {
        let negative = if self.eat(b'-') {
            true
        } else {
            self.eat(b'+');
            false
        };
        let mut total = self.number()? * 3600;
        if self.eat(b':') {
            total += self.number()? * 60;
            if self.eat(b':') {
                total += self.number()?;
            }
        }
        Some(if negative { -total } else { total })
    }

    fn rule(&mut self) -> Option<(RuleDay, i32)> {
        let day = if self.eat(b'J') {
            RuleDay::Julian1(u16::try_from(self.number()?).ok()?)
        } else if self.eat(b'M') {
            let m = self.number()?;
            if !self.eat(b'.') {
                return None;
            }
            let w = self.number()?;
            if !self.eat(b'.') {
                return None;
            }
            let d = self.number()?;
            if !(1..=12).contains(&m) || !(1..=5).contains(&w) || !(0..=6).contains(&d) {
                return None;
            }
            RuleDay::MonthWeekDay(m as u8, w as u8, d as u8)
        } else {
            RuleDay::Julian0(u16::try_from(self.number()?).ok()?)
        };
        let time = if self.eat(b'/') { self.hms()? } else { 7200 };
        Some((day, time))
    }
}

fn parse_posix_tz(s: &str) -> Option<PosixTz> {
    let mut c = Cursor {
        s: s.as_bytes(),
        i: 0,
    };
    let std_abbreviation = c.abbreviation()?;
    // POSIX offsets are west-positive.
    let std_offset = -c.hms()?;
    let mut dst = None;
    if c.peek().is_some() {
        let abbreviation = c.abbreviation()?;
        let offset = match c.peek() {
            Some(b',') | None => std_offset + 3600,
            _ => -c.hms()?,
        };
        let ((start, start_time), (end, end_time)) = if c.eat(b',') {
            let start = c.rule()?;
            if !c.eat(b',') {
                return None;
            }
            (start, c.rule()?)
        } else {
            (
                (RuleDay::MonthWeekDay(3, 2, 0), 7200),
                (RuleDay::MonthWeekDay(11, 1, 0), 7200),
            )
        };
        dst = Some(PosixDst {
            abbreviation,
            offset,
            start,
            start_time,
            end,
            end_time,
        });
    }
    if c.peek().is_some() {
        return None;
    }
    Some(PosixTz {
        std_abbreviation,
        std_offset,
        dst,
    })
}

fn rule_epoch_day(rule: RuleDay, year: i64) -> i64 {
    use crate::temporal::{epoch_days_from_iso_date, is_leap_year, iso_days_in_month};
    let jan1 = epoch_days_from_iso_date(year, 1, 1);
    match rule {
        RuleDay::Julian1(n) => {
            let n = i64::from(n.clamp(1, 365));
            // Day 60 is March 1 in every year.
            let leap = i64::from(is_leap_year(year) && n >= 60);
            jan1 + n - 1 + leap
        }
        RuleDay::Julian0(n) => jan1 + i64::from(n.min(365)),
        RuleDay::MonthWeekDay(m, w, d) => {
            let first = epoch_days_from_iso_date(year, i64::from(m), 1);
            // 1970-01-01 was a Thursday (4).
            let weekday_of_first = (first + 4).rem_euclid(7);
            let mut day = first + (i64::from(d) - weekday_of_first).rem_euclid(7);
            day += 7 * (i64::from(w) - 1);
            let last = first + iso_days_in_month(year, i64::from(m)) - 1;
            while day > last {
                day -= 7;
            }
            day
        }
    }
}

impl PosixTz {
    /// The two transitions of `year` in epoch seconds, each with whether
    /// DST starts there.
    fn transitions_in(&self, year: i64) -> Vec<(i64, bool)> {
        let Some(dst) = &self.dst else {
            return Vec::new();
        };
        let start = rule_epoch_day(dst.start, year) * SECONDS_PER_DAY + i64::from(dst.start_time)
            - i64::from(self.std_offset);
        let end = rule_epoch_day(dst.end, year) * SECONDS_PER_DAY + i64::from(dst.end_time)
            - i64::from(dst.offset);
        let mut v = vec![(start, true), (end, false)];
        v.sort_unstable();
        v
    }

    fn in_dst(&self, t: i64) -> bool {
        if self.dst.is_none() {
            return false;
        }
        let (year, ..) = crate::temporal::iso_date_from_epoch_days(t.div_euclid(SECONDS_PER_DAY));
        let mut state = None;
        for y in year - 1..=year + 1 {
            for (when, starts) in self.transitions_in(y) {
                if when <= t {
                    state = Some(starts);
                }
            }
        }
        state.unwrap_or(false)
    }

    fn offset_at(&self, t: i64) -> i32 {
        match &self.dst {
            Some(dst) if self.in_dst(t) => dst.offset,
            _ => self.std_offset,
        }
    }

    fn local_type_at(&self, t: i64) -> LocalTimeType {
        match &self.dst {
            Some(dst) if self.in_dst(t) => LocalTimeType {
                offset: dst.offset,
                is_dst: true,
                abbreviation: dst.abbreviation.clone(),
            },
            _ => LocalTimeType {
                offset: self.std_offset,
                is_dst: false,
                abbreviation: self.std_abbreviation.clone(),
            },
        }
    }
}

// ----- Identifier index -----

/// The available named time zone identifiers.
struct ZoneIndex {
    /// Identifiers in their case-normalized form.
    ids: Vec<String>,
    /// Lowercase identifier → index into `ids`.
    by_lower: HashMap<String, usize>,
    /// Index into `ids` of the primary identifier of each identifier.
    primary: Vec<usize>,
}

fn is_tzif(path: &Path) -> bool {
    use std::io::Read;
    let mut magic = [0u8; 4];
    std::fs::File::open(path)
        .and_then(|mut f| f.read_exact(&mut magic))
        .is_ok()
        && &magic == b"TZif"
}

fn walk(dir: &Path, prefix: &str, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let id = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        let path = entry.path();
        if path.is_dir() {
            if prefix.is_empty() && (name == "posix" || name == "right") {
                continue;
            }
            walk(&path, &id, out);
        } else if !matches!(name.as_str(), "posixrules" | "localtime" | "Factory") && is_tzif(&path)
        {
            out.push(id);
        }
    }
}

/// Identifiers whose primary identifier is `UTC` (ECMA-262 §21.4.1.20
/// and ECMA-402 treat these links of `Etc/UTC` and `Etc/GMT` as `UTC`).
fn is_utc_alias(id: &str) -> bool {
    matches!(id, "Etc/UTC" | "Etc/GMT" | "GMT" | "UTC")
}

fn build_index() -> ZoneIndex {
    let mut ids = Vec::new();
    walk(Path::new(ZONEINFO), "", &mut ids);
    if !ids.iter().any(|i| i == "UTC") {
        ids.push("UTC".to_string());
    }
    ids.sort();
    ids.dedup();
    let mut links: HashMap<String, String> = HashMap::new();
    if let Ok(zi) = std::fs::read_to_string(format!("{ZONEINFO}/tzdata.zi")) {
        for line in zi.lines() {
            let mut parts = line.split_whitespace();
            if parts.next() == Some("L")
                && let (Some(target), Some(link)) = (parts.next(), parts.next())
            {
                links.insert(link.to_string(), target.to_string());
            }
        }
    }
    let by_lower: HashMap<String, usize> = ids
        .iter()
        .enumerate()
        .map(|(i, id)| (id.to_ascii_lowercase(), i))
        .collect();
    let utc = by_lower["utc"];
    let primary = ids
        .iter()
        .enumerate()
        .map(|(i, id)| {
            let mut target = id.as_str();
            for _ in 0..8 {
                match links.get(target) {
                    Some(next) => target = next,
                    None => break,
                }
            }
            if is_utc_alias(target) || is_utc_alias(id) {
                return utc;
            }
            by_lower
                .get(&target.to_ascii_lowercase())
                .copied()
                .unwrap_or(i)
        })
        .collect();
    ZoneIndex {
        ids,
        by_lower,
        primary,
    }
}

fn index() -> &'static ZoneIndex {
    static INDEX: OnceLock<ZoneIndex> = OnceLock::new();
    INDEX.get_or_init(build_index)
}

/// A time zone identifier record (GetAvailableNamedTimeZoneIdentifier):
/// the identifier in its case-normalized form and its primary identifier.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ZoneIdentifier {
    pub identifier: String,
    pub primary: String,
}

/// GetAvailableNamedTimeZoneIdentifier(timeZoneIdentifier): an ASCII
/// case-insensitive lookup.
pub fn find_zone(name: &str) -> Option<ZoneIdentifier> {
    let index = index();
    let i = *index.by_lower.get(&name.to_ascii_lowercase())?;
    Some(ZoneIdentifier {
        identifier: index.ids[i].clone(),
        primary: index.ids[index.primary[i]].clone(),
    })
}

/// AvailableNamedTimeZoneIdentifiers: every identifier, sorted.
pub fn available_zones() -> Vec<ZoneIdentifier> {
    let index = index();
    index
        .ids
        .iter()
        .enumerate()
        .map(|(i, id)| ZoneIdentifier {
            identifier: id.clone(),
            primary: index.ids[index.primary[i]].clone(),
        })
        .collect()
}

/// The rules of a named zone (any case, links included).
pub fn zone(name: &str) -> Option<Arc<TimeZoneData>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Arc<TimeZoneData>>>> = OnceLock::new();
    let id = find_zone(name)?.identifier;
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(data) = cache.lock().ok()?.get(&id) {
        return Some(data.clone());
    }
    let data = match std::fs::read(format!("{ZONEINFO}/{id}")) {
        Ok(bytes) => TimeZoneData::parse(&bytes)?,
        Err(_) if id == "UTC" => TimeZoneData::utc(),
        Err(_) => return None,
    };
    let data = Arc::new(data);
    cache.lock().ok()?.insert(id, data.clone());
    Some(data)
}

/// The host's local time zone.
pub struct LocalZone {
    /// An available named identifier or an offset identifier
    /// (`+05:30`).
    pub id: String,
    pub data: Arc<TimeZoneData>,
}

fn offset_identifier(seconds: i32) -> String {
    let sign = if seconds < 0 { '-' } else { '+' };
    let minutes = seconds.unsigned_abs() / 60;
    format!("{sign}{:02}:{:02}", minutes / 60, minutes % 60)
}

fn zone_from_path(path: &str) -> Option<LocalZone> {
    let bytes = std::fs::read(path).ok()?;
    let data = Arc::new(TimeZoneData::parse(&bytes)?);
    let target = std::fs::canonicalize(path)
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
        .or_else(|| {
            std::fs::read_link(path)
                .ok()
                .map(|p| p.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| path.to_string());
    let link = std::fs::read_link(path)
        .ok()
        .map(|p| p.to_string_lossy().into_owned());
    let id = [link.as_deref(), Some(target.as_str()), Some(path)]
        .into_iter()
        .flatten()
        .find_map(|p| {
            let (_, rel) = p.rsplit_once("zoneinfo/")?;
            find_zone(rel).map(|z| z.identifier)
        })
        .unwrap_or_else(|| offset_identifier(data.offset_at(0)));
    Some(LocalZone { id, data })
}

fn utc_zone() -> LocalZone {
    LocalZone {
        id: "UTC".to_string(),
        data: zone("UTC").unwrap_or_else(|| Arc::new(TimeZoneData::utc())),
    }
}

fn detect_local() -> LocalZone {
    match std::env::var("TZ") {
        Ok(tz) => {
            let tz = tz.strip_prefix(':').unwrap_or(&tz);
            if tz.is_empty() {
                return utc_zone();
            }
            if tz.starts_with('/') {
                return zone_from_path(tz).unwrap_or_else(utc_zone);
            }
            if let Some(found) = find_zone(tz)
                && let Some(data) = zone(&found.identifier)
            {
                return LocalZone {
                    id: found.identifier,
                    data,
                };
            }
            match TimeZoneData::from_posix(tz) {
                Some(data) => LocalZone {
                    id: offset_identifier(data.offset_at(0)),
                    data: Arc::new(data),
                },
                None => utc_zone(),
            }
        }
        Err(_) => zone_from_path("/etc/localtime").unwrap_or_else(utc_zone),
    }
}

/// The local time zone, read once per process.
pub fn local() -> &'static LocalZone {
    static LOCAL: OnceLock<LocalZone> = OnceLock::new();
    LOCAL.get_or_init(detect_local)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn posix_rules_of_new_york() {
        let tz = TimeZoneData::from_posix("EST5EDT,M3.2.0,M11.1.0").unwrap();
        // 2024-03-10T06:59:59Z is 01:59:59 EST; one second later is
        // 03:00:00 EDT.
        let t = 1_710_053_999;
        assert_eq!(tz.offset_at(t), -5 * 3600);
        assert_eq!(tz.offset_at(t + 1), -4 * 3600);
        // 2024-11-03T05:59:59Z is 01:59:59 EDT, then 01:00:00 EST.
        let t = 1_730_613_599;
        assert_eq!(tz.offset_at(t), -4 * 3600);
        assert_eq!(tz.offset_at(t + 1), -5 * 3600);
        assert_eq!(tz.local_type_at(t).abbreviation, "EDT");
        assert_eq!(tz.next_transition(t), Some(t + 1));
    }

    #[test]
    fn gaps_and_folds() {
        let tz = TimeZoneData::from_posix("EST5EDT,M3.2.0,M11.1.0").unwrap();
        // 2024-03-10T02:30 local does not exist.
        let local = 1_710_037_800;
        assert!(tz.possible_instants(local).is_empty());
        // 2024-11-03T01:30 local happens twice.
        let local = 1_730_597_400;
        assert_eq!(
            tz.possible_instants(local),
            vec![local + 4 * 3600, local + 5 * 3600]
        );
    }

    #[test]
    fn southern_hemisphere_and_quoted_abbreviations() {
        let tz = TimeZoneData::from_posix("<-03>3").unwrap();
        assert_eq!(tz.offset_at(0), -3 * 3600);
        let tz = TimeZoneData::from_posix("AEST-10AEDT,M10.1.0,M4.1.0/3").unwrap();
        // 2024-01-15 is summer in Sydney.
        assert_eq!(tz.offset_at(1_705_276_800), 11 * 3600);
        // 2024-07-15 is winter.
        assert_eq!(tz.offset_at(1_721_001_600), 10 * 3600);
    }
}
