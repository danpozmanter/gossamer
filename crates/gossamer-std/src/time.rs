//! Runtime support for `std::time`.

#![forbid(unsafe_code)]
#![allow(missing_docs)]

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
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct SystemTime(StdSystemTime);

impl SystemTime {
    /// Current wall-clock time.
    #[must_use]
    pub fn now() -> Self {
        Self(StdSystemTime::now())
    }

    /// Signed milliseconds from the Unix epoch.
    ///
    /// Values before 1970 are negative. Values outside the `i64`
    /// millisecond range are clamped at the corresponding bound.
    #[must_use]
    pub fn unix_millis(self) -> i64 {
        match self.0.duration_since(std::time::UNIX_EPOCH) {
            Ok(duration) => i64::try_from(duration.as_millis()).unwrap_or(i64::MAX),
            Err(error) => {
                let duration = error.duration();
                let millis = u128::from(duration.as_secs()) * 1_000
                    + u128::from(duration.subsec_nanos()).div_ceil(1_000_000);
                i64::try_from(millis).map_or(i64::MIN, |value| -value)
            }
        }
    }

    /// Wraps a `std::time::SystemTime` into the Gossamer-native
    /// `SystemTime` type. Useful when bridging from filesystem
    /// metadata (`fs::Metadata::modified()` etc.) into formatters
    /// like [`format_rfc1123_gmt`].
    #[must_use]
    pub fn from_std(t: StdSystemTime) -> Self {
        Self(t)
    }

    /// Returns the underlying `std::time::SystemTime`.
    #[must_use]
    pub fn as_std(self) -> StdSystemTime {
        self.0
    }

    /// Returns seconds since the Unix epoch (negative for
    /// pre-1970 instants).
    #[must_use]
    pub fn unix_seconds(self) -> i64 {
        match self.0.duration_since(std::time::UNIX_EPOCH) {
            Ok(duration) => i64::try_from(duration.as_secs()).unwrap_or(i64::MAX),
            Err(error) => {
                let duration = error.duration();
                let seconds = duration.as_secs() + u64::from(duration.subsec_nanos() > 0);
                i64::try_from(seconds).map_or(i64::MIN, |value| -value)
            }
        }
    }

