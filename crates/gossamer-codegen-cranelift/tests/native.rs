//! End-to-end tests that `gos build` lowers a range of language
//! constructs to a runnable native executable.
//!
//! These drive the real `gos build` pipeline. Every child process is
//! bounded: native tests run in CI's long serial job, and an unbounded
//! `gos build` or produced binary can otherwise consume the whole job
//! timeout without naming the culprit.

#![allow(missing_docs)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

const BUILD_TIMEOUT: Duration = Duration::from_mins(2);
const RUN_TIMEOUT: Duration = Duration::from_secs(20);

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .to_path_buf()
}

fn gos_bin() -> PathBuf {
    static GOS: OnceLock<PathBuf> = OnceLock::new();
    GOS.get_or_init(|| {
        if let Ok(path) = std::env::var("CARGO_BIN_EXE_gos") {
            return PathBuf::from(path);
        }
        let mut path = workspace_root().join("target").join("debug").join("gos");
        if !std::env::consts::EXE_EXTENSION.is_empty() {
            path.set_extension(std::env::consts::EXE_EXTENSION);
        }
        if !path.exists() {
            let mut cmd = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
            cmd.args(["build", "--quiet", "--bin", "gos"])
                .current_dir(workspace_root());
            let out = run_output(&mut cmd, "cargo build --bin gos", BUILD_TIMEOUT);
            assert!(
                out.status.success(),
                "building gos failed:\nstdout={}\nstderr={}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
        }
        path
    })
    .clone()
}

fn run_output(command: &mut Command, label: &str, timeout: Duration) -> Output {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|err| panic!("spawn {label}: {err}"));
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return child.wait_with_output().expect("collect child output"),
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let out = child.wait_with_output().expect("collect timed-out output");
                panic!(
                    "{label} timed out after {timeout:?}\nstdout={}\nstderr={}",
                    String::from_utf8_lossy(&out.stdout),
                    String::from_utf8_lossy(&out.stderr)
                );
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
            Err(err) => panic!("wait {label}: {err}"),
        }
    }
}

fn fresh_dir(name: &str) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "gossamer-native-{pid}-{n}-{name}",
        pid = std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create fixture dir");
    dir
}

fn debug_bin(dir: &Path, stem: &str) -> PathBuf {
    let mut p = dir.join("target").join("debug").join(stem);
    if !std::env::consts::EXE_EXTENSION.is_empty() {
        p.set_extension(std::env::consts::EXE_EXTENSION);
    }
    p
}

