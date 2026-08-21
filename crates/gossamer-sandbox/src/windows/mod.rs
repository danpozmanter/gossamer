//! Windows enforcement: restricted tokens, job objects, and
//! `AppContainer`.
//!
//! The constraint that shapes this backend: `AppContainer`'s filesystem
//! model is an allow-list by ACL on the host objects. There is no
//! overlay and no profile file, so letting a sandboxed build write to
//! `target\` means granting the container's package SID on `target\`
//! in the real filesystem and revoking it afterwards. That is a
//! mutation of the user's host ACLs performed by a sandbox, so it is
//! stated in `--explain`, the SID is derived deterministically from a
//! stable container name (a crashed run's grants stay findable), and
//! [`clean_stale_grants`] removes what a crash left behind.
//!
//! The second constraint: loopback is blocked inside an `AppContainer`
//! without an admin exemption, so `network = Allow` there does not
//! imply `127.0.0.1` works. The capability report says so rather than
//! letting a test discover it.

pub(crate) mod acl;
pub(crate) mod appcontainer;
pub(crate) mod job;
pub(crate) mod spawn;
pub(crate) mod token;

use std::os::windows::io::AsRawHandle;

use crate::error::{SandboxError, SandboxOutput};
use crate::exec::{self, Stdio};
use crate::level::{Enforcement, Level, Platform, SandboxCapabilities};
use crate::policy::{CompiledPolicy, Network};

/// What this Windows host can honor.
#[must_use]
pub(crate) fn capabilities() -> SandboxCapabilities {
    let mut notes = Vec::new();
    let restricted = token::Token::restricted();
    let restricted_token = restricted.is_ok();
    match &restricted {
        Ok(token) => notes.push(format!(
            "restricted token available: {} privilege(s) remain after the drop",
            token.privilege_count()
        )),
        Err(reason) => notes.push(format!("restricted tokens are unavailable: {reason}")),
    }
    let app_container = appcontainer::is_available();
    if app_container {
        notes.push(
            "AppContainer available: each granted path needs a package-SID ACE on the host \
             object, granted before the run and revoked after"
                .to_string(),
        );
        notes.push(
            "loopback is blocked inside an AppContainer without an admin CheckNetIsolation \
             exemption, so network=allow does not imply 127.0.0.1"
                .to_string(),
        );
    } else {
        notes.push("AppContainer is unavailable on this host".to_string());
    }
    let stale = acl::stale_grant_count();
    if stale > 0 {
        notes.push(format!(
            "{stale} stale AppContainer ACL grant(s) from an interrupted run - \
             `clean_stale_grants` removes them"
        ));
    }

    let max_level = if app_container {
        Level::Strict
    } else if restricted_token {
        Level::Standard
    } else {
        Level::Basic
    };

    SandboxCapabilities {
        platform: Platform::Windows,
        os_description: format!("Windows {}", std::env::consts::ARCH),
        filesystem: if app_container {
            Enforcement::Full
        } else if restricted_token {
            Enforcement::Partial(
                "restricted token only: objects granted to the full user SID are unreachable, \
                 but no per-path policy is applied"
                    .to_string(),
            )
        } else {
            Enforcement::None
        },
        network: if app_container {
            Enforcement::Full
        } else {
            Enforcement::None
        },
        process_isolation: if app_container {
            Enforcement::Full
        } else {
            Enforcement::Partial("job object only: tree cleanup, not isolation".to_string())
        },
        // Job objects bound processes, memory, and CPU time. A
        // per-file size limit has no job-object equivalent, so it is
        // named here rather than accepted and dropped.
        resource_limits: Enforcement::Partial(
            "job objects only: processes, memory, and CPU time; no per-file size limit".to_string(),
        ),
        max_level,
        notes,
    }
}

/// Names the mechanisms a run at this policy's level will install.
#[must_use]
pub(crate) fn mechanisms(policy: &CompiledPolicy) -> Vec<String> {
    let mut lines = Vec::new();
    match policy.level {
        Level::None => lines.push("no enforcement".to_string()),
        Level::Basic => {
            lines.push(
                "environment allowlist, private temp, handle-inheritance allowlist".to_string(),
            );
            lines.push("job object with kill-on-close".to_string());
        }
        Level::Standard => {
            lines.push("restricted token: privileges dropped, deny-only SIDs".to_string());
            lines.push("process mitigation policies".to_string());
            lines.push("job object with kill-on-close".to_string());
        }
        Level::Strict => {
            lines.push("AppContainer with a deterministic package SID".to_string());
            lines.push(format!(
                "host ACL grants on {} path(s), revoked on exit",
                policy.grants().count()
            ));
            match policy.network {
                Network::None => lines.push("no network capability SID".to_string()),
                Network::Client => lines.push(
                    "INTERNET_CLIENT capability; loopback still blocked without an admin \
                     exemption"
                        .to_string(),
                ),
                Network::Open => lines.push(
                    "INTERNET_CLIENT, INTERNET_CLIENT_SERVER, and \
                     PRIVATE_NETWORK_CLIENT_SERVER capabilities"
                        .to_string(),
                ),
            }
            lines.push("job object with kill-on-close".to_string());
        }
    }
    lines
}

