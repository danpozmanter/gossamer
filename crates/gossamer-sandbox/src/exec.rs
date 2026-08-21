//! Process lifecycle: spawn, wait, time out, and tear the tree down.
//!
//! The parts that are the same on every OS live here; a backend
//! contributes only the enforcement it applies between spawn and exec.

use std::process::{Child, Command, Stdio as ProcessStdio};
use std::time::Duration;

use crate::error::{SandboxError, SandboxOutput};
use crate::policy::CompiledPolicy;

/// A running child the supervisor can wait on, poll, and kill.
///
/// `std::process::Child` implements it; so does the Windows backend's
/// own child, which exists because a restricted token has to be
/// attached at creation and `Command` cannot express that.
pub(crate) trait ChildProcess {
    /// The child's exit, once it has one.
    fn poll(&mut self) -> std::io::Result<Option<Exit>>;
    /// Blocks until the child exits.
    fn wait(&mut self) -> std::io::Result<Exit>;
    /// Kills the child and every descendant it started.
    fn kill_tree(&mut self);
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
        Ok(self.try_wait()?.map(exit_of))
    }

    fn wait(&mut self) -> std::io::Result<Exit> {
        Ok(exit_of(Child::wait(self)?))
    }

    fn kill_tree(&mut self) {
        kill_tree(self);
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
    /// Pipes the sandbox drains and hands back in [`SandboxOutput`].
    Capture,
    /// Discarded.
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
    command.stdin(ProcessStdio::null());
    command.stdout(stdio.to_process_stdio());
    command.stderr(stdio.to_process_stdio());
    Ok(command)
}

/// Waits for `child`, bounded by the policy's timeout, and tears the
/// whole tree down when it runs out or when the policy says to.
pub(crate) fn wait_for<C: ChildProcess>(
    policy: &CompiledPolicy,
    mut child: C,
    stdio: Stdio,
) -> Result<SandboxOutput, SandboxError> {
    if stdio == Stdio::Capture {
        // Drain on threads: a child that fills one pipe's buffer while
        // the sandbox reads the other blocks forever otherwise.
        let out = child.take_stdout();
        let err = child.take_stderr();
        let out_reader = std::thread::spawn(move || read_all(out));
        let err_reader = std::thread::spawn(move || read_all(err));
        let exit = wait_bounded(policy, &mut child)?;
        let stdout = out_reader.join().unwrap_or_default();
        let stderr = err_reader.join().unwrap_or_default();
        return finish(exit, stdout, stderr);
    }
    let exit = wait_bounded(policy, &mut child)?;
    finish(exit, Vec::new(), Vec::new())
}

fn read_all(reader: Option<Box<dyn std::io::Read + Send>>) -> Vec<u8> {
    let mut buffer = Vec::new();
    if let Some(mut reader) = reader {
        let _ = std::io::Read::read_to_end(&mut reader, &mut buffer);
    }
    buffer
}

fn wait_bounded<C: ChildProcess>(
    policy: &CompiledPolicy,
    child: &mut C,
) -> Result<Exit, SandboxError> {
    let Some(limit) = policy.resources.timeout else {
        return child.wait().map_err(|error| {
            SandboxError::Spawn(format!("waiting for the child failed: {error}"))
        });
    };
    let deadline = std::time::Instant::now() + limit;
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
        if std::time::Instant::now() >= deadline {
            child.kill_tree();
            let _ = child.wait();
            return Err(SandboxError::Timeout(limit));
        }
        // The wait is bounded by the caller's own timeout, so the
        // granularity only decides how long past the deadline the tree
        // survives.
        std::thread::sleep(Duration::from_millis(10));
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

/// Kills `child` and every descendant it started.
#[cfg(unix)]
pub(crate) fn kill_tree(child: &mut Child) {
    // The child leads its own process group (see the Unix backend's
    // pre-exec `setsid`), so one signal to the negated group id reaches
    // every descendant that has not left the group.
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
