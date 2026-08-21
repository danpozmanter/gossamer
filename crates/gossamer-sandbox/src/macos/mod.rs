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
        resource_limits: Enforcement::Partial(
            "rlimits and a process-group kill only: macOS has no cgroup equivalent".to_string(),
        ),
        max_level,
        notes,
    }
}

fn os_description() -> String {
    format!("macOS {}", std::env::consts::ARCH)
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
            if policy.network == Network::Deny {
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
) -> Result<SandboxOutput, SandboxError> {
    let mut command = exec::base_command(policy, argv, stdio)?;
    // Kept alive until the child exits: dropping it removes the
    // profile file the child was started with.
    let _applied = if policy.level >= Level::Standard {
        let applier = seatbelt::available().ok_or_else(|| SandboxError::LevelUnavailable {
            requested: policy.level,
            available: Level::Basic,
            reason: "no Seatbelt mechanism is present on this host".to_string(),
        })?;
        let rendered = profile::render(policy);
        Some(
            applier
                .apply(&mut command, &rendered)
                .map_err(SandboxError::Spawn)?,
        )
    } else {
        None
    };

    apply_process_group(&mut command);
    let child = command.spawn().map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            SandboxError::CommandNotFound(argv[0].clone())
        } else {
            SandboxError::Spawn(format!("{error}"))
        }
    })?;
    exec::wait_for(policy, child, stdio)
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
