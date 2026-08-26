//! `gos test --fuzz` - the loop, the minimiser, and the gate.
//!
//! The property that matters is the last one: a crash the fuzzer finds
//! has to arrive as a deterministic test that fails until it is fixed.
//! A fuzzer whose findings are only printed is a report generator.

#![allow(missing_docs)]

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

fn gos_bin() -> PathBuf {
    PathBuf::from(std::env::var("CARGO_BIN_EXE_gos").expect("CARGO_BIN_EXE_gos"))
}

fn case(name: &str, source: &str) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "gos-fuzz-{}-{}-{name}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create case dir");
    let file = dir.join(format!("{name}.gos"));
    std::fs::write(&file, source).expect("write source");
    file
}

fn gos(args: &[&str], file: &PathBuf) -> (String, bool) {
    let out = Command::new(gos_bin())
        .args(args)
        .arg(file)
        .output()
        .expect("spawn gos");
    (
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
        out.status.success(),
    )
}

/// A header the fuzzer has to discover before it can reach the fault,
/// so finding it at all is evidence the loop is coverage-guided rather
/// than throwing random bytes at a wall.
const PLANTED: &str = r#"
fn decode(data: [u8]) -> i64 {
    if data.len() < 3 { return 0 }
    if data[0] != 71 { return 0 }
    if data[1] != 79 { return 0 }
    let want = data[2] as i64
    if want > 4 {
        panic("length byte too large")
    }
    want
}

#[fuzz]
fn fuzz_decode(data: [u8]) {
    let _ = decode(data)
}
"#;

#[test]
fn a_planted_fault_is_found_minimised_and_committed() {
    let file = case("planted", PLANTED);

    let (report, ok) = gos(&["test", "--fuzz"], &file);
    assert!(!ok, "the fuzzer must report the planted fault: {report}");
    assert!(report.contains("length byte too large"), "{report}");
    assert!(report.contains("minimised"), "{report}");

    // The minimal input is the three bytes that reach the panic: the
    // two header bytes plus a length over the limit.
    let dir = file.parent().unwrap().join("testdata/fuzz/fuzz_decode");
    let crash = std::fs::read_dir(&dir)
        .expect("corpus directory")
        .flatten()
        .map(|e| e.path())
        .find(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("crash-"))
        })
        .expect("a crash entry was written");
    let bytes = std::fs::read(&crash).unwrap();
    assert_eq!(bytes.len(), 3, "minimiser left {bytes:?}");
    assert_eq!(&bytes[..2], b"GO", "minimiser dropped the header");

    // The committed crash is now a gate: plain `gos test` fails on it.
    let (report, ok) = gos(&["test"], &file);
    assert!(!ok, "a committed crash must fail `gos test`: {report}");
    assert!(
        report.contains("fuzz corpus: 0 passed, 1 failed"),
        "{report}"
    );

    // And it passes once the fault is fixed - the regression is real,
    // not a permanently red marker.
    let fixed = PLANTED.replace("panic(\"length byte too large\")", "return 4");
    std::fs::write(&file, fixed).unwrap();
    let (report, ok) = gos(&["test"], &file);
    assert!(ok, "the regression must pass once fixed: {report}");
    assert!(
        report.contains("fuzz corpus: 1 passed, 0 failed"),
        "{report}"
    );

    let _ = std::fs::remove_dir_all(file.parent().unwrap());
}

#[test]
fn a_target_with_no_fault_reports_clean() {
    let file = case(
        "clean",
        "#[fuzz]\nfn fuzz_sum(data: [u8]) {\n    let mut n = 0\n    for b in data { n += b as i64 }\n    let _ = n\n}\n",
    );
    let (report, ok) = gos(&["test", "--fuzz"], &file);
    assert!(ok, "a clean target must not fail: {report}");
    assert!(report.contains("no crash"), "{report}");
    let _ = std::fs::remove_dir_all(file.parent().unwrap());
}

#[test]
fn the_same_seed_reproduces_a_run() {
    let a = case("seeded_a", PLANTED);
    let b = case("seeded_b", PLANTED);
    let (ra, _) = gos(&["test", "--fuzz", "--seed", "7"], &a);
    let (rb, _) = gos(&["test", "--fuzz", "--seed", "7"], &b);
    let count = |text: &str| {
        text.lines()
            .find(|l| l.contains("crash after"))
            .map(str::to_string)
    };
    assert_eq!(
        count(&ra).map(|l| l.split_whitespace().nth(2).unwrap_or("").to_string()),
        count(&rb).map(|l| l.split_whitespace().nth(2).unwrap_or("").to_string()),
        "the same seed must reach the fault after the same number of inputs\n{ra}\n{rb}"
    );
    let _ = std::fs::remove_dir_all(a.parent().unwrap());
    let _ = std::fs::remove_dir_all(b.parent().unwrap());
}
