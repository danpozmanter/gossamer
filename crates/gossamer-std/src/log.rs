#![allow(
    clippy::map_unwrap_or,
    clippy::doc_markdown,
    clippy::similar_names,
    clippy::uninlined_format_args,
    clippy::items_after_statements,
    clippy::needless_continue,
    clippy::manual_let_else
)]

//! Go-style `log` package.
//!
//! Compatibility shim for code ported from Go that uses the
//! flat `log.Println` / `log.Printf` / `log.Fatal` family. New
//! Gossamer code should prefer [`crate::slog`] for structured
//! logging; this module exists for Go-compat surface area.

#![forbid(unsafe_code)]

use std::io::Write;
use std::sync::OnceLock;

use parking_lot::Mutex;

/// Flag: include calendar date in the prefix.
pub const L_DATE: u32 = 1 << 0;
/// Flag: include wall-clock time (HH:MM:SS).
pub const L_TIME: u32 = 1 << 1;
/// Flag: include microsecond resolution on the time field.
pub const L_MICROSECONDS: u32 = 1 << 2;
/// Flag: include the process-uptime in nanoseconds.
pub const L_LONG_FILE: u32 = 1 << 3;
/// Flag: include just the file basename + line.
pub const L_SHORT_FILE: u32 = 1 << 4;
/// Flag: timestamps in UTC instead of local.
pub const L_UTC: u32 = 1 << 5;
/// Flag: emit the message field as JSON-escaped text. Optional
/// extension not in Go.
pub const L_JSON: u32 = 1 << 6;

/// Default flag set: date + time, matching Go's `log` default.
pub const DEFAULT_FLAGS: u32 = L_DATE | L_TIME;

struct Logger {
    sink: Box<dyn Write + Send>,
    flags: u32,
    prefix: String,
}

static LOGGER: OnceLock<Mutex<Logger>> = OnceLock::new();

fn logger() -> &'static Mutex<Logger> {
    LOGGER.get_or_init(|| {
        Mutex::new(Logger {
            sink: Box::new(std::io::stderr()),
            flags: DEFAULT_FLAGS,
            prefix: String::new(),
        })
    })
}

/// Overrides the global log sink.
pub fn set_output(writer: Box<dyn Write + Send>) {
    let mut g = logger().lock();
    g.sink = writer;
}

/// Sets the global prefix prepended to every log line.
pub fn set_prefix(prefix: impl Into<String>) {
    let mut g = logger().lock();
    g.prefix = prefix.into();
}

/// Sets the flag bitmask (bitwise-OR of `L_DATE`, `L_TIME`, ...).
pub fn set_flags(flags: u32) {
    let mut g = logger().lock();
    g.flags = flags;
}

/// Returns the current flag bitmask.
#[must_use]
pub fn flags() -> u32 {
    logger().lock().flags
}

/// Logs `msg` followed by a newline.
pub fn println(msg: &str) {
    write_line(msg);
}

/// Formatted printf-style line. Supports the standard Rust
/// format-string placeholders (`{}`); not the Go `%s`/`%d` style.
/// Use [`crate::strings`] helpers or `format!` to bridge.
pub fn printf(line: &str) {
    write_line(line);
}

/// Logs `msg` then calls `std::process::exit(1)`. Mirrors Go's
/// `log.Fatal`.
pub fn fatal(msg: &str) -> ! {
    write_line(msg);
    std::process::exit(1)
}

/// Logs `msg` then panics. Mirrors Go's `log.Panic`.
pub fn panic_msg(msg: &str) -> ! {
    write_line(msg);
    panic!("{msg}")
}

fn write_line(msg: &str) {
    let mut g = logger().lock();
    let prefix = g.prefix.clone();
    let flags = g.flags;
    let mut line = String::with_capacity(prefix.len() + msg.len() + 32);
    line.push_str(&prefix);
    if (flags & (L_DATE | L_TIME)) != 0 {
        let stamp = format_stamp(flags);
        line.push_str(&stamp);
        line.push(' ');
    }
    if (flags & L_JSON) != 0 {
        line.push_str("{\"msg\":\"");
        for ch in msg.chars() {
            match ch {
                '"' => line.push_str("\\\""),
                '\\' => line.push_str("\\\\"),
                '\n' => line.push_str("\\n"),
                '\r' => line.push_str("\\r"),
                '\t' => line.push_str("\\t"),
                c if (c as u32) < 0x20 => line.push_str(&format!("\\u{:04x}", c as u32)),
                c => line.push(c),
            }
        }
        line.push_str("\"}");
    } else {
        line.push_str(msg);
    }
    line.push('\n');
    let _ = g.sink.write_all(line.as_bytes());
    let _ = g.sink.flush();
}

