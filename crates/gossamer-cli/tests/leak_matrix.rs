//! Memory-leak gate matrix (Phase 0 of the leak-plugging plan in
//! `~/dev/contexts/gos/leaks.md`).
//!
//! Each shape allocates heap memory inside a loop that runs many iterations.
//! If the per-iteration heap is reclaimed at scope end, peak RSS stays bounded
//! regardless of the iteration count; if it leaks, RSS grows with N and blows
//! past the cap. The control shape (`enum_tree`) is RC-managed today and MUST
//! stay bounded - it proves the harness and the existing RC both work. The
//! other shapes are flipped into `MUST_BE_BOUNDED` as each phase fixes them.
//!
//! Run just this gate: `cargo test -p gossamer-cli --test leak_matrix -- --nocapture`

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn gos_bin() -> PathBuf {
    PathBuf::from(env::var("CARGO_BIN_EXE_gos").expect("CARGO_BIN_EXE_gos"))
}

/// Peak RSS below this is "bounded"; a per-iteration leak at these iteration
/// counts pushes well past it (millions of small heap objects retained).
const CAP_KB: u64 = 60_000;

/// Shapes whose per-iteration heap MUST be reclaimed. Grows as phases land.
/// Phase 0: only the RC-managed control. Strings/Vec/Map/payloads are added by
/// their phases.
const MUST_BE_BOUNDED: &[&str] = &[
    "enum_tree_control",
    "transient_string",
    "returned_string",
    "string_in_struct",
    "string_in_nested_struct",
];

/// (name, source). N is baked into each source, sized so a leak clears the cap.
const SHAPES: &[(&str, &str)] = &[
    (
        "enum_tree_control",
        r#"
enum Tree { Node(i64, Tree, Tree), Leaf }
fn build(d: i64) -> Tree {
    if d == 0 { Tree::Leaf } else { Tree::Node(d, build(d - 1), build(d - 1)) }
}
fn count(t: Tree) -> i64 {
    match t { Tree::Node(v, l, r) => v + count(l) + count(r), Tree::Leaf => 0 }
}
fn main() {
    let mut total: i64 = 0
    let mut i: i64 = 0
    while i < 200000 {
        let t = build(12)
        total += count(t)
        i += 1
    }
    println("{}", total)
}
"#,
    ),
    (
        "transient_string",
        r#"
fn main() {
    let mut total: i64 = 0
    let mut i: i64 = 0
    while i < 4000000 {
        let s = format("value-{}", i)
        total += s.len()
        i += 1
    }
    println("{}", total)
}
"#,
    ),
    (
        "returned_string",
        r#"
fn make(i: i64) -> String { format("value-{}", i) }
fn main() {
    let mut total: i64 = 0
    let mut i: i64 = 0
    while i < 4000000 {
        let s = make(i)
        total += s.len()
        i += 1
    }
    println("{}", total)
}
"#,
    ),
    (
        "string_in_struct",
        r#"
struct Holder { s: String }
fn main() {
    let mut total: i64 = 0
    let mut i: i64 = 0
    while i < 4000000 {
        let h = Holder { s: format("value-{}", i) }
        total += h.s.len()
        i += 1
    }
    println("{}", total)
}
"#,
    ),
    (
        "string_in_enum",
        r#"
enum E { S(String), N }
fn main() {
    let mut total: i64 = 0
    let mut i: i64 = 0
    while i < 4000000 {
        let e = E::S(format("value-{}", i))
        match e { E::S(s) => total += s.len(), E::N => {} }
        i += 1
    }
    println("{}", total)
}
"#,
    ),
    (
        "string_in_option",
        r#"
fn main() {
    let mut total: i64 = 0
    let mut i: i64 = 0
    while i < 4000000 {
        let o: Option<String> = Some(format("value-{}", i))
        match o { Some(s) => total += s.len(), None => {} }
        i += 1
    }
    println("{}", total)
}
"#,
    ),
    (
        "string_in_vec",
        r#"
fn main() {
    let mut total: i64 = 0
    let mut i: i64 = 0
    while i < 1500000 {
        let mut v: Vec<String> = Vec::from([])
        v.push(format("key-{}", i))
        v.push(format("val-{}", i))
        total += v[0].len() + v[1].len()
        i += 1
    }
    println("{}", total)
}
"#,
    ),
    (
        "nested_vec_string",
        r#"
fn main() {
    let mut total: i64 = 0
    let mut i: i64 = 0
    while i < 1000000 {
        let mut outer: Vec<Vec<String>> = Vec::from([])
        let mut inner: Vec<String> = Vec::from([])
        inner.push(format("value-{}", i))
        outer.push(inner)
        total += outer[0][0].len()
        i += 1
    }
    println("{}", total)
}
"#,
    ),
    (
        // 0.18.1: a `String` nested inside a by-value sub-struct was never
        // released when the outer struct died (the per-field RC teardown
        // walked only the outer struct's direct fields), so RSS grew with N.
        // The teardown now recurses into by-value sub-structs, with matching
        // recursive retains at every sub-aggregate copy / extract / `..base`
        // site so the nested share is freed exactly once.
        "string_in_nested_struct",
        r#"
struct Inner { name: String, tag: String }
struct Outer { inner: Inner, id: i64 }
fn make(i: i64) -> i64 {
    let o = Outer { inner: Inner { name: format("n-{}", i), tag: format("t-{}", i) }, id: i }
    o.id
}
fn main() {
    let mut total: i64 = 0
    let mut i: i64 = 0
    while i < 3000000 {
        total += make(i)
        i += 1
    }
    println("{}", total)
}
"#,
    ),
];

