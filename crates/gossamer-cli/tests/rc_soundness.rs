//! Heap-soundness regression for the compiled-tier reference-counting
//! release pass. A recursive enum value aliased to multiple bindings
//! (`let b = a; let c = a`) must not be released more than once. An
//! earlier ownership-move heuristic propagated ownership to every copy
//! target, triple-freeing the shared node - `glibc` aborted with
//! "unaligned fastbin chunk detected" once enough allocations exercised
//! the corrupted free list. We re-run the program under
//! `MALLOC_CHECK_=3` (glibc's strict allocator consistency mode) so any
//! double/invalid free aborts the process.

use std::env;
use std::path::PathBuf;
use std::process::Command;

fn gos_bin() -> PathBuf {
    PathBuf::from(env::var("CARGO_BIN_EXE_gos").expect("CARGO_BIN_EXE_gos"))
}

#[test]
fn aliased_recursive_enum_release_is_not_double_free() {
    // MALLOC_CHECK_ is a glibc feature; skip elsewhere.
    if !cfg!(target_os = "linux") {
        eprintln!("skipping: MALLOC_CHECK_ heap checking is glibc/Linux-only");
        return;
    }
    let dir = env::temp_dir().join(format!("gos-rcsound-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("alias.gos");
    std::fs::write(
        &source,
        "
enum Tree { Leaf, Node(i64, Tree, Tree) }

fn build(d: i64) -> Tree {
    if d == 0 {
        Tree::Leaf
    } else {
        Tree::Node(d, build(d - 1), build(d - 1))
    }
}

fn checksum(t: Tree) -> i64 {
    match t {
        Tree::Leaf => 1,
        Tree::Node(v, l, r) => *v + checksum(l) + checksum(r),
    }
}

// Alias one owned tree to three bindings, then keep allocating so any
// corrupted free list is exercised.
fn aliased(d: i64) -> i64 {
    let a = build(d)
    let b = a
    let c = a
    checksum(a) + checksum(b) + checksum(c)
}

fn main() {
    let mut total = 0
    let mut i = 0
    while i < 500 {
        total += aliased(8)
        i += 1
    }
    println(\"total = {}\", total)
}
",
    )
    .unwrap();

    let build = Command::new(gos_bin())
        .arg("build")
        .arg("--release")
        .arg(&source)
        .output()
        .expect("spawn gos build --release");
    assert!(
        build.status.success(),
        "release build failed: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let bin = dir
        .join("target")
        .join("release")
        .join(format!("alias{}", std::env::consts::EXE_SUFFIX));
    assert!(bin.exists(), "missing {}", bin.display());

    // Run several times - heap corruption from a double free is
    // nondeterministic and may only abort on some runs.
    for run in 0..5 {
        let out = Command::new(&bin)
            .env("MALLOC_CHECK_", "3")
            .output()
            .expect("spawn alias binary");
        assert!(
            out.status.success(),
            "run {run}: binary aborted (likely double free) - status {:?}, stderr={}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("total = 1137000"),
            "run {run}: unexpected output: {stdout}"
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}
