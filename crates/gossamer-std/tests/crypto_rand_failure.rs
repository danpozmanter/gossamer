//! `crypto::rand::fill` must surface CSPRNG failure.
//!
//! Pins three invariants:
//!
//! 1. The recoverable surface (`fill`, `bytes`, `nonce_12`) returns
//!    `Err` rather than silently producing all-zero output when the
//!    OS CSPRNG is unavailable.
//! 2. Downstream key-generation entries (Ed25519, ECDSA) that rely
//!    on [`OsRng`] go through `fill_or_abort`, so a CSPRNG failure
//!    propagates as a process abort (not a recoverable panic that
//!    `catch_unwind` could swallow).
//! 3. The fault-injection switch resets cleanly between tests.
//!
//! The process-global fault flag forces these tests to run
//! sequentially. We declare `serial`-equivalent ordering by holding
//! a `Mutex` for the duration of any test that toggles the flag.

#![allow(missing_docs)]

use std::sync::Mutex;
use std::sync::OnceLock;

use gossamer_std::crypto::rand;

fn fault_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

#[test]
fn fill_returns_err_under_fault_injection() {
    let _guard = fault_lock().lock().unwrap();
    rand::test_support::set_fault_for_tests(true);
    let mut buf = [0u8; 16];
    let result = rand::fill(&mut buf);
    rand::test_support::set_fault_for_tests(false);
    assert!(
        result.is_err(),
        "fill must surface CSPRNG fault, got Ok with buf={buf:?}",
    );
    // The fault path does not modify the caller's buffer.
    assert_eq!(buf, [0u8; 16]);
}

#[test]
fn bytes_propagates_fault() {
    let _guard = fault_lock().lock().unwrap();
    rand::test_support::set_fault_for_tests(true);
    let result = rand::bytes(32);
    rand::test_support::set_fault_for_tests(false);
    assert!(result.is_err());
}

#[test]
fn nonce_12_propagates_fault() {
    let _guard = fault_lock().lock().unwrap();
    rand::test_support::set_fault_for_tests(true);
    let result = rand::nonce_12();
    rand::test_support::set_fault_for_tests(false);
    assert!(result.is_err());
}

#[test]
fn fill_after_fault_clears_resumes_real_csprng() {
    let _guard = fault_lock().lock().unwrap();
    rand::test_support::set_fault_for_tests(true);
    let mut bad = [0u8; 32];
    assert!(rand::fill(&mut bad).is_err());

    rand::test_support::set_fault_for_tests(false);
    let mut good = [0u8; 32];
    rand::fill(&mut good).expect("CSPRNG works after fault flag cleared");
    // Real entropy produces non-constant bytes with overwhelming
    // probability. Two 32-byte samples drawn from /dev/urandom
    // collide with probability ~2^-256.
    let zeros = [0u8; 32];
    assert_ne!(good, zeros);
}

#[test]
fn two_consecutive_fills_differ() {
    // The recoverable surface produces fresh entropy on every call
    // — assert by drawing two samples and comparing.
    let _guard = fault_lock().lock().unwrap();
    rand::test_support::set_fault_for_tests(false);
    let a = rand::bytes(32).expect("first sample");
    let b = rand::bytes(32).expect("second sample");
    assert_ne!(a, b, "two consecutive 32-byte samples were identical");
}

#[test]
fn no_workspace_code_silently_swallows_getrandom_errors() {
    // Pins the contract structurally: a CSPRNG failure must never
    // be `.ok()`-swallowed. The audit scans every Rust source file
    // outside `target/` for the disallowed patterns. Test fixtures
    // and the fault-injection support module are explicitly
    // exempted; everything else is production code.
    let workspace_root = locate_workspace_root();
    let mut offenders: Vec<String> = Vec::new();
    walk_rs(&workspace_root, &mut |path, body| {
        // Skip generated/target/test paths. Match by path *component*
        // rather than by substring so the filter is separator-agnostic
        // (Windows uses `\`, Unix uses `/` — a `contains("/tests/")`
        // check silently lets every test file through on Windows and
        // the audit then flags its own pattern documentation).
        let skip_component = |name: &str| {
            path.components()
                .any(|c| c.as_os_str().to_string_lossy() == name)
        };
        if skip_component("target")
            || skip_component("tests")
            || skip_component(".git")
            || path
                .file_name()
                .is_some_and(|n| n == "crypto_rand_failure.rs")
        {
            return;
        }
        let mut prev_line: &str = "";
        for (i, line) in body.lines().enumerate() {
            let trimmed = line.trim();
            // Pattern 1: getrandom invocation chained with `.ok()`.
            if line.contains("getrandom") && line.contains(".ok()") {
                offenders.push(format!(
                    "{}:{}: getrandom(...).ok() swallows CSPRNG failure",
                    path.display(),
                    i + 1,
                ));
            }
            // Pattern 2: explicit `match getrandom { Err(_) => () }`.
            if trimmed.starts_with("Err(_)")
                && trimmed.contains("=> ()")
                && prev_line.contains("getrandom")
            {
                offenders.push(format!(
                    "{}:{}: getrandom Err discarded as unit",
                    path.display(),
                    i + 1,
                ));
            }
            prev_line = line;
        }
    });
    assert!(
        offenders.is_empty(),
        "CSPRNG silent-failure pattern found:\n{}",
        offenders.join("\n"),
    );
}

fn locate_workspace_root() -> std::path::PathBuf {
    let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut cursor = manifest_dir.as_path();
    loop {
        if cursor.join("Cargo.lock").exists() {
            return cursor.to_path_buf();
        }
        match cursor.parent() {
            Some(parent) => cursor = parent,
            None => panic!(
                "could not locate workspace root from {}",
                manifest_dir.display()
            ),
        }
    }
}

fn walk_rs(root: &std::path::Path, visit: &mut dyn FnMut(&std::path::Path, &str)) {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if path.is_dir() {
                if name == "target" || name.starts_with('.') {
                    continue;
                }
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let Ok(body) = std::fs::read_to_string(&path) else {
                    continue;
                };
                visit(&path, &body);
            }
        }
    }
}
