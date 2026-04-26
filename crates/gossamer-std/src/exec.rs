//! Runtime support for `std::os::exec`.
//!
//! Wraps `std::process::Command` in the surface Gossamer programs
//! see. The user-facing API is a `Command` builder that mirrors
//! Go's `os/exec` package:
//!
//! - `Command::new(prog)` constructs a builder with no args.
//! - `cmd.arg(s)` / `cmd.args(xs)` append positional arguments.
//! - `cmd.env(k, v)` overrides an environment variable; `cmd.envs`
//!   merges a map.
//! - `cmd.cwd(path)` sets the working directory.
//! - `cmd.stdin(stdin)` / `cmd.stdout(...)` / `cmd.stderr(...)` wire
//!   I/O streams. `Stdio::piped()` captures into a `Vec<u8>`;
//!   `Stdio::inherit()` is the default.
//! - `cmd.output()` runs the child to completion and returns a
//!   captured `Output { status, stdout, stderr }`.
//! - `cmd.status()` runs to completion and returns the `ExitStatus`.
//! - `cmd.spawn()` returns a `Child` handle that the caller can
//!   wait on later.
//!
//! Goroutine semantics: `output()` and `status()` block the calling
//! goroutine on `wait`; the runtime scheduler releases the OS thread
//! while waiting. `spawn()` does not block.

#![forbid(unsafe_code)]
#![allow(clippy::needless_pass_by_value)]

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::{self};

use crate::io::IoError;

/// Spec for spawning a child process. Built up via the builder
/// methods, then executed via `output`, `status`, or `spawn`.
#[derive(Debug, Clone)]
pub struct Command {
    program: String,
    args: Vec<String>,
    envs: HashMap<String, String>,
    env_clear: bool,
    cwd: Option<PathBuf>,
    stdin: Stdio,
    stdout: Stdio,
    stderr: Stdio,
}

/// What to wire a child's stdin/stdout/stderr to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stdio {
    /// Inherit from the parent process. Default.
    Inherit,
    /// Capture into a buffer (only meaningful for stdout/stderr) or
    /// supply an empty buffer (stdin).
    Piped,
    /// Discard / source `/dev/null`-equivalent.
    Null,
}

/// Captured output from a finished child.
#[derive(Debug, Clone)]
pub struct Output {
    /// Exit status of the child.
    pub status: ExitStatus,
    /// Captured bytes from the child's stdout. Empty unless stdout
    /// was set to `Stdio::Piped`.
    pub stdout: Vec<u8>,
    /// Captured bytes from the child's stderr. Empty unless stderr
    /// was set to `Stdio::Piped`.
    pub stderr: Vec<u8>,
}

/// Exit status of a finished child.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExitStatus {
    code: Option<i32>,
}

impl ExitStatus {
    /// Numeric exit code, or `None` if the child was killed by a
    /// signal (Unix) or terminated abnormally (Windows).
    #[must_use]
    pub const fn code(self) -> Option<i32> {
        self.code
    }

    /// Returns whether the child exited with code 0.
    #[must_use]
    pub fn success(self) -> bool {
        self.code == Some(0)
    }
}

/// Handle to a running child process.
#[derive(Debug)]
pub struct Child {
    inner: process::Child,
}

impl Child {
    /// Blocks until the child exits; returns its [`ExitStatus`].
    pub fn wait(mut self) -> Result<ExitStatus, IoError> {
        let status = self
            .inner
            .wait()
            .map_err(|e| IoError::from_std(e, "wait"))?;
        Ok(ExitStatus {
            code: status.code(),
        })
    }

    /// Sends SIGKILL (Unix) / `TerminateProcess` (Windows). The
    /// caller must still `wait` on the child afterwards to reap it.
    pub fn kill(&mut self) -> Result<(), IoError> {
        self.inner.kill().map_err(|e| IoError::from_std(e, "kill"))
    }

    /// Returns the child's PID.
    #[must_use]
    pub fn pid(&self) -> u32 {
        self.inner.id()
    }
}

