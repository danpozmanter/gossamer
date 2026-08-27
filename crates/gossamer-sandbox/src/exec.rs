//! Process lifecycle: spawn, wait, forward signals, time out, and tear
//! the tree down.
//!
//! The parts that are the same on every OS live here; a backend
//! contributes only the enforcement it applies between spawn and exec,
//! and whatever mechanism it has for reaching a descendant that left
//! the process group.

use std::process::{Child, Command, Stdio as ProcessStdio};
use std::time::Duration;

use crate::error::{SandboxError, SandboxOutput};
use crate::policy::CompiledPolicy;

/// A running child the supervisor can wait on, poll, and kill.
///
/// `std::process::Child` implements it; so does the Windows backend's
/// own child, which exists because a restricted token has to be
/// attached at creation and `Command` cannot express that, and the
/// Linux backend's, which carries the run's cgroup.
pub(crate) trait ChildProcess {
    /// The child's exit, once it has one, read without releasing its
    /// pid: the process group and session a teardown names are that
    /// pid, and the kernel may hand a released one to a new process.
    fn poll(&mut self) -> std::io::Result<Option<Exit>>;
    /// Releases the child's entry in the process table, once nothing
    /// else will name its pid. Blocks until the child has exited.
    fn reap(&mut self);
    /// Kills the child and every descendant it started, including one
    /// that left the process group.
    fn kill_tree(&mut self);
    /// Delivers `signal` to the child's process group, for forwarding
    /// an interrupt the supervisor received.
    ///
    /// Unix only: a Windows console interrupt already reaches every
    /// process in the console group, so there is nothing to forward.
    #[cfg(unix)]
    fn signal_group(&mut self, signal: i32);
    /// Takes the captured standard output stream, if there is one.
    fn take_stdout(&mut self) -> Option<Box<dyn std::io::Read + Send>>;
    /// Takes the captured standard error stream, if there is one.
    fn take_stderr(&mut self) -> Option<Box<dyn std::io::Read + Send>>;
}

/// How a child ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Exit {
    /// Exited with this code.
    Code(i32),
    /// Died on this signal. Unix only; Windows reports every ending
    /// as an exit code.
    #[cfg(unix)]
    Signal(i32),
}

impl ChildProcess for Child {
    fn poll(&mut self) -> std::io::Result<Option<Exit>> {
        peek(self)
    }

    fn reap(&mut self) {
        let _ = Child::wait(self);
    }

    fn kill_tree(&mut self) {
        kill_tree(self);
    }

    #[cfg(unix)]
    fn signal_group(&mut self, signal: i32) {
        signal_group(self, signal);
    }

    fn take_stdout(&mut self) -> Option<Box<dyn std::io::Read + Send>> {
        self.stdout
            .take()
            .map(|stream| Box::new(stream) as Box<dyn std::io::Read + Send>)
    }

    fn take_stderr(&mut self) -> Option<Box<dyn std::io::Read + Send>> {
        self.stderr
            .take()
            .map(|stream| Box::new(stream) as Box<dyn std::io::Read + Send>)
    }
}

/// The child's exit, if it has one, leaving it in the process table.
///
/// `WNOWAIT` reports the status without collecting the child, so the
/// pid stays this run's for as long as the teardown still needs to name
/// its process group and session.
#[cfg(unix)]
fn peek(child: &mut Child) -> std::io::Result<Option<Exit>> {
    let mut status = 0;
    #[allow(
        unsafe_code,
        reason = "waitpid has no safe wrapper; the pid is this process's own child \
                  and the status is a stack local the call writes through"
    )]
    let seen = unsafe {
        libc::waitpid(
            child.id() as libc::pid_t,
            &raw mut status,
            libc::WNOHANG | libc::WNOWAIT,
        )
    };
    if seen == 0 {
        return Ok(None);
    }
    if seen < 0 {
        // `ECHILD` means the child was already collected, which only the
        // cached status can answer for.
        return Ok(child.try_wait()?.map(exit_of));
    }
    if libc::WIFSIGNALED(status) {
        Ok(Some(Exit::Signal(libc::WTERMSIG(status))))
    } else {
        Ok(Some(Exit::Code(libc::WEXITSTATUS(status))))
    }
}

