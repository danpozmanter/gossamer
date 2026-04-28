//! Runtime support for `std::time`.

#![forbid(unsafe_code)]

use std::time::{Duration as StdDuration, Instant as StdInstant, SystemTime as StdSystemTime};

/// Monotonic point-in-time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Instant(StdInstant);

impl Instant {
    /// Returns the current monotonic instant.
    #[must_use]
    pub fn now() -> Self {
        Self(StdInstant::now())
    }

    /// Returns the duration elapsed since `earlier`, saturating at
    /// zero if `earlier` is in the future.
    #[must_use]
    pub fn duration_since(self, earlier: Self) -> Duration {
        Duration(self.0.saturating_duration_since(earlier.0))
    }

    /// Returns how much time has elapsed since this instant was
    /// captured.
    #[must_use]
    pub fn elapsed(self) -> Duration {
        Duration(self.0.elapsed())
    }
}

/// Difference between two [`Instant`]s.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Duration(StdDuration);

impl Duration {
    /// Zero duration.
    pub const ZERO: Self = Self(StdDuration::ZERO);

    /// Builds a duration from whole milliseconds.
    #[must_use]
    pub const fn from_millis(ms: u64) -> Self {
        Self(StdDuration::from_millis(ms))
    }

    /// Builds a duration from whole microseconds.
    #[must_use]
    pub const fn from_micros(us: u64) -> Self {
        Self(StdDuration::from_micros(us))
    }

    /// Builds a duration from whole seconds.
    #[must_use]
    pub const fn from_secs(secs: u64) -> Self {
        Self(StdDuration::from_secs(secs))
    }

    /// Returns the duration as whole milliseconds, saturating at
    /// `u64::MAX`.
    #[must_use]
    pub const fn as_millis(self) -> u128 {
        self.0.as_millis()
    }

    /// Returns the duration as whole microseconds.
    #[must_use]
    pub const fn as_micros(self) -> u128 {
        self.0.as_micros()
    }

    /// Returns the seconds portion.
    #[must_use]
    pub const fn as_secs(self) -> u64 {
        self.0.as_secs()
    }
}

/// Wall-clock point-in-time.
#[derive(Debug, Clone, Copy)]
pub struct SystemTime(StdSystemTime);

impl SystemTime {
    /// Current wall-clock time.
    #[must_use]
    pub fn now() -> Self {
        Self(StdSystemTime::now())
    }

    /// Milliseconds since the Unix epoch. Returns zero for times
    /// before 1970 (extremely unlikely in tests).
    #[must_use]
    pub fn unix_millis(self) -> u128 {
        self.0
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis())
    }

    /// Constructs a `SystemTime` from a millisecond offset relative
    /// to the Unix epoch. Negative offsets refer to pre-1970 times.
    /// Mirrors Go's `time.UnixMilli`.
    #[must_use]
    pub fn from_unix_millis(ms: i64) -> Self {
        let inner = if ms >= 0 {
            std::time::UNIX_EPOCH + std::time::Duration::from_millis(ms as u64)
        } else {
            std::time::UNIX_EPOCH - std::time::Duration::from_millis((-ms) as u64)
        };
        Self(inner)
    }
}

/// Suspends the current goroutine (or OS thread, when called from
/// outside a goroutine context) for `duration`. Internally registers
/// a one-shot timer with the netpoller so a sleeping goroutine does
/// not consume an OS thread while it waits.
pub fn sleep(duration: Duration) {
    if duration.0.is_zero() {
        return;
    }
    let deadline = std::time::Instant::now() + duration.0;
    crate::sched_global::sleep_until(deadline);
}

/// Convenience wrapper around [`Instant::now`].
#[must_use]
pub fn now() -> Instant {
    Instant::now()
}