impl Command {
    /// Constructs a builder for `program`. Args, env, and cwd default
    /// to inherited.
    #[must_use]
    pub fn new<S: Into<String>>(program: S) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            envs: HashMap::new(),
            env_clear: false,
            cwd: None,
            stdin: Stdio::Inherit,
            stdout: Stdio::Inherit,
            stderr: Stdio::Inherit,
        }
    }

    /// Appends a positional argument.
    #[must_use]
    pub fn arg<S: Into<String>>(mut self, arg: S) -> Self {
        self.args.push(arg.into());
        self
    }

    /// Appends every entry of `args` in order.
    #[must_use]
    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    /// Sets an environment variable in the child. Stacks; the last
    /// `env` for a given key wins.
    #[must_use]
    pub fn env<K: Into<String>, V: Into<String>>(mut self, key: K, value: V) -> Self {
        self.envs.insert(key.into(), value.into());
        self
    }

    /// Wipes the parent's environment for the child; only entries
    /// supplied via [`Self::env`] are passed through.
    #[must_use]
    pub fn env_clear(mut self) -> Self {
        self.env_clear = true;
        self
    }

    /// Sets the child's working directory.
    #[must_use]
    pub fn cwd<P: Into<PathBuf>>(mut self, dir: P) -> Self {
        self.cwd = Some(dir.into());
        self
    }

    /// Wires the child's stdin.
    #[must_use]
    pub const fn stdin(mut self, stdio: Stdio) -> Self {
        self.stdin = stdio;
        self
    }

    /// Wires the child's stdout.
    #[must_use]
    pub const fn stdout(mut self, stdio: Stdio) -> Self {
        self.stdout = stdio;
        self
    }

    /// Wires the child's stderr.
    #[must_use]
    pub const fn stderr(mut self, stdio: Stdio) -> Self {
        self.stderr = stdio;
        self
    }

    fn build(&self) -> process::Command {
        let mut cmd = process::Command::new(&self.program);
        cmd.args(&self.args);
        if self.env_clear {
            cmd.env_clear();
        }
        for (k, v) in &self.envs {
            cmd.env(k, v);
        }
        if let Some(cwd) = &self.cwd {
            cmd.current_dir(cwd);
        }
        cmd.stdin(map_stdio(self.stdin));
        cmd.stdout(map_stdio(self.stdout));
        cmd.stderr(map_stdio(self.stderr));
        cmd
    }

    /// Runs the child to completion and returns its captured output.
    pub fn output(&self) -> Result<Output, IoError> {
        let raw = self
            .build()
            .output()
            .map_err(|e| IoError::from_std(e, &self.program))?;
        Ok(Output {
            status: ExitStatus {
                code: raw.status.code(),
            },
            stdout: raw.stdout,
            stderr: raw.stderr,
        })
    }

    /// Runs the child to completion and returns just its exit status.
    /// Stdin/stdout/stderr are inherited unless overridden.
    pub fn status(&self) -> Result<ExitStatus, IoError> {
        let raw = self
            .build()
            .status()
            .map_err(|e| IoError::from_std(e, &self.program))?;
        Ok(ExitStatus { code: raw.code() })
    }

    /// Starts the child and returns a [`Child`] handle without
    /// waiting.
    pub fn spawn(&self) -> Result<Child, IoError> {
        let raw = self
            .build()
            .spawn()
            .map_err(|e| IoError::from_std(e, &self.program))?;
        Ok(Child { inner: raw })
    }
}

fn map_stdio(s: Stdio) -> process::Stdio {
    match s {
        Stdio::Inherit => process::Stdio::inherit(),
        Stdio::Piped => process::Stdio::piped(),
        Stdio::Null => process::Stdio::null(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn echo_program() -> &'static str {
        if cfg!(target_os = "windows") {
            "cmd"
        } else {
            "sh"
        }
    }

    fn echo_args(text: &str) -> Vec<String> {
        if cfg!(target_os = "windows") {
            vec!["/C".to_string(), format!("echo {text}")]
        } else {
            vec!["-c".to_string(), format!("printf '%s' {text}")]
        }
    }

    #[test]
    fn output_captures_stdout() {
        let cmd = Command::new(echo_program())
            .args(echo_args("hello"))
            .stdout(Stdio::Piped)
            .stderr(Stdio::Piped);
        let out = cmd.output().expect("output");
        assert!(out.status.success(), "exit code: {:?}", out.status.code());
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(text.contains("hello"), "stdout was: {text:?}");
    }

    #[test]
    fn status_reports_zero_for_success() {
        let cmd = Command::new(echo_program())
            .args(echo_args(""))
            .stdout(Stdio::Null)
            .stderr(Stdio::Null);
        let status = cmd.status().expect("status");
        assert!(status.success());
        assert_eq!(status.code(), Some(0));
    }

    #[test]
    fn nonzero_exit_is_not_a_rust_error() {
        let cmd = Command::new(echo_program())
            .args({
                if cfg!(target_os = "windows") {
                    vec!["/C".to_string(), "exit 7".to_string()]
                } else {
                    vec!["-c".to_string(), "exit 7".to_string()]
                }
            })
            .stdout(Stdio::Null)
            .stderr(Stdio::Null);
        let status = cmd.status().expect("status");
        assert!(!status.success());
        assert_eq!(status.code(), Some(7));
    }

    #[test]
    fn env_overrides_propagate_to_child() {
        if cfg!(target_os = "windows") {
            return;
        }
        let cmd = Command::new("sh")
            .args(["-c", "printf '%s' \"$GOSSAMER_TEST_VAR\""].map(String::from))
            .env("GOSSAMER_TEST_VAR", "value123")
            .stdout(Stdio::Piped)
            .stderr(Stdio::Piped);
        let out = cmd.output().expect("output");
        assert_eq!(String::from_utf8_lossy(&out.stdout), "value123");
    }

    #[test]
    fn missing_program_returns_io_error_not_panic() {
        let cmd = Command::new("definitely-not-a-real-binary-xyzzy-zorch");
        match cmd.output() {
            Ok(_) => panic!("should have failed"),
            Err(IoError::NotFound(_) | IoError::Other(_)) => {}
            Err(e) => panic!("unexpected error kind: {e}"),
        }
    }

    #[test]
    fn spawn_kill_wait_round_trip() {
        if cfg!(target_os = "windows") {
            return;
        }
        let mut child = Command::new("sleep")
            .arg("60")
            .stdout(Stdio::Null)
            .spawn()
            .expect("spawn");
        let pid = child.pid();
        assert!(pid > 0);
        child.kill().expect("kill");
        let status = child.wait().expect("wait");
        // Killed by signal: exit code is None on Unix.
        assert!(!status.success());
    }
}