/// The child's exit, if it has one, leaving it in the process table.
///
/// The supervisor holds the child's process handle, so Windows keeps
/// the pid reserved until that handle is dropped and a status read
/// cannot make it name anything else.
#[cfg(not(unix))]
fn peek(child: &mut Child) -> std::io::Result<Option<Exit>> {
    Ok(child.try_wait()?.map(exit_of))
}

/// How a `std::process::ExitStatus` ended.
fn exit_of(status: std::process::ExitStatus) -> Exit {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return Exit::Signal(signal);
        }
    }
    Exit::Code(status.code().unwrap_or(1))
}

/// What the child's standard streams are connected to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Stdio {
    /// The caller's own streams, so the child writes straight through.
    #[default]
    Inherit,
    /// Standard output and error are pipes the sandbox drains and hands
    /// back in [`SandboxOutput`]. Standard input is still the caller's:
    /// capturing what a command says does not mean silencing what it
    /// is told.
    Capture,
    /// Discarded, standard input included.
    Null,
}

impl Stdio {
    fn to_process_stdio(self) -> ProcessStdio {
        match self {
            Self::Inherit => ProcessStdio::inherit(),
            Self::Capture => ProcessStdio::piped(),
            Self::Null => ProcessStdio::null(),
        }
    }

    /// Where the child's standard input comes from.
    ///
    /// Inherited unless the caller asked for `Null`. A sandbox that
    /// silently substituted an empty stdin would break every piped
    /// invocation for a property no policy asked for.
    fn to_stdin_stdio(self) -> ProcessStdio {
        match self {
            Self::Inherit | Self::Capture => ProcessStdio::inherit(),
            Self::Null => ProcessStdio::null(),
        }
    }
}

// Only the macOS backend needs to resolve a program before the launch
// does: every other backend spawns `argv[0]` itself and reports the
// launch's own `NotFound`. Compiled under `test` as well, so the
// resolution rules are proven on every host rather than only on the
// one platform that installs them.
/// The `PATH` the exec family falls back to when the environment
/// carries none. POSIX names it `_CS_PATH`; every Unix ships the same
/// two directories in it, and Windows searches its own system
/// directories whatever the environment says.
#[cfg(any(target_os = "macos", test))]
fn default_path() -> &'static str {
    if cfg!(windows) {
        "C:\\Windows\\system32;C:\\Windows"
    } else {
        "/usr/bin:/bin"
    }
}

/// The file `program` names, resolved the way the launch itself
/// resolves it: a spelling with a separator against the policy's
/// working directory, a bare name through the `PATH` the child is
/// given.
///
/// A backend that wraps `argv` in another program - macOS runs the
/// command through `sandbox-exec` - launches a wrapper that always
/// exists, so the launch cannot report that the command does not.
/// Resolving first is what keeps `command not found` a distinct
/// outcome on every platform rather than whatever code the wrapper
/// happens to exit with.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn resolve_program(
    policy: &CompiledPolicy,
    program: &str,
) -> Result<std::path::PathBuf, SandboxError> {
    let not_found = || SandboxError::CommandNotFound(program.to_string());
    let candidate = std::path::Path::new(program);
    if candidate.components().count() > 1 {
        let full = if candidate.is_absolute() {
            candidate.to_path_buf()
        } else {
            working_directory(policy).join(candidate)
        };
        return crate::discover::is_executable_file(&full)
            .then_some(full)
            .ok_or_else(not_found);
    }
    // The child's own environment, because that is what the search
    // the child's launch performs will read.
    let path = policy
        .environment()
        .get("PATH")
        .filter(|value| !value.is_empty())
        .map_or_else(|| std::ffi::OsString::from(default_path()), Into::into);
    crate::discover::search_path(program, &path).ok_or_else(not_found)
}

/// The directory a relative path in `policy` resolves against.
#[cfg(any(target_os = "macos", test))]
fn working_directory(policy: &CompiledPolicy) -> std::path::PathBuf {
    policy.working_directory.clone().unwrap_or_else(|| {
        std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
    })
}

/// Builds the `Command` for `argv` with the policy's environment and
/// working directory applied.
///
/// The environment is replaced rather than extended: an allowlist that
/// only adds is not an allowlist.
pub(crate) fn base_command(
    policy: &CompiledPolicy,
    argv: &[String],
    stdio: Stdio,
) -> Result<Command, SandboxError> {
    let (program, arguments) = argv
        .split_first()
        .ok_or_else(|| SandboxError::Policy("no command to run".to_string()))?;
    let mut command = Command::new(program);
    command.args(arguments);
    command.env_clear();
    for (name, value) in policy.environment() {
        command.env(name, value);
    }
    if let Some(directory) = &policy.working_directory {
        command.current_dir(directory);
    }
    command.stdin(stdio.to_stdin_stdio());
    command.stdout(stdio.to_process_stdio());
    command.stderr(stdio.to_process_stdio());
    Ok(command)
}