/// Errors raised by [`format_rfc3339`] / [`parse_rfc3339`].
#[derive(Debug, Clone, thiserror::Error)]
pub enum FormatError {
    /// Input string did not match the expected layout.
    #[error("time::parse: {0}")]
    BadInput(String),
    /// Time fell outside the representable Gregorian range.
    #[error("time::format: {0}")]
    OutOfRange(String),
}

/// Renders a wall-clock instant in RFC 3339 form
/// (`2006-01-02T15:04:05Z`). Always emits UTC; offset-aware
/// formatting waits on a real timezone surface.
pub fn format_rfc3339(when: SystemTime) -> Result<String, FormatError> {
    let secs = match when.0.duration_since(std::time::UNIX_EPOCH) {
        Ok(dur) => i128::from(dur.as_secs()),
        Err(err) => -i128::from(err.duration().as_secs()),
    };
    let civil = unix_to_civil(secs)?;
    Ok(format!(
        "{year:04}-{mo:02}-{day:02}T{hour:02}:{min:02}:{sec:02}Z",
        year = civil.year,
        mo = civil.month,
        day = civil.day,
        hour = civil.hour,
        min = civil.minute,
        sec = civil.second,
    ))
}

/// Parses an RFC 3339 timestamp. Accepts `T` or space as the
/// date/time separator; accepts `Z`, `+HH:MM`, `-HH:MM`, or no
/// suffix (assumes UTC). Sub-second fractions are accepted but
/// silently dropped — full precision waits on a real time type.
pub fn parse_rfc3339(s: &str) -> Result<SystemTime, FormatError> {
    let bytes = s.as_bytes();
    let bad = || FormatError::BadInput(s.to_string());
    if bytes.len() < 19 {
        return Err(bad());
    }
    let year: i32 = parse_signed(&bytes[0..4]).ok_or_else(bad)?;
    if bytes[4] != b'-' {
        return Err(bad());
    }
    let month: u32 = parse_unsigned(&bytes[5..7]).ok_or_else(bad)?;
    if bytes[7] != b'-' {
        return Err(bad());
    }
    let day: u32 = parse_unsigned(&bytes[8..10]).ok_or_else(bad)?;
    if !matches!(bytes[10], b'T' | b' ') {
        return Err(bad());
    }
    let hour: u32 = parse_unsigned(&bytes[11..13]).ok_or_else(bad)?;
    if bytes[13] != b':' {
        return Err(bad());
    }
    let minute: u32 = parse_unsigned(&bytes[14..16]).ok_or_else(bad)?;
    if bytes[16] != b':' {
        return Err(bad());
    }
    let second: u32 = parse_unsigned(&bytes[17..19]).ok_or_else(bad)?;
    let mut cursor = 19;
    if bytes.get(cursor) == Some(&b'.') {
        cursor += 1;
        while cursor < bytes.len() && bytes[cursor].is_ascii_digit() {
            cursor += 1;
        }
    }
    let mut offset_seconds: i64 = 0;
    if cursor < bytes.len() {
        match bytes[cursor] {
            b'Z' => cursor += 1,
            b'+' | b'-' => {
                if cursor + 5 >= bytes.len() {
                    return Err(bad());
                }
                let sign: i64 = if bytes[cursor] == b'+' { 1 } else { -1 };
                let oh: u32 = parse_unsigned(&bytes[cursor + 1..cursor + 3]).ok_or_else(bad)?;
                if bytes[cursor + 3] != b':' {
                    return Err(bad());
                }
                let om: u32 = parse_unsigned(&bytes[cursor + 4..cursor + 6]).ok_or_else(bad)?;
                offset_seconds = sign * i64::from(oh * 3600 + om * 60);
                cursor += 6;
            }
            _ => return Err(bad()),
        }
    }
    if cursor != bytes.len() {
        return Err(bad());
    }
    if !valid_civil(year, month, day, hour, minute, second) {
        return Err(bad());
    }
    let unix = civil_to_unix(&CivilTime {
        year,
        month,
        day,
        hour,
        minute,
        second,
    }) - offset_seconds;
    let stdtime = if unix >= 0 {
        std::time::UNIX_EPOCH + std::time::Duration::from_secs(unix as u64)
    } else {
        std::time::UNIX_EPOCH - std::time::Duration::from_secs((-unix) as u64)
    };
    Ok(SystemTime(stdtime))
}

