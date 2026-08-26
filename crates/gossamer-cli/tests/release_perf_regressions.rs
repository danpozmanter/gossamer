//! Release-tier performance-shape regressions for production-like hot loops.

#![allow(missing_docs)]

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

static SERIAL: AtomicU64 = AtomicU64::new(0);

fn fixture(name: &str, source: &str) -> (PathBuf, PathBuf) {
    let id = SERIAL.fetch_add(1, Ordering::Relaxed);
    let dir = env::temp_dir().join(format!(
        "gos-release-perf-{name}-{}-{id}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).expect("create fixture directory");
    let source_path = dir.join(format!("{name}.gos"));
    fs::write(&source_path, source).expect("write fixture");
    (dir, source_path)
}

fn build_release(name: &str, source: &str) -> PathBuf {
    let (dir, source_path) = fixture(name, source);
    let output = Command::new(env!("CARGO_BIN_EXE_gos"))
        .args(["build", "--release", "--out-dir"])
        .arg(&dir)
        .arg(&source_path)
        .output()
        .expect("run gos build --release");
    assert!(
        output.status.success(),
        "release build failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    dir.join(name)
}

fn timed(binary: &Path, n: usize) -> Duration {
    let start = Instant::now();
    let output = Command::new(binary)
        .arg(n.to_string())
        .output()
        .expect("run release fixture");
    let elapsed = start.elapsed();
    assert!(
        output.status.success(),
        "fixture failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    elapsed
}

fn assert_linear_and_fast(name: &str, small: Duration, large: Duration, ceiling: Duration) {
    assert!(
        large <= ceiling,
        "{name} exceeded its release speed ceiling: {large:?} > {ceiling:?}"
    );
    // Include a fixed allowance for process startup and noisy shared CI hosts.
    assert!(
        large <= small.saturating_mul(3) + Duration::from_millis(250),
        "{name} lost near-linear scaling: small={small:?}, large={large:?}"
    );
}

fn assert_bounded_live_vecs(binary: &Path) {
    let output = Command::new(binary)
        .arg("1000")
        .env("GOS_LEAK_LEDGER", "1")
        .output()
        .expect("run release fixture with allocation ledger");
    let stderr = String::from_utf8_lossy(&output.stderr);
    let live = stderr
        .split("vec=")
        .nth(1)
        .and_then(|tail| tail.split_whitespace().next())
        .and_then(|n| n.parse::<usize>().ok())
        .expect("allocation ledger must report live Vec count");
    assert!(
        live <= 3,
        "radix ping-pong retained per-pass Vec clones: {stderr}"
    );
}

#[test]
fn json_serde_like_release_work_stays_linear_and_fast() {
    let binary = build_release(
        "json_serde_shape",
        r#"
use std::{encoding::json, env}

let n: i64 = env::args()[0].to_i64().unwrap_or(3000)
let mut text = String::with_capacity(1024)
text.push_str("[")
for i in 0..n {
    if i > 0 { text.push_str(",") }
    text.push_str("{\"id\":")
    text.push_str(i.to_string())
    text.push_str(",\"name\":\"user-")
    text.push_str(format("{:06}", i))
    text.push_str("\",\"tags\":[\"a\",\"b\"]}")
}
text.push_str("]")
let parsed = json::parse(text)?
let rendered = json::render(parsed)
let mut checksum = 0
let mut i = 0
while i < rendered.len() {
    checksum += rendered.byte_at(i)
    i += 1
}
println("{} {}", rendered.len(), checksum)
"#,
    );
    let small = timed(&binary, 3_000);
    let large = timed(&binary, 6_000);
    assert_linear_and_fast("json-serde-like", small, large, Duration::from_secs(3));
}

#[test]
fn radix_sort_like_release_work_stays_linear_and_fast() {
    let binary = build_release(
        "radix_sort_shape",
        r#"
use std::env

let n: i64 = env::args()[0].to_i64().unwrap_or(500000)
let mut src: Vec<i64> = Vec::with_capacity(n)
let mut dst: Vec<i64> = Vec::with_capacity(n)
for i in 0..n {
    src.push(i * 6364136223846793005 + 1442695040888963407)
    dst.push(0)
}
let mut pass = 0
while pass < 8 {
    let shift = pass * 8
    let mut count = [0; 256]
    for i in 0..n { count[(src[i] >> shift) & 0xff] += 1 }
    let mut total = 0
    for k in 0..256 {
        let c = count[k]
        count[k] = total
        total += c
    }
    for i in 0..n {
        let byte = (src[i] >> shift) & 0xff
        let pos = count[byte]
        count[byte] = pos + 1
        dst[pos] = src[i]
    }
    let tmp = src
    src = dst
    dst = tmp
    pass += 1
}
println("{}", src[0])
"#,
    );
    let small = timed(&binary, 500_000);
    let large = timed(&binary, 1_000_000);
    assert_linear_and_fast("radix-sort-like", small, large, Duration::from_secs(3));
    assert_bounded_live_vecs(&binary);
}