/// Waits for `child`, forwarding an interrupt the supervisor receives,
/// and tearing the whole tree down when the policy says to.
///
/// `bound` is the caller's own clock rather than anything the policy
/// carries: a wrapper that must answer its own caller within a deadline
/// asks for one here, and a run with no deadline costs no wakeups.
pub(crate) fn wait_for<C: ChildProcess>(
    policy: &CompiledPolicy,
    mut child: C,
    stdio: Stdio,
    bound: Option<Duration>,
) -> Result<SandboxOutput, SandboxError> {
    if stdio == Stdio::Capture {
        // Drain on threads: a child that fills one pipe's buffer while
        // the sandbox reads the other blocks forever otherwise.
        let out = child.take_stdout();
        let err = child.take_stderr();
        let out_reader = std::thread::spawn(move || read_all(out));
        let err_reader = std::thread::spawn(move || read_all(err));
        let exit = wait_for_exit(&mut child, bound);
        // A descendant that outlived the child holds the write end of
        // both pipes, so the teardown comes before the readers are
        // joined.
        teardown(policy, &mut child);
        let stdout = out_reader.join().unwrap_or_default();
        let stderr = err_reader.join().unwrap_or_default();
        exit.and_then(|exit| finish(exit, stdout, stderr))
    } else {
        let exit = wait_for_exit(&mut child, bound);
        teardown(policy, &mut child);
        exit.and_then(|exit| finish(exit, Vec::new(), Vec::new()))
    }
}

/// Kills whatever outlived the child, then releases the child's pid.
///
/// The order is the point. Every mechanism that reaches a descendant
/// names the child's pid - its process group, its session, the sweep
/// through `/proc` - and the kernel is free to hand that pid to an
/// unrelated process the moment the child is collected. Collecting it
/// last is what keeps a teardown inside its own run.
///
/// This is the only place the descendant guarantee is kept on every
/// exit path, including the timed-out and interrupted ones.
fn teardown<C: ChildProcess>(policy: &CompiledPolicy, child: &mut C) {
    if policy.kill_tree_on_exit {
        child.kill_tree();
    }
    child.reap();
}

fn read_all(reader: Option<Box<dyn std::io::Read + Send>>) -> Vec<u8> {
    let mut buffer = Vec::new();
    if let Some(mut reader) = reader {
        let _ = std::io::Read::read_to_end(&mut reader, &mut buffer);
    }
    buffer
}

/// Waits for the child until it exits, its bound runs out, or the
/// supervisor is interrupted.
///
/// The wait is event-driven: [`signals::Waiter`] blocks until a signal
/// arrives, which includes `SIGCHLD` when the child exits, so a waiting
/// run costs no wakeups at all.
fn wait_for_exit<C: ChildProcess>(
    child: &mut C,
    bound: Option<Duration>,
) -> Result<Exit, SandboxError> {
    let deadline = bound.map(Deadline::new);
    let mut waiter = signals::Waiter::new();
    // Only the Unix waiter has an interrupt to forward.
    #[cfg(unix)]
    let mut forwarded = false;
    loop {
        match child.poll() {
            Ok(Some(exit)) => return Ok(exit),
            Ok(None) => {}
            Err(error) => {
                return Err(SandboxError::Spawn(format!(
                    "waiting for the child failed: {error}"
                )));
            }
        }
        if let Some(deadline) = &deadline
            && deadline.passed()
        {
            child.kill_tree();
            return Err(SandboxError::Timeout(deadline.limit));
        }

        match waiter.wait(deadline.as_ref().map(Deadline::remaining)) {
            signals::Wake::ChildChanged | signals::Wake::Deadline => {}
            #[cfg(unix)]
            signals::Wake::Interrupt(signal) => {
                if forwarded {
                    // The operator asked twice. The first ask was the
                    // child's chance to stop on its own terms.
                    child.kill_tree();
                    return Err(SandboxError::Interrupted(signal));
                }
                forwarded = true;
                // The child leads its own session, so a terminal signal
                // never reached it; the supervisor is the only path it
                // has to the interrupt the operator meant for it.
                child.signal_group(signal);
            }
        }
    }
}

