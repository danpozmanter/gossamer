//! Shared helpers for the native-build integration suites.
//!
//! Included by each harness via `mod common;`. Not every harness uses
//! every helper, so dead-code (and the `pub` items it makes
//! unreachable in a given includer) is allowed here rather than
//! per-call.
#![allow(dead_code, unreachable_pub)]

use std::process::ExitStatus;

/// Outcome of running a built native binary, with the exit status
/// rendered into a human-readable form that names the crash cause.
pub struct RunExit {
    pub success: bool,
    /// e.g. `exit 0`, `killed by signal 11 (SIGSEGV)`,
    /// `exit code 0xC0000005 (STATUS_ACCESS_VIOLATION)`.
    pub text: String,
}

/// Renders an `ExitStatus` so a crash reads as its cause instead of an
/// opaque number. A native binary miscompiled for a target dies by
/// signal (unix) or with an NTSTATUS exit code (Windows); a bare
/// `exit: Some(-1073741819)` hides that this was an access violation.
pub fn describe_exit(status: ExitStatus) -> RunExit {
    RunExit {
        success: status.success(),
        text: render_exit(status),
    }
}

/// Outcome for a run that never produced an exit status (timed out and
/// was killed, or failed to spawn).
pub fn aborted(reason: &str) -> RunExit {
    RunExit {
        success: false,
        text: format!("no exit status: {reason}"),
    }
}

#[cfg(unix)]
fn render_exit(status: ExitStatus) -> String {
    use std::os::unix::process::ExitStatusExt;
    if let Some(code) = status.code() {
        format!("exit {code}")
    } else if let Some(sig) = status.signal() {
        format!("killed by signal {sig} ({})", signal_name(sig))
    } else {
        format!("{status:?}")
    }
}

#[cfg(windows)]
fn render_exit(status: ExitStatus) -> String {
    match status.code() {
        Some(0) => "exit 0".to_string(),
        Some(code) => match ntstatus_name(code as u32) {
            Some(name) => format!("exit code {:#010x} ({name})", code as u32),
            None => format!("exit {code}"),
        },
        None => format!("{status:?}"),
    }
}

#[cfg(not(any(unix, windows)))]
fn render_exit(status: ExitStatus) -> String {
    format!("{status:?}")
}

/// Names the common fatal signals. `SIGBUS` is 7 on Linux but 10 on
/// Darwin, so it is resolved per-OS; the rest share numbers across
/// the unixes Gossamer targets.
fn signal_name(sig: i32) -> &'static str {
    if cfg!(target_os = "linux") && sig == 7 {
        return "SIGBUS";
    }
    if cfg!(target_os = "macos") && sig == 10 {
        return "SIGBUS";
    }
    match sig {
        4 => "SIGILL",
        5 => "SIGTRAP",
        6 => "SIGABRT",
        8 => "SIGFPE",
        9 => "SIGKILL",
        11 => "SIGSEGV",
        13 => "SIGPIPE",
        15 => "SIGTERM",
        _ => "unknown",
    }
}

/// Names the NTSTATUS exit codes a crashing Windows process reports
/// through its exit code. Pure and platform-independent so it can be
/// unit-tested on any host.
fn ntstatus_name(code: u32) -> Option<&'static str> {
    match code {
        0xC000_0005 => Some("STATUS_ACCESS_VIOLATION"),
        0xC000_001D => Some("STATUS_ILLEGAL_INSTRUCTION"),
        0xC000_0025 => Some("STATUS_NONCONTINUABLE_EXCEPTION"),
        0xC000_008C => Some("STATUS_ARRAY_BOUNDS_EXCEEDED"),
        0xC000_0094 => Some("STATUS_INTEGER_DIVIDE_BY_ZERO"),
        0xC000_00FD => Some("STATUS_STACK_OVERFLOW"),
        0xC000_0374 => Some("STATUS_HEAP_CORRUPTION"),
        0xC000_0409 => Some("STATUS_STACK_BUFFER_OVERRUN"),
        0x8000_0003 => Some("STATUS_BREAKPOINT"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{ntstatus_name, signal_name};

    #[test]
    fn ntstatus_names_access_violation() {
        assert_eq!(
            ntstatus_name(0xC000_0005),
            Some("STATUS_ACCESS_VIOLATION")
        );
    }

    #[test]
    fn ntstatus_names_stack_overflow() {
        assert_eq!(ntstatus_name(0xC000_00FD), Some("STATUS_STACK_OVERFLOW"));
    }

    #[test]
    fn ntstatus_unknown_code_is_none() {
        assert_eq!(ntstatus_name(0), None);
        assert_eq!(ntstatus_name(1), None);
    }

    #[test]
    fn signal_names_segv_and_abort() {
        assert_eq!(signal_name(11), "SIGSEGV");
        assert_eq!(signal_name(6), "SIGABRT");
    }
}
