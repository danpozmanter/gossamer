//! `cohort { }` through `gos build --release`.
//!
//! Tier parity compares the VM, the JIT, and the debug AOT build. The
//! release path is a different pipeline - full LLVM `-O3`, static-musl -
//! and structured concurrency reaches it through runtime shims that a
//! missing dispatch entry would silently zero. This gate compiles a
//! program that uses the whole surface and checks its transcript, so a
//! shim that stops being wired fails here rather than in a user's build.

#![allow(missing_docs)]

use std::env;
use std::path::PathBuf;
use std::process::Command;

fn gos_bin() -> PathBuf {
    PathBuf::from(env::var("CARGO_BIN_EXE_gos").expect("CARGO_BIN_EXE_gos"))
}

const PROGRAM: &str = r#"
use std::{errors, runtime, time}

fn work(n: i64) -> Result<i64, errors::Error> {
    if n < 0 {
        return Err(errors::newf("negative {}", n))
    }
    Ok(n * 2)
}

fn nap() -> Result<i64, errors::Error> {
    time::sleep(30_000)
    Ok(0)
}

fn polls() -> Result<i64, errors::Error> {
    let mut i = 0
    while i < 1000 {
        if runtime::cohort_cancelled() {
            return Ok(i)
        }
        time::sleep(5)
        i += 1
    }
    Ok(i)
}

fn main() {
    let ok = cohort {
        let _a = spawn(|| work(1))
        let _b = spawn(|| work(2))
    }
    println!("ok: {:?}", ok)

    let failed = cohort {
        let _a = spawn(|| work(-1))
        let _b = spawn(|| polls())
    }
    println!("failed: {:?}", failed)

    let collected = cohort(policy: Policy::CollectAll) {
        let _a = spawn(|| work(-1))
        let _b = spawn(|| work(-2))
    }
    println!("collected: {:?}", collected)

    let bounded = cohort(timeout: 100) {
        let _s = spawn(|| nap())
    }
    println!("bounded: {}", bounded.is_err())

    let isolated = cohort(context: Context::Isolated) {
        let _a = spawn(|| work(3))
    }
    println!("isolated: {:?}", isolated)
}
"#;

const EXPECTED: &str = "ok: Ok(())\n\
     failed: Err(negative -1)\n\
     collected: Err(negative -1; negative -2)\n\
     bounded: true\n\
     isolated: Ok(())\n";

/// Cancellation racing a park: a receiver checks its cohort, then
/// registers as a waiter. A cancel landing between the two once found no
/// waiter to wake and left the child parked for good, which showed up as
/// a release build that finished its work and then hung at exit. The
/// shape is timing-dependent, so this runs the program repeatedly.
const RACE_PROGRAM: &str = r#"
use std::{errors, time}

fn fail_now() -> Result<i64, errors::Error> {
    Err(errors::new("stop"))
}

fn drain(rx: Receiver<i64>) -> Result<i64, errors::Error> {
    let mut seen = 0
    while let Some(_v) = rx.recv() {
        seen += 1
    }
    Ok(seen)
}

fn nap() -> Result<i64, errors::Error> {
    time::sleep(30_000)
    Ok(1)
}

fn main() {
    let (_tx, rx) = channel(1)
    let blocked = cohort {
        let _f = spawn(|| fail_now())
        let _d = spawn(|| drain(rx))
    }
    let sleeping = cohort {
        let _f = spawn(|| fail_now())
        let _s = spawn(|| nap())
    }
    println!("{} {}", blocked.is_err(), sleeping.is_err())
}
"#;

#[test]
fn cancelling_a_parked_child_never_strands_it() {
    let dir = env::temp_dir().join(format!("gos-cohort-race-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    let src = dir.join("cohort_race.gos");
    std::fs::write(&src, RACE_PROGRAM).expect("write source");

    let built = Command::new(gos_bin())
        .arg("build")
        .arg("--release")
        .arg("--out-dir")
        .arg(&dir)
        .arg(&src)
        .output()
        .expect("spawn gos build");
    assert!(
        built.status.success(),
        "gos build --release failed: {}",
        String::from_utf8_lossy(&built.stderr)
    );

    let binary = dir.join("cohort_race");
    for attempt in 0..8 {
        let run = Command::new(&binary).output().expect("run release binary");
        assert!(
            run.status.success(),
            "attempt {attempt} exited {:?}: stderr={}",
            run.status.code(),
            String::from_utf8_lossy(&run.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&run.stdout),
            "true true\n",
            "attempt {attempt} transcript differs"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cohort_surface_runs_in_a_release_build() {
    let dir = env::temp_dir().join(format!("gos-cohort-release-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    let src = dir.join("cohort_release.gos");
    std::fs::write(&src, PROGRAM).expect("write source");

    let built = Command::new(gos_bin())
        .arg("build")
        .arg("--release")
        .arg("--out-dir")
        .arg(&dir)
        .arg(&src)
        .output()
        .expect("spawn gos build");
    assert!(
        built.status.success(),
        "gos build --release failed: {}",
        String::from_utf8_lossy(&built.stderr)
    );

    let binary = dir.join("cohort_release");
    let run = Command::new(&binary).output().expect("run release binary");
    assert!(
        run.status.success(),
        "release binary exited {:?}: stderr={}",
        run.status.code(),
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        EXPECTED,
        "release transcript differs; stderr={}",
        String::from_utf8_lossy(&run.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}
