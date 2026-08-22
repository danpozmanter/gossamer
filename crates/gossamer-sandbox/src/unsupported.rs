//! Fallback for a target with no backend.
//!
//! Reports `Enforcement::None` and refuses every level above `none`,
//! rather than failing to compile: a crate that cannot be built for
//! `wasm32` or `riscv64gc` breaks the cross-target check jobs for
//! everyone, and a sandbox that silently pretends to enforce is worse
//! than one that says it cannot.

use crate::error::{SandboxError, SandboxOutput};
use crate::exec::Stdio;
use crate::level::{Level, Platform, SandboxCapabilities};
use crate::policy::CompiledPolicy;

/// What a host with no backend can honor: nothing.
#[must_use]
pub(crate) fn capabilities() -> SandboxCapabilities {
    SandboxCapabilities::unsupported(
        Platform::Unsupported,
        format!("{} {}", std::env::consts::OS, std::env::consts::ARCH),
        "no sandbox backend exists for this target",
    )
}

/// Names the mechanisms a run would install: none.
#[must_use]
/// How completely `policy`'s network setting is enforced here: not at
/// all, because there is no backend.
#[must_use]
pub(crate) fn network_enforcement(_policy: &CompiledPolicy) -> crate::level::Enforcement {
    crate::level::Enforcement::None
}

pub(crate) fn mechanisms(_policy: &CompiledPolicy) -> Vec<String> {
    vec!["no enforcement: this target has no sandbox backend".to_string()]
}

/// Runs `argv` with no enforcement, which is only reachable at
/// [`Level::None`] because every higher level fails closed first.
pub(crate) fn run(
    policy: &CompiledPolicy,
    argv: &[String],
    stdio: Stdio,
) -> Result<SandboxOutput, SandboxError> {
    if policy.level != Level::None {
        return Err(SandboxError::LevelUnavailable {
            requested: policy.level,
            available: Level::None,
            reason: "no sandbox backend exists for this target".to_string(),
        });
    }
    let mut command = crate::exec::base_command(policy, argv, stdio)?;
    let child = command.spawn().map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            SandboxError::CommandNotFound(argv[0].clone())
        } else {
            SandboxError::Spawn(format!("{error}"))
        }
    })?;
    crate::exec::wait_for(policy, child, stdio)
}

/// Whether this host can enforce every limit in `resources`: only a
/// timeout, which is the supervisor's own clock.
#[must_use]
pub(crate) fn resource_enforcement(
    resources: &crate::policy::Resources,
    _level: crate::level::Level,
) -> crate::level::Enforcement {
    let only_timeout = crate::policy::Resources {
        timeout: resources.timeout,
        ..crate::policy::Resources::default()
    };
    if resources == &only_timeout {
        crate::level::Enforcement::Full
    } else {
        crate::level::Enforcement::None
    }
}
