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
            std::time::UNIX_EPOCH
                - std::time::Duration::from_millis((-ms) as u64)
        };
        Self(inner)
    }
}

/// Suspends the current thread for `duration`.
pub fn sleep(duration: Duration) {
    std::thread::sleep(duration.0);
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
                let oh: u32 = parse_unsigned(&bytes[cursor + 1..cursor + 3])
                    .ok_or_else(bad)?;
                if bytes[cursor + 3] != b':' {
                    return Err(bad());
                }
                let om: u32 = parse_unsigned(&bytes[cursor + 4..cursor + 6])
                    .ok_or_else(bad)?;
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
    let era = if y_adj >= 0 { y_adj / 400 } else { (y_adj - 399) / 400 };
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
        return Err(FormatError::OutOfRange(format!("{secs} seconds out of range")));
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
    let era = if z >= 0 { z / 146_097 } else { (z - 146_096) / 146_097 };
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
