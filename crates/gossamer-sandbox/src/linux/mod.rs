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
use std::sync::{Mutex, PoisonError, RwLock};

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

    if cgroup::available() {
        notes.push(
            "cgroup v2 delegated: the whole tree is killed together, including a \
             descendant that left its process group"
                .to_string(),
        );
    }

    SandboxCapabilities {
        platform: Platform::Linux,
        os_description: os_description(),
        filesystem,
        network,
        process_isolation,
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
        lines.push("cgroup v2 holding the process tree".to_string());
    }
    lines
}

/// The network setting Landlock's TCP layer has to hold, or `None`
/// when another mechanism holds it.
///
/// A network namespace is the whole boundary: it covers every
/// protocol, and the only interface inside it is the child's own
/// loopback, which the namespace brings up on purpose because a JVM
/// or Node tool asks the machine for a local address before it will
/// start. Landlock's layer matches on port and never on address, so
/// stacking it on top cannot deny anything the namespace has not
/// already denied - it can only take that loopback away.
fn landlock_network(policy: &CompiledPolicy) -> Option<Network> {
    if network_namespace_holds_it(policy) {
        return None;
    }
    Some(policy.network)
}

/// Whether this run's network setting is held by a network namespace.
///
/// The namespace is entered only at `strict`, and only for a policy
/// that asks for no network at all: any other setting needs the host's
/// own stack, so there is nothing to unshare.
fn network_namespace_holds_it(policy: &CompiledPolicy) -> bool {
    policy.level == Level::Strict && policy.network == Network::None
}

/// The one line naming what holds the policy's network setting here.
fn network_mechanism(policy: &CompiledPolicy) -> String {
    let abi = landlock::abi_version();
    match (policy.level, policy.network) {
        (Level::Strict, Network::None) => {
            "network namespace: no network, every protocol; the child's own \
             loopback is inside it"
                .to_string()
        }
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
    bound: Option<std::time::Duration>,
) -> Result<SandboxOutput, SandboxError> {
    // Created before the command so the child can place itself in it
    // between fork and exec, leaving no window in which a descendant
    // exists outside the cgroup.
    let group = cgroup::Cgroup::create().map_err(SandboxError::Spawn)?;
    become_child_subreaper();
    let mut command = exec::base_command(policy, argv, stdio)?;
    install_enforcement(&mut command, policy, group.as_ref());
    // A sweep tells one run's tree from another's by the session
    // registry, so the child has to be in it before any sweep can see
    // the child at all: the guard makes the spawn and the registration
    // one step as far as a concurrent teardown is concerned.
    let child = {
        let _spawning = SPAWNING.read().unwrap_or_else(PoisonError::into_inner);
        let child = command.spawn().map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                SandboxError::CommandNotFound(argv[0].clone())
            } else {
                SandboxError::Spawn(format!("{error}"))
            }
        })?;
        LinuxChild::new(child, group)
    };
    exec::wait_for(policy, child, stdio, bound)
}

/// The session of every run this process is supervising right now.
///
/// [`kill_strays`] reaches an orphan by asking `/proc` which processes
/// were reparented here, and a concurrent run's own child answers that
/// question exactly as this run's orphan does. The registry is what
/// separates them, so a teardown reaches only its own tree.
static LIVE_SESSIONS: Mutex<Vec<libc::pid_t>> = Mutex::new(Vec::new());

/// Held shared while a run starts a child, and exclusively while a
/// teardown sweeps.
///
/// A child exists from the fork onwards, and joins [`LIVE_SESSIONS`]
/// only once the spawn hands its pid back. A sweep that read `/proc`
/// in between would find a live child of this process that no run
/// claims - which is exactly the description of a stray - so the two
/// are kept apart rather than ordered by chance.
static SPAWNING: RwLock<()> = RwLock::new(());

/// Whether `session` is the session of some run other than `mine`.
fn another_run_owns(session: libc::pid_t, mine: libc::pid_t) -> bool {
    LIVE_SESSIONS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .any(|live| *live != mine && *live == session)
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
        LIVE_SESSIONS
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(session);
        Self {
            child,
            session,
            group,
        }
    }
}

impl Drop for LinuxChild {
    fn drop(&mut self) {
        let mut live = LIVE_SESSIONS.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(at) = live.iter().position(|live| *live == self.session) {
            live.swap_remove(at);
        }
    }
}

impl exec::ChildProcess for LinuxChild {
    fn poll(&mut self) -> std::io::Result<Option<exec::Exit>> {
        exec::ChildProcess::poll(&mut self.child)
    }