fn parse_unsigned(bytes: &[u8]) -> Option<u32> {
    let s = std::str::from_utf8(bytes).ok()?;
    s.parse::<u32>().ok()
}

fn parse_signed(bytes: &[u8]) -> Option<i32> {
    let s = std::str::from_utf8(bytes).ok()?;
    s.parse::<i32>().ok()
}

fn valid_civil(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> bool {
    if !(1..=12).contains(&mo) {
        return false;
    }
    if !(1..=days_in_month(y, mo)).contains(&d) {
        return false;
    }
    h < 24 && mi < 60 && s < 60
}

const fn is_leap(y: i32) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

const fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if is_leap(year) {
                29
            } else {
                28
            }
        }
        _ => 0,
    }
}

// Howard Hinnant's days_from_civil algorithm.
fn civil_to_days(y: i32, m: u32, d: u32) -> i64 {
    let m_i = m as i32;
    let y_adj = y - i32::from(m_i <= 2);
    let era = if y_adj >= 0 {
        y_adj / 400
    } else {
        (y_adj - 399) / 400
    };
    let yoe = (y_adj - era * 400) as u32;
    let m_eff = if m_i > 2 { m_i - 3 } else { m_i + 9 };
    let doy = (153 * m_eff as u32 + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    i64::from(era) * 146_097 + i64::from(doe) - 719_468
}

struct CivilDate {
    year: i32,
    month: u32,
    day: u32,
}

struct CivilTime {
    year: i32,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
}

fn civil_to_unix(c: &CivilTime) -> i64 {
    civil_to_days(c.year, c.month, c.day) * 86_400
        + i64::from(c.hour) * 3600
        + i64::from(c.minute) * 60
        + i64::from(c.second)
}

fn unix_to_civil(secs: i128) -> Result<CivilTime, FormatError> {
    if secs > i128::from(i64::MAX) || secs < i128::from(i64::MIN) {
        return Err(FormatError::OutOfRange(format!(
            "{secs} seconds out of range"
        )));
    }
    let secs = secs as i64;
    let days = secs.div_euclid(86_400);
    let time_of_day = secs.rem_euclid(86_400);
    let date = days_to_civil(days);
    Ok(CivilTime {
        year: date.year,
        month: date.month,
        day: date.day,
        hour: (time_of_day / 3600) as u32,
        minute: ((time_of_day % 3600) / 60) as u32,
        second: (time_of_day % 60) as u32,
    })
}

fn days_to_civil(days: i64) -> CivilDate {
    let z = days + 719_468;
    let era = if z >= 0 {
        z / 146_097
    } else {
        (z - 146_096) / 146_097
    };
    let doe = (z - era * 146_097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe as i32 + era as i32 * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    CivilDate {
        year: year + i32::from(month <= 2),
        month,
        day,
    }
}

/// IANA timezone-aware operations. Gated on the `tz` feature so the
/// stdlib stays slim by default; once the feature is on, callers
/// can construct a [`Location`] from any IANA name and convert
/// `SystemTime`s into local civil time and back.
#[cfg(feature = "tz")]
pub mod tz {

    use std::str::FromStr;

    use chrono::{DateTime, Datelike, NaiveDateTime, TimeZone, Timelike, Utc};
    use chrono_tz::Tz;

    use super::{FormatError, SystemTime};

    /// Reference to an IANA timezone (e.g. `"America/Los_Angeles"`).
    /// Cheap to clone — wraps the `chrono_tz::Tz` enum by value.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Location {
        tz: Tz,
    }

    impl Location {
        /// Resolves an IANA timezone name. Returns `Err` when the
        /// name is not in the bundled tzdata set.
        pub fn lookup(name: &str) -> Result<Self, FormatError> {
            Tz::from_str(name)
                .map(|tz| Self { tz })
                .map_err(|e| FormatError::BadInput(format!("unknown timezone {name:?}: {e}")))
        }

        /// UTC location (always available; never traps).
        #[must_use]
        pub fn utc() -> Self {
            Self { tz: Tz::UTC }
        }

        /// IANA name of the timezone.
        #[must_use]
        pub fn name(self) -> &'static str {
            self.tz.name()
        }

        /// Civil time fields for `when` rendered through `self`.
        #[must_use]
        pub fn civil(self, when: SystemTime) -> Civil {
            let unix = i64::try_from(when.unix_millis() / 1000).unwrap_or(0);
            let utc: DateTime<Utc> = DateTime::from_timestamp(unix, 0)
                .unwrap_or_else(|| DateTime::from_timestamp(0, 0).expect("epoch is valid"));
            let local = utc.with_timezone(&self.tz);
            Civil {
                year: local.year(),
                month: local.month(),
                day: local.day(),
                hour: local.hour(),
                minute: local.minute(),
                second: local.second(),
                offset_seconds: chrono::Offset::fix(local.offset()).local_minus_utc(),
                weekday: local.weekday().num_days_from_monday(),
            }
        }
    }

    /// Civil time fields rendered in a specific [`Location`].
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Civil {
        /// Calendar year (e.g. `2026`).
        pub year: i32,
        /// 1..=12 calendar month.
        pub month: u32,
        /// 1..=31 calendar day.
        pub day: u32,
        /// 0..=23 hour-of-day.
        pub hour: u32,
        /// 0..=59 minute.
        pub minute: u32,
        /// 0..=59 second.
        pub second: u32,
        /// Offset from UTC in seconds (positive east of Greenwich).
        pub offset_seconds: i32,
        /// 0=Mon … 6=Sun.
        pub weekday: u32,
    }

    /// Parses `input` against the supplied `layout` in Go's reference-time
    /// format. The reference time is `2006-01-02 15:04:05 MST` (Mon Jan 2,
    /// 03:04:05 PM 2006). Extra layout tokens are passed through verbatim.
    /// Returns the time normalised to UTC.
    pub fn parse(layout: &str, input: &str) -> Result<SystemTime, FormatError> {
        let chrono_fmt = go_layout_to_chrono(layout);
        // Try with timezone first, fall back to naive.
        if let Ok(dt) = DateTime::parse_from_str(input, &chrono_fmt) {
            let unix = dt.with_timezone(&Utc).timestamp();
            return Ok(SystemTime::from_unix_millis(unix.saturating_mul(1000)));
        }
        match NaiveDateTime::parse_from_str(input, &chrono_fmt) {
            Ok(dt) => {
                let utc = Utc.from_utc_datetime(&dt);
                Ok(SystemTime::from_unix_millis(
                    utc.timestamp().saturating_mul(1000),
                ))
            }
            Err(e) => Err(FormatError::BadInput(format!(
                "time::parse({layout:?}, {input:?}): {e}"
            ))),
        }
    }

    /// Renders `when` according to the Go-shaped `layout` in `loc`.
    pub fn format_in(layout: &str, when: SystemTime, loc: Location) -> Result<String, FormatError> {
        let chrono_fmt = go_layout_to_chrono(layout);
        let unix = i64::try_from(when.unix_millis() / 1000)
            .map_err(|_| FormatError::OutOfRange("time too far from epoch".into()))?;
        let utc: DateTime<Utc> = DateTime::from_timestamp(unix, 0)
            .ok_or_else(|| FormatError::OutOfRange(format!("{unix} seconds out of range")))?;
        let local = utc.with_timezone(&loc.tz);
        Ok(local.format(&chrono_fmt).to_string())
    }

    /// Adds `years`, `months`, and `days` to `when` in the supplied
    /// location, mirroring Go's `Time.AddDate`. Negative values
    /// subtract; month-end clamping matches `chrono`'s behaviour.
    pub fn add_date(
        when: SystemTime,
        loc: Location,
        years: i32,
        months: i32,
        days: i32,
    ) -> Result<SystemTime, FormatError> {
        let unix = i64::try_from(when.unix_millis() / 1000)
            .map_err(|_| FormatError::OutOfRange("time too far from epoch".into()))?;
        let utc: DateTime<Utc> = DateTime::from_timestamp(unix, 0)
            .ok_or_else(|| FormatError::OutOfRange(format!("{unix} out of range")))?;
        let local = utc.with_timezone(&loc.tz);
        // Year/month manually so we clamp to the last day of the
        // target month rather than wrapping into the next.
        let mut new_year = local.year() + years;
        let mut new_month_zero = (local.month0() as i32) + months;
        new_year += new_month_zero.div_euclid(12);
        new_month_zero = new_month_zero.rem_euclid(12);
        let new_month = (new_month_zero as u32) + 1;
        let dim = days_in_month(new_year, new_month);
        let new_day = local.day().min(dim);
        let candidate = loc
            .tz
            .with_ymd_and_hms(
                new_year,
                new_month,
                new_day,
                local.hour(),
                local.minute(),
                local.second(),
            )
            .single()
            .ok_or_else(|| FormatError::BadInput("ambiguous local time".into()))?
            + chrono::Duration::days(i64::from(days));
        let new_unix = candidate.with_timezone(&Utc).timestamp();
        Ok(SystemTime::from_unix_millis(new_unix.saturating_mul(1000)))
    }

    fn days_in_month(year: i32, month: u32) -> u32 {
        match month {
            1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
            4 | 6 | 9 | 11 => 30,
            2 => {
                if super::is_leap(year) {
                    29
                } else {
                    28
                }
            }
            _ => 0,
        }
    }

    /// Maps Go's reference-time tokens onto chrono's `strftime` format.
    /// The reference time is:
    ///   Mon Jan  2 15:04:05 MST 2006
    /// We translate the well-known tokens and pass everything else
    /// through verbatim. Not exhaustive — covers RFC3339 / common log
    /// shapes.
    fn go_layout_to_chrono(layout: &str) -> String {
        let mut out = String::with_capacity(layout.len() + 8);
        let bytes = layout.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            // Match longest-token-first.
            let rest = &bytes[i..];
            if rest.starts_with(b"2006") {
                out.push_str("%Y");
                i += 4;
            } else if rest.starts_with(b"06") {
                out.push_str("%y");
                i += 2;
            } else if rest.starts_with(b"01") {
                out.push_str("%m");
                i += 2;
            } else if rest.starts_with(b"Jan") {
                out.push_str("%b");
                i += 3;
            } else if rest.starts_with(b"02") {
                out.push_str("%d");
                i += 2;
            } else if rest.starts_with(b"Mon") {
                out.push_str("%a");
                i += 3;
            } else if rest.starts_with(b"15") {
                out.push_str("%H");
                i += 2;
            } else if rest.starts_with(b"04") {
                out.push_str("%M");
                i += 2;
            } else if rest.starts_with(b"05") {
                out.push_str("%S");
                i += 2;
            } else if rest.starts_with(b"-0700") {
                out.push_str("%z");
                i += 5;
            } else if rest.starts_with(b"Z07:00") {
                out.push_str("%:z");
                i += 6;
            } else if rest.starts_with(b"MST") {
                out.push_str("%Z");
                i += 3;
            } else if rest[0] == b'%' {
                out.push_str("%%");
                i += 1;
            } else {
                out.push(rest[0] as char);
                i += 1;
            }
        }
        out
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn lookup_known_zone() {
            let la = Location::lookup("America/Los_Angeles").unwrap();
            assert_eq!(la.name(), "America/Los_Angeles");
        }

        #[test]
        fn lookup_unknown_zone_errors() {
            assert!(Location::lookup("Pluto/Crater").is_err());
        }

        #[test]
        fn parse_go_layout() {
            let t = parse("2006-01-02T15:04:05Z07:00", "2026-04-27T12:34:56-07:00").unwrap();
            assert_eq!(
                super::super::format_rfc3339(t).unwrap(),
                "2026-04-27T19:34:56Z"
            );
        }

        #[test]
        fn parse_naive_layout() {
            let t = parse("2006-01-02 15:04:05", "2026-04-27 12:00:00").unwrap();
            assert_eq!(
                super::super::format_rfc3339(t).unwrap(),
                "2026-04-27T12:00:00Z"
            );
        }

        #[test]
        fn add_date_handles_month_overflow() {
            let t = super::super::parse_rfc3339("2026-01-31T12:00:00Z").unwrap();
            let utc = Location::utc();
            let plus_month = add_date(t, utc, 0, 1, 0).unwrap();
            // Feb has 28 days in 2026, so day clamps to 28.
            assert_eq!(
                super::super::format_rfc3339(plus_month).unwrap(),
                "2026-02-28T12:00:00Z"
            );
            let plus_year = add_date(t, utc, 1, 0, 0).unwrap();
            assert_eq!(
                super::super::format_rfc3339(plus_year).unwrap(),
                "2027-01-31T12:00:00Z"
            );
        }

        #[test]
        fn civil_in_location_includes_offset() {
            let when = super::super::parse_rfc3339("2026-04-27T12:00:00Z").unwrap();
            let la = Location::lookup("America/Los_Angeles").unwrap();
            let civil = la.civil(when);
            // Pacific Daylight Time (UTC-7).
            assert_eq!(civil.offset_seconds, -7 * 3600);
            assert_eq!(civil.hour, 5);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_epoch_renders_zero() {
        let formatted = format_rfc3339(SystemTime(std::time::UNIX_EPOCH)).unwrap();
        assert_eq!(formatted, "1970-01-01T00:00:00Z");
    }

    #[test]
    fn round_trip_known_timestamp() {
        let t = parse_rfc3339("2026-04-25T16:30:45Z").unwrap();
        let formatted = format_rfc3339(t).unwrap();
        assert_eq!(formatted, "2026-04-25T16:30:45Z");
    }

    #[test]
    fn parse_accepts_offset_then_normalises_to_utc() {
        let t = parse_rfc3339("2026-04-25T18:30:00+02:00").unwrap();
        let formatted = format_rfc3339(t).unwrap();
        assert_eq!(formatted, "2026-04-25T16:30:00Z");
    }

    #[test]
    fn parse_accepts_space_separator_and_fractional_seconds() {
        let t = parse_rfc3339("2026-04-25 16:30:45.123456Z").unwrap();
        assert_eq!(format_rfc3339(t).unwrap(), "2026-04-25T16:30:45Z");
    }

    #[test]
    fn parse_rejects_invalid_dates() {
        assert!(parse_rfc3339("2026-13-01T00:00:00Z").is_err());
        assert!(parse_rfc3339("2026-02-30T00:00:00Z").is_err());
        assert!(parse_rfc3339("totally bogus").is_err());
        assert!(parse_rfc3339("2026-04-25T25:00:00Z").is_err());
    }

    #[test]
    fn handles_leap_year_february_29() {
        let t = parse_rfc3339("2024-02-29T12:00:00Z").unwrap();
        assert_eq!(format_rfc3339(t).unwrap(), "2024-02-29T12:00:00Z");
        // 2025 is not a leap year.
        assert!(parse_rfc3339("2025-02-29T12:00:00Z").is_err());
    }

    #[test]
    fn duration_constructors_round_trip() {
        let d = Duration::from_secs(42);
        assert_eq!(d.as_secs(), 42);
        assert_eq!(d.as_millis(), 42_000);
    }
}
