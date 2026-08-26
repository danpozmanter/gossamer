//! Advisory file locks are enforced between processes, not merely within one.
//!
//! A single-process test proves nothing here: the lock the runtime takes has
//! to be the operating system's, so this builds one native binary and runs
//! two of it against the same file. The holder announces on stdout that it
//! has the lock and then blocks on stdin, so the contender runs while the
//! lock is provably held rather than after a guessed delay.

#![allow(missing_docs)]

use std::env;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

const LOCKER_SOURCE: &str = r#"use std::{env, errors, fs, io}

fn main() -> Result<(), errors::Error> {
    let args = env::args()
    let mode = args.first().unwrap_or("")
    let path = args.get(1).unwrap_or("")
    let opts = fs::OpenOptions::new()
    let opts = opts.read(true)
    let opts = opts.write(true)
    let opts = opts.create(true)
    let f = opts.open(path)?
    if mode == "hold" {
        println("held {}", f.try_lock_exclusive()?)
        let mut line = ""
        let _ = io::stdin().read_line(&mut line)
        f.unlock()?
    } else {
        println("acquired {}", f.try_lock_exclusive()?)
    }
    f.close()
    Ok(())
}
"#;

fn gos_bin() -> PathBuf {
    PathBuf::from(env::var("CARGO_BIN_EXE_gos").expect("CARGO_BIN_EXE_gos"))
}

fn contend(locker: &PathBuf, db: &PathBuf) -> String {
    let out = Command::new(locker)
        .arg("try")
        .arg(db)
        .output()
        .expect("spawn contender");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn an_exclusive_lock_is_enforced_against_another_process() {
    let dir = env::temp_dir().join(format!("gos-file-lock-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create scratch dir");
    let source = dir.join("locker.gos");
    fs::write(&source, LOCKER_SOURCE).expect("write locker source");

    let build = Command::new(gos_bin())
        .arg("build")
        .arg(&source)
        .output()
        .expect("spawn gos build");
    assert!(
        build.status.success(),
        "gos build failed: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let locker = dir.join("target").join("debug").join(if cfg!(windows) {
        "locker.exe"
    } else {
        "locker"
    });
    assert!(locker.is_file(), "no built locker at {}", locker.display());

    let db = dir.join("pages.db");
    let mut holder = Command::new(&locker)
        .arg("hold")
        .arg(&db)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn holder");

    // The holder's own line is the synchronisation edge: it is written after
    // the lock is taken and before the read that parks the process.
    let mut announced = String::new();
    let mut out = BufReader::new(holder.stdout.take().expect("holder stdout"));
    out.read_line(&mut announced).expect("read holder line");
    assert_eq!(
        announced.trim(),
        "held true",
        "holder failed to take the lock"
    );

    assert_eq!(
        contend(&locker, &db),
        "acquired false",
        "a second process took a lock the first holds"
    );

    // Releasing the holder's stdin ends its read, so it unlocks and exits.
    drop(holder.stdin.take());
    let status = holder.wait().expect("holder exit");
    assert!(status.success(), "holder exited with {status:?}");

    assert_eq!(
        contend(&locker, &db),
        "acquired true",
        "the lock outlived the process that held it"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_disjoint_byte_range_is_free_while_another_is_held() {
    let dir = env::temp_dir().join(format!("gos-range-lock-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create scratch dir");
    let source = dir.join("ranges.gos");
    fs::write(
        &source,
        r#"use std::{env, errors, fs, io}

fn main() -> Result<(), errors::Error> {
    let args = env::args()
    let mode = args.first().unwrap_or("")
    let path = args.get(1).unwrap_or("")
    let opts = fs::OpenOptions::new()
    let opts = opts.read(true)
    let opts = opts.write(true)
    let opts = opts.create(true)
    let f = opts.open(path)?
    if mode == "hold" {
        println("held {}", f.try_lock_range(0, 16, true)?)
        let mut line = ""
        let _ = io::stdin().read_line(&mut line)
        f.unlock_range(0, 16)?
    } else {
        println("same {} disjoint {}", f.try_lock_range(0, 16, true)?, f.try_lock_range(64, 16, true)?)
    }
    f.close()
    Ok(())
}
"#,
    )
    .expect("write ranges source");

    let build = Command::new(gos_bin())
        .arg("build")
        .arg(&source)
        .output()
        .expect("spawn gos build");
    assert!(
        build.status.success(),
        "gos build failed: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let locker = dir.join("target").join("debug").join(if cfg!(windows) {
        "ranges.exe"
    } else {
        "ranges"
    });

    let db = dir.join("pages.db");
    let mut holder = Command::new(&locker)
        .arg("hold")
        .arg(&db)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn holder");
    let mut announced = String::new();
    let mut out = BufReader::new(holder.stdout.take().expect("holder stdout"));
    out.read_line(&mut announced).expect("read holder line");
    assert_eq!(announced.trim(), "held true");

    let contended = Command::new(&locker)
        .arg("try")
        .arg(&db)
        .output()
        .expect("spawn contender");
    assert_eq!(
        String::from_utf8_lossy(&contended.stdout).trim(),
        "same false disjoint true",
        "range locks must exclude only their own bytes"
    );

    let mut stdin = holder.stdin.take().expect("holder stdin");
    let _ = stdin.write_all(b"\n");
    drop(stdin);
    let _ = holder.wait();
    let _ = fs::remove_dir_all(&dir);
}
