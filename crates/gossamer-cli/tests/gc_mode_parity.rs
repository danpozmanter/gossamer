//! GC-mode parity smoke test.
//!
//! Runs a curated set of allocation-heavy examples under both
//! `GOSSAMER_GC_MODE=stw` (no concurrent cycle from the
//! allocation-driven `drive_incremental` path) and the default
//! (concurrent) mode. Asserts the observed stdout matches across
//! modes. The runtime's `gos_rt_write_barrier` greys the new target
//! every time the MIR `insert_gc_barriers` pass inserted a barrier
//! — if the barrier-insertion pass were missing or under-emitting,
//! concurrent-mode runs would intermittently lose live objects and
//! diverge from STW.
//!
//! Lightweight by design: a full parity walk over `tier_parity.rs`'s
//! SPECS is heavier than needed because most examples don't
//! allocate enough to trip a cycle. The shortlist below exercises
//! `Vec` / `HashMap` / `String` allocation in tight loops, which is where
//! a missing barrier would actually corrupt output.

#![allow(missing_docs)]

use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

fn gos_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_gos"))
}

fn workspace_root() -> PathBuf {
    let mut here = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    here.pop();
    here.pop();
    here
}

fn run_with_mode(rel_path: &str, mode: Option<&str>) -> (String, Option<i32>) {
    let src = workspace_root().join(rel_path);
    let mut cmd = Command::new(gos_bin());
    cmd.arg("run").arg(&src);
    if let Some(m) = mode {
        cmd.env("GOSSAMER_GC_MODE", m);
    } else {
        cmd.env_remove("GOSSAMER_GC_MODE");
    }
    let mut child = cmd
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn gos run");
    let deadline = std::time::Instant::now() + Duration::from_secs(45);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("{rel_path} (mode={mode:?}) did not terminate within 45s");
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => panic!("wait error: {e}"),
        }
    }
    let out = child.wait_with_output().expect("wait_with_output");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        out.status.code(),
    )
}

/// Examples curated for GC-mode parity: each allocates heap objects
/// (`String`, `Vec`, `HashMap`) inside loops or struct constructors
/// — exactly the surface the write-barrier protects.
const GC_PARITY_EXAMPLES: &[&str] = &[
    "examples/hello_world.gos",
    "examples/factorial.gos",
    "examples/generic_struct.gos",
    "examples/data_structures.gos",
    "examples/reverse_string.gos",
];

#[test]
fn concurrent_and_stw_modes_produce_identical_stdout() {
    let mut failures: Vec<String> = Vec::new();
    for path in GC_PARITY_EXAMPLES {
        let full = workspace_root().join(path);
        if !full.exists() {
            eprintln!("skip (missing): {path}");
            continue;
        }
        let (concurrent_out, concurrent_code) = run_with_mode(path, Some("concurrent"));
        let (stw_out, stw_code) = run_with_mode(path, Some("stw"));
        if concurrent_code != stw_code {
            failures.push(format!(
                "{path}: exit divergence concurrent={concurrent_code:?} stw={stw_code:?}",
            ));
            continue;
        }
        if concurrent_out != stw_out {
            failures.push(format!(
                "{path}: stdout divergence\n  concurrent: {concurrent_out:?}\n  stw:        {stw_out:?}",
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} GC-mode parity failures:\n{}",
        failures.len(),
        failures.join("\n\n"),
    );
}