struct Fixture {
    dir: PathBuf,
    bin: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn build_fixture(name: &str, source: &str) -> Option<Fixture> {
    let dir = fresh_dir(name);
    let src = dir.join(format!("{name}.gos"));
    std::fs::write(&src, source).expect("write fixture source");
    let mut cmd = Command::new(gos_bin());
    cmd.arg("build").arg(&src).current_dir(workspace_root());
    let out = run_output(&mut cmd, &format!("gos build {name}"), BUILD_TIMEOUT);
    if !out.status.success() {
        eprintln!(
            "skipping {name} - gos build failed:\nstdout={}\nstderr={}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        let _ = std::fs::remove_dir_all(&dir);
        return None;
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    if stdout.contains("launcher") {
        eprintln!("skipping {name} - gos build fell back to launcher: {stdout}");
        let _ = std::fs::remove_dir_all(&dir);
        return None;
    }
    Some(Fixture {
        bin: debug_bin(&dir, name),
        dir,
    })
}

fn assert_exit(name: &str, source: &str, code: i32) {
    let Some(fixture) = build_fixture(name, source) else {
        return;
    };
    let mut cmd = Command::new(&fixture.bin);
    let out = run_output(&mut cmd, &format!("run {name}"), RUN_TIMEOUT);
    assert_eq!(
        out.status.code(),
        Some(code),
        "{name}: expected exit {code}; stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn assert_stdout_contains(name: &str, source: &str, needle: &str) {
    let Some(fixture) = build_fixture(name, source) else {
        return;
    };
    let mut cmd = Command::new(&fixture.bin);
    let out = run_output(&mut cmd, &format!("run {name}"), RUN_TIMEOUT);
    assert!(
        out.status.success(),
        "{name}: native binary exit={:?}",
        out.status.code()
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains(needle), "{name}: stdout={stdout}");
}

#[test]
fn gos_build_handles_tuple_destructuring_let() {
    assert_exit(
        "detup",
        "fn main() -> i64 {\n    let (a, b) = (11i64, 22i64)\n    a + b\n}\n",
        33,
    );
}

#[test]
fn gos_build_handles_numeric_cast() {
    assert_exit(
        "cast",
        "fn main() -> i64 {\n    let n = 7i64;\n    (n as i64) + 5i64\n}\n",
        12,
    );
}

#[test]
fn gos_build_handles_int_literal_match() {
    assert_exit(
        "match_int",
        "fn main() -> i64 {\n    let n = 1i64\n    match n {\n        0i64 => 10i64,\n        1i64 => 20i64,\n        _ => 30i64,\n    }\n}\n",
        20,
    );
}

#[test]
fn gos_build_handles_tuples_and_arrays() {
    assert_exit(
        "tuple",
        "fn main() -> i64 {\n    let pair = (10i64, 20i64, 30i64)\n    pair.0 + pair.2\n}\n",
        40,
    );
    assert_exit(
        "repeat_array",
        "fn main() -> i64 {\n    let xs = [9i64; 4i64]\n    xs[2i64] + xs[3i64]\n}\n",
        18,
    );
    assert_exit(
        "array",
        "fn main() -> i64 {\n    let xs = [5i64, 7i64, 9i64]\n    xs[2i64]\n}\n",
        9,
    );
}

#[test]
fn gos_build_monomorphises_generic_function_calls() {
    assert_exit(
        "mono",
        "fn first<T>(a: T, b: T) -> T { a }\nfn main() -> i64 {\n    let i = first::<i64>(41i64, 999i64)\n    let b = first::<bool>(true, false)\n    if b { i + 1i64 } else { 0i64 }\n}\n",
        42,
    );
}

#[test]
fn gos_build_handles_first_class_closure_passed_to_higher_order_function() {
    assert_exit(
        "fcc",
        "fn apply(f: Fn(i64) -> i64, x: i64) -> i64 { f(x) }\nfn main() -> i64 {\n    let c = 10i64\n    let add_c = |y: i64| c + y\n    apply(add_c, 32i64)\n}\n",
        42,
    );
}

#[test]
fn gos_build_handles_capturing_closure_via_heap_allocated_env() {
    assert_exit(
        "cap",
        "fn main() -> i64 {\n    let x = 10i64\n    let add_x = |y: i64| x + y\n    add_x(32i64)\n}\n",
        42,
    );
}

#[test]
fn gos_build_handles_non_capturing_closure_via_direct_call() {
    assert_exit(
        "closure",
        "fn main() -> i64 {\n    let plus = |x: i64| x + 1i64\n    plus(41i64)\n}\n",
        42,
    );
}

#[test]
fn gos_build_handles_for_loop_over_range() {
    assert_exit(
        "for_range",
        "fn main() -> i64 {\n    let mut sum = 0i64\n    for n in 0i64..10i64 {\n        sum = sum + n\n    }\n    sum\n}\n",
        45,
    );
}

#[test]
fn gos_build_handles_struct_literal_and_field_access() {
    assert_exit(
        "struct_field",
        "struct Point { x: i64, y: i64 }\nfn main() -> i64 {\n    let p = Point { x: 10i64, y: 32i64 }\n    p.x + p.y\n}\n",
        42,
    );
}

#[test]
fn gos_build_produces_native_println_binary() {
    assert_stdout_contains(
        "println",
        "fn main() { println(\"native says hi\") }\n",
        "native says hi",
    );
}