/// A wall-clock bound a caller asked for, and the instant it runs out.
struct Deadline {
    limit: Duration,
    at: std::time::Instant,
}

impl Deadline {
    fn new(limit: Duration) -> Self {
        Self {
            limit,
            at: std::time::Instant::now() + limit,
        }
    }

    fn passed(&self) -> bool {
        std::time::Instant::now() >= self.at
    }

    fn remaining(&self) -> Duration {
        self.at.saturating_duration_since(std::time::Instant::now())
    }
}

#[cfg_attr(
    not(unix),
    allow(
        clippy::unnecessary_wraps,
        reason = "the signalled arm exists on unix; the signature is shared"
    )
)]
fn finish(exit: Exit, stdout: Vec<u8>, stderr: Vec<u8>) -> Result<SandboxOutput, SandboxError> {
    match exit {
        #[cfg(unix)]
        Exit::Signal(signal) => Err(SandboxError::Signalled {
            signal,
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
        }),
        Exit::Code(code) => Ok(SandboxOutput {
            code,
            stdout,
            stderr,
        }),
    }
}

/// Sends `signal` to `child`'s process group.
#[cfg(unix)]
pub(crate) fn signal_group(child: &Child, signal: i32) {
    // The child leads its own process group (see the Unix backend's
    // pre-exec `setsid`), so the negated pid names the group.
    #[allow(
        unsafe_code,
        reason = "killpg has no safe wrapper; the argument is a pid this process owns"
    )]
    unsafe {
        libc::kill(-(child.id() as libc::pid_t), signal);
    }
}

/// Kills `child` and every descendant it started.
///
/// The process-group signal reaches everything that stayed in the
/// group. Reaching a descendant that called `setsid` needs a mechanism
/// the group does not have, which is what the Linux backend's cgroup
/// and reparent sweep supply; this is the last-resort form for a host
/// with neither.
#[cfg(unix)]
pub(crate) fn kill_tree(child: &mut Child) {
    #[allow(
        unsafe_code,
        reason = "killpg has no safe wrapper; the argument is a pid this process owns"
    )]
    unsafe {
        libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL);
    }
    let _ = child.kill();
}

/// Kills `child` and every descendant it started.
#[cfg(windows)]
pub(crate) fn kill_tree(child: &mut Child) {
    // The child is assigned to a job object created with
    // `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, so dropping the job handle
    // ends the tree; killing the root as well covers the `basic` level,
    // where no job is created.
    let _ = child.kill();
}

/// Kills `child` and every descendant it started.
#[cfg(not(any(unix, windows)))]
pub(crate) fn kill_tree(child: &mut Child) {
    let _ = child.kill();
}