/// Runs `argv` under `policy`.
///
/// Below `standard` the child is an ordinary `std::process::Command`
/// in a job object; at `standard` and above it is created directly,
/// because a restricted token and an `AppContainer` both have to be
/// attached at creation and `Command` cannot express either.
pub(crate) fn run(
    policy: &CompiledPolicy,
    argv: &[String],
    stdio: Stdio,
) -> Result<SandboxOutput, SandboxError> {
    if policy.level < Level::Standard {
        return run_unrestricted(policy, argv, stdio);
    }

    // Held for the length of the run: dropping the container revokes
    // every host ACL grant it made.
    let container = if policy.level >= Level::Strict {
        Some(appcontainer::Container::create(policy).map_err(SandboxError::Spawn)?)
    } else {
        None
    };
    let token = token::Token::restricted().map_err(|reason| SandboxError::LevelUnavailable {
        requested: policy.level,
        available: Level::Basic,
        reason,
    })?;
    let child = spawn::spawn(
        policy,
        argv,
        stdio,
        &token,
        container
            .as_ref()
            .map(appcontainer::Container::security_capabilities),
    )
    .map_err(|reason| {
        if reason.contains("The system cannot find the file") {
            SandboxError::CommandNotFound(argv[0].clone())
        } else {
            SandboxError::Spawn(reason)
        }
    })?;

    let _job = attach_job(policy, child.process())?;
    exec::wait_for(policy, child, stdio)
}

/// The `none` and `basic` path: an ordinary child in a job object.
fn run_unrestricted(
    policy: &CompiledPolicy,
    argv: &[String],
    stdio: Stdio,
) -> Result<SandboxOutput, SandboxError> {
    let mut command = exec::base_command(policy, argv, stdio)?;
    appcontainer::apply_standard_mitigations(&mut command);
    let child = command.spawn().map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            SandboxError::CommandNotFound(argv[0].clone())
        } else {
            SandboxError::Spawn(format!("{error}"))
        }
    })?;
    let _job = attach_job(policy, child.as_raw_handle().cast())?;
    exec::wait_for(policy, child, stdio)
}

/// Puts `process` in a kill-on-close job so the whole tree ends with
/// the run.
fn attach_job(
    policy: &CompiledPolicy,
    process: windows_sys::Win32::Foundation::HANDLE,
) -> Result<Option<job::Job>, SandboxError> {
    if !policy.kill_tree_on_exit {
        return Ok(None);
    }
    let job = job::Job::create(&policy.resources).map_err(SandboxError::Spawn)?;
    job.assign(process).map_err(SandboxError::Spawn)?;
    Ok(Some(job))
}

/// Revokes every ACL grant an interrupted run left behind, for
/// a consumer's cleanup command. Returns how many paths were revoked.
pub fn clean_stale_grants() -> Result<usize, String> {
    appcontainer::clean_stale_grants()
}

/// How many interrupted runs left grants behind.
#[must_use]
pub fn stale_grant_count() -> usize {
    acl::stale_grant_count()
}

/// Removes the sandbox's `AppContainer` profile.
pub fn delete_container_profile() -> Result<(), String> {
    appcontainer::delete_profile()
}

#[cfg(test)]
mod windows_tests {
    use super::*;

    #[test]
    fn a_job_object_alone_never_reaches_standard() {
        let report = capabilities();
        if report.max_level >= Level::Standard {
            assert!(
                report
                    .notes
                    .iter()
                    .any(|note| note.contains("restricted token available")),
                "{:?}",
                report.notes
            );
        }
    }

    #[test]
    fn strict_states_that_host_acls_are_modified() {
        let policy = crate::policy::SandboxPolicy::new()
            .level(Level::Strict)
            .compile()
            .expect("compile");
        let lines = mechanisms(&policy);
        assert!(
            lines.iter().any(|line| line.contains("host ACL grants")),
            "{lines:?}"
        );
    }
}
