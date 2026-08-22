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
            (
                landlock_only_network(abi),
                Enforcement::None,
                if abi.is_some() {
                    Level::Standard
                } else {
                    Level::Basic
                },
            )
        }
    };
    // The report answers for the best level this host can reach, and a
    // run at the default level does not get it. Saying so here is the
    // difference between a report about the host and a report about
    // what a command will actually be run under.
    if max_level == Level::Strict {
        notes.push(format!(
            "network denial is the network namespace, which is `strict` only: at \
             `standard` this host gives {}",
            landlock_only_network(abi)
        ));
    }
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

    let resource_limits = cgroup::enforcement(&crate::policy::Resources::default());
    if cgroup::available() {
        notes.push(
            "cgroup v2 delegated: memory and process-count limits are enforced, and the \
             whole tree is killed together"
                .to_string(),
        );
    }

    SandboxCapabilities {
        platform: Platform::Linux,
        os_description: os_description(),
        filesystem,
        network,
        process_isolation,
        resource_limits,
        max_level,
        notes,
    }
}

/// What network denial means with no network namespace: the Landlock
/// TCP layer, or nothing at all below the ABI that has it.
fn landlock_only_network(abi: Option<u32>) -> Enforcement {
    if abi.is_some_and(|version| version >= 4) {
        Enforcement::Partial(
            "Landlock TCP only: UDP, unix, and raw sockets stay reachable".to_string(),
        )
    } else {
        Enforcement::None
    }
}

/// How completely `policy`'s network setting is enforced on this host.
///
/// The policy says what was asked for; this says what the kernel will
/// hold to. A banner that renders the first and calls it the second is
/// how an operator ends up believing a denial that is not installed.
#[must_use]
pub(crate) fn network_enforcement(policy: &CompiledPolicy) -> Enforcement {
    if policy.network == Network::Open {
        return Enforcement::Full;
    }
    match policy.level {
        Level::None | Level::Basic => Enforcement::None,
        // The network namespace covers every protocol; below it there
        // is only the Landlock TCP layer.
        Level::Strict if policy.network == Network::None => Enforcement::Full,
        Level::Standard | Level::Strict => landlock_only_network(landlock::abi_version()),
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
            }
            lines.push(network_mechanism(policy));
            if policy.level == Level::Strict {
                lines.push(format!(
                    "seccomp filter refusing {} syscalls",
                    seccomp::Filter::denied_count()
                ));
            }
        }
    }
    if cgroup::available() {
        lines.push("cgroup v2 for the process tree, and its limits".to_string());
    }
    lines
}

/// The one line naming what holds the policy's network setting here.
fn network_mechanism(policy: &CompiledPolicy) -> String {
    let abi = landlock::abi_version();
    match (policy.level, policy.network) {
        (Level::Strict, Network::None) => "network namespace: no network, every protocol".to_string(),
        (_, Network::Open) => "no network restriction".to_string(),
        (_, network) if abi.is_some_and(|version| version >= 4) => {
            let landlock = if network == Network::None {
                "Landlock TCP bind and connect denied"
            } else {
                "Landlock TCP bind denied, connect allowed"
            };
            format!("{landlock} - UDP, unix, and raw sockets are NOT restricted here")
        }
        _ => "no network restriction: this kernel's Landlock has no network layer".to_string(),
    }
}

/// Runs `argv` under `policy`.
pub(crate) fn run(
    policy: &CompiledPolicy,
    argv: &[String],
    stdio: Stdio,
) -> Result<SandboxOutput, SandboxError> {
    // Created before the command so the child can place itself in it
    // between fork and exec, leaving no window in which a descendant
    // exists outside the cgroup.
    let group = cgroup::Cgroup::create(&policy.resources).map_err(SandboxError::Spawn)?;
    become_child_subreaper();
    let mut command = exec::base_command(policy, argv, stdio)?;
    install_enforcement(&mut command, policy, group.as_ref());
    let child = command.spawn().map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            SandboxError::CommandNotFound(argv[0].clone())
        } else {
            SandboxError::Spawn(format!("{error}"))
        }
    })?;
    exec::wait_for(policy, LinuxChild::new(child, group), stdio)
}