/// Blocking until something the supervisor cares about happens.
///
/// A supervisor has three things to wait for at once: the child
/// exiting, an operator's interrupt, and a deadline. On Unix all three
/// are signals or a timeout on a signal wait, so the whole wait is one
/// blocking call rather than a loop that wakes up to ask.
#[cfg(unix)]
pub(crate) mod signals {
    #![allow(
        unsafe_code,
        reason = "a signal disposition and a pipe have no safe wrappers; the \
                  handler touches only an atomic and `write`, which is the \
                  async-signal-safety contract"
    )]

    use std::sync::atomic::{AtomicI32, Ordering};
    use std::sync::{Condvar, Mutex, Once, PoisonError};
    use std::time::Duration;

    /// Why a wait ended.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum Wake {
        /// A child of this process changed state.
        ChildChanged,
        /// An operator's interrupt arrived, with the signal number.
        Interrupt(i32),
        /// The caller's deadline ran out.
        Deadline,
    }

    /// Signals a supervisor forwards rather than dies on.
    const FORWARDED: [i32; 4] = [libc::SIGINT, libc::SIGTERM, libc::SIGHUP, libc::SIGQUIT];

    /// What every waiter in this process has to be told about.
    struct Arrivals {
        /// Signals seen so far. A waiter blocks until this moves past
        /// what it last saw, which is what makes a signal that lands
        /// before the wait begins a wake-up rather than a missed one.
        seen: u64,
        /// Interrupts seen so far, counted apart from `seen` so each
        /// waiter forwards one interrupt once.
        interrupts: u64,
        /// The most recent interrupt's number.
        signal: i32,
        /// Whether a dispatcher is still carrying signals here. A
        /// waiter blocks without a deadline only while this holds:
        /// once nothing can wake it, the wait has to be bounded or the
        /// supervisor would never look at its child again.
        dispatching: bool,
    }

    static ARRIVALS: Mutex<Arrivals> = Mutex::new(Arrivals {
        seen: 0,
        interrupts: 0,
        signal: 0,
        dispatching: false,
    });
    static ARRIVED: Condvar = Condvar::new();

    /// The write end of the pipe the handler reports on, or `-1` before
    /// the dispositions are installed and when the process could not
    /// give the supervisor a pipe.
    static REPORT: AtomicI32 = AtomicI32::new(-1);

    /// The longest a waiter blocks when no dispatcher is carrying
    /// signals to it, so a supervisor whose process could not spare a
    /// pipe still re-reads its child rather than waiting on a wake-up
    /// that cannot come.
    const UNDISPATCHED_SLICE: Duration = Duration::from_millis(50);

    /// One byte down the report pipe. Async-signal-safe: an atomic load
    /// and `write`.
    extern "C" fn handler(signal: i32) {
        let fd = REPORT.load(Ordering::SeqCst);
        if fd >= 0 {
            let byte = u8::try_from(signal.clamp(0, 255)).unwrap_or(0);
            unsafe {
                libc::write(fd, std::ptr::from_ref(&byte).cast(), 1);
            }
        }
    }

    /// Installs the dispositions and starts the dispatcher, once per
    /// process.
    ///
    /// `SIGCHLD` is caught rather than left default so a child exiting
    /// is an event the wait can block on; the forwarded signals are
    /// caught so an interrupt reaches the child instead of ending the
    /// supervisor and orphaning it.
    ///
    /// A signal handler may not lock, so what it can do is write a byte
    /// the dispatcher reads on an ordinary thread; the fan-out to every
    /// waiter happens there, under the lock, where it costs nothing to
    /// have as many waiters as the caller has runs.
    fn install() {
        static ONCE: Once = Once::new();
        ONCE.call_once(|| {
            let Some(read) = report_pipe() else {
                return;
            };
            if std::thread::Builder::new()
                .name("gos-sandbox-signals".to_string())
                .spawn(move || dispatch(read))
                .is_err()
            {
                REPORT.store(-1, Ordering::SeqCst);
                unsafe { libc::close(read) };
                return;
            }
            for signal in FORWARDED.iter().copied().chain([libc::SIGCHLD]) {
                let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
                action.sa_sigaction = handler as *const () as usize;
                action.sa_flags = libc::SA_RESTART;
                unsafe {
                    libc::sigemptyset(&raw mut action.sa_mask);
                    libc::sigaction(signal, &raw const action, std::ptr::null_mut());
                }
            }
        });
    }

    /// Creates the pipe the handler reports on and publishes its write
    /// end, answering the read end the dispatcher owns.
    ///
    /// The write end is non-blocking so a handler can never stall the
    /// thread it interrupted, and both ends are close-on-exec so no
    /// child inherits the supervisor's wake-ups.
    fn report_pipe() -> Option<libc::c_int> {
        let mut fds = [-1; 2];
        if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
            return None;
        }
        for fd in fds {
            unsafe {
                libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
            }
        }
        unsafe {
            let flags = libc::fcntl(fds[1], libc::F_GETFL);
            libc::fcntl(fds[1], libc::F_SETFL, flags | libc::O_NONBLOCK);
        }
        REPORT.store(fds[1], Ordering::SeqCst);
        Some(fds[0])
    }

    /// Carries what the handler reported to every waiter.
    fn dispatch(read: libc::c_int) {
        {
            let mut arrivals = ARRIVALS.lock().unwrap_or_else(PoisonError::into_inner);
            arrivals.dispatching = true;
        }
        let mut buffer = [0_u8; 64];
        loop {
            let count = unsafe { libc::read(read, buffer.as_mut_ptr().cast(), buffer.len()) };
            if count < 0 {
                // A signal delivered to this thread ends the read
                // without ending the stream. Any thread may be the one
                // a signal is delivered to, and a handler installed
                // elsewhere in the process need not restart the calls
                // it interrupts, so the report is still open and the
                // next read is the one that carries it.
                if std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                    continue;
                }
            }
            if count <= 0 {
                stop_dispatching();
                return;
            }
            let mut arrivals = ARRIVALS.lock().unwrap_or_else(PoisonError::into_inner);
            arrivals.seen += 1;
            for byte in &buffer[..count as usize] {
                let signal = i32::from(*byte);
                if FORWARDED.contains(&signal) {
                    arrivals.interrupts += 1;
                    arrivals.signal = signal;
                }
            }
            drop(arrivals);
            ARRIVED.notify_all();
        }
    }

    /// The report descriptor as it stands, for a caller that will put
    /// it back.
    #[cfg(test)]
    pub(crate) fn report_descriptor() -> libc::c_int {
        REPORT.load(Ordering::SeqCst)
    }

    /// Puts a stopped dispatcher's report back, so stopping it in a
    /// test leaves the process as it was found.
    #[cfg(test)]
    pub(crate) fn resume_dispatching(report: libc::c_int) {
        REPORT.store(report, Ordering::SeqCst);
        let mut arrivals = ARRIVALS.lock().unwrap_or_else(PoisonError::into_inner);
        arrivals.dispatching = true;
    }

    /// Records that nothing will carry a signal to a waiter any more.
    ///
    /// The stop counts as an arrival. A waiter already blocked with no
    /// deadline is waiting for `seen` to move, and nothing else would
    /// ever move it: releasing every waiter here is what turns a
    /// stopped dispatcher into a slower wait rather than a wait that
    /// never ends.
    pub(crate) fn stop_dispatching() {
        REPORT.store(-1, Ordering::SeqCst);
        let mut arrivals = ARRIVALS.lock().unwrap_or_else(PoisonError::into_inner);
        arrivals.dispatching = false;
        arrivals.seen += 1;
        drop(arrivals);
        ARRIVED.notify_all();
    }

    /// A supervisor's place in the process's signal stream.
    pub(crate) struct Waiter {
        seen: u64,
        interrupts: u64,
    }

    impl Waiter {
        /// Starts watching from whatever has already arrived, so a
        /// signal older than this supervisor is not its business.
        pub(crate) fn new() -> Self {
            install();
            let arrivals = ARRIVALS.lock().unwrap_or_else(PoisonError::into_inner);
            Self {
                seen: arrivals.seen,
                interrupts: arrivals.interrupts,
            }
        }

        /// Blocks until a signal arrives or `timeout` runs out.
        pub(crate) fn wait(&mut self, timeout: Option<Duration>) -> Wake {
            let arrivals = ARRIVALS.lock().unwrap_or_else(PoisonError::into_inner);
            // Read under the lock that changes it, so a dispatcher
            // stopping cannot land between the choice and the wait.
            let limit = match (timeout, arrivals.dispatching) {
                (Some(limit), true) => Some(limit),
                (Some(limit), false) => Some(limit.min(UNDISPATCHED_SLICE)),
                (None, true) => None,
                (None, false) => Some(UNDISPATCHED_SLICE),
            };
            let (arrivals, ran_out) = match limit {
                Some(limit) => {
                    let (arrivals, outcome) = ARRIVED
                        .wait_timeout_while(arrivals, limit, |arrivals| arrivals.seen == self.seen)
                        .unwrap_or_else(PoisonError::into_inner);
                    (arrivals, outcome.timed_out())
                }
                None => (
                    ARRIVED
                        .wait_while(arrivals, |arrivals| arrivals.seen == self.seen)
                        .unwrap_or_else(PoisonError::into_inner),
                    false,
                ),
            };
            self.seen = arrivals.seen;
            let interrupted = arrivals.interrupts > self.interrupts;
            self.interrupts = arrivals.interrupts;
            let signal = arrivals.signal;
            drop(arrivals);
            if interrupted {
                Wake::Interrupt(signal)
            } else if ran_out {
                Wake::Deadline
            } else {
                Wake::ChildChanged
            }
        }
    }
}

