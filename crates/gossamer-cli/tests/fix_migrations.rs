//! `gos fix` - the toolchain's source migrations.
//!
//! A migration is a mechanical upgrade, so the bar is higher than for a
//! lint: it must be deterministic, idempotent, and behaviour-preserving.
//! These tests pin all three, plus the `--check` gate CI runs.

#![allow(missing_docs)]

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

fn gos_bin() -> PathBuf {
    PathBuf::from(std::env::var("CARGO_BIN_EXE_gos").expect("CARGO_BIN_EXE_gos"))
}

/// One directory per case: `gos` bundles sibling `.gos` files.
fn case(name: &str, source: &str) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "gos-fix-{}-{}-{name}",
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

/// A well-formed source with no registered migration applying to it.
const UNTOUCHED: &str = "use std::iter\n\nfn dbl(n: i64) -> i64 { n * 2 }\n\nfn main() {\n    let xs = #[1, 2, 3]\n    println!(\"{}\", iter::sum(iter::map(dbl, xs)))\n}\n";

#[test]
fn fix_leaves_a_source_no_migration_applies_to() {
    let file = case("untouched", UNTOUCHED);
    let (report, ok) = gos(&["fix"], &file);
    assert!(ok, "{report}");
    assert!(report.contains("0 edit"), "{report}");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), UNTOUCHED);

    let _ = std::fs::remove_dir_all(file.parent().unwrap());
}

#[test]
fn check_passes_when_no_migration_is_pending() {
    let file = case("pending", UNTOUCHED);
    let (report, ok) = gos(&["fix", "--check"], &file);
    assert!(ok, "no pending migration must pass --check: {report}");
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        UNTOUCHED,
        "--check must not write"
    );

    let _ = std::fs::remove_dir_all(file.parent().unwrap());
}

#[test]
fn list_names_every_rewriter() {
    let out = Command::new(gos_bin())
        .args(["fix", "--list"])
        .output()
        .expect("spawn gos fix --list");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success());
    assert!(out.status.success(), "{text}");
}