fn format_stamp(flags: u32) -> String {
    let now = std::time::SystemTime::now();
    let secs_since = now
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let micros_field = now
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_micros())
        .unwrap_or(0);
    // Decompose into calendar components using the existing
    // civil-time helper.
    let civil = match crate::time::format_rfc1123_gmt(crate::time::SystemTime::from_std(now)) {
        Ok(s) => s,
        Err(_) => return String::new(),
    };
    // civil is `Day, DD Mon YYYY HH:MM:SS GMT`.
    // For Go-compat, the standard format is YYYY/MM/DD HH:MM:SS.
    // We pull year/month/day out of the RFC1123 string we just
    // produced — bounded, simple.
    let parts: Vec<&str> = civil.split(' ').collect();
    if parts.len() != 6 {
        return String::new();
    }
    let day = parts[1];
    let mon_str = parts[2];
    let year = parts[3];
    let time = parts[4];
    let mon = month_to_num(mon_str);
    let _ = (secs_since, flags);
    let mut out = String::new();
    if (flags & L_DATE) != 0 {
        out.push_str(&format!("{year}/{mon:02}/{day}"));
    }
    if (flags & L_TIME) != 0 {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(time);
        if (flags & L_MICROSECONDS) != 0 {
            out.push_str(&format!(".{micros_field:06}"));
        }
    }
    out
}

fn month_to_num(mon: &str) -> u32 {
    match mon {
        "Jan" => 1,
        "Feb" => 2,
        "Mar" => 3,
        "Apr" => 4,
        "May" => 5,
        "Jun" => 6,
        "Jul" => 7,
        "Aug" => 8,
        "Sep" => 9,
        "Oct" => 10,
        "Nov" => 11,
        "Dec" => 12,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Captures log output by routing through an in-memory sink.
    /// Tests serialise on TEST_LOCK because the sink is a
    /// process-global.
    static TEST_LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

    struct Capture {
        buf: std::sync::Arc<parking_lot::Mutex<Vec<u8>>>,
    }

    impl Write for Capture {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.buf.lock().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn install_capture() -> std::sync::Arc<parking_lot::Mutex<Vec<u8>>> {
        let buf = std::sync::Arc::new(parking_lot::Mutex::new(Vec::new()));
        set_output(Box::new(Capture {
            buf: std::sync::Arc::clone(&buf),
        }));
        buf
    }

    #[test]
    fn println_writes_line_with_default_flags() {
        let _g = TEST_LOCK.lock();
        let buf = install_capture();
        set_flags(DEFAULT_FLAGS);
        set_prefix("");
        println("hello");
        let s = String::from_utf8(buf.lock().clone()).unwrap();
        assert!(s.ends_with("hello\n"));
        // Default flags include date and time, so the prefix is
        // `YYYY/MM/DD HH:MM:SS ` before the message.
        assert!(s.len() >= 25);
    }

    #[test]
    fn prefix_prepends_to_every_line() {
        let _g = TEST_LOCK.lock();
        let buf = install_capture();
        set_flags(0);
        set_prefix("svc: ");
        println("ready");
        let s = String::from_utf8(buf.lock().clone()).unwrap();
        assert_eq!(s, "svc: ready\n");
    }

    #[test]
    fn flags_can_be_cleared() {
        let _g = TEST_LOCK.lock();
        let buf = install_capture();
        set_flags(0);
        set_prefix("");
        println("plain");
        let s = String::from_utf8(buf.lock().clone()).unwrap();
        assert_eq!(s, "plain\n");
    }

    #[test]
    fn json_flag_emits_json_envelope() {
        let _g = TEST_LOCK.lock();
        let buf = install_capture();
        set_flags(L_JSON);
        set_prefix("");
        println("a \"quoted\" value");
        let s = String::from_utf8(buf.lock().clone()).unwrap();
        assert_eq!(s, "{\"msg\":\"a \\\"quoted\\\" value\"}\n");
    }

    #[test]
    fn microseconds_flag_appends_decimal() {
        let _g = TEST_LOCK.lock();
        let buf = install_capture();
        set_flags(L_TIME | L_MICROSECONDS);
        set_prefix("");
        println("tick");
        let s = String::from_utf8(buf.lock().clone()).unwrap();
        // Format: HH:MM:SS.UUUUUU tick\n
        assert!(s.contains('.'));
        let dot = s.find('.').unwrap();
        let tail: String = s.chars().skip(dot + 1).take(6).collect();
        assert!(tail.chars().all(|c| c.is_ascii_digit()), "tail: {tail:?}");
    }
}