/// Blocking until something the supervisor cares about happens.
///
/// Windows has no signal a child can be sent, and a console interrupt
/// already reaches the whole console group, so the supervisor's only
/// job here is the deadline.
#[cfg(not(unix))]
pub(crate) mod signals {
    use std::time::Duration;

    /// Why a wait ended.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum Wake {
        /// A child of this process changed state.
        ChildChanged,
        /// The caller's deadline ran out.
        Deadline,
    }

    /// A deadline-only waiter.
    pub(crate) struct Waiter;

    impl Waiter {
        pub(crate) fn new() -> Self {
            Self
        }

        /// Waits out `timeout`, or a slice of it. A console interrupt
        /// on Windows already reaches every process in the console
        /// group, so there is no signal for the supervisor to forward
        /// and nothing else to block on.
        pub(crate) fn wait(&mut self, timeout: Option<Duration>) -> Wake {
            let slice = Duration::from_millis(20);
            match timeout {
                Some(remaining) if remaining <= slice => {
                    std::thread::sleep(remaining);
                    Wake::Deadline
                }
                _ => {
                    std::thread::sleep(slice);
                    Wake::ChildChanged
                }
            }
        }
    }
}

#[cfg(all(test, unix))]
mod signal_tests {
    use super::signals::{Waiter, Wake};

