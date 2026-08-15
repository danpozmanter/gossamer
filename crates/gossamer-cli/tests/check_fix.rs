//! `gos check --fix` applies the rewrites the diagnostics carry.
//!
//! A suggestion is a guess - the diagnostic carries no applicability
//! marker separating a certain rewrite from a speculative one - so the
//! command keeps an edit only when a re-check proves the file got
//! better. These tests pin both directions: a correct suggestion lands,
//! a wrong one leaves the source alone.

#![allow(missing_docs)]

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

fn gos_bin() -> PathBuf {
    PathBuf::from(std::env::var("CARGO_BIN_EXE_gos").expect("CARGO_BIN_EXE_gos"))
}

/// One directory per case: `gos check` bundles every sibling `.gos`, so
/// two cases sharing a directory would check each other's source.
fn case(name: &str, source: &str) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "gos-check-fix-{}-{}-{name}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create case dir");
    let file = dir.join(format!("{name}.gos"));
    std::fs::write(&file, source).expect("write case source");
    file
}

fn run_fix(file: &PathBuf) -> String {
    let out = Command::new(gos_bin())
        .args(["check", "--fix"])
        .arg(file)
        .output()
        .expect("spawn gos check --fix");
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn a_resolvable_name_suggestion_is_applied() {
    let file = case(
        "typo",
        "fn main() {\n    let total = 3\n    println!(\"{}\", totl)\n}\n",
    );
    let report = run_fix(&file);
    assert!(report.contains("fix: 1 edit"), "report was: {report}");
    let after = std::fs::read_to_string(&file).unwrap();
    assert!(after.contains("println!(\"{}\", total)"), "after: {after}");
    let _ = std::fs::remove_dir_all(file.parent().unwrap());
}

#[test]
fn a_suggestion_that_does_not_resolve_the_error_is_refused() {
    // `coun` draws a did-you-mean pointing at `count`, a `String`;
    // substituting it trades the unresolved name for a type error, so
    // the source must survive untouched.
    let before = "fn main() {\n    let count = \"text\"\n    println!(\"{}\", count)\n    \
                  let total: i64 = coun + 1\n    println!(\"{}\", total)\n}\n";
    let file = case("wrong_suggestion", before);
    let report = run_fix(&file);
    assert!(report.contains("fix: 0 edit"), "report was: {report}");
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        before,
        "a refused suggestion must leave the file byte-identical"
    );
    let _ = std::fs::remove_dir_all(file.parent().unwrap());
}

#[test]
fn lint_fixes_are_applied_alongside_diagnostic_suggestions() {
    let file = case(
        "lints",
        "fn main() {\n    let unused = 1\n    let x = !!true\n    println!(\"{}\", x)\n}\n",
    );
    let report = run_fix(&file);
    assert!(report.contains("fix: 2 edit"), "report was: {report}");
    let after = std::fs::read_to_string(&file).unwrap();
    assert!(after.contains("let _unused = 1"), "after: {after}");
    assert!(after.contains("let x = true"), "after: {after}");
    let _ = std::fs::remove_dir_all(file.parent().unwrap());
}

#[test]
fn a_clean_file_is_left_untouched() {
    let before = "fn main() {\n    println!(\"{}\", 42)\n}\n";
    let file = case("clean", before);
    let report = run_fix(&file);
    assert!(report.contains("fix: 0 edit"), "report was: {report}");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), before);
    let _ = std::fs::remove_dir_all(file.parent().unwrap());
}
