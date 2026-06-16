//! Runtime edge-case battery. Pins behaviour on programs that
//! exercise corners the runtime has to handle cleanly: NaN
//! propagation, channel double-close, and stack-overflow
//! diagnostics. Each test runs the example through the bytecode VM
//! and asserts the observed exit code / output.

#![allow(missing_docs)]

use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

fn gos_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_gos"))
}

fn workspace_root() -> PathBuf {
    let mut here = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    here.pop();
    here.pop();
    here
}

fn run_with_timeout(mut cmd: Command, timeout: Duration) -> (String, String, Option<i32>) {
    let mut child = cmd
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn");
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("subprocess did not terminate within {timeout:?}");
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => panic!("wait error: {e}"),
        }
    }
    let out = child.wait_with_output().expect("wait_with_output");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code(),
    )
}

fn run_example_vm(name: &str) -> (String, String, Option<i32>) {
    let src = workspace_root().join("examples").join(name);
    let mut cmd = Command::new(gos_bin());
    cmd.arg("run").arg(&src);
    run_with_timeout(cmd, Duration::from_secs(30))
}

#[test]
fn nan_division_propagates_and_is_not_self_equal() {
    let (stdout, _stderr, code) = run_example_vm("edge_nan_propagation.gos");
    assert_eq!(code, Some(0), "stdout={stdout:?}");
    let lower = stdout.to_ascii_lowercase();
    assert!(
        lower.contains("nan"),
        "expected NaN on stdout, got: {stdout:?}",
    );
    assert!(
        lower.contains("false"),
        "NaN must compare unequal to itself, got: {stdout:?}",
    );
}

#[test]
fn double_close_channel_panics_with_clear_message() {
    let (_stdout, stderr, code) = run_example_vm("edge_double_close.gos");
    assert_ne!(code, Some(0), "double-close must not exit clean");
    let lower = stderr.to_ascii_lowercase();
    assert!(
        lower.contains("close") || lower.contains("panic"),
        "expected close/panic diagnostic on stderr, got: {stderr:?}",
    );
}

#[test]
fn stack_overflow_diagnostic_or_clean_abort() {
    // Use a hard wall-clock cap - unbounded recursion must either
    // produce a stack-overflow diagnostic and exit, or be aborted
    // by the OS within the timeout. Silently looping is the
    // failure mode we're pinning against.
    let src = workspace_root()
        .join("examples")
        .join("edge_stack_overflow.gos");
    let mut child = Command::new(gos_bin())
        .arg("run")
        .arg(&src)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn gos run");
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("stack-overflow program did not terminate within 10s");
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => panic!("wait err: {e}"),
        }
    }
    let status = child.wait().expect("wait");
    assert!(
        !status.success(),
        "stack-overflow program must not exit cleanly: {status:?}",
    );
}
