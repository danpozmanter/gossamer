//! macOS enforcement: a generated Seatbelt profile.
//!
//! What Seatbelt buys: a deny-by-default filesystem policy on resolved
//! paths, complete network denial across every protocol, process-exec
//! control, an explicit mach-service allowlist, and inheritance by
//! every descendant.
//!
//! What it does not buy: PID isolation, UID isolation, a private mount
//! view, or resource limits. There is no macOS equivalent of a
//! process namespace, so `strict` is reported unavailable rather than
//! offered as something weaker under the same name. That is the single
//! most important honesty property of the level model.

pub(crate) mod seatbelt;

use crate::macos_profile as profile;

use crate::error::{SandboxError, SandboxOutput};
use crate::exec::{self, Stdio};
use crate::level::{Enforcement, Level, Platform, SandboxCapabilities};
use crate::policy::{CompiledPolicy, Network};

/// What this macOS host can honor.
#[must_use]
pub(crate) fn capabilities() -> SandboxCapabilities {
    let applier = seatbelt::available();
    let mut notes = vec![
        "strict is unavailable on macOS by design: the platform has no \
         process-namespace equivalent"
            .to_string(),
    ];
    let (filesystem, network, max_level) = if let Some(applier) = &applier {
        notes.push(format!(
            "Seatbelt applied through {} (the underlying SPI is deprecated)",
            applier.name()
        ));
        notes.push(
            "Seatbelt matches on paths, so every rule is generated against the resolved path"
                .to_string(),
        );
        (Enforcement::Full, Enforcement::Full, Level::Standard)
    } else {
        notes.push("no Seatbelt mechanism is present on this host".to_string());
        (Enforcement::None, Enforcement::None, Level::Basic)
    };

    SandboxCapabilities {
        platform: Platform::MacOs,
        os_description: os_description(),
        filesystem,
        network,
        // Seatbelt has no process-table isolation at all.
        process_isolation: Enforcement::None,
        max_level,
        notes,
    }
}

fn os_description() -> String {
    format!("macOS {}", std::env::consts::ARCH)
}

/// How completely `policy`'s network setting is enforced here.
///
/// Seatbelt's `(deny network*)` covers every protocol, so at any level
/// that installs a profile the policy's setting is what holds.
#[must_use]
pub(crate) fn network_enforcement(policy: &CompiledPolicy) -> Enforcement {
    if policy.level >= crate::level::Level::Standard {
        Enforcement::Full
    } else {
        Enforcement::None
    }
}

/// Names the mechanisms a run at this policy's level will install.
#[must_use]
pub(crate) fn mechanisms(policy: &CompiledPolicy) -> Vec<String> {
    let mut lines = Vec::new();
    match policy.level {
        Level::None => lines.push("no enforcement".to_string()),
        Level::Basic => {
            lines.push("environment allowlist, private temp, descriptor hygiene".to_string());
        }
        Level::Standard | Level::Strict => {
            let applied = seatbelt::available().map_or("unavailable".to_string(), |applier| {
                applier.name().to_string()
            });
            lines.push(format!("Seatbelt profile via {applied}"));
            if policy.network == Network::None {
                lines.push("(deny network*) - every protocol".to_string());
            }
            lines.push("mach-lookup allowlist".to_string());
        }
    }
    lines
}

/// Runs `argv` under `policy`.
pub(crate) fn run(
    policy: &CompiledPolicy,
    argv: &[String],
    stdio: Stdio,
    bound: Option<std::time::Duration>,
) -> Result<SandboxOutput, SandboxError> {
    // The wrapper is decided before the command is built, so the
    // policy's environment, working directory, and stdio are installed
    // on the program that actually runs.
    //
    // `applied` is kept alive until the child exits: dropping it removes
    // the profile file the child was started with.
    let (launched, _applied) = if policy.level >= Level::Standard {
        let applier = seatbelt::available().ok_or_else(|| SandboxError::LevelUnavailable {
            requested: policy.level,
            available: Level::Basic,
            reason: "no Seatbelt mechanism is present on this host".to_string(),
        })?;
        let rendered = profile::render(policy);
        let (wrapped, state) = applier
            .apply(argv, &rendered)
            .map_err(SandboxError::Spawn)?;
        (wrapped, Some(state))
    } else {
        (argv.to_vec(), None)
    };

    let mut command = exec::base_command(policy, &launched, stdio)?;
    apply_process_group(&mut command);
    let child = command.spawn().map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            SandboxError::CommandNotFound(argv[0].clone())
        } else {
            SandboxError::Spawn(format!("{error}"))
        }
    })?;
    let outcome = exec::wait_for(policy, child, stdio, bound);
    match outcome {
        Err(SandboxError::Signalled { signal, stderr }) if policy.level >= Level::Standard => {
            Err(SandboxError::Signalled {
                signal,
                stderr: with_denials(stderr),
            })
        }
        other => other,
    }
}

/// Adds what Seatbelt recorded to a signalled child's own output.
///
/// A denial the loader hits ends the process through `abort` before it
/// runs a line of its own, so it prints nothing and the reason lives
/// only in the system log. Asking for it turns a bare signal number
/// into the operation and path that were refused.
fn with_denials(stderr: String) -> String {
    let Some(denials) = recorded_denials() else {
        return stderr;
    };
    if stderr.trim().is_empty() {
        return format!("the sandbox refused:\n{denials}");
    }
    format!("{stderr}\nthe sandbox refused:\n{denials}")
}

/// The most recent Seatbelt denials this host logged.
fn recorded_denials() -> Option<String> {
    let output = std::process::Command::new("/usr/bin/log")
        .args([
            "show",
            "--last",
            "30s",
            "--style",
            "compact",
            "--predicate",
            "sender == \"Sandbox\"",
        ])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let mut lines: Vec<&str> = text
        .lines()
        .filter(|line| line.contains("deny"))
        .rev()
        .take(8)
        .collect();
    if lines.is_empty() {
        return None;
    }
    lines.reverse();
    Some(lines.join("\n"))
}

/// Puts the child in its own session so the whole tree can be reached
/// with one signal.
fn apply_process_group(command: &mut std::process::Command) {
    use std::os::unix::process::CommandExt;
    // SAFETY: the closure runs between `fork` and `exec` and calls
    // only `setsid`, which is async-signal-safe.
    #[allow(unsafe_code, reason = "documented above")]
    unsafe {
        command.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
}

#[cfg(test)]
mod macos_tests {
    use super::*;

    #[test]
    fn strict_is_never_offered_on_macos() {
        assert!(capabilities().max_level < Level::Strict);
    }

    #[test]
    fn the_report_says_why_strict_is_unavailable() {
        let report = capabilities();
        assert!(
            report
                .notes
                .iter()
                .any(|note| note.contains("no") && note.contains("process-namespace")),
            "{:?}",
            report.notes
        );
    }
}
