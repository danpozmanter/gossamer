//! Subprocess runner for the exec-family tools. Spawning the `gos`
//! binary keeps user-program panics and `process::exit` out of the
//! server process.
//!
//! The program `execute` runs is written by a model, so it runs under a
//! policy rather than with the server's own privileges: the working
//! directory and the package cache are writable, the toolchain and the
//! system are readable, credentials are denied, and the network is
//! closed. Every other tool here drives the toolchain over a project
//! the caller pointed at, and runs as the caller does.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use gossamer_sandbox::{Network, Sandbox, SandboxPolicy, Stdio as SandboxStdio};

/// Per-stream capture cap; a chatty program must not flood the client.
const STREAM_CAP: usize = 256 * 1024;

/// Result of one subprocess run.
pub struct ExecOutcome {
    /// Process exit code; `None` when killed by a signal.
    pub exit_code: Option<i64>,
    /// Captured stdout, truncated at 256 KiB.
    pub stdout: String,
    /// Captured stderr, truncated at 256 KiB.
    pub stderr: String,
    /// True when the timeout expired and the process was killed.
    pub timed_out: bool,
}

/// Runs `exe args...` with stdin closed, bounded by `timeout`.
pub(crate) fn run_gos(
    exe: &Path,
    args: &[String],
    timeout: Duration,
) -> Result<ExecOutcome, String> {
    let child = Command::new(exe)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawning {}: {e}", exe.display()))?;
    let pid = child.id();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });
    let (output, timed_out) = match rx.recv_timeout(timeout) {
        Ok(result) => (result, false),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            kill_pid(pid);
            // The child cannot outlive the kill and its pid cannot be
            // reused before the waiter reaps it, so this recv returns.
            (rx.recv().map_err(|e| format!("reaping child: {e}"))?, true)
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            return Err("waiter thread exited without a result".to_string());
        }
    };
    let output = output.map_err(|e| format!("waiting for {}: {e}", exe.display()))?;
    Ok(ExecOutcome {
        exit_code: output.status.code().map(i64::from),
        stdout: truncate(&output.stdout),
        stderr: truncate(&output.stderr),
        timed_out,
    })
}

/// Runs `exe args...` under a policy, bounded by `timeout`.
///
/// The program is model-written, so what it may reach is stated rather
/// than inherited. A host with no sandbox backend answers `none` for
/// its maximum level, and the run is then exactly what [`run_gos`]
/// does - the tool keeps working, without claiming an enforcement the
/// host cannot deliver.
pub(crate) fn run_gos_sandboxed(
    exe: &Path,
    args: &[String],
    timeout: Duration,
    source: Option<&Path>,
) -> Result<ExecOutcome, String> {
    let policy = execute_policy(exe, source);
    let Ok(sandbox) = Sandbox::new(&policy) else {
        // A policy this host cannot compile is not a reason to refuse
        // the tool call; it is a reason to run it the way the tool ran
        // it before there was a policy at all.
        return run_gos(exe, args, timeout);
    };
    let mut command = vec![exe.to_string_lossy().into_owned()];
    command.extend(args.iter().cloned());
    match sandbox.run_bounded(&command, SandboxStdio::Capture, timeout) {
        Ok(output) => Ok(ExecOutcome {
            exit_code: Some(i64::from(output.code)),
            stdout: truncate(&output.stdout),
            stderr: truncate(&output.stderr),
            timed_out: false,
        }),
        Err(gossamer_sandbox::SandboxError::Timeout(_)) => Ok(ExecOutcome {
            exit_code: None,
            stdout: String::new(),
            stderr: String::new(),
            timed_out: true,
        }),
        Err(error) => Err(format!("sandboxed run failed: {error}")),
    }
}

/// What a model-written program may reach.
///
/// The grants are the ones a run genuinely needs: the working directory
/// it was started in, the package cache `gos run` resolves dependencies
/// through, the toolchain binary itself, and - read-only - the directory
/// holding the source, which for an inline program is the server's own
/// temp directory. Credential paths, the network, and a writable temp
/// are denied by [`SandboxPolicy::command_default`], which gives the run
/// a private temp of its own.
fn execute_policy(exe: &Path, source: Option<&Path>) -> SandboxPolicy {
    let working = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut policy = SandboxPolicy::command_default(&working)
        .network(Network::None)
        .level(gossamer_sandbox::capabilities().max_level);
    for cache in package_caches() {
        if cache.is_dir() {
            policy = policy.read_write(cache);
        }
    }
    if let Some(directory) = exe.parent().filter(|path| path.is_dir()) {
        policy = policy.read_only(directory);
    }
    if let Some(directory) = source.and_then(Path::parent).filter(|path| path.is_dir()) {
        policy = policy.read_only(directory);
    }
    policy
}

/// Where `gos run` keeps fetched packages and its build cache.
fn package_caches() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(dir) = std::env::var_os("GOS_CACHE_DIR") {
        roots.push(PathBuf::from(dir));
    }
    if let Some(home) = gossamer_sandbox::home_directory() {
        roots.push(home.join(".gossamer"));
    }
    roots
}

/// Force-kills `pid` via the platform's standard process-control
/// utility, keeping this crate dependency free and unsafe free. The
/// utility's streams are detached: the server's inherited stdout IS
/// the JSON-RPC transport, and these tools report their outcome to
/// stdout/stderr.
#[cfg(unix)]
fn kill_pid(pid: u32) {
    let _ = Command::new("kill")
        .args(["-9", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// `/T` fells the whole child tree (`gos build` spawns llc / cc).
#[cfg(windows)]
fn kill_pid(pid: u32) {
    let _ = Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

fn truncate(bytes: &[u8]) -> String {
    let total = bytes.len();
    let mut text = String::from_utf8_lossy(&bytes[..total.min(STREAM_CAP)]).into_owned();
    if total > STREAM_CAP {
        text.push_str(&format!("\n[truncated {} bytes]", total - STREAM_CAP));
    }
    text
}
