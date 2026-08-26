//! `gos build --reproducible` must produce a bit-identical artifact from two
//! clean builds of the same source, including when the two builds run from
//! different directories.

use std::path::{Path, PathBuf};
use std::process::Command;

fn gos_bin() -> PathBuf {
    let mut p = std::env::current_exe().unwrap();
    p.pop(); // deps/
    p.pop(); // debug/
    p.push("gos");
    p
}

fn tmp_dir(tag: &str) -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let p = std::env::temp_dir().join(format!(
        "gos-repro-{tag}-{}-{n}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&p).unwrap();
    p
}

const SOURCE: &str = r#"
struct Point { x: i64, y: i64 }

fn area(p: Point) -> i64 { p.x * p.y }

fn main() {
    let mut total = 0
    for i in 0..8 {
        let p = Point { x: i, y: i + 1 }
        total += area(p)
    }
    let names = #["alpha", "beta", "gamma"]
    for n in names {
        total += n.len()
    }
    println("{}", total)
}
"#;

/// Builds `SOURCE` under `dir` with a private cache so the run cannot be
/// served from, or influenced by, another build's artifacts.
fn build_reproducible(dir: &Path) -> Vec<u8> {
    let src = dir.join("app.gos");
    std::fs::write(&src, SOURCE).unwrap();
    let out = Command::new(gos_bin())
        .current_dir(dir)
        .args(["build", "--release", "--reproducible", "app.gos"])
        .env("GOSSAMER_CACHE_DIR", dir.join("cache"))
        .output()
        .expect("gos build");
    assert!(
        out.status.success(),
        "build failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let binary = dir
        .join("target")
        .join("release")
        .join(format!("app{}", std::env::consts::EXE_SUFFIX));
    std::fs::read(&binary).unwrap_or_else(|e| panic!("reading {}: {e}", binary.display()))
}

#[test]
fn two_reproducible_builds_of_one_source_are_byte_identical() {
    let first = build_reproducible(&tmp_dir("same-a"));
    let second = build_reproducible(&tmp_dir("same-b"));
    assert_eq!(
        first.len(),
        second.len(),
        "reproducible builds differ in length"
    );
    assert!(
        first == second,
        "two --reproducible builds of the same source are not byte-identical"
    );
}

#[test]
fn reproducible_build_does_not_depend_on_the_build_directory() {
    // A path of a different length would change any embedded absolute path or
    // temporary directory name, which is the shape that breaks reproducibility.
    let short = build_reproducible(&tmp_dir("p"));
    let long = build_reproducible(&tmp_dir("a-considerably-longer-directory-name"));
    assert!(
        short == long,
        "--reproducible output depends on the directory the build ran in"
    );
}
