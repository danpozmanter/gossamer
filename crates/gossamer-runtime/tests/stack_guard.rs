//! End-to-end check for the stack-overflow guard.
//!
//! Strategy: the test binary re-execs itself with a small thread
//! stack and an env-var trigger. The child runs unbounded
//! recursion; the test asserts the child exits non-zero with a
//! stack-overflow line on stderr.
//!
//! This shape is portable (no `cargo_bin`, no extra fixture
//! binary) because integration-test binaries are themselves real
//! executables that Cargo links and can run again as a
//! subprocess.

#![allow(missing_docs)]

use std::env;
use std::process::{Command, Stdio};
use std::time::Duration;

const TRIGGER_ENV: &str = "GOS_STACK_GUARD_TRIGGER_RECURSE";

#[test]
#[cfg(unix)]
fn unbounded_recursion_reports_stack_overflow() {
    // Child mode: install the guard and recurse until the stack
    // is exhausted.
    if env::var(TRIGGER_ENV).is_ok() {
        gossamer_runtime::stack_guard::install_stack_guard();
        // Use a small thread stack so the overflow happens fast
        // and uses a constrained amount of memory.
        let handle = std::thread::Builder::new()
            .name("recurser".to_string())
            .stack_size(128 * 1024)
            .spawn(|| {
                gossamer_runtime::stack_guard::install_stack_guard();
                let _ = recurse(0);
            })
            .expect("spawn recurser");
        let _ = handle.join();
        // If the child reaches here the guard didn't fire; exit
        // cleanly so the parent sees a normal status and the
        // assertion below fails with a clear message.
        std::process::exit(0);
    }

    // Parent mode: re-exec the test binary with the trigger env
    // and capture stderr.
    let exe = env::current_exe().expect("current_exe");
    let output = Command::new(&exe)
        .env(TRIGGER_ENV, "1")
        // Run only the trigger test in the child to avoid running
        // every test recursively.
        .args(["--exact", "unbounded_recursion_reports_stack_overflow"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("spawn child");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "child should have aborted; got exit status {:?}\nstderr: {stderr}",
        output.status,
    );
    assert!(
        stderr.contains("stack overflow"),
        "child stderr should mention stack overflow; got:\n{stderr}",
    );
    // The Unix abort path writes through SIGABRT (signal 6) or
    // exits with 134; either way the status is non-success.
    let _ = Duration::from_secs(1);
}

#[inline(never)]
#[allow(unconditional_recursion)]
fn recurse(depth: usize) -> usize {
    // A small stack-resident array keeps every frame meaningful
    // even after the optimiser inlines neighbour calls. The
    // `black_box` keeps the compiler from converting this into a
    // tail call. The recursion is genuinely unbounded — the
    // function exists to exhaust the OS stack.
    let buf = [depth; 64];
    std::hint::black_box(&buf);
    recurse(depth + 1) + buf[0]
}

#[test]
fn install_on_main_thread_is_idempotent() {
    gossamer_runtime::stack_guard::install_stack_guard();
    gossamer_runtime::stack_guard::install_stack_guard();
}