    /// Constructs a `SystemTime` from a millisecond offset relative
    /// to the Unix epoch. Negative offsets refer to pre-1970 times.
    /// Mirrors Go's `time.UnixMilli`.
    #[must_use]
    pub fn from_unix_millis(ms: i64) -> Self {
        let distance = std::time::Duration::from_millis(ms.unsigned_abs());
        let inner = if ms >= 0 {
            std::time::UNIX_EPOCH + distance
        } else {
            std::time::UNIX_EPOCH - distance
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

/// Cancellation-aware variant of [`sleep`].
///
/// Behaves identically to `sleep(duration)` when `ctx` is not
/// cancelled. If `ctx` is cancelled while the goroutine is parked -
/// either via `Cancel::cancel_with` or via a `with_deadline`
/// elapsing - the sleep returns early. The return value is
/// `Ok(())` for a natural completion and `Err(context error)`
/// for a cancellation-driven wake-up.
///
/// Race-free against an already-cancelled `ctx`: the function
/// checks before parking and again after parking; the wait-list
/// registration is the synchronisation point that lets
/// `Cancel::cancel_with` reach the goroutine while it sleeps.
pub fn sleep_ctx(
    ctx: &crate::context::Context,
    duration: Duration,
) -> Result<(), crate::errors::Error> {
    if let Some(err) = ctx.err() {
        return Err(err);
    }
    if duration.0.is_zero() {
        return Ok(());
    }
    let gid = crate::sched_global::current_gid().expect(
        "time::sleep_ctx must be called from a goroutine; use time::sleep outside a goroutine",
    );
    ctx.register_waiter(gid);
    // Re-check after registration; cancel may have fired between
    // the entry check and the registration.
    if ctx.is_cancelled() {
        ctx.deregister_waiter(gid);
        return Err(ctx
            .err()
            .unwrap_or_else(|| crate::errors::Error::new("context cancelled")));
    }
    let deadline = std::time::Instant::now() + duration.0;
    crate::sched_global::sleep_until(deadline);
    ctx.deregister_waiter(gid);
    if ctx.is_cancelled() {
        return Err(ctx
            .err()
            .unwrap_or_else(|| crate::errors::Error::new("context cancelled")));
    }
    Ok(())
}

/// Convenience wrapper around [`Instant::now`].
#[must_use]
pub fn now() -> Instant {
    Instant::now()
}

/// Errors raised by [`format_rfc3339`] / [`parse_rfc3339`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FormatError {
    /// Input string did not match the expected layout.
    #[error("time::parse: {0}")]
    BadInput(String),
    /// Time fell outside the representable Gregorian range.
    #[error("time::format: {0}")]
    OutOfRange(String),
}

/// Renders a wall-clock instant in RFC 1123 (HTTP date) form
/// (`Sun, 06 Nov 1994 08:49:37 GMT`). Always GMT - this is the
/// canonical encoding for HTTP `Date`, `Last-Modified`, and
/// `If-Modified-Since` headers.
pub fn format_rfc1123_gmt(when: SystemTime) -> Result<String, FormatError> {
    let secs = match when.0.duration_since(std::time::UNIX_EPOCH) {
        Ok(dur) => i128::from(dur.as_secs()),
        Err(err) => -i128::from(err.duration().as_secs()),
    };
    if secs > i128::from(i64::MAX) || secs < i128::from(i64::MIN) {
        return Err(FormatError::OutOfRange(format!(
            "{secs} seconds out of range"
        )));
    }
    let civil = unix_to_civil(secs)?;
    // Day-of-week from days-since-1970-01-01 (a Thursday).
    let days = (secs as i64).div_euclid(86_400);
    let dow_idx = ((days + 4).rem_euclid(7)) as usize;
    let dow = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"][dow_idx];
    let month_names = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let mo_idx = (civil.month as usize)
        .checked_sub(1)
        .ok_or_else(|| FormatError::OutOfRange(format!("month {} out of range", civil.month)))?;
    let mo = month_names
        .get(mo_idx)
        .ok_or_else(|| FormatError::OutOfRange(format!("month {} out of range", civil.month)))?;
    Ok(format!(
        "{dow}, {day:02} {mo} {year:04} {hour:02}:{min:02}:{sec:02} GMT",
        day = civil.day,
        year = civil.year,
        hour = civil.hour,
        min = civil.minute,
        sec = civil.second,
    ))
}

/// Renders a wall-clock instant in RFC 3339 form
/// (`2006-01-02T15:04:05Z`). Always emits UTC; offset-aware
/// formatting waits on a real timezone surface.
pub fn format_rfc3339(when: SystemTime) -> Result<String, FormatError> {
    let secs = match when.0.duration_since(std::time::UNIX_EPOCH) {
        Ok(dur) => i128::from(dur.as_secs()),
        // Floor toward negative infinity so a pre-epoch instant maps to
        // the whole second that contains it (e.g. -1500ms is 23:59:58,
        // not 23:59:59); this matches the compiled tier's `div_euclid`.
        Err(err) => {
            let dur = err.duration();
            -(i128::from(dur.as_secs()) + i128::from(dur.subsec_nanos() > 0))
        }
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
/// silently dropped - full precision waits on a real time type.
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

/// Parses an HTTP-date (RFC 7231 §7.1.1.1). The preferred RFC 1123
/// form (`Sun, 06 Nov 1994 08:49:37 GMT`) is what browsers send in
/// `If-Modified-Since` / `If-Unmodified-Since`, echoing the server's
/// `Last-Modified`. The obsolete RFC 850 (`Sunday, 06-Nov-94
/// 08:49:37 GMT`) and asctime (`Sun Nov  6 08:49:37 1994`) forms are
/// also accepted, as RFC 7231 requires of recipients. Always GMT/UTC.
pub fn parse_rfc1123_gmt(s: &str) -> Result<SystemTime, FormatError> {
    let bad = || FormatError::BadInput(s.to_string());
    let trimmed = s.trim();
    let (day, month, year, hms) = if let Some(comma) = trimmed.find(',') {
        let toks: Vec<&str> = trimmed[comma + 1..].split_whitespace().collect();
        if toks.len() < 3 {
            return Err(bad());
        }
        if toks[0].contains('-') {
            // RFC 850: `06-Nov-94 08:49:37 GMT`.
            let p: Vec<&str> = toks[0].split('-').collect();
            if p.len() != 3 {
                return Err(bad());
            }
            let day: u32 = p[0].parse().map_err(|_| bad())?;
            let month = month_index(p[1]).ok_or_else(bad)?;
            let yy: i32 = p[2].parse().map_err(|_| bad())?;
            // Two-digit year window (RFC 6265 §5.1.1): 00..=68 -> 2000s.
            let year = if yy < 70 { 2000 + yy } else { 1900 + yy };
            (day, month, year, toks[1])
        } else {
            // RFC 1123: `06 Nov 1994 08:49:37 GMT`.
            if toks.len() < 4 {
                return Err(bad());
            }
            let day: u32 = toks[0].parse().map_err(|_| bad())?;
            let month = month_index(toks[1]).ok_or_else(bad)?;
            let year: i32 = toks[2].parse().map_err(|_| bad())?;
            (day, month, year, toks[3])
        }
    } else {
        // asctime: `Sun Nov  6 08:49:37 1994` (no comma).
        let toks: Vec<&str> = trimmed.split_whitespace().collect();
        if toks.len() < 5 {
            return Err(bad());
        }
        let month = month_index(toks[1]).ok_or_else(bad)?;
        let day: u32 = toks[2].parse().map_err(|_| bad())?;
        let year: i32 = toks[4].parse().map_err(|_| bad())?;
        (day, month, year, toks[3])
    };
    let t: Vec<&str> = hms.split(':').collect();
    if t.len() != 3 {
        return Err(bad());
    }
    let hour: u32 = t[0].parse().map_err(|_| bad())?;
    let minute: u32 = t[1].parse().map_err(|_| bad())?;
    let second: u32 = t[2].parse().map_err(|_| bad())?;
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
    });
    let stdtime = if unix >= 0 {
        std::time::UNIX_EPOCH + std::time::Duration::from_secs(unix as u64)
    } else {
        std::time::UNIX_EPOCH - std::time::Duration::from_secs((-unix) as u64)
    };
    Ok(SystemTime(stdtime))
}

/// Month abbreviation (`Jan`..`Dec`, case-insensitive) to 1-based index.
fn month_index(name: &str) -> Option<u32> {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    MONTHS
        .iter()
        .position(|m| m.eq_ignore_ascii_case(name))
        .map(|i| i as u32 + 1)
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

// --- Ticker / AfterFunc (Go's time.Ticker / time.AfterFunc) -------

/// Recurring timer. Calls `tick` on each interval until the
/// `stop` flag flips. The callback runs on the ticker's own
/// thread; long-running callbacks block subsequent ticks.
///
/// Returned [`Ticker`] handle is `Drop`-safe - dropping it
/// signals stop and joins the worker thread.
pub struct Ticker {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Ticker {
    /// Starts a new ticker that invokes `tick` every `interval`.
    pub fn start(interval: Duration, mut tick: impl FnMut() + Send + 'static) -> Self {
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop_for_thread = std::sync::Arc::clone(&stop);
        let std_interval = interval.0;
        let handle = std::thread::spawn(move || {
            let mut next = std::time::Instant::now() + std_interval;
            while !stop_for_thread.load(std::sync::atomic::Ordering::Acquire) {
                let now = std::time::Instant::now();
                if now < next {
                    let remaining = next - now;
                    // Sleep in short slices so we observe stop
                    // promptly without busy-looping.
                    let slice = std::cmp::min(remaining, std::time::Duration::from_millis(100));
                    std::thread::sleep(slice);
                    continue;
                }
                tick();
                next += std_interval;
            }
        });
        Self {
            stop,
            handle: Some(handle),
        }
    }

    /// Stops the ticker and waits for the worker thread to
    /// finish. Idempotent - subsequent calls are no-ops.
    pub fn stop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Release);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for Ticker {
    fn drop(&mut self) {
        self.stop();
    }
}

/// One-shot timer (Go's `time.AfterFunc`). Schedules `f` to run
/// after `delay` on a background thread. The returned
/// [`TimerHandle`] can cancel the timer before it fires.
pub fn after_func(delay: Duration, f: impl FnOnce() + Send + 'static) -> TimerHandle {
    let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let cancelled_for_thread = std::sync::Arc::clone(&cancelled);
    let std_delay = delay.0;
    let handle = std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + std_delay;
        loop {
            if cancelled_for_thread.load(std::sync::atomic::Ordering::Acquire) {
                return;
            }
            let now = std::time::Instant::now();
            if now >= deadline {
                break;
            }
            let slice = std::cmp::min(deadline - now, std::time::Duration::from_millis(100));
            std::thread::sleep(slice);
        }
        if !cancelled_for_thread.load(std::sync::atomic::Ordering::Acquire) {
            f();
        }
    });
    TimerHandle {
        cancelled,
        handle: Some(handle),
    }
}

/// Handle returned by [`after_func`].
pub struct TimerHandle {
    cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl TimerHandle {
    /// Cancels the timer (no-op if it has already fired).
    /// Returns `true` if the cancel happened before the timer
    /// fired.
    pub fn cancel(&mut self) -> bool {
        let was = self
            .cancelled
            .swap(true, std::sync::atomic::Ordering::AcqRel);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
        !was
    }
}

impl Drop for TimerHandle {
    fn drop(&mut self) {
        // Don't auto-cancel on drop - callers that fire-and-
        // forget the handle expect the timer to still fire.
        // We just detach the worker thread.
        if let Some(h) = self.handle.take() {
            let _ = h;
        }
    }
}

/// IANA timezone-aware operations. Native callers can construct a
/// `Location` from any bundled IANA name and convert
/// `SystemTime`s into local civil time and back.
///
/// Backed by `chrono` / `chrono-tz`, which target the host clock and
/// are gated out of the wasm playground; the rest of `std::time`
/// (instants, durations, formatting) stays available there.
#[cfg(not(target_arch = "wasm32"))]
pub mod tz {

    use std::str::FromStr;

    use chrono::{
        DateTime, Datelike, FixedOffset, LocalResult, NaiveDate, NaiveDateTime, Offset, TimeZone,
        Timelike, Utc,
    };
    use chrono_tz::Tz;

    use super::{FormatError, SystemTime};

    /// An explicit UTC, fixed-offset, or IANA timezone.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Location {
        kind: LocationKind,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum LocationKind {
        Iana(Tz),
        Fixed(FixedOffset),
    }

    impl Location {
        /// Resolves an IANA timezone name. Returns `Err` when the
        /// name is not in the bundled tzdata set.
        pub fn lookup(name: &str) -> Result<Self, FormatError> {
            Tz::from_str(name)
                .map(|tz| Self {
                    kind: LocationKind::Iana(tz),
                })
                .map_err(|e| FormatError::BadInput(format!("unknown timezone {name:?}: {e}")))
        }

        /// UTC location (always available; never traps).
        #[must_use]
        pub fn utc() -> Self {
            Self {
                kind: LocationKind::Iana(Tz::UTC),
            }
        }

        /// Creates a fixed offset east of UTC. The accepted range is
        /// strictly less than 24 hours in either direction.
        pub fn fixed(offset_seconds: i32) -> Result<Self, FormatError> {
            let offset = FixedOffset::east_opt(offset_seconds).ok_or_else(|| {
                FormatError::OutOfRange(format!(
                    "fixed UTC offset {offset_seconds} seconds is outside the supported range"
                ))
            })?;
            Ok(Self {
                kind: LocationKind::Fixed(offset),
            })
        }

        /// IANA name of the timezone.
        #[must_use]
        pub fn name(&self) -> String {
            match self.kind {
                LocationKind::Iana(zone) => zone.name().to_string(),
                LocationKind::Fixed(offset) => {
                    let seconds = offset.local_minus_utc();
                    let sign = if seconds < 0 { '-' } else { '+' };
                    let absolute = seconds.unsigned_abs();
                    format!(
                        "UTC{sign}{:02}:{:02}",
                        absolute / 3600,
                        (absolute / 60) % 60
                    )
                }
            }
        }

        /// Civil time fields for `when` rendered through `self`.
        pub fn civil(&self, when: SystemTime) -> Result<CivilTime, FormatError> {
            let utc = utc_datetime(when)?;
            Ok(match self.kind {
                LocationKind::Iana(zone) => civil_fields(utc.with_timezone(&zone)),
                LocationKind::Fixed(offset) => civil_fields(utc.with_timezone(&offset)),
            })
        }

        /// Resolves local calendar fields without guessing during a daylight-saving
        /// transition. A fold returns both valid instants in chronological order.
        pub fn resolve(&self, civil: CivilTime) -> Result<CivilResolution, FormatError> {
            let naive = civil.naive()?;
            let result = match self.kind {
                LocationKind::Iana(zone) => map_local_result(zone.from_local_datetime(&naive)),
                LocationKind::Fixed(offset) => map_local_result(offset.from_local_datetime(&naive)),
            };
            Ok(result)
        }

        /// Resolves calendar fields under an explicit fold/gap policy.
        pub fn to_system_time(
            &self,
            civil: CivilTime,
            policy: ResolvePolicy,
        ) -> Result<SystemTime, CivilTimeError> {
            match self.resolve(civil).map_err(CivilTimeError::Invalid)? {
                CivilResolution::Unique(time) => Ok(time),
                CivilResolution::Gap => Err(CivilTimeError::Gap {
                    civil,
                    location: self.name(),
                }),
                CivilResolution::Fold { earlier, later } => match policy {
                    ResolvePolicy::Reject => Err(CivilTimeError::Fold {
                        civil,
                        location: self.name(),
                        earlier,
                        later,
                    }),
                    ResolvePolicy::Earlier => Ok(earlier),
                    ResolvePolicy::Later => Ok(later),
                },
            }
        }
    }

    /// Civil time fields rendered in a specific [`Location`].
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct CivilTime {
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
        /// 0..=999,999,999 nanoseconds within the second.
        pub nanosecond: u32,
        /// Offset from UTC in seconds (positive east of Greenwich).
        pub offset_seconds: i32,
        /// 0=Mon … 6=Sun.
        pub weekday: u32,
    }

    /// Compatibility name retained for callers of the original Rust-only API.
    pub type Civil = CivilTime;

    impl CivilTime {
        fn naive(self) -> Result<NaiveDateTime, FormatError> {
            NaiveDate::from_ymd_opt(self.year, self.month, self.day)
                .and_then(|date| {
                    date.and_hms_nano_opt(self.hour, self.minute, self.second, self.nanosecond)
                })
                .ok_or_else(|| FormatError::BadInput(format!("invalid civil time {self:?}")))
        }
    }

    /// Complete result of mapping civil fields into an absolute timeline.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum CivilResolution {
        Unique(SystemTime),
        Gap,
        Fold {
            earlier: SystemTime,
            later: SystemTime,
        },
    }

    /// Policy for selecting one side of an ambiguous DST fold.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum ResolvePolicy {
        Reject,
        Earlier,
        Later,
    }

    /// Typed civil-time resolution error.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum CivilTimeError {
        Invalid(FormatError),
        Gap {
            civil: CivilTime,
            location: String,
        },
        Fold {
            civil: CivilTime,
            location: String,
            earlier: SystemTime,
            later: SystemTime,
        },
    }

