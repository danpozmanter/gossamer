//! The fault handler names the JIT body that was running when a hard
//! fault (a non-stack-overflow SIGSEGV / Windows access violation)
//! strikes. A child process arms the guard, sets the breadcrumb, and
//! dereferences an unmapped address; the parent asserts the breadcrumb
//! line reached the child's stderr.

use std::process::Command;

const CHILD_ENV: &str = "GOS_BREADCRUMB_CRASH_CHILD";
const BODY_NAME: &str = "deliberately_faulting_body";

#[test]
fn fault_handler_names_the_running_jit_body() {
    if std::env::var(CHILD_ENV).is_ok() {
        run_as_crashing_child();
    }

    let exe = std::env::current_exe().expect("current_exe");
    let output = Command::new(exe)
        .args([
            "--exact",
            "fault_handler_names_the_running_jit_body",
            "--nocapture",
        ])
        .env(CHILD_ENV, "1")
        .output()
        .expect("spawn crashing child");

    assert!(
        !output.status.success(),
        "child was expected to crash, but exited cleanly"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    let expected = format!("fault inside JIT-compiled body '{BODY_NAME}'");
    assert!(
        stderr.contains(&expected),
        "breadcrumb missing from child stderr.\nexpected substring: {expected}\nstderr was:\n{stderr}"
    );
}

fn run_as_crashing_child() -> ! {
    gossamer_runtime::stack_guard::install_stack_guard();
    gossamer_runtime::stack_guard::set_jit_breadcrumb(BODY_NAME);
    // An unmapped, non-null, non-stack address: faults as a plain access
    // violation (not stack overflow), the path that prints the breadcrumb.
    let bad = 0xdead_0000_usize as *const u8;
    // SAFETY: intentional fault to exercise the handler; this read never
    // returns - the process is killed by the re-raised signal.
    let _ = unsafe { std::ptr::read_volatile(bad) };
    std::process::exit(0);
}
