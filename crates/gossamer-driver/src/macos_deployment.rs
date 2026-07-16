//! Shared macOS deployment-target policy for compiler subprocesses.

use std::process::Command;

/// Supported deployment target used by release artifacts and CI.
pub const DEFAULT_MACOSX_DEPLOYMENT_TARGET: &str = "15.0";

/// Environment variable understood by Cargo, rustc, clang, and Apple ld.
pub const MACOSX_DEPLOYMENT_TARGET_ENV: &str = "MACOSX_DEPLOYMENT_TARGET";

/// Returns the configured target, falling back to the supported default.
///
/// A non-empty environment value is intentionally preserved so users building
/// the complete toolchain from source can opt into an unsupported older macOS
/// target.
#[must_use]
pub fn effective_deployment_target() -> String {
    resolve_deployment_target(std::env::var(MACOSX_DEPLOYMENT_TARGET_ENV).ok().as_deref())
}

/// Resolves an optional environment value without reading process-global state.
#[must_use]
pub fn resolve_deployment_target(configured: Option<&str>) -> String {
    configured
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(DEFAULT_MACOSX_DEPLOYMENT_TARGET)
        .to_string()
}

/// Reports whether a Cargo target is macOS.
///
/// A missing triple means a native build, so callers pass whether their host
/// is macOS. An explicit triple always wins over the host.
#[must_use]
pub fn is_macos_target(target_triple: Option<&str>, host_is_macos: bool) -> bool {
    target_triple.map_or(host_is_macos, |target| target.ends_with("-apple-darwin"))
}

/// Applies a resolved deployment target to a child process.
pub fn set_command_deployment_target(command: &mut Command, deployment_target: &str) {
    command.env(MACOSX_DEPLOYMENT_TARGET_ENV, deployment_target);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_macos_15() {
        assert_eq!(resolve_deployment_target(None), "15.0");
        assert_eq!(resolve_deployment_target(Some("")), "15.0");
    }

    #[test]
    fn source_build_override_is_preserved() {
        assert_eq!(resolve_deployment_target(Some(" 11.0 ")), "11.0");
    }

    #[test]
    fn explicit_cargo_target_wins_over_host() {
        assert!(is_macos_target(Some("aarch64-apple-darwin"), false));
        assert!(!is_macos_target(Some("aarch64-unknown-linux-gnu"), true));
        assert!(is_macos_target(None, true));
    }

    #[test]
    fn command_inherits_resolved_target() {
        let mut command = Command::new("cargo");
        set_command_deployment_target(&mut command, DEFAULT_MACOSX_DEPLOYMENT_TARGET);
        let configured = command
            .get_envs()
            .find(|(name, _)| *name == MACOSX_DEPLOYMENT_TARGET_ENV)
            .and_then(|(_, value)| value)
            .expect("deployment target environment");
        assert_eq!(configured, "15.0");
    }
}