    impl std::fmt::Display for CivilTimeError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                Self::Invalid(error) => write!(f, "{error}"),
                Self::Gap { civil, location } => {
                    write!(f, "{civil:?} does not exist in {location}")
                }
                Self::Fold {
                    civil, location, ..
                } => write!(f, "{civil:?} occurs twice in {location}"),
            }
        }
    }

    impl std::error::Error for CivilTimeError {}

    fn civil_fields<T: TimeZone>(local: DateTime<T>) -> CivilTime {
        CivilTime {
            year: local.year(),
            month: local.month(),
            day: local.day(),
            hour: local.hour(),
            minute: local.minute(),
            second: local.second(),
            nanosecond: local.nanosecond(),
            offset_seconds: local.offset().fix().local_minus_utc(),
            weekday: local.weekday().num_days_from_monday(),
        }
    }

    fn map_local_result<T: TimeZone>(result: LocalResult<DateTime<T>>) -> CivilResolution {
        match result {
            LocalResult::None => CivilResolution::Gap,
            LocalResult::Single(value) => CivilResolution::Unique(system_time(value)),
            LocalResult::Ambiguous(a, b) => {
                let a = system_time(a);
                let b = system_time(b);
                if a.unix_millis() <= b.unix_millis() {
                    CivilResolution::Fold {
                        earlier: a,
                        later: b,
                    }
                } else {
                    CivilResolution::Fold {
                        earlier: b,
                        later: a,
                    }
                }
            }
        }
    }

    fn system_time<T: TimeZone>(value: DateTime<T>) -> SystemTime {
        let seconds = value.timestamp();
        let nanos = value.timestamp_subsec_nanos();
        let base = if seconds >= 0 {
            std::time::UNIX_EPOCH.checked_add(std::time::Duration::from_secs(seconds as u64))
        } else {
            std::time::UNIX_EPOCH
                .checked_sub(std::time::Duration::from_secs(seconds.unsigned_abs()))
        };
        let instant = base
            .and_then(|time| time.checked_add(std::time::Duration::from_nanos(u64::from(nanos))))
            .unwrap_or_else(|| SystemTime::from_unix_millis(value.timestamp_millis()).as_std());
        SystemTime::from_std(instant)
    }

    fn utc_datetime(when: SystemTime) -> Result<DateTime<Utc>, FormatError> {
        let (seconds, nanos) = match when.as_std().duration_since(std::time::UNIX_EPOCH) {
            Ok(duration) => (
                i64::try_from(duration.as_secs()).map_err(|_| {
                    FormatError::OutOfRange("instant is too far after the Unix epoch".into())
                })?,
                duration.subsec_nanos(),
            ),
            Err(error) => {
                let duration = error.duration();
                let seconds = i64::try_from(duration.as_secs()).map_err(|_| {
                    FormatError::OutOfRange("instant is too far before the Unix epoch".into())
                })?;
                if duration.subsec_nanos() == 0 {
                    (-seconds, 0)
                } else {
                    (
                        seconds
                            .checked_add(1)
                            .and_then(i64::checked_neg)
                            .ok_or_else(|| {
                                FormatError::OutOfRange(
                                    "instant is too far before the Unix epoch".into(),
                                )
                            })?,
                        1_000_000_000 - duration.subsec_nanos(),
                    )
                }
            }
        };
        DateTime::<Utc>::from_timestamp(seconds, nanos).ok_or_else(|| {
            FormatError::OutOfRange(format!(
                "timestamp {seconds}.{nanos:09} is outside the Gregorian range"
            ))
        })
    }

    /// Parses `input` against the supplied `layout` in Go's reference-time
    /// format. The reference time is `2006-01-02 15:04:05 MST` (Mon Jan 2,
    /// 03:04:05 PM 2006). Extra layout tokens are passed through verbatim.
    /// Returns the time normalised to UTC.
    pub fn parse(layout: &str, input: &str) -> Result<SystemTime, FormatError> {
        let chrono_fmt = go_layout_to_chrono(layout);
        // Try with timezone first, fall back to naive.
        if let Ok(dt) = DateTime::parse_from_str(input, &chrono_fmt) {
            return Ok(SystemTime::from_unix_millis(dt.timestamp_millis()));
        }
        match NaiveDateTime::parse_from_str(input, &chrono_fmt) {
            Ok(dt) => {
                let utc = Utc.from_utc_datetime(&dt);
                Ok(SystemTime::from_unix_millis(utc.timestamp_millis()))
            }
            Err(e) => Err(FormatError::BadInput(format!(
                "time::parse({layout:?}, {input:?}): {e}"
            ))),
        }
    }

    /// Renders `when` according to the Go-shaped `layout` in `loc`.
    pub fn format_in(layout: &str, when: SystemTime, loc: Location) -> Result<String, FormatError> {
        let chrono_fmt = go_layout_to_chrono(layout);
        let utc = utc_datetime(when)?;
        Ok(match loc.kind {
            LocationKind::Iana(zone) => utc.with_timezone(&zone).format(&chrono_fmt).to_string(),
            LocationKind::Fixed(offset) => {
                utc.with_timezone(&offset).format(&chrono_fmt).to_string()
            }
        })
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
        let local = loc.civil(when)?;
        // Year/month manually so we clamp to the last day of the
        // target month rather than wrapping into the next.
        let mut new_year = local.year + years;
        let mut new_month_zero = (local.month as i32 - 1) + months;
        new_year += new_month_zero.div_euclid(12);
        new_month_zero = new_month_zero.rem_euclid(12);
        let new_month = (new_month_zero as u32) + 1;
        let dim = days_in_month(new_year, new_month);
        let new_day = local.day.min(dim);
        let date = NaiveDate::from_ymd_opt(new_year, new_month, new_day)
            .ok_or_else(|| FormatError::OutOfRange("calendar addition is out of range".into()))?
            .checked_add_signed(chrono::Duration::days(i64::from(days)))
            .ok_or_else(|| FormatError::OutOfRange("calendar addition is out of range".into()))?;
        let target = CivilTime {
            year: date.year(),
            month: date.month(),
            day: date.day(),
            hour: local.hour,
            minute: local.minute,
            second: local.second,
            nanosecond: local.nanosecond,
            offset_seconds: 0,
            weekday: date.weekday().num_days_from_monday(),
        };
        loc.to_system_time(target, ResolvePolicy::Reject)
            .map_err(|error| {
                FormatError::BadInput(format!(
                    "calendar addition could not resolve local time: {error}"
                ))
            })
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
    /// through verbatim. Not exhaustive - covers RFC3339 / common log
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
            let civil = la.civil(when).unwrap();
            // Pacific Daylight Time (UTC-7).
            assert_eq!(civil.offset_seconds, -7 * 3600);
            assert_eq!(civil.hour, 5);
        }

        fn civil(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> CivilTime {
            CivilTime {
                year,
                month,
                day,
                hour,
                minute,
                second: 0,
                nanosecond: 0,
                offset_seconds: 0,
                weekday: 0,
            }
        }

        #[test]
        fn dst_gap_and_fold_are_never_guessed() {
            for (zone, gap, fold) in [
                (
                    "America/New_York",
                    civil(2026, 3, 8, 2, 30),
                    civil(2026, 11, 1, 1, 30),
                ),
                (
                    "Europe/Berlin",
                    civil(2026, 3, 29, 2, 30),
                    civil(2026, 10, 25, 2, 30),
                ),
            ] {
                let location = Location::lookup(zone).unwrap();
                assert_eq!(location.resolve(gap).unwrap(), CivilResolution::Gap);
                let CivilResolution::Fold { earlier, later } = location.resolve(fold).unwrap()
                else {
                    panic!("expected fold in {zone}");
                };
                assert!(earlier < later);
                assert!(matches!(
                    location.to_system_time(fold, ResolvePolicy::Reject),
                    Err(CivilTimeError::Fold { .. })
                ));
                assert_eq!(
                    location
                        .to_system_time(fold, ResolvePolicy::Earlier)
                        .unwrap(),
                    earlier
                );
                assert_eq!(
                    location.to_system_time(fold, ResolvePolicy::Later).unwrap(),
                    later
                );
            }
        }

        #[test]
        fn fixed_offset_and_subseconds_round_trip() {
            let location = Location::fixed(5 * 3600 + 30 * 60).unwrap();
            let source = CivilTime {
                nanosecond: 123_456_789,
                ..civil(1965, 7, 4, 12, 30)
            };
            let instant = location
                .to_system_time(source, ResolvePolicy::Reject)
                .unwrap();
            assert!(instant.unix_millis() < 0);
            let round_trip = location.civil(instant).unwrap();
            assert_eq!(
                (
                    round_trip.year,
                    round_trip.month,
                    round_trip.day,
                    round_trip.hour,
                    round_trip.minute,
                    round_trip.nanosecond
                ),
                (1965, 7, 4, 12, 30, 123_456_789)
            );
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
    fn signed_unix_millis_preserve_pre_epoch_values() {
        for millis in [-1, -999, -1_001, -123_456_789] {
            assert_eq!(SystemTime::from_unix_millis(millis).unix_millis(), millis);
        }
        assert_eq!(
            SystemTime::from_unix_millis(i64::MIN).unix_millis(),
            i64::MIN
        );
    }

    #[test]
    fn round_trip_known_timestamp() {
        let t = parse_rfc3339("2026-04-25T16:30:45Z").unwrap();
        let formatted = format_rfc3339(t).unwrap();
        assert_eq!(formatted, "2026-04-25T16:30:45Z");
    }

    #[test]
    fn rfc1123_epoch() {
        let formatted = format_rfc1123_gmt(SystemTime(std::time::UNIX_EPOCH)).unwrap();
        // 1970-01-01 is a Thursday.
        assert_eq!(formatted, "Thu, 01 Jan 1970 00:00:00 GMT");
    }

    #[test]
    fn rfc1123_known_timestamp() {
        // 1994-11-06 was a Sunday (canonical example from RFC 7231).
        let t = parse_rfc3339("1994-11-06T08:49:37Z").unwrap();
        let formatted = format_rfc1123_gmt(t).unwrap();
        assert_eq!(formatted, "Sun, 06 Nov 1994 08:49:37 GMT");
    }

    #[test]
    fn rfc1123_weekday_rotation_across_seven_days() {
        // 1970-01-01 = Thu, so 01..=07 covers all 7 weekday names.
        let expected = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
        for (i, exp_dow) in expected.iter().enumerate() {
            let day = i + 1;
            let iso = format!("1970-01-0{day}T00:00:00Z");
            let t = parse_rfc3339(&iso).unwrap();
            let formatted = format_rfc1123_gmt(t).unwrap();
            let actual_dow = &formatted[..3];
            assert_eq!(
                actual_dow, *exp_dow,
                "1970-01-0{day} should be {exp_dow}, got {actual_dow} ({formatted})"
            );
        }
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

    #[test]
    fn ticker_fires_repeatedly_until_stopped() {
        // Structural check, not a rate check: receive N ticks on a
        // channel and verify they all arrive. A slow CI scheduler
        // stretches the test runtime; it does not stretch the
        // assertion. The earlier wall-clock form ("sleep 150ms,
        // expect >=4 ticks") encoded a runner-responsiveness
        // assumption that breaks under macOS CI load.
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let mut t = Ticker::start(Duration::from_millis(5), move || {
            let _ = tx.send(());
        });
        // A genuinely broken ticker that never fires would hang
        // forever without this bound; five seconds per tick is the
        // failure envelope, not a rate expectation.
        let per_tick_budget = std::time::Duration::from_secs(5);
        for i in 0..4 {
            rx.recv_timeout(per_tick_budget)
                .unwrap_or_else(|e| panic!("ticker should fire tick {i}: {e}"));
        }
        t.stop();
    }

    #[test]
    fn ticker_stops_on_drop() {
        let counter = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter_for_tick = std::sync::Arc::clone(&counter);
        {
            let _t = Ticker::start(Duration::from_millis(10), move || {
                counter_for_tick.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            });
            std::thread::sleep(std::time::Duration::from_millis(40));
        }
        // After drop, no more ticks should land.
        let snapshot = counter.load(std::sync::atomic::Ordering::Relaxed);
        std::thread::sleep(std::time::Duration::from_millis(80));
        let post = counter.load(std::sync::atomic::Ordering::Relaxed);
        assert_eq!(snapshot, post, "ticker should stop on drop");
    }

    #[test]
    fn after_func_fires_after_delay() {
        // Condition-based wait: the callback signals a channel and the
        // test blocks on recv with a generous bound. This fails only if
        // the timer genuinely never fires; a slow CI scheduler cannot
        // turn it flaky the way a fixed post-delay sleep + load could.
        let (tx, rx) = std::sync::mpsc::channel();
        let _handle = after_func(Duration::from_millis(20), move || {
            let _ = tx.send(());
        });
        assert!(
            rx.recv_timeout(std::time::Duration::from_secs(5)).is_ok(),
            "timer should fire within the bound"
        );
    }

    #[test]
    fn after_func_cancel_prevents_firing() {
        // Long delay so the cancel deterministically beats the deadline
        // regardless of scheduler jitter. `cancel()` joins the timer
        // thread, so once it returns the thread has exited and `fired`
        // holds its final value - no post-cancel sleep race.
        let fired = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let fired_for_cb = std::sync::Arc::clone(&fired);
        let mut handle = after_func(Duration::from_secs(3600), move || {
            fired_for_cb.store(true, std::sync::atomic::Ordering::Release);
        });
        let before_fire = handle.cancel();
        assert!(before_fire, "cancel should land before the timer fires");
        assert!(
            !fired.load(std::sync::atomic::Ordering::Acquire),
            "cancelled timer must not run its callback"
        );
    }
}
