//! ANSI styling for diagnostic output. Detects whether stderr is
//! a TTY at first use and disables colour for piped/redirected
//! output. Honours `NO_COLOR` (any value) and `CLICOLOR=0`.

use std::io::IsTerminal;
use std::sync::OnceLock;

static ENABLED: OnceLock<bool> = OnceLock::new();

pub(crate) fn terminal_width(fallback: usize, minimum: usize) -> usize {
    std::env::var("COLUMNS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .or_else(|| {
            terminal_size::terminal_size()
                .map(|(terminal_size::Width(width), _)| usize::from(width))
        })
        .unwrap_or(fallback)
        .max(minimum)
}

fn enabled() -> bool {
    *ENABLED.get_or_init(|| {
        if std::env::var_os("NO_COLOR").is_some() {
            return false;
        }
        if matches!(std::env::var("CLICOLOR").as_deref(), Ok("0")) {
            return false;
        }
        std::io::stderr().is_terminal()
    })
}

/// Force-enable colour. The REPL uses this when its readline
/// backend owns the terminal.
pub(crate) fn force_enable() {
    let _ = ENABLED.set(true);
}

const RESET: &str = "\x1b[0m";

fn wrap(prefix: &'static str, s: &str) -> String {
    if enabled() && !s.is_empty() {
        format!("{prefix}{s}{RESET}")
    } else {
        s.to_string()
    }
}

#[must_use]
pub(crate) fn error(s: &str) -> String {
    wrap("\x1b[1;31m", s)
}

#[must_use]
pub(crate) fn heading(s: &str) -> String {
    wrap("\x1b[1;36m", s)
}

#[must_use]
pub(crate) fn accent(s: &str) -> String {
    wrap("\x1b[36m", s)
}

#[must_use]
pub(crate) fn detail(s: &str) -> String {
    wrap("\x1b[2m", s)
}
