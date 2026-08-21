//! Linux enforcement: Landlock for the filesystem, namespaces for
//! process and network isolation, seccomp for kernel surface.
//!
//! The level ladder here is what the capability report answers with:
//! `standard` needs only Landlock, so it works inside containers and
//! on hosts with user namespaces disabled, and `strict` adds the
//! namespaces and fails closed when the host will not give them.

#![allow(
    unsafe_code,
    reason = "pre-exec enforcement is raw syscalls by nature; every call \
              operates on data allocated before the fork"
)]

pub(crate) mod cgroup;
pub(crate) mod landlock;
pub(crate) mod namespaces;
pub(crate) mod seccomp;

use std::os::unix::process::CommandExt;
use std::process::Command;

use crate::error::{SandboxError, SandboxOutput};
use crate::exec::{self, Stdio};
use crate::level::{Enforcement, Level, Platform, SandboxCapabilities};
use crate::policy::{CompiledPolicy, Network, Temp};

/// What this Linux host can honor.
#[must_use]
pub(crate) fn capabilities() -> SandboxCapabilities {
    let mut notes = Vec::new();
    let abi = landlock::abi_version();
    let filesystem = if let Some(version) = abi {
        notes.push(format!("Landlock ABI {version}"));
        if version < 3 {
            notes.push(
                "Landlock ABI below 3: `truncate` cannot be restricted separately".to_string(),
            );
        }
        if version >= 4 {
            notes.push(
                "Landlock ABI 4: TCP bind and connect only - no UDP, no unix sockets, no raw"
                    .to_string(),
            );
        }
        Enforcement::Full
    } else {
        notes
            .push("Landlock is unavailable: this kernel enforces no filesystem policy".to_string());
        Enforcement::None
    };

    let namespace_blocker = namespaces::user_namespace_blocker();
    let (network, process_isolation, max_level) = match &namespace_blocker {
        None => (Enforcement::Full, Enforcement::Full, Level::Strict),
        Some(reason) => {
            notes.push(format!(
                "unprivileged user namespaces unavailable: {reason}"
            ));
            let network = if abi.is_some_and(|version| version >= 4) {
                Enforcement::Partial(
                    "no network namespace: Landlock restricts TCP only, so UDP and unix \
                     sockets stay reachable"
                        .to_string(),
                )
            } else {
                Enforcement::None
            };
            (
                network,
                Enforcement::None,
                if abi.is_some() {
                    Level::Standard
                } else {
                    Level::Basic
                },
            )
        }
    };
    if seccomp::Filter::is_supported() {
        notes.push(format!(
            "seccomp refuses {} syscalls",
            seccomp::Filter::denied_count()
        ));
    } else {
        notes.push(
            "no seccomp syscall table for this architecture: the kernel-surface layer is \
             not installed"
                .to_string(),
        );
    }

    SandboxCapabilities {
        platform: Platform::Linux,
        os_description: os_description(),
        filesystem,
        network,
        process_isolation,
        resource_limits: cgroup::enforcement(&crate::policy::Resources::default()),
        max_level,
        notes,
    }
}

fn os_description() -> String {
    let release = std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .map_or_else(|_| "unknown".to_string(), |text| text.trim().to_string());
    format!("Linux {release} {}", std::env::consts::ARCH)
}

/// Names the mechanisms a run at `level` will actually install, for
/// `--explain`.
#[must_use]
pub(crate) fn mechanisms(policy: &CompiledPolicy) -> Vec<String> {
    let mut lines = Vec::new();
    match policy.level {
        Level::None => lines.push("no enforcement".to_string()),
        Level::Basic => {
            lines.push("environment allowlist, private temp, descriptor hygiene".to_string());
        }
        Level::Standard | Level::Strict => {
            if let Some(abi) = landlock::abi_version() {
                lines.push(format!("Landlock ABI {abi} filesystem rights"));
            }
            lines.push("no_new_privs".to_string());
            if policy.level == Level::Strict {
                lines.push("user, mount, IPC, UTS, and PID namespaces".to_string());
                lines.push("private /proc, reaper at PID 1".to_string());
                if policy.network == Network::Deny {
                    lines.push("network namespace".to_string());
                }
                lines.push(format!(
                    "seccomp filter refusing {} syscalls",
                    seccomp::Filter::denied_count()
                ));
            }
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
    install_enforcement(&mut command, policy);
    let child = command.spawn().map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            SandboxError::CommandNotFound(argv[0].clone())
        } else {
            SandboxError::Spawn(format!("{error}"))
        }
    })?;
    exec::wait_for(policy, child, stdio)
}