fn gnu_time_ok() -> bool {
    if !Path::new("/usr/bin/time").exists() {
        return false;
    }
    Command::new("/usr/bin/time")
        .arg("-v")
        .arg("true")
        .output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stderr).contains("Maximum resident set size"))
}

fn build_release(dir: &Path, name: &str, source: &str) -> Option<PathBuf> {
    let src = dir.join(format!("{name}.gos"));
    std::fs::write(&src, source).unwrap();
    let out = Command::new(gos_bin())
        .arg("build")
        .arg("--release")
        .arg(&src)
        .output()
        .expect("spawn gos build");
    if !out.status.success() {
        return None;
    }
    let bin = dir
        .join("target")
        .join("release")
        .join(format!("{name}{}", env::consts::EXE_SUFFIX));
    bin.exists().then_some(bin)
}

fn peak_rss_kb(bin: &Path) -> u64 {
    let out = Command::new("/usr/bin/time")
        .arg("-v")
        .arg(bin)
        .output()
        .expect("spawn /usr/bin/time");
    assert!(
        out.status.success(),
        "binary {} failed: {}",
        bin.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    for line in stderr.lines() {
        if let Some(rest) = line
            .trim()
            .strip_prefix("Maximum resident set size (kbytes):")
        {
            return rest.trim().parse().unwrap();
        }
    }
    panic!("no RSS line for {}", bin.display());
}

#[test]
fn leak_matrix_report() {
    if !gnu_time_ok() {
        eprintln!("skipping: GNU /usr/bin/time -v not available");
        return;
    }
    let dir = env::temp_dir().join(format!("gos-leak-matrix-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();

    let mut failures = Vec::new();
    eprintln!("\n  leak matrix (cap {CAP_KB} KB)");
    eprintln!("  {:<22} {:>10}  verdict", "shape", "peak KB");
    for (name, source) in SHAPES {
        let gated = MUST_BE_BOUNDED.contains(name);
        let Some(bin) = build_release(&dir, name, source) else {
            let dash: &str = "-";
            eprintln!("  {name:<22} {dash:>10}  BUILD FAIL");
            if gated {
                failures.push(*name);
            }
            continue;
        };
        let rss = peak_rss_kb(&bin);
        let bounded = rss < CAP_KB;
        let verdict = match (bounded, gated) {
            (true, _) => "bounded",
            (false, true) => "LEAK (gated!)",
            (false, false) => "leak (not yet gated)",
        };
        eprintln!("  {name:<22} {rss:>10}  {verdict}");
        if gated && !bounded {
            failures.push(*name);
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert!(failures.is_empty(), "gated shapes leaked: {failures:?}");
}
