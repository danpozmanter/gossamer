//! Failure taxonomy and the exit-code contract.
//!
//! A wrapper's exit code is ambiguous by default: a child that exits 1
//! and a sandbox that could not start both look like failure. Each
//! failure class gets its own variant and its own code, so a caller can
//! tell a policy mistake from a program that simply failed.

use std::time::Duration;

use crate::level::Level;

/// Exit code for a policy that could not be compiled, a sandbox that
/// could not start, or a run whose tree was killed for exceeding its
/// timeout. Each is a failure of the sandbox rather than of the child,
/// so none of them impersonates a child's own code.
pub const EXIT_POLICY_ERROR: i32 = 126;
/// Exit code for a command that was not found inside the sandbox.
pub const EXIT_COMMAND_NOT_FOUND: i32 = 127;
/// Exit code for a level the host cannot honor.
pub const EXIT_LEVEL_UNAVAILABLE: i32 = 64;
/// Base added to a signal number when a child dies on a signal.
pub const EXIT_SIGNAL_BASE: i32 = 128;

/// Everything that can go wrong between a policy and a finished child.
#[derive(Debug, thiserror::Error)]
pub enum SandboxError {
    /// The policy itself is not usable: a path that does not resolve, a
    /// rule that contradicts another, a grant that would lift a denial.
    #[error("sandbox policy error: {0}")]
    Policy(String),

    /// The requested level is higher than this host can honor. Never
    /// silently downgraded; the blocking primitive is named so the
    /// operator can decide what to do about it.
    #[error(
        "{requested} sandbox unavailable on this host: {reason}\n  \
         the highest level this host can honor is: {available}"
    )]
    LevelUnavailable {
        /// Level the caller asked for.
        requested: Level,
        /// Highest level the host can honor.
        available: Level,
        /// The primitive, sysctl, or API that blocks the request.
        reason: String,
    },

    /// The sandbox could not be applied or the child could not start.
    #[error("sandbox could not start the child: {0}")]
    Spawn(String),

    /// The command does not exist, or exists but is not reachable
    /// under the compiled policy.
    #[error("command not found inside the sandbox: {0}")]
    CommandNotFound(String),

    /// The child died on a signal rather than exiting.
    #[error("child terminated by signal {signal}{}", said(.stderr))]
    Signalled {
        /// The signal that ended the child.
        signal: i32,
        /// What is known about why the child died: what it printed when
        /// the streams were captured, plus whatever record the host
        /// kept of stopping it. A bare signal number is not a reason,
        /// and a child stopped in the loader never gets to give one.
        stderr: String,
    },

    /// The child outlived the policy's timeout and its tree was killed.
    #[error("child exceeded the {} ms timeout and its process tree was killed", .0.as_millis())]
    Timeout(Duration),
}

impl SandboxError {
    /// The process exit code this failure maps to.
    ///
    /// Encoded once here and used by every consumer, so
    /// every consumer reports the same failure the same way.
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        match self {
            Self::Policy(_) => EXIT_POLICY_ERROR,
            Self::LevelUnavailable { .. } => EXIT_LEVEL_UNAVAILABLE,
            Self::CommandNotFound(_) => EXIT_COMMAND_NOT_FOUND,
            // A sandbox that could not start is a policy failure from
            // the caller's side: nothing ran, so the child's own codes
            // are not in play.
            Self::Spawn(_) | Self::Timeout(_) => EXIT_POLICY_ERROR,
            Self::Signalled { signal, .. } => EXIT_SIGNAL_BASE + *signal,
        }
    }
}

/// The child's last words, as a clause to append to a signal report.
fn said(stderr: &str) -> String {
    let text = stderr.trim();
    if text.is_empty() {
        String::new()
    } else {
        format!(": {text}")
    }
}

/// What a finished child left behind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxOutput {
    /// The child's own exit code.
    pub code: i32,
    /// Captured standard output, empty when the streams were
    /// inherited.
    pub stdout: Vec<u8>,
    /// Captured standard error, empty when the streams were
    /// inherited.
    pub stderr: Vec<u8>,
}

impl SandboxOutput {
    /// Standard output as text, with invalid UTF-8 replaced.
    #[must_use]
    pub fn stdout_text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }

    /// Standard error as text, with invalid UTF-8 replaced.
    #[must_use]
    pub fn stderr_text(&self) -> String {
        String::from_utf8_lossy(&self.stderr).into_owned()
    }
}

/// The exit code a consumer should exit with, given a run's outcome.
///
/// A child's own code passes through unchanged in `0..=125`; anything
/// above that range is a code the contract reserves, so it is reported
/// as a policy error rather than impersonating a sandbox failure.
#[must_use]
pub fn exit_code_for(outcome: &Result<SandboxOutput, SandboxError>) -> i32 {
    match outcome {
        Ok(output) if (0..=125).contains(&output.code) => output.code,
        Ok(_) => EXIT_POLICY_ERROR,
        Err(error) => error.exit_code(),
    }
}

#[cfg(test)]
mod error_tests {
    use super::*;

    #[test]
    fn each_failure_class_has_its_own_exit_code() {
        assert_eq!(
            SandboxError::Policy("bad".into()).exit_code(),
            EXIT_POLICY_ERROR
        );
        assert_eq!(
            SandboxError::CommandNotFound("nope".into()).exit_code(),
            EXIT_COMMAND_NOT_FOUND
        );
        assert_eq!(
            SandboxError::LevelUnavailable {
                requested: Level::Strict,
                available: Level::Standard,
                reason: "no user namespaces".into(),
            }
            .exit_code(),
            EXIT_LEVEL_UNAVAILABLE
        );
        assert_eq!(
            SandboxError::Signalled {
                signal: 9,
                stderr: String::new(),
            }
            .exit_code(),
            137
        );
    }

    #[test]
    fn a_signalled_child_reports_what_it_printed_before_it_died() {
        let error = SandboxError::Signalled {
            signal: 6,
            stderr: "dyld: could not map the shared cache\n".to_string(),
        };
        let text = error.to_string();
        assert!(text.contains("signal 6"), "{text}");
        assert!(text.contains("could not map the shared cache"), "{text}");
        let bare = SandboxError::Signalled {
            signal: 9,
            stderr: String::new(),
        };
        assert_eq!(bare.to_string(), "child terminated by signal 9");
    }

    #[test]
    fn a_childs_own_code_passes_through_but_a_reserved_one_does_not() {
        let ok = |code| {
            Ok(SandboxOutput {
                code,
                stdout: Vec::new(),
                stderr: Vec::new(),
            })
        };
        assert_eq!(exit_code_for(&ok(0)), 0);
        assert_eq!(exit_code_for(&ok(125)), 125);
        assert_eq!(exit_code_for(&ok(126)), EXIT_POLICY_ERROR);
    }
}