/// Attaches the pre-exec enforcement for `policy` to `command`.
///
/// Everything the pre-exec closure needs is built here, before the
/// fork: between `fork` and `exec` only async-signal-safe calls are
/// permitted, so the closure allocates nothing and touches only what
/// this function captured.
fn install_enforcement(command: &mut Command, policy: &CompiledPolicy) {
    if policy.level == Level::None {
        return;
    }
    let level = policy.level;
    let deny_network = policy.network == Network::Deny;
    let private_temp = (policy.temp == Temp::Private)
        .then(|| policy.temp_directory.clone())
        .flatten();
    let abi = landlock::abi_version();
    let ruleset = abi.map(|abi| landlock::Ruleset::compile(abi, &policy.rules, deny_network));
    let namespace_plan =
        (level == Level::Strict).then(|| namespaces::Plan::new(private_temp.as_deref()));
    let filter =
        (level == Level::Strict && seccomp::Filter::is_supported()).then(seccomp::Filter::new);
    let file_size_limit = policy.resources.max_file_size;
    let cpu_limit = policy
        .resources
        .max_cpu_time
        .map(|limit| limit.as_secs().max(1));
    let process_limit = policy.resources.max_processes;

    // SAFETY: the closure runs between `fork` and `exec` in a
    // single-threaded child. It calls only `prctl`, `setsid`,
    // `setrlimit`, `close`, `unshare`, `mount`, `open`, `write`,
    // `fork`, and `waitpid` - all async-signal-safe - over data this
    // function allocated before the fork.
    #[allow(unsafe_code, reason = "documented above")]
    unsafe {
        command.pre_exec(move || {
            // Its own session and process group, so the supervisor can
            // reach the whole tree with one signal and the child
            // cannot steal the terminal.
            libc::setsid();
            mark_inherited_descriptors_close_on_exec();
            set_rlimits(file_size_limit, cpu_limit, process_limit);
            // Mandatory before an unprivileged Landlock ruleset or
            // seccomp filter, and inherited across exec, which is what
            // makes descendant inheritance work.
            if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            if let Some(plan) = &namespace_plan {
                plan.enter(deny_network)
                    .map_err(std::io::Error::from_raw_os_error)?;
                // PID 1 of the new namespace has to exist before
                // `/proc` can be mounted for it.
                namespaces::fork_reaper().map_err(std::io::Error::from_raw_os_error)?;
                plan.mount_private_filesystems()
                    .map_err(std::io::Error::from_raw_os_error)?;
            }
            if let Some(ruleset) = &ruleset {
                ruleset
                    .restrict_self()
                    .map_err(std::io::Error::from_raw_os_error)?;
            }
            if let Some(filter) = &filter {
                filter
                    .install()
                    .map_err(std::io::Error::from_raw_os_error)?;
            }
            Ok(())
        });
    }
}

/// Marks every descriptor above the three standard streams
/// close-on-exec, so the child gets exactly the streams the caller
/// passed.
///
/// A descriptor the caller left open is outside every filesystem
/// policy: the child inherits the object, not the path, so no rule can
/// reach it. They are marked rather than closed because the spawn
/// itself holds one - the pipe `pre_exec` failures are reported
/// through - and closing that pipe turns a failed exec into a silent
/// one.
fn mark_inherited_descriptors_close_on_exec() {
    const SYS_CLOSE_RANGE: libc::c_long = 436;
    /// `CLOSE_RANGE_CLOEXEC`: mark the range instead of closing it.
    const CLOSE_RANGE_CLOEXEC: libc::c_uint = 1 << 2;
    let marked = unsafe {
        libc::syscall(
            SYS_CLOSE_RANGE,
            3u32,
            u32::MAX,
            CLOSE_RANGE_CLOEXEC as libc::c_uint,
        )
    };
    if marked == 0 {
        return;
    }
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    let ceiling = if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut limit) } == 0 {
        limit.rlim_cur.min(4096) as libc::c_int
    } else {
        1024
    };
    for fd in 3..ceiling {
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        if flags >= 0 {
            unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) };
        }
    }
}

/// Applies the limits `setrlimit` can express. Memory and process
/// counts that need a cgroup are reported unenforced rather than set
/// here, because an unprivileged process cannot make them stick.
fn set_rlimits(
    max_file_size: Option<u64>,
    max_cpu_seconds: Option<u64>,
    max_processes: Option<u32>,
) {
    let apply = |resource, value: u64| {
        let limit = libc::rlimit {
            rlim_cur: value as libc::rlim_t,
            rlim_max: value as libc::rlim_t,
        };
        unsafe { libc::setrlimit(resource, &raw const limit) };
    };
    if let Some(bytes) = max_file_size {
        apply(libc::RLIMIT_FSIZE, bytes);
    }
    if let Some(seconds) = max_cpu_seconds {
        apply(libc::RLIMIT_CPU, seconds);
    }
    if let Some(count) = max_processes {
        apply(libc::RLIMIT_NPROC, u64::from(count));
    }
}

#[cfg(test)]
mod linux_tests {
    use super::*;

    #[test]
    fn the_capability_report_names_the_landlock_abi_when_there_is_one() {
        let report = capabilities();
        assert_eq!(report.platform, Platform::Linux);
        if landlock::abi_version().is_some() {
            assert!(
                report
                    .notes
                    .iter()
                    .any(|note| note.starts_with("Landlock ABI")),
                "{:?}",
                report.notes
            );
            assert!(report.max_level >= Level::Standard);
        }
    }

    #[test]
    fn strict_names_every_mechanism_it_installs() {
        let policy = crate::policy::SandboxPolicy::new()
            .level(Level::Strict)
            .compile()
            .expect("compile");
        let lines = mechanisms(&policy);
        assert!(
            lines.iter().any(|line| line.contains("namespaces")),
            "{lines:?}"
        );
        assert!(
            lines.iter().any(|line| line.contains("seccomp")),
            "{lines:?}"
        );
        assert!(
            lines.iter().any(|line| line.contains("network namespace")),
            "{lines:?}"
        );
    }
}
