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
        let bin = dir.join("target").join(profile).join("mem");
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

fn parse_max_rss_kb(stderr: &str) -> Option<u64> {
    for line in stderr.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("Maximum resident set size (kbytes):") {
            return rest.trim().parse().ok();
        }
    }
    None
}