    fn reap(&mut self) {
        exec::ChildProcess::reap(&mut self.child);
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
/// Reparenting is how an orphan becomes findable, so a pid whose parent
/// is this process is a candidate. Two things then have to hold for it
/// to be this run's. Its session is one this process leads no more:
/// every descendant of a run inherits the run's session until it calls
/// `setsid`, so a child sharing this process's own session was started
/// by the caller rather than by any run, and a build tool a caller
/// spawned itself is not a stray. And no other live run claims its
/// session - several runs share one supervisor whenever a caller
/// sandboxes concurrently, and each one's tree stops at its own
/// session.
///
/// The sweep repeats because a process killed while forking can leave a
/// child behind, and stops as soon as a pass finds nothing.
fn kill_strays(session: libc::pid_t) {
    // A run registers its session after the fork that creates it, so a
    // sweep that overlapped a spawn would meet a live child no run
    // claims yet. Sweeping exclusively is what makes the registry an
    // answer rather than a race.
    let _sweeping = SPAWNING.write().unwrap_or_else(PoisonError::into_inner);
    let own = std::process::id() as libc::pid_t;
    let own_session = unsafe { libc::getsid(0) };
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
            let Some(stray) = stat_of(pid) else {
                continue;
            };
            let (parent, stray_session) = (stray.parent, stray.session);
            // A process that has already exited cannot be killed and
            // cannot start anything, so counting it would keep the
            // sweep looking for work that is finished. The child this
            // run waits on is exactly that until it is reaped.
            if stray.exited {
                continue;
            }
            let ours = stray_session == session;
            let adopted = parent == own
                && stray_session != own_session
                && !another_run_owns(stray_session, session);
            if !ours && !adopted {
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

/// What the sweep needs to know about one process.
struct Stat {
    parent: libc::pid_t,
    session: libc::pid_t,
    /// Whether the process is a zombie: gone, but still holding its
    /// entry until whoever started it collects the status.
    exited: bool,
}

/// Reads `pid`'s parent, session, and liveness from `/proc/<pid>/stat`.
///
/// The command name is parsed by skipping past its closing parenthesis:
/// it is the only field that can itself contain a space or a
/// parenthesis, which is why the kernel puts it in brackets.
fn stat_of(pid: libc::pid_t) -> Option<Stat> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let fields: Vec<&str> = stat[stat.rfind(')')? + 1..].split_whitespace().collect();
    // After the command name the fields are state, ppid, pgrp, session.
    Some(Stat {
        parent: fields.get(1)?.parse().ok()?,
        session: fields.get(3)?.parse().ok()?,
        exited: *fields.first()? == "Z",
    })
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
    let private_temp = (policy.temp == Temp::Private)
        .then(|| policy.temp_directory.clone())
        .flatten();
    let abi = landlock::abi_version();
    let ruleset =
        abi.map(|abi| landlock::Ruleset::compile(abi, &policy.rules, landlock_network(policy)));
    // A `connect` to a pathname socket is outside Landlock's access
    // rights, so a denied socket needs the mount namespace to be
    // unreachable rather than merely refused.
    let denied_sockets: Vec<std::path::PathBuf> = policy
        .denials()
        .map(|rule| rule.path.clone())
        .filter(|path| {
            std::fs::symlink_metadata(path)
                .is_ok_and(|meta| std::os::unix::fs::FileTypeExt::is_socket(&meta.file_type()))
        })
        .collect();
    let namespace_plan = (level == Level::Strict)
        .then(|| namespaces::Plan::new(private_temp.as_deref(), &denied_sockets));
    let filter =
        (level == Level::Strict && seccomp::Filter::is_supported()).then(seccomp::Filter::new);

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

    /// A session number no run in this process was ever given, for a
    /// sweep that must decide on the adoption rule alone.
    const NO_RUNS_SESSION: libc::pid_t = libc::pid_t::MAX;

    /// Starts a child in a session of its own, the way a sandboxed run
    /// does, and answers its pid.
    fn child_in_its_own_session(script: &str) -> std::process::Child {
        let mut command = Command::new("/bin/sh");
        command.arg("-c").arg(script);
        command.stdout(std::process::Stdio::null());
        command.stderr(std::process::Stdio::null());
        #[allow(
            unsafe_code,
            reason = "the pre-exec closure calls setsid only, which allocates \
                      nothing and touches no state the fork shares"
        )]
        unsafe {
            command.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        command.spawn().expect("spawn a child for the sweep to see")
    }

    /// Starts a stand-in for a live run: a child in a session of its
    /// own, registered before any sweep can see it, exactly as
    /// [`run`] starts one.
    fn live_run(script: &str) -> std::process::Child {
        let _spawning = SPAWNING.read().unwrap_or_else(PoisonError::into_inner);
        let child = child_in_its_own_session(script);
        LIVE_SESSIONS
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(child.id() as libc::pid_t);
        child
    }

    /// Retires the session a [`live_run`] registered.
    fn end_run(child: &mut std::process::Child) {
        let session = child.id() as libc::pid_t;
        let mut live = LIVE_SESSIONS.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(at) = live.iter().position(|live| *live == session) {
            live.swap_remove(at);
        }
        drop(live);
        let _ = child.kill();
        let _ = child.wait();
    }

    /// Whether `pid` is still a running process.
    ///
    /// A killed process answers `kill(pid, 0)` for as long as it is an
    /// unreaped zombie, so liveness has to come from its state rather
    /// than from whether it is signallable.
    fn running(pid: libc::pid_t) -> bool {
        stat_of(pid).is_some_and(|stat| !stat.exited)
    }

    /// The sweep reaches an orphan by asking `/proc` who was reparented
    /// here, which a concurrent run's own child answers identically.
    /// One run's teardown must not reach another run's tree.
    #[test]
    fn a_sweep_leaves_another_live_runs_child_alone() {
        let mut theirs = live_run("sleep 30");
        let their_session = theirs.id() as libc::pid_t;

        let mut ours = live_run("sleep 30");
        let our_session = ours.id() as libc::pid_t;
        kill_strays(our_session);

        let theirs_survived = running(their_session);
        let ours_reached = !running(our_session);

        end_run(&mut theirs);
        end_run(&mut ours);

        assert!(
            theirs_survived,
            "the sweep killed a child belonging to another live run"
        );
        assert!(
            ours_reached,
            "the sweep must still reach its own run's tree"
        );
    }

    /// A caller that sandboxes one command still starts others itself -
    /// a compiler, a fetch, an editor. They are children of this
    /// process and they never left its session, so no run's teardown
    /// may reach them.
    #[test]
    fn a_sweep_leaves_a_plain_child_of_this_process_alone() {
        let mut theirs = Command::new("/bin/sh")
            .arg("-c")
            .arg("sleep 30")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn a child of this process");
        let pid = theirs.id() as libc::pid_t;

        kill_strays(NO_RUNS_SESSION);

        let survived = running(pid);
        let _ = theirs.kill();
        let _ = theirs.wait();
        assert!(survived, "the sweep killed a child no run started");
    }

    /// A child is a live process from the fork onwards and joins the
    /// session registry only once the spawn returns. A sweep that ran
    /// in between would find a child of this process that no run claims
    /// and read it as a stray, so a run that started a command must be
    /// able to finish starting it before any sweep looks.
    #[test]
    fn a_concurrent_sweep_leaves_a_run_that_is_still_spawning_alone() {
        let policy = crate::policy::SandboxPolicy::new()
            .level(Level::None)
            .compile()
            .expect("compile policy");
        let sweeper = std::thread::spawn(|| {
            for _ in 0..64 {
                kill_strays(NO_RUNS_SESSION);
            }
        });

        let argv = ["/bin/sh", "-c", "printf ok"].map(String::from);
        let outcomes: Vec<Result<SandboxOutput, SandboxError>> = (0..64)
            .map(|_| run(&policy, &argv, Stdio::Capture, None))
            .collect();
        sweeper.join().expect("join the sweeper");

        for outcome in outcomes {
            let output = outcome.expect("a run a concurrent sweep must not have reached");
            assert_eq!(output.code, 0);
            assert_eq!(output.stdout, b"ok");
        }
    }

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

    /// The namespace is the boundary at strict, so the Landlock layer
    /// is not asked to hold the same setting a second time - it would
    /// only take away the loopback the namespace brings up.
    #[test]
    fn the_tcp_layer_is_left_out_where_the_namespace_is_the_boundary() {
        let compiled = |level, network| {
            crate::policy::SandboxPolicy::new()
                .level(level)
                .network(network)
                .compile()
                .expect("compile")
        };
        assert_eq!(
            landlock_network(&compiled(Level::Strict, Network::None)),
            None
        );
        // Every other combination reaches the host's own stack, so the
        // TCP layer is all there is.
        assert_eq!(
            landlock_network(&compiled(Level::Standard, Network::None)),
            Some(Network::None)
        );
        assert_eq!(
            landlock_network(&compiled(Level::Strict, Network::Client)),
            Some(Network::Client)
        );
        assert_eq!(
            landlock_network(&compiled(Level::Standard, Network::Client)),
            Some(Network::Client)
        );
    }

    /// What the run reports has to keep matching what it installs: a
    /// namespace-held policy is still fully enforced, and says so.
    #[test]
    fn a_namespace_held_policy_is_still_reported_as_fully_enforced() {
        let policy = crate::policy::SandboxPolicy::new()
            .level(Level::Strict)
            .network(Network::None)
            .compile()
            .expect("compile");
        assert_eq!(network_enforcement(&policy), Enforcement::Full);
        assert!(
            mechanisms(&policy)
                .iter()
                .any(|line| line.contains("loopback")),
            "the mechanism line names what is inside the namespace"
        );
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
