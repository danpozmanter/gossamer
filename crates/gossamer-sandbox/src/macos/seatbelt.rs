//! Applying a Seatbelt profile to a child.
//!
//! macOS offers no supported public API for sandboxing an arbitrary
//! child process. `sandbox_init` is deprecated private SPI,
//! `/usr/bin/sandbox-exec` is a deprecated CLI over the same SPI, and
//! the App Sandbox needs entitlements plus code signing and cannot
//! apply to an arbitrary child. There is no fourth option, and that is
//! why macOS tops out below `strict`.
//!
//! Both implementations sit behind [`Applier`] so the policy engine
//! never depends on which is in use and the deprecation of either is a
//! contained change.

use std::path::PathBuf;

/// How a profile reaches the child.
pub(crate) trait Applier {
    /// Whether this mechanism is present on the running host.
    fn is_available(&self) -> bool;

    /// A short name for the capability report and `--explain`.
    fn name(&self) -> &'static str;

    /// The command line that runs `argv` under `profile`.
    ///
    /// A command line rather than a configured `Command`: the policy's
    /// environment, working directory, and stdio are installed once, on
    /// whatever program ends up being launched, so nothing a wrapper
    /// adds can drop them.
    fn apply(&self, argv: &[String], profile: &str) -> Result<(Vec<String>, Applied), String>;
}

/// State the caller has to keep alive until the child exits, such as
/// the temporary file a profile was written to.
pub(crate) struct Applied {
    /// Profile file to remove once the child is gone.
    pub(crate) profile_file: Option<PathBuf>,
}

impl Drop for Applied {
    fn drop(&mut self) {
        if let Some(path) = self.profile_file.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// The `sandbox-exec` CLI: writes the profile to a file and runs the
/// command through it.
pub(crate) struct SandboxExec;

/// Where the CLI lives when it is present.
const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

impl Applier for SandboxExec {
    fn is_available(&self) -> bool {
        std::path::Path::new(SANDBOX_EXEC).exists()
    }

    fn name(&self) -> &'static str {
        "sandbox-exec"
    }

    fn apply(&self, argv: &[String], profile: &str) -> Result<(Vec<String>, Applied), String> {
        let path = profile_path();
        std::fs::write(&path, profile)
            .map_err(|error| format!("writing the sandbox profile failed: {error}"))?;
        let mut wrapped = vec![
            SANDBOX_EXEC.to_string(),
            "-f".to_string(),
            path.to_string_lossy().into_owned(),
        ];
        wrapped.extend(argv.iter().cloned());
        Ok((
            wrapped,
            Applied {
                profile_file: Some(path),
            },
        ))
    }
}

/// A profile file name unique to this process and this call, so two
/// concurrent runs never share one.
fn profile_path() -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let serial = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "gossamer-sandbox-{}-{serial}.sb",
        std::process::id()
    ))
}

/// The applier this host has, or `None` when it has none.
#[must_use]
pub(crate) fn available() -> Option<Box<dyn Applier>> {
    let candidate = SandboxExec;
    candidate
        .is_available()
        .then(|| Box::new(candidate) as Box<dyn Applier>)
}

#[cfg(test)]
mod seatbelt_tests {
    use super::*;

    #[test]
    fn two_profile_paths_never_collide() {
        assert_ne!(profile_path(), profile_path());
    }

    #[test]
    fn availability_is_probed_not_assumed() {
        assert_eq!(
            SandboxExec.is_available(),
            std::path::Path::new(SANDBOX_EXEC).exists()
        );
    }
}