    /// A supervisor with no deadline waits for the dispatcher to tell
    /// it a signal arrived, so the dispatcher stopping has to release
    /// it. Nothing else ever would: it is waiting for a count that
    /// only the dispatcher moves, and the wait it chose has no
    /// deadline to fall back on.
    #[test]
    fn a_waiter_with_no_deadline_is_released_when_the_dispatcher_stops() {
        let (blocking, blocked) = std::sync::mpsc::channel();
        let waiting = std::thread::spawn(move || {
            let mut waiter = Waiter::new();
            blocking.send(()).expect("announce the wait");
            waiter.wait(None)
        });
        blocked.recv().expect("the waiter reached its wait");

        let report = super::signals::report_descriptor();
        super::signals::stop_dispatching();
        let woken = waiting.join().expect("join the waiter");
        super::signals::resume_dispatching(report);

        assert_eq!(woken, Wake::ChildChanged);
    }
}

#[cfg(test)]
mod exec_tests {
    use super::*;
    use crate::error::EXIT_COMMAND_NOT_FOUND;
    use crate::level::Level;
    use crate::policy::SandboxPolicy;
    use std::path::{Path, PathBuf};

    /// A directory holding one executable file named `name`.
    fn bin_directory(tag: &str, name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gos-exec-resolve-{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create fixture directory");
        let file = dir.join(name);
        std::fs::write(&file, b"#!/bin/sh\nexit 0\n").expect("write fixture command");
        make_executable(&file);
        dir
    }

    #[cfg(unix)]
    fn make_executable(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
            .expect("mark the fixture executable");
    }

    #[cfg(not(unix))]
    fn make_executable(_path: &Path) {}

    /// The name a command carries on this platform, since Windows
    /// resolves an unqualified name through its executable suffixes.
    fn command_name(stem: &str) -> String {
        if cfg!(windows) {
            format!("{stem}.exe")
        } else {
            stem.to_string()
        }
    }

    fn policy_with_path(directory: &Path) -> crate::policy::CompiledPolicy {
        SandboxPolicy::new()
            .level(Level::None)
            .read_write(directory)
            .env_set("PATH", directory.to_string_lossy().into_owned())
            .compile()
            .expect("the policy compiles")
    }

    /// A pipe on disk that a reader blocks on until a writer opens it.
    #[cfg(unix)]
    fn make_gate(path: &Path) {
        let spelling = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())
            .expect("a path with no interior nul");
        #[allow(
            unsafe_code,
            reason = "mkfifo has no safe wrapper; the argument is a C string this \
                      function owns"
        )]
        let made = unsafe { libc::mkfifo(spelling.as_ptr(), 0o600) };
        assert_eq!(made, 0, "create the gate at {}", path.display());
    }

    /// A supervisor blocks until a signal says its child changed, and a
    /// process supervises as many runs at once as its caller starts.
    /// Every one of those waits has to be reachable: a supervisor no
    /// signal can wake never learns its child exited, and a run with no
    /// deadline has nothing else to end it.
    #[cfg(unix)]
    #[test]
    fn every_concurrent_supervisor_is_woken_by_its_own_child() {
        use std::io::Write;

        const RUNS: usize = 16;
        let root = std::env::temp_dir().join("gos-exec-concurrent-supervisors");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create the fixture directory");
        let gates: Vec<PathBuf> = (0..RUNS)
            .map(|index| {
                let gate = root.join(format!("gate-{index}"));
                make_gate(&gate);
                gate
            })
            .collect();

        let sandbox = std::sync::Arc::new(
            crate::Sandbox::new(&SandboxPolicy::new().level(Level::None)).expect("build a sandbox"),
        );
        let runs: Vec<_> = gates
            .iter()
            .enumerate()
            .map(|(index, gate)| {
                let sandbox = std::sync::Arc::clone(&sandbox);
                let script = format!("read _ < {}; printf {index}", gate.display());
                std::thread::spawn(move || {
                    let argv = ["/bin/sh".to_string(), "-c".to_string(), script];
                    sandbox.run_captured(&argv).map(|output| output.stdout)
                })
            })
            .collect();

        // Opening the writing end blocks until the child has the
        // reading end open, so holding every gate open at once is what
        // puts every supervisor on a child that has not exited yet.
        let held: Vec<std::fs::File> = gates
            .iter()
            .map(|gate| {
                std::fs::OpenOptions::new()
                    .write(true)
                    .open(gate)
                    .expect("open a gate")
            })
            .collect();
        for mut gate in held {
            gate.write_all(b"go\n").expect("let a child go");
        }

        for (index, run) in runs.into_iter().enumerate() {
            let stdout = run
                .join()
                .expect("join a supervisor")
                .expect("a run whose child's exit reached its supervisor");
            assert_eq!(stdout, index.to_string().into_bytes());
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_bare_name_resolves_through_the_path_the_child_is_given() {
        let dir = bin_directory("bare-name", &command_name("tool"));
        let policy = policy_with_path(&dir);
        let resolved =
            resolve_program(&policy, "tool").expect("the command is on the child's PATH");
        assert_eq!(resolved, dir.join(command_name("tool")));
    }

    /// The whole point of the resolution: a command that does not exist
    /// is that failure class and not any other, whatever the backend
    /// would have launched.
    #[test]
    fn a_command_that_is_on_no_path_is_not_found() {
        let dir = bin_directory("missing", &command_name("tool"));
        let policy = policy_with_path(&dir);
        let error = resolve_program(&policy, "gossamer-sandbox-no-such-command-7c1e")
            .expect_err("a command on no PATH does not resolve");
        assert_eq!(error.exit_code(), EXIT_COMMAND_NOT_FOUND, "{error}");
        assert!(
            error
                .to_string()
                .contains("gossamer-sandbox-no-such-command"),
            "{error}"
        );
    }

    /// A command spelled with a separator names a file rather than a
    /// search, and a relative one names it from where the child starts.
    #[test]
    fn a_path_shaped_command_is_resolved_against_the_working_directory() {
        let dir = bin_directory("relative", &command_name("tool"));
        let policy = SandboxPolicy::new()
            .level(Level::None)
            .read_write(&dir)
            .working_directory(&dir)
            .compile()
            .expect("the policy compiles");
        let spelled = format!(".{}{}", std::path::MAIN_SEPARATOR, command_name("tool"));
        let resolved = resolve_program(&policy, &spelled).expect("a relative command resolves");
        // Compared as the file it names rather than as text: the
        // policy's working directory is canonicalized, which on macOS
        // prefixes `/private` and on Windows `\\?\`, and the spelling
        // keeps the `.` component the caller wrote.
        assert_eq!(
            resolved.canonicalize().ok(),
            dir.join(command_name("tool")).canonicalize().ok(),
            "resolved {} should name the fixture command",
            resolved.display(),
        );
        let error = resolve_program(&policy, "./gossamer-sandbox-no-such-file")
            .expect_err("a relative command that is not there does not resolve");
        assert_eq!(error.exit_code(), EXIT_COMMAND_NOT_FOUND, "{error}");
    }

    /// A policy that passes no `PATH` still launches the commands the
    /// exec family would find, so the resolution cannot be stricter
    /// than the launch it stands in for.
    #[test]
    fn a_policy_with_no_path_falls_back_to_the_default_search_path() {
        let policy = SandboxPolicy::new()
            .level(Level::None)
            .compile()
            .expect("the policy compiles");
        assert!(!policy.environment().contains_key("PATH"));
        let name = if cfg!(windows) { "cmd" } else { "sh" };
        assert!(
            resolve_program(&policy, name).is_ok(),
            "{name} is in the default search path"
        );
    }
}
