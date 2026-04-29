//! Catches multi-thread tearing on `GOS_RT_STDOUT_*` (C3 in
//! `~/dev/contexts/lang/adversarial_analysis.md`).

use std::collections::HashSet;
use std::env;
use std::path::PathBuf;
use std::process::Command;

fn gos_bin() -> PathBuf {
    PathBuf::from(env::var("CARGO_BIN_EXE_gos").expect("CARGO_BIN_EXE_gos"))
}

#[test]
fn concurrent_println_lines_do_not_tear() {
    let dir = env::temp_dir().join(format!("gos-stdout-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("concurrent.gos");
    std::fs::write(
        &source,
        "
fn worker(id: i64, wg: WaitGroup) {
    let mut i = 0
    while i < 256 {
        println!(\"G{}:{}\", id, i)
        i = i + 1
    }
    wg.done()
}

fn main() {
    let wg = WaitGroup::new()
    let mut k: i64 = 0
    while k < 16 {
        wg.add(1)
        go worker(k, wg)
        k = k + 1
    }
    wg.wait()
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
        let bin = dir.join("target").join(profile).join("concurrent");
        assert!(bin.exists(), "missing {}", bin.display());

        let out = Command::new(&bin).output().expect("run concurrent");
        assert!(
            out.status.success(),
            "binary exited non-zero (release={release}): stderr={}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8(out.stdout).expect("non-utf8 stdout");

        let mut seen: HashSet<(i64, i64)> = HashSet::new();
        let mut bad: Vec<&str> = Vec::new();
        for line in stdout.lines() {
            let Some(rest) = line.strip_prefix('G') else {
                bad.push(line);
                continue;
            };
            let Some((id_text, count_text)) = rest.split_once(':') else {
                bad.push(line);
                continue;
            };
            match (id_text.parse::<i64>(), count_text.parse::<i64>()) {
                (Ok(id), Ok(i)) => {
                    seen.insert((id, i));
                }
                _ => bad.push(line),
            }
        }
        assert!(
            bad.is_empty(),
            "found {} torn line(s) (release={release}) — first 5: {:?}",
            bad.len(),
            &bad.iter().take(5).collect::<Vec<_>>()
        );
        assert_eq!(
            seen.len(),
            16 * 256,
            "unique line count mismatch (release={release}): saw {} lines",
            seen.len()
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}
