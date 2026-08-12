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

const DATA_LAST: &str = "use std::iter\n\nfn dbl(n: i64) -> i64 { n * 2 }\n\nfn main() {\n    let xs = #[1, 2, 3]\n    println!(\"{}\", iter::map(dbl, xs).sum())\n}\n";

#[test]
fn the_data_last_free_form_is_rewritten_to_the_method_form() {
    let file = case("combinators", DATA_LAST);
    let (before, _) = gos(&["run"], &file);

    let (report, ok) = gos(&["fix"], &file);
    assert!(ok, "fix failed: {report}");
    assert!(report.contains("1 edit"), "{report}");

    let after_source = std::fs::read_to_string(&file).unwrap();
    assert!(
        after_source.contains("xs.map(dbl)"),
        "rewrite did not land: {after_source}"
    );

    // A migration that changes what the program prints is not a
    // migration.
    let (after, _) = gos(&["run"], &file);
    assert_eq!(before, after, "the program's output changed");

    let _ = std::fs::remove_dir_all(file.parent().unwrap());
}

#[test]
fn a_migration_is_idempotent() {
    let file = case("idempotent", DATA_LAST);
    let (_, ok) = gos(&["fix"], &file);
    assert!(ok);
    let once = std::fs::read_to_string(&file).unwrap();

    let (report, ok) = gos(&["fix"], &file);
    assert!(ok, "second run failed: {report}");
    assert!(report.contains("0 edit"), "second run edited: {report}");
    assert_eq!(once, std::fs::read_to_string(&file).unwrap());

    let _ = std::fs::remove_dir_all(file.parent().unwrap());
}

#[test]
fn check_reports_pending_migrations_without_writing() {
    let file = case("pending", DATA_LAST);
    let (report, ok) = gos(&["fix", "--check"], &file);
    assert!(!ok, "pending migrations must fail --check: {report}");
    assert!(report.contains("would rewrite"), "{report}");
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        DATA_LAST,
        "--check must not write"
    );

    let _ = std::fs::remove_dir_all(file.parent().unwrap());
}

#[test]
fn a_pipeline_step_is_left_alone() {
    // `xs |> iter::map(f)` supplies the sequence through the pipe, so the
    // call has fewer arguments than the combinator takes. Rewriting it
    // would drop the piped value.
    let source = "use std::iter\n\nfn dbl(n: i64) -> i64 { n * 2 }\n\nfn main() {\n    let xs = #[1, 2, 3]\n    let out = xs |> iter::map(dbl)\n    println!(\"{}\", out.sum())\n}\n";
    let file = case("piped", source);
    let (report, ok) = gos(&["fix"], &file);
    assert!(ok, "{report}");
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        source,
        "a pipeline step must not be rewritten"
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
    assert!(text.contains("method_form_combinators"), "{text}");
}
