//! A newline-terminated write reaches the descriptor before the program does.
//!
//! The stdout buffer coalesces the several writes a formatted line arrives as,
//! and drains on the line's newline. A program that announces a line and then
//! blocks - a server printing the address the kernel assigned it, a worker
//! reporting readiness to a supervisor - is therefore visible to whatever is
//! reading its stdout while it is still running.
//!
//! The bytecode VM reaches fd 1 through Rust's line-buffered `stdout`, so it
//! has always behaved this way; the compiled tiers reach it through the
//! runtime's own buffer. This gate covers all of them, because a drain that
//! stops being emitted on one tier is invisible to tier parity: the transcript
//! a finished process leaves behind is identical either way.

#![allow(missing_docs)]

use std::env;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

/// Bound on how long the announcement may take to arrive. It is the oracle,
/// not a settling delay: the property under test is that the line appears
/// while the program is still blocked, and an unflushed line never appears at
/// all, so without a bound the failure is a hung test rather than a diagnosis.
const ANNOUNCEMENT_BOUND: Duration = Duration::from_secs(30);

const PROGRAM: &str = r#"
use std::io

fn main() {
    println("ready")
    let mut line = ""
    let _ = io::stdin().read_line(&mut line)
    println("got: {}", line.trim())
}
"#;

fn gos_bin() -> PathBuf {
    PathBuf::from(env::var("CARGO_BIN_EXE_gos").expect("CARGO_BIN_EXE_gos"))
}

/// Reads the announcement, releases the child by feeding its stdin, and
/// returns the whole transcript.
fn transcript_of(mut child: Child, label: &str) -> String {
    let stdout = child.stdout.take().expect("piped stdout");
    let (tx, rx) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut first = String::new();
        let announced = reader.read_line(&mut first).is_ok() && !first.is_empty();
        let _ = tx.send(if announced { Some(first.clone()) } else { None });
        let mut rest = String::new();
        let _ = reader.read_to_string(&mut rest);
        format!("{first}{rest}")
    });

    let announcement = match rx.recv_timeout(ANNOUNCEMENT_BOUND) {
        Ok(Some(line)) => line,
        Ok(None) => {
            let _ = child.kill();
            panic!("{label}: stdout closed before the announcement arrived");
        }
        Err(_) => {
            let _ = child.kill();
            panic!(
                "{label}: no announcement within {ANNOUNCEMENT_BOUND:?} while the program was \
                 blocked - the newline-terminated write did not leave the stdout buffer"
            );
        }
    };
    assert_eq!(announcement, "ready\n", "{label}: announcement differs");

    let mut stdin = child.stdin.take().expect("piped stdin");
    stdin.write_all(b"go\n").expect("feed child stdin");
    drop(stdin);

    let status = child.wait().expect("wait for child");
    assert!(status.success(), "{label}: exited {:?}", status.code());
    reader.join().expect("stdout reader thread")
}

fn spawn_piped(command: &mut Command) -> Child {
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn program")
}

fn build(dir: &Path, src: &Path, release: bool) -> PathBuf {
    let mut command = Command::new(gos_bin());
    command.arg("build");
    if release {
        command.arg("--release");
    }
    let built = command
        .arg("--out-dir")
        .arg(dir)
        .arg(src)
        .output()
        .expect("spawn gos build");
    assert!(
        built.status.success(),
        "gos build failed: {}",
        String::from_utf8_lossy(&built.stderr)
    );
    dir.join("announce")
}

#[test]
fn an_announced_line_is_visible_while_the_program_blocks() {
    let dir = env::temp_dir().join(format!("gos-stdout-line-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    let src = dir.join("announce.gos");
    std::fs::write(&src, PROGRAM).expect("write source");

    let expected = "ready\ngot: go\n";

    let vm = spawn_piped(Command::new(gos_bin()).arg("run").arg(&src));
    assert_eq!(transcript_of(vm, "gos run"), expected);

    let debug_dir = dir.join("debug");
    std::fs::create_dir_all(&debug_dir).expect("create debug dir");
    let debug = build(&debug_dir, &src, false);
    assert_eq!(
        transcript_of(spawn_piped(&mut Command::new(&debug)), "gos build"),
        expected
    );

    let release_dir = dir.join("release");
    std::fs::create_dir_all(&release_dir).expect("create release dir");
    let release = build(&release_dir, &src, true);
    assert_eq!(
        transcript_of(
            spawn_piped(&mut Command::new(&release)),
            "gos build --release"
        ),
        expected
    );

    let _ = std::fs::remove_dir_all(&dir);
}
