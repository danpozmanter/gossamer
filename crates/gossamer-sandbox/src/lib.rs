//! A cross-platform process sandbox with one policy model.
//!
//! A [`SandboxPolicy`] says what a command may reach; a backend
//! compiles it into whatever the host enforces with - Landlock,
//! namespaces and seccomp on Linux, a Seatbelt profile on macOS, a
//! restricted token and an `AppContainer` on Windows. A level name means
//! the same guarantee everywhere: a host that cannot meet one reports
//! it unavailable rather than offering a weaker thing under the same
//! name.
//!
//! ```no_run
//! use gossamer_sandbox::{Level, Network, Sandbox, SandboxPolicy};
//!
//! let policy = SandboxPolicy::new()
//!     .read_write(".")
//!     .network(Network::None)
//!     .env_allow(["PATH", "HOME"])
//!     .level(Level::Standard);
//! let sandbox = Sandbox::new(&policy)?;
//! let output = sandbox.run(&["cargo".to_string(), "build".to_string()])?;
//! println!("{}", output.code);
//! # Ok::<(), gossamer_sandbox::SandboxError>(())
//! ```
//!
//! This crate depends on no other Gossamer crate, and is publishable on
//! its own: a sandbox exists to contain a build system, so it must not
//! need one in order to build.

pub mod discover;
pub mod error;
pub mod exec;
pub mod level;
pub mod policy;
pub mod presets;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
// The Seatbelt profile generator is a pure function from a compiled
// policy to a profile string, so it is compiled and tested on every
// host: the part of the macOS backend most likely to be wrong should
// not need a Mac to check.
#[path = "macos/profile.rs"]
mod macos_profile;
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
mod unsupported;
#[cfg(windows)]
pub mod windows;

pub use error::{
    EXIT_COMMAND_NOT_FOUND, EXIT_LEVEL_UNAVAILABLE, EXIT_POLICY_ERROR, EXIT_SIGNAL_BASE,
    SandboxError, SandboxOutput, exit_code_for,
};
pub use exec::Stdio;
pub use level::{Enforcement, Level, Platform, SandboxCapabilities};
pub use policy::{
    Access, CompiledPolicy, NEVER_PASSED_ENVIRONMENT, Network, PathRule, SandboxPolicy, Temp,
    is_never_passed, never_granted_paths,
};
pub use presets::{
    base_environment, build_environment, credential_paths, device_paths, home_directory,
    resolver_read_paths, system_read_paths,
};

#[cfg(target_os = "linux")]
use linux as backend;
#[cfg(target_os = "macos")]
use macos as backend;
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
use unsupported as backend;
#[cfg(windows)]
use windows as backend;

/// What this host can honor.
///
/// Probes rather than assumes: the answer describes the machine the
/// call was made on, including which primitives are missing and what
/// blocks them.
#[must_use]
pub fn capabilities() -> SandboxCapabilities {
    // Cached because the probe forks to observe whether a user
    // namespace is permitted, and a banner, a limit check, and an
    // `--explain` all ask within one run. What it describes - the
    // kernel and this process's delegation - does not change under a
    // running program.
    static REPORT: std::sync::OnceLock<SandboxCapabilities> = std::sync::OnceLock::new();
    REPORT.get_or_init(backend::capabilities).clone()
}

/// A compiled policy, checked against what this host can honor.
///
/// Constructing one is the only place a level is compared against
/// [`SandboxCapabilities::max_level`], so no backend decides on its own
/// whether to downgrade.
#[derive(Debug)]
pub struct Sandbox {
    policy: CompiledPolicy,
    /// A private temporary directory this sandbox created, removed
    /// when the sandbox is dropped. `None` when the policy inherits
    /// the caller's temp directory or names one of its own.
    private_temp: Option<std::path::PathBuf>,
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        if let Some(directory) = self.private_temp.take() {
            let _ = std::fs::remove_dir_all(directory);
        }
    }
}

impl Sandbox {
    /// Compiles `policy` and refuses it when this host cannot honor the
    /// level it asks for.
    ///
    /// Fails closed: a level the host cannot meet is an error naming
    /// the blocking primitive, never a silent downgrade.
    pub fn new(policy: &SandboxPolicy) -> Result<Self, SandboxError> {
        let host = capabilities();
        if policy.level > host.max_level {
            return Err(SandboxError::LevelUnavailable {
                requested: policy.level,
                available: host.max_level,
                reason: blocking_reason(&host),
            });
        }
        let (policy, private_temp) = materialize_temp(policy)?;
        Ok(Self {
            policy: policy.compile()?,
            private_temp,
        })
    }

    /// The compiled policy, for `--explain` and for tests.
    #[must_use]
    pub fn policy(&self) -> &CompiledPolicy {
        &self.policy
    }

    /// The enforcement mechanisms a run will install, in the order they
    /// are applied.
    #[must_use]
    pub fn mechanisms(&self) -> Vec<String> {
        backend::mechanisms(&self.policy)
    }

    /// How completely this run's network setting is enforced.
    ///
    /// Separate from [`CompiledPolicy::network`], which is what was
    /// asked for. A consumer that reports the request as the guarantee
    /// tells the operator a denial is in force that the kernel never
    /// installed, so the two are distinct calls on purpose.
    #[must_use]
    pub fn network_enforcement(&self) -> Enforcement {
        backend::network_enforcement(&self.policy)
    }

    /// Runs `argv` with the caller's streams inherited.
    pub fn run(&self, argv: &[String]) -> Result<SandboxOutput, SandboxError> {
        backend::run(&self.policy, argv, Stdio::Inherit, None)
    }

