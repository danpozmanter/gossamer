//! The escape corpus, run through `std::sandbox`.
//!
//! The native `gosbox` application and `std::sandbox` are two front
//! ends over one library, so the same attempt must produce the same
//! verdict through either. If it cannot, the module is missing surface,
//! and that is what this gate catches - it is the acceptance test for
//! the Gossamer surface, not a second copy of the enforcement tests.
//!
//! `gosbox` is a separate application, so its corpus and its Gossamer
//! front end live in its own checkout. `GOSBOX_HOME` names that
//! checkout, defaulting to the conventional `~/dev/gosbox`; a machine
//! without one reports the gate as skipped rather than failing on a
//! missing file.

#![allow(missing_docs)]

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn gos_bin() -> PathBuf {
    PathBuf::from(env::var("CARGO_BIN_EXE_gos").expect("CARGO_BIN_EXE_gos"))
}

/// The `gosbox` product checkout, or `None` when this machine has none.
fn product_home() -> Option<PathBuf> {
    let named = env::var_os("GOSBOX_HOME").map(PathBuf::from).or_else(|| {
        env::var_os("HOME").map(|home| PathBuf::from(home).join("dev").join("gosbox"))
    })?;
    named
        .join("escapes")
        .join("corpus.gos")
        .is_file()
        .then_some(named)
}

fn run_corpus(home: &Path) -> String {
    let out = Command::new(gos_bin())
        .arg("run")
        .arg(home.join("escapes").join("corpus.gos"))
        .output()
        .expect("run the corpus");
    assert!(
        out.status.success(),
        "the corpus failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn every_escape_is_denied_through_the_gossamer_surface() {
    let Some(home) = product_home() else {
        return;
    };
    let report = run_corpus(&home);
    if report.contains("unenforced:") {
        // A host that enforces nothing says so, and the corpus has
        // nothing to assert there.
        return;
    }
    assert!(
        !report.contains("ALLOWED"),
        "an escape succeeded through std::sandbox:\n{report}"
    );
    assert!(
        !report.contains("BROKEN"),
        "the sandbox refused an operation it is supposed to permit:\n{report}"
    );
    for expected in [
        "a path outside the policy cannot be read",
        "a write outside the policy fails",
        "a raw device node is unreachable",
        "an inherited credential variable is absent",
        "the network is unreachable",
        "a grant never lifts a denial",
    ] {
        assert!(
            report.contains(&format!("denied  {expected}")),
            "`{expected}` was not reported denied:\n{report}"
        );
    }
    assert!(
        report.contains("works   a granted directory is writable"),
        "the sandbox must still permit what the policy grants:\n{report}"
    );
}

#[test]
fn the_gossamer_front_end_enforces_what_the_native_one_does() {
    let Some(home) = product_home() else {
        return;
    };
    let front_end = home.join("src").join("main.gos");
    let level = Command::new(gos_bin())
        .arg("run")
        .arg(&front_end)
        .arg("doctor")
        .output()
        .expect("run the front end's doctor");
    let text = String::from_utf8_lossy(&level.stdout).into_owned();
    assert!(text.contains("Max level:"), "{text}");

    if !text.contains("Max level:       standard") && !text.contains("Max level:       strict") {
        return;
    }
    let escape = env::temp_dir().join("gos-frontend-acceptance-escape.txt");
    let _ = std::fs::remove_file(&escape);
    let attempted = Command::new(gos_bin())
        .arg("run")
        .arg(&front_end)
        .arg("-q")
        .arg("--")
        .arg("/bin/sh")
        .arg("-c")
        .arg(format!("echo escaped > {}", escape.display()))
        .output()
        .expect("run the front end");
    assert_ne!(
        attempted.status.code().unwrap_or(0),
        0,
        "{}",
        String::from_utf8_lossy(&attempted.stderr)
    );
    assert!(
        !escape.exists(),
        "the Gossamer front end must enforce what the native one does"
    );
}