/// A running child, with whatever this host can use to reach a
/// descendant that left the process group.
struct LinuxChild {
    child: std::process::Child,
    session: libc::pid_t,
    group: Option<cgroup::Cgroup>,
}

impl LinuxChild {
    fn new(child: std::process::Child, group: Option<cgroup::Cgroup>) -> Self {
        // The child calls `setsid` before exec, so its own pid is its
        // session id, and every descendant inherits that session unless
        // it starts one of its own.
        let session = child.id() as libc::pid_t;
        Self {
            child,
            session,
            group,
        }
    }
}

impl exec::ChildProcess for LinuxChild {
    fn poll(&mut self) -> std::io::Result<Option<exec::Exit>> {
        exec::ChildProcess::poll(&mut self.child)
    }

    fn wait(&mut self) -> std::io::Result<exec::Exit> {
        exec::ChildProcess::wait(&mut self.child)
    }

    fn kill_tree(&mut self) {
        // The cgroup reaches every descendant whatever session it
        // moved to, so it is the answer when the host has one.
        if let Some(group) = &self.group {
            group.kill();
            let _ = self.child.kill();
            return;
        }
        exec::kill_tree(&mut self.child);
        kill_strays(self.session);
    }

    fn signal_group(&mut self, signal: i32) {
        exec::signal_group(&self.child, signal);
    }

    fn take_stdout(&mut self) -> Option<Box<dyn std::io::Read + Send>> {
        exec::ChildProcess::take_stdout(&mut self.child)
    }

    fn take_stderr(&mut self) -> Option<Box<dyn std::io::Read + Send>> {
        exec::ChildProcess::take_stderr(&mut self.child)
    }
}

/// Makes orphaned descendants reparent here instead of to `init`.
///
/// Without a cgroup this is the only way a descendant that called
/// `setsid` and double-forked stays findable: it becomes a child of
/// this process, so [`kill_strays`] can see it in `/proc`.
fn become_child_subreaper() {
    #[allow(
        unsafe_code,
        reason = "prctl has no safe wrapper; the call takes no pointer and \
                  affects only this process"
    )]
    unsafe {
        libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0);
    }
}

/// Kills what the process-group signal could not reach: a descendant
/// still in the run's session, and one that started a session of its
/// own and reparented here.
///
/// The sweep repeats because a process killed while forking can leave a
/// child behind, and stops as soon as a pass finds nothing.
fn kill_strays(session: libc::pid_t) {
    let own = std::process::id() as libc::pid_t;
    for _ in 0..8 {
        let mut found = false;
        let Ok(entries) = std::fs::read_dir("/proc") else {
            return;
        };
        for entry in entries.filter_map(Result::ok) {
            let Ok(pid) = entry.file_name().to_string_lossy().parse::<libc::pid_t>() else {
                continue;
            };
            if pid == own {
                continue;
            }
            let Some((parent, stray_session)) = parent_and_session(pid) else {
                continue;
            };
            if stray_session != session && parent != own {
                continue;
            }
            found = true;
            #[allow(
                unsafe_code,
                reason = "kill has no safe wrapper; the pid names a process in this \
                          run's session or a descendant reparented to this process"
            )]
            unsafe {
                libc::kill(pid, libc::SIGKILL);
            }
        }
        if !found {
            return;
        }
    }
}

/// The parent pid and session id of `pid`, from `/proc/<pid>/stat`.
///
/// The command name is parsed by skipping past its closing parenthesis:
/// it is the only field that can itself contain a space or a
/// parenthesis, which is why the kernel puts it in brackets.
fn parent_and_session(pid: libc::pid_t) -> Option<(libc::pid_t, libc::pid_t)> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let fields: Vec<&str> = stat[stat.rfind(')')? + 1..].split_whitespace().collect();
    // After the command name the fields are state, ppid, pgrp, session.
    Some((fields.get(1)?.parse().ok()?, fields.get(3)?.parse().ok()?))
}