    /// Runs `argv` and captures its output.
    pub fn run_captured(&self, argv: &[String]) -> Result<SandboxOutput, SandboxError> {
        backend::run(&self.policy, argv, Stdio::Capture, None)
    }

    /// Runs `argv` with the streams `stdio` chooses.
    pub fn run_with(&self, argv: &[String], stdio: Stdio) -> Result<SandboxOutput, SandboxError> {
        backend::run(&self.policy, argv, stdio, None)
    }

    /// Runs `argv` and kills its tree if it outlives `bound`.
    ///
    /// The bound is the caller's own clock rather than part of the
    /// policy: a supervisor that owes its own caller an answer within a
    /// deadline needs one, and a policy says what a command may reach,
    /// never how long it may take.
    pub fn run_bounded(
        &self,
        argv: &[String],
        stdio: Stdio,
        bound: std::time::Duration,
    ) -> Result<SandboxOutput, SandboxError> {
        backend::run(&self.policy, argv, stdio, Some(bound))
    }
}

/// Turns the policy's temp choice into a real directory the child can
/// write to, and points `TMPDIR` / `TEMP` / `TMP` at it.
///
/// A `Private` temp is not a flag a backend reads: it is a directory
/// that has to exist, be granted, and be named in the environment, or
/// the first tool that writes a temporary file fails with a permission
/// error nobody can trace back to the policy.
///
/// Under `strict` on Linux the private temp is the fresh `tmpfs` the
/// mount namespace puts at `/tmp`, so the grant names `/tmp` and the
/// backend supplies the isolation.
fn materialize_temp(
    policy: &SandboxPolicy,
) -> Result<(SandboxPolicy, Option<std::path::PathBuf>), SandboxError> {
    let mut policy = policy.clone();
    match policy.temp.clone() {
        Temp::Inherit => {
            let inherited = std::env::temp_dir();
            policy = policy.read_write(&inherited);
            Ok((policy, None))
        }
        Temp::Path(directory) => {
            std::fs::create_dir_all(&directory).map_err(|error| {
                SandboxError::Policy(format!(
                    "the temp directory {} could not be created: {error}",
                    directory.display()
                ))
            })?;
            let directory = resolved(&directory)?;
            policy = grant_temp(policy, &directory);
            Ok((policy, None))
        }
        Temp::Private => {
            let directory = private_temp_path();
            std::fs::create_dir_all(&directory).map_err(|error| {
                SandboxError::Policy(format!(
                    "a private temp directory could not be created at {}: {error}",
                    directory.display()
                ))
            })?;
            let directory = resolved(&directory)?;
            policy = grant_temp(policy, &directory);
            Ok((policy, Some(directory)))
        }
    }
}

/// The temp directory in the same resolved form the policy compiler
/// gives every other path.
///
/// The grant, the recorded directory, and the `TMPDIR` the child reads
/// must all name one path: a backend that compares them - or a caller
/// that asks [`CompiledPolicy::access`] about its own
/// temp directory - otherwise sees two directories where there is one.
fn resolved(directory: &std::path::Path) -> Result<std::path::PathBuf, SandboxError> {
    directory
        .canonicalize()
        .map(|path| policy::simplified(&path))
        .map_err(|error| {
            SandboxError::Policy(format!(
                "the temp directory {} could not be resolved: {error}",
                directory.display()
            ))
        })
}

/// Grants `directory` read-write, names it in every spelling a
/// toolchain looks for, and records it so a backend can put a private
/// filesystem there.
fn grant_temp(mut policy: SandboxPolicy, directory: &std::path::Path) -> SandboxPolicy {
    let text = directory.to_string_lossy().into_owned();
    policy.temp_directory = Some(directory.to_path_buf());
    policy
        .read_write(directory)
        .env_set("TMPDIR", text.clone())
        .env_set("TEMP", text.clone())
        .env_set("TMP", text)
}

/// A temp directory name no other run shares.
fn private_temp_path() -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let serial = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("gossamer-sandbox-{}-{serial}", std::process::id()))
}

/// The single sentence explaining why a host tops out where it does.
fn blocking_reason(host: &SandboxCapabilities) -> String {
    host.notes
        .iter()
        .find(|note| note.contains("unavailable") || note.contains("disabled"))
        .cloned()
        .unwrap_or_else(|| {
            format!(
                "this host tops out at level {} - read `capabilities()` for what it can enforce",
                host.max_level
            )
        })
}

#[cfg(test)]
mod sandbox_tests {
    use super::*;

    #[test]
    fn a_level_the_host_cannot_honor_fails_closed() {
        let host = capabilities();
        if host.max_level == Level::Strict {
            return;
        }
        let policy = SandboxPolicy::new().level(Level::Strict);
        let error = Sandbox::new(&policy).expect_err("strict must not silently downgrade");
        assert!(
            matches!(error, SandboxError::LevelUnavailable { .. }),
            "{error}"
        );
        assert_eq!(error.exit_code(), EXIT_LEVEL_UNAVAILABLE);
    }

    #[test]
    fn a_capability_report_is_self_consistent() {
        let host = capabilities();
        if host.max_level >= Level::Standard {
            assert!(
                host.filesystem.is_enforced(),
                "a host claiming standard must enforce a filesystem policy"
            );
        }
        if host.max_level >= Level::Strict {
            assert!(
                host.process_isolation.is_enforced(),
                "a host claiming strict must isolate the process table"
            );
        }
    }
}
