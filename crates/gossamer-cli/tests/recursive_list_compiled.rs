#![allow(missing_docs)]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn gos_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_gos"))
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

fn run_with_timeout(mut command: Command, timeout: Duration) -> std::process::Output {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn child");
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return child.wait_with_output().expect("collect child output"),
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Ok(None) => {
                let _ = child.kill();
                let output = child.wait_with_output().expect("collect timed-out child");
                panic!(
                    "child timed out after {timeout:?}: stdout={} stderr={}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr),
                );
            }
            Err(error) => panic!("wait for child: {error}"),
        }
    }
}

fn assert_success(output: &std::process::Output, label: &str) {
    assert!(
        output.status.success(),
        "{label} failed with {:?}: stdout={} stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

fn assert_run(source: &Path, function: &str, n: i64, expected: &str, jit: bool) {
    let mut command = Command::new(gos_bin());
    command
        .arg(source)
        .arg(n.to_string())
        .env("GOS_RC_DEBUG", "1");
    if jit {
        command
            .env("GOS_JIT_ONLY", function)
            .env("GOS_JIT_TRACE", "1");
    } else {
        command.env("GOS_JIT", "0");
    }
    let output = run_with_timeout(command, Duration::from_secs(20));
    assert_success(&output, if jit { "forced JIT" } else { "VM" });
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), expected);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr
            .lines()
            .any(|line| line == "RC_LIVE_AT_EXIT=0 shared_live=0 reused=0"),
        "run did not reclaim every recursive node: {stderr}"
    );
    if jit && function == "linked_list" {
        assert!(
            stderr.contains(&format!("jit: native hit {function}")),
            "the regression must execute {function} natively: {stderr}"
        );
    }
}

#[test]
fn recursive_enum_and_option_box_reassignment_match_vm_and_forced_jit() {
    let n = 8_000;
    let expected = "8000 32004000";
    for (file, function) in [
        ("recursive_list_reassignment.gos", "linked_list"),
        ("recursive_option_box_reassignment.gos", "linked_option"),
    ] {
        let source = fixture(file);
        assert_run(&source, function, n, expected, false);
        assert_run(&source, function, n, expected, true);
    }
}

#[test]
fn recursive_enum_release_binary_has_exact_bounded_traversal_and_teardown() {
    for file in [
        "recursive_list_reassignment.gos",
        "recursive_option_box_reassignment.gos",
    ] {
        let source = fixture(file);
        let out_dir = std::env::temp_dir().join(format!(
            "gos-recursive-list-native-{}-{file}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&out_dir);
        std::fs::create_dir_all(&out_dir).expect("create native output dir");

        let mut build = Command::new(gos_bin());
        build
            .arg("build")
            .arg("--release")
            .arg("--out-dir")
            .arg(&out_dir)
            .arg(&source);
        let output = run_with_timeout(build, Duration::from_mins(1));
        assert_success(&output, "release build");

        let binary = std::fs::read_dir(&out_dir)
            .expect("read native output dir")
            .flatten()
            .map(|entry| entry.path())
            .find(|path| path.is_file() && path.extension().is_none())
            .expect("find native executable");
        let mut run = Command::new(binary);
        run.arg("8000").env("GOS_RC_DEBUG", "1");
        let output = run_with_timeout(run, Duration::from_secs(20));
        assert_success(&output, "release binary");
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim(),
            "8000 32004000"
        );
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .lines()
                .any(|line| line == "RC_LIVE_AT_EXIT=0 shared_live=0 reused=0"),
            "release binary leaked recursive nodes: {}",
            String::from_utf8_lossy(&output.stderr),
        );

        let _ = std::fs::remove_dir_all(out_dir);
    }
}