/// Attaches the pre-exec enforcement for `policy` to `command`.
///
/// Everything the pre-exec closure needs is built here, before the
/// fork: between `fork` and `exec` only async-signal-safe calls are
/// permitted, so the closure allocates nothing and touches only what
/// this function captured.
fn install_enforcement(
    command: &mut Command,
    policy: &CompiledPolicy,
    group: Option<&cgroup::Cgroup>,
) {
    let join_cgroup = group.map(|group| group.join_path().clone());
    if policy.level == Level::None {
        // The cgroup is not enforcement, it is bookkeeping: at every
        // level it is how the supervisor reaches the tree afterwards,
        // and how a memory or process-count limit is applied at all.
        if let Some(path) = join_cgroup {
            #[allow(unsafe_code, reason = "documented on the closure below")]
            unsafe {
                command.pre_exec(move || join_cgroup_now(&path));
            }
        }
        return;
    }
    let level = policy.level;
    let deny_network = policy.network == Network::None;
    let network = policy.network;
    let private_temp = (policy.temp == Temp::Private)
        .then(|| policy.temp_directory.clone())
        .flatten();
    let abi = landlock::abi_version();
    let ruleset = abi.map(|abi| landlock::Ruleset::compile(abi, &policy.rules, network));
    let namespace_plan =
        (level == Level::Strict).then(|| {
            namespaces::Plan::new(private_temp.as_deref(), policy.resources.max_temp_size)
        });
    let filter =
        (level == Level::Strict && seccomp::Filter::is_supported()).then(seccomp::Filter::new);
    let file_size_limit = policy.resources.max_file_size;
    let cpu_limit = policy
        .resources
        .max_cpu_time
        .map(|limit| limit.as_secs().max(1));

    // SAFETY: the closure runs between `fork` and `exec` in a
    // single-threaded child. It calls only `prctl`, `setsid`,
    // `setrlimit`, `close`, `unshare`, `mount`, `open`, `write`,
    // `fork`, and `waitpid` - all async-signal-safe - over data this
    // function allocated before the fork.
    #[allow(unsafe_code, reason = "documented above")]
    unsafe {
        command.pre_exec(move || {
            // Before anything else: a process cannot leave the cgroup
            // it was placed in, so placing the child here means every
            // descendant it goes on to fork is inside it too.
            if let Some(path) = &join_cgroup {
                join_cgroup_now(path)?;
            }
            // Its own session and process group, so the supervisor can
            // reach the whole tree with one signal and the child
            // cannot steal the terminal.
            libc::setsid();
            mark_inherited_descriptors_close_on_exec();
            set_rlimits(file_size_limit, cpu_limit);
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

/// Applies the limits `setrlimit` can express.
///
/// Memory and process counts are not among them. `RLIMIT_AS` bounds
/// address space rather than resident memory, and `RLIMIT_NPROC` counts
/// every process the real user already has rather than the ones this
/// run started, so neither means what the policy says. Both ride the
/// cgroup instead, and are refused where there is none.
fn set_rlimits(max_file_size: Option<u64>, max_cpu_seconds: Option<u64>) {
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
}

/// Writes `0` into the run's `cgroup.procs`, placing the calling
/// process in the cgroup.
///
/// Async-signal-safe: `open`, `write`, and `close` over a path this
/// process turned into a C string before the fork.
fn join_cgroup_now(path: &std::ffi::CStr) -> std::io::Result<()> {
    let fd = unsafe { libc::open(path.as_ptr(), libc::O_WRONLY | libc::O_CLOEXEC) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let written = unsafe { libc::write(fd, c"0\n".as_ptr().cast(), 2) };
    unsafe { libc::close(fd) };
    if written == 2 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
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

/// Whether this host can enforce every limit in `resources`.
#[must_use]
pub(crate) fn resource_enforcement(
    resources: &crate::policy::Resources,
    level: Level,
) -> Enforcement {
    // A bound on the private temporary filesystem is a mount option,
    // and the mount exists only where there is a mount namespace.
    if resources.max_temp_size.is_some() && level < Level::Strict {
        return Enforcement::Partial(
            "a private temporary filesystem needs a mount namespace, which is `strict` \
             only: --max-temp-mb cannot be applied below it"
                .to_string(),
        );
    }
    if resources == &crate::policy::Resources::default() {
        return Enforcement::Full;
    }
    cgroup::enforcement(resources)
}
