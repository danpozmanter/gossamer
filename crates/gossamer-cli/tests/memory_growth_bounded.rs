//! Catches the `gos_rt_heap_*_free` regression (C2 in
//! `~/dev/contexts/lang/adversarial_analysis.md`).

use std::env;
use std::path::PathBuf;
use std::process::Command;

fn gos_bin() -> PathBuf {
    PathBuf::from(env::var("CARGO_BIN_EXE_gos").expect("CARGO_BIN_EXE_gos"))
}

#[test]
fn compiled_vec_alloc_and_drop_stays_under_rss_cap() {
    if !std::path::Path::new("/usr/bin/time").exists() {
        eprintln!("skipping: /usr/bin/time not available on this host");
        return;
    }
    let probe = Command::new("/usr/bin/time").arg("-v").arg("true").output();
    let is_gnu_time = probe.as_ref().is_ok_and(|o| {
        let stderr = String::from_utf8_lossy(&o.stderr);
        stderr.contains("Maximum resident set size")
    });
    if !is_gnu_time {
        eprintln!("skipping: /usr/bin/time does not support GNU -v on this host");
        return;
    }
    let dir = env::temp_dir().join(format!("gos-mem-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("mem.gos");
    std::fs::write(
        &source,
        "
fn pump() {
    let buf = U8Vec::new(8388608)
    let mut i = 0
    while i < 1024 {
        buf.set_byte(i, ((i * 7) % 256) as i64)
        i = i + 1
    }
}

fn main() {
    let mut k = 0
    while k < 32 {
        pump()
        k = k + 1
    }
}
",
    )
    .unwrap();

    for release in [false, true] {
        let mut cmd = Command::new(gos_bin());
        cmd.arg("build");
        if release {
            cmd.arg("--release");
        }
        cmd.arg(&source);
        let build = cmd.output().expect("spawn gos build");
        assert!(
            build.status.success(),
            "build failed (release={release}): {}",
            String::from_utf8_lossy(&build.stderr)
        );

        let profile = if release { "release" } else { "debug" };
        let bin = dir
            .join("target")
            .join(profile)
            .join(format!("mem{}", std::env::consts::EXE_SUFFIX));
        assert!(bin.exists(), "missing {}", bin.display());

        let out = Command::new("/usr/bin/time")
            .arg("-v")
            .arg(&bin)
            .output()
            .expect("spawn /usr/bin/time");
        assert!(
            out.status.success(),
            "binary failed (release={release}): stderr={}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        let kb = parse_max_rss_kb(&stderr)
            .unwrap_or_else(|| panic!("could not parse Maximum resident set size:\n{stderr}"));
        let cap_kb = 96 * 1024;
        assert!(
            kb < cap_kb,
            "RSS {kb} KiB exceeded {cap_kb} KiB cap (release={release}); heap_*_free regression"
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

/// Gates the compiled-tier reference-counting release of recursive enum
/// values. A loop builds and discards a depth-14 binary tree on every
/// iteration; each per-iteration temporary must be released. Before RC
/// landed these `gos_rc_alloc`'d (formerly `malloc`'d) nodes leaked
/// unboundedly - 200 iterations would accumulate well over 100 MiB and
/// keep growing with depth. With deterministic RC release the peak stays
/// near a single tree's footprint. Runs under the full `-O3` release
/// pipeline, where the old tracing GC was unsound.
#[test]
fn compiled_recursive_enum_loop_stays_under_rss_cap() {
    if !std::path::Path::new("/usr/bin/time").exists() {
        eprintln!("skipping: /usr/bin/time not available on this host");
        return;
    }
    let probe = Command::new("/usr/bin/time").arg("-v").arg("true").output();
    let is_gnu_time = probe.as_ref().is_ok_and(|o| {
        let stderr = String::from_utf8_lossy(&o.stderr);
        stderr.contains("Maximum resident set size")
    });
    if !is_gnu_time {
        eprintln!("skipping: /usr/bin/time does not support GNU -v on this host");
        return;
    }
    let dir = env::temp_dir().join(format!("gos-rcmem-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("rcmem.gos");
    std::fs::write(
        &source,
        "
enum Tree { Leaf, Node(i64, Box<Tree>, Box<Tree>) }

fn build(d: i64) -> Tree {
    if d == 0 {
        Tree::Leaf
    } else {
        Tree::Node(d, Box::new(build(d - 1)), Box::new(build(d - 1)))
    }
}

fn checksum(t: &Tree) -> i64 {
    match t {
        Tree::Leaf => 1,
        Tree::Node(v, l, r) => *v + checksum(l) + checksum(r),
    }
}

fn main() {
    let mut total = 0
    let mut i = 0
    while i < 200 {
        total += checksum(&build(14))
        i += 1
    }
    println!(\"total = {}\", total)
}
",
    )
    .unwrap();

    let mut cmd = Command::new(gos_bin());
    cmd.arg("build").arg("--release").arg(&source);
    let build = cmd.output().expect("spawn gos build --release");
    assert!(
        build.status.success(),
        "release build failed: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let bin = dir
        .join("target")
        .join("release")
        .join(format!("rcmem{}", std::env::consts::EXE_SUFFIX));
    assert!(bin.exists(), "missing {}", bin.display());

    let out = Command::new("/usr/bin/time")
        .arg("-v")
        .arg(&bin)
        .output()
        .expect("spawn /usr/bin/time");
    assert!(
        out.status.success(),
        "binary failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("total = 9827200"),
        "unexpected output: {stdout}"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    let kb = parse_max_rss_kb(&stderr)
        .unwrap_or_else(|| panic!("could not parse Maximum resident set size:\n{stderr}"));
    let cap_kb = 64 * 1024;
    assert!(
        kb < cap_kb,
        "RSS {kb} KiB exceeded {cap_kb} KiB cap; recursive-enum RC release regression \
         (per-iteration trees are leaking)"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A recursive-enum tree bound to a *named local* and rebuilt each loop
/// iteration must release the previous iteration's value before
/// reassignment. Before the fix this leaked every iteration's tree
/// (the release fired only at function return), so 200 depth-14 trees stayed
/// resident (~hundreds of MB). The earlier test only exercised the
/// *temporary* shape (`checksum(&build(14))`), which the single-use path
/// already released - this is the gap it missed.
#[test]
fn compiled_named_binding_loop_stays_under_rss_cap() {
    if !std::path::Path::new("/usr/bin/time").exists() {
        eprintln!("skipping: /usr/bin/time not available on this host");
        return;
    }
    let probe = Command::new("/usr/bin/time").arg("-v").arg("true").output();
    let is_gnu_time = probe
        .as_ref()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stderr).contains("Maximum resident set size"));
    if !is_gnu_time {
        eprintln!("skipping: /usr/bin/time does not support GNU -v on this host");
        return;
    }
    let dir = env::temp_dir().join(format!("gos-rcnamed-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("rcnamed.gos");
    std::fs::write(
        &source,
        "
enum Tree { Leaf, Node(i64, Box<Tree>, Box<Tree>) }

fn build(d: i64) -> Tree {
    if d == 0 { Tree::Leaf } else { Tree::Node(d, Box::new(build(d - 1)), Box::new(build(d - 1))) }
}

fn checksum(t: &Tree) -> i64 {
    match t { Tree::Leaf => 1, Tree::Node(v, l, r) => *v + checksum(l) + checksum(r) }
}

fn main() {
    let mut total = 0
    let mut i = 0
    while i < 200 {
        let t = build(14)
        total += checksum(&t)
        i += 1
    }
    println!(\"total = {}\", total)
}
",
    )
    .unwrap();

    let mut cmd = Command::new(gos_bin());
    cmd.arg("build").arg("--release").arg(&source);
    let build = cmd.output().expect("spawn gos build --release");
    assert!(
        build.status.success(),
        "release build failed: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let bin = dir
        .join("target")
        .join("release")
        .join(format!("rcnamed{}", std::env::consts::EXE_SUFFIX));
    assert!(bin.exists(), "missing {}", bin.display());

    let out = Command::new("/usr/bin/time")
        .arg("-v")
        .arg(&bin)
        .output()
        .expect("spawn /usr/bin/time");
    assert!(
        out.status.success(),
        "binary failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("total = 9827200"),
        "unexpected output: {stdout}"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    let kb = parse_max_rss_kb(&stderr)
        .unwrap_or_else(|| panic!("could not parse Maximum resident set size:\n{stderr}"));
    let cap_kb = 64 * 1024;
    assert!(
        kb < cap_kb,
        "RSS {kb} KiB exceeded {cap_kb} KiB cap; named-binding loop is leaking \
         (release fires only at function return, not before reassignment)"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Gates the three container / accumulator leak classes fixed in the
/// drop-pass move-transfer plus return-copy-move work. A1 is an owning
/// container binding reassigned each loop iteration (`v = make()`); A2 is a
/// dynamic repeat array `[x; n]` built in a function called in a loop; A3
/// is a String accumulator built and returned by a function called in a
/// loop. Each previously retained one buffer or reference per iteration,
/// hundreds of MB over the loop; with the fixes the peak stays near a
/// single buffer's footprint. Runs under the full `-O3` release pipeline.
#[test]
fn compiled_container_and_accumulator_loops_stay_under_rss_cap() {
    if !std::path::Path::new("/usr/bin/time").exists() {
        eprintln!("skipping: /usr/bin/time not available on this host");
        return;
    }
    let probe = Command::new("/usr/bin/time").arg("-v").arg("true").output();
    let is_gnu_time = probe.as_ref().is_ok_and(|o| {
        let stderr = String::from_utf8_lossy(&o.stderr);
        stderr.contains("Maximum resident set size")
    });
    if !is_gnu_time {
        eprintln!("skipping: /usr/bin/time does not support GNU -v on this host");
        return;
    }
    let dir = env::temp_dir().join(format!("gos-leakclass-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("leakclass.gos");
    std::fs::write(
        &source,
        "
fn make_vec(n: i64) -> [i64] {
    let mut v: [i64] = []
    let mut i = 0
    while i < n {
        v.push(i * 2)
        i += 1
    }
    v
}

fn repeat_sum(n: i64) -> i64 {
    let d = [7; n]
    d[0] + d[n - 1]
}

fn make_str(n: i64) -> String {
    let mut s = \"\"
    let mut i = 0
    while i < n {
        s += \"ab\"
        i += 1
    }
    s
}

fn main() {
    let mut v: [i64] = []
    let mut total = 0
    let mut r = 0
    while r < 200000 {
        v = make_vec(64)
        total += repeat_sum(64)
        let s = make_str(16)
        total += s.len()
        r += 1
    }
    total += v.len()
    println!(\"total = {}\", total)
}
",
    )
    .unwrap();

    let mut cmd = Command::new(gos_bin());
    cmd.arg("build").arg("--release").arg(&source);
    let build = cmd.output().expect("spawn gos build --release");
    assert!(
        build.status.success(),
        "release build failed: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let bin = dir
        .join("target")
        .join("release")
        .join(format!("leakclass{}", std::env::consts::EXE_SUFFIX));
    assert!(bin.exists(), "missing {}", bin.display());

    let out = Command::new("/usr/bin/time")
        .arg("-v")
        .arg(&bin)
        .output()
        .expect("spawn /usr/bin/time");
    assert!(
        out.status.success(),
        "binary failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("total = 9200064"),
        "unexpected output: {stdout}"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    let kb = parse_max_rss_kb(&stderr)
        .unwrap_or_else(|| panic!("could not parse Maximum resident set size:\n{stderr}"));
    let cap_kb = 32 * 1024;
    assert!(
        kb < cap_kb,
        "RSS {kb} KiB exceeded {cap_kb} KiB cap; container-reassign / repeat-array / \
         string-accumulator loop is leaking one buffer or reference per iteration"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Correctness and leak gate for a by-value struct with a `Vec`/`[T]` field that
/// is moved into the struct in a returning function and back out across the call
/// boundary (`struct { data: [i64], name: String }`). The struct and its indexed
/// field must read correctly on the bytecode VM, the Cranelift JIT, and both LLVM
/// AOT modes, and each iteration's field buffer must be released when the struct
/// drops, so peak RSS stays bounded across 300k iterations.
#[test]
fn compiled_struct_vec_field_loop_runs_correctly_on_all_tiers() {
    if !std::path::Path::new("/usr/bin/time").exists() {
        eprintln!("skipping: /usr/bin/time not available on this host");
        return;
    }
    let probe = Command::new("/usr/bin/time").arg("-v").arg("true").output();
    let is_gnu_time = probe
        .as_ref()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stderr).contains("Maximum resident set size"));
    if !is_gnu_time {
        eprintln!("skipping: /usr/bin/time does not support GNU -v on this host");
        return;
    }
    let dir = env::temp_dir().join(format!("gos-structvec-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("structvec.gos");
    std::fs::write(
        &source,
        "
struct Rec { data: [i64], name: String }

fn make(n: i64) -> Rec {
    let mut v: [i64] = []
    let mut i = 0
    while i < n {
        v.push(i * 2)
        i += 1
    }
    Rec(v, \"row\")
}

fn main() {
    let mut total = 0
    let mut r = 0
    while r < 300000 {
        let rec = make(16)
        total += rec.data.len() + rec.name.len()
        r += 1
    }
    println!(\"total = {}\", total)
}
",
    )
    .unwrap();

    // gos run (bytecode VM + Cranelift JIT): output only - RSS is dominated by
    // the interpreter baseline, but a per-iteration leak would still crash /
    // OOM, and this exercises the JIT single-slot / field-free paths.
    let run = Command::new(gos_bin())
        .arg("run")
        .arg(&source)
        .output()
        .expect("spawn gos run");
    assert!(
        run.status.success() && String::from_utf8_lossy(&run.stdout).contains("total = 5700000"),
        "gos run failed: stdout={} stderr={}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );

    for release in [false, true] {
        let mut cmd = Command::new(gos_bin());
        cmd.arg("build");
        if release {
            cmd.arg("--release");
        }
        cmd.arg(&source);
        let build = cmd.output().expect("spawn gos build");
        assert!(
            build.status.success(),
            "build failed (release={release}): {}",
            String::from_utf8_lossy(&build.stderr)
        );
        let profile = if release { "release" } else { "debug" };
        let bin = dir
            .join("target")
            .join(profile)
            .join(format!("structvec{}", std::env::consts::EXE_SUFFIX));
        assert!(bin.exists(), "missing {}", bin.display());

        let out = Command::new("/usr/bin/time")
            .arg("-v")
            .arg(&bin)
            .output()
            .expect("spawn /usr/bin/time");
        assert!(
            out.status.success(),
            "binary failed (release={release}): stderr={}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            String::from_utf8_lossy(&out.stdout).contains("total = 5700000"),
            "unexpected output (release={release}): {}",
            String::from_utf8_lossy(&out.stdout)
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        let kb = parse_max_rss_kb(&stderr)
            .unwrap_or_else(|| panic!("could not parse Maximum resident set size:\n{stderr}"));
        let cap_kb = 32 * 1024;
        assert!(
            kb < cap_kb,
            "RSS {kb} KiB exceeded {cap_kb} KiB cap (release={release}); struct Vec-field \
             buffer is leaking one allocation per iteration"
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

fn parse_max_rss_kb(stderr: &str) -> Option<u64> {
    for line in stderr.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("Maximum resident set size (kbytes):") {
            return rest.trim().parse().ok();
        }
    }
    None
}
