//! `--comptime-io` end to end.
//!
//! A `comptime` region folds on the bytecode VM while the program is
//! being compiled, so what it can reach is bounded by the level in
//! force rather than by what the compiling user happens to be allowed
//! to do. Each case here runs `gos check` on a program whose comptime
//! region attempts one capability and asserts the level's verdict -
//! and, where the attempt would leave a trace, that no trace exists.

use std::path::{Path, PathBuf};
use std::process::Command;

fn gos_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_gos"))
}

struct Checked {
    ok: bool,
    output: String,
}

fn check(dir: &Path, file: &str, level: Option<&str>) -> Checked {
    let mut command = Command::new(gos_binary());
    command.current_dir(dir).arg("check");
    if let Some(level) = level {
        command.arg(format!("--comptime-io={level}"));
    }
    let out = command.arg(file).output().expect("run gos check");
    Checked {
        ok: out.status.success(),
        output: format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    }
}

fn write(dir: &Path, name: &str, body: &str) {
    std::fs::write(dir.join(name), body).expect("write fixture");
}

fn workspace(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("gos-comptime-policy-{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create fixture dir");
    dir
}

const EXEC: &str = r#"use std::process

fn main() {
    let n = comptime { let _ = process::run(&"/bin/sh", &#["-c", "true"]); 1 }
    println!("{}", n)
}
"#;

const WRITE: &str = r#"use std::fs

fn main() {
    let n = comptime { let _ = fs::write(&"escaped.txt", &#[120u8]); 1 }
    println!("{}", n)
}
"#;

const NETWORK: &str = r#"use std::net

fn main() {
    let n = comptime { let _ = net::TcpStream::connect(&"127.0.0.1:1"); 1 }
    println!("{}", n)
}
"#;

const ENV_MUTATION: &str = r#"use std::env

fn main() {
    let n = comptime { env::set_var(&"GOS_COMPTIME_PROBE", &"1"); 1 }
    println!("{}", n)
}
"#;

const READ_OUT_OF_TREE: &str = r#"use std::fs

fn main() {
    let s = comptime { fs::read_to_string(&"../outside.txt").unwrap_or("?") }
    println!("{}", s)
}
"#;

const READ_IN_TREE: &str = r#"use std::fs

fn main() {
    let s = comptime { fs::read_to_string(&"asset.txt").unwrap_or("?") }
    println!("{}", s)
}
"#;

const READ_THROUGH_SYMLINK: &str = r#"use std::fs

fn main() {
    let s = comptime { fs::read_to_string(&"link.txt").unwrap_or("?") }
    println!("{}", s)
}
"#;

const CODEGEN: &str = r#"comptime fn emit() -> String {
    "\"forty-two\""
}

fn label() -> String {
    codegen!(emit())
}

fn main() {
    println!("{}", label())
}
"#;

fn denied(checked: &Checked, operation: &str) {
    assert!(
        !checked.ok,
        "expected `{operation}` to be denied, but check succeeded:\n{}",
        checked.output
    );
    assert!(
        checked.output.contains("GX0010"),
        "expected a GX0010 capability denial for `{operation}`:\n{}",
        checked.output
    );
    assert!(
        checked.output.contains(operation),
        "the denial must name the builtin `{operation}`:\n{}",
        checked.output
    );
}

fn permitted(checked: &Checked, what: &str) {
    assert!(
        checked.ok,
        "expected `{what}` to be permitted:\n{}",
        checked.output
    );
}

#[test]
fn confined_denies_write_exec_network_and_env_mutation() {
    let dir = workspace("confined-denials");
    write(&dir, "exec.gos", EXEC);
    write(&dir, "write.gos", WRITE);
    write(&dir, "network.gos", NETWORK);
    write(&dir, "env.gos", ENV_MUTATION);

    denied(&check(&dir, "exec.gos", None), "process::run");
    denied(&check(&dir, "write.gos", None), "fs::write");
    denied(&check(&dir, "network.gos", None), "net::TcpStream::connect");
    denied(&check(&dir, "env.gos", None), "env::set_var");

    assert!(
        !dir.join("escaped.txt").exists(),
        "a denied comptime write must leave no file behind"
    );
}

#[test]
fn none_denies_the_same_capabilities_and_reads_as_well() {
    let dir = workspace("none-denials");
    write(&dir, "exec.gos", EXEC);
    write(&dir, "read.gos", READ_IN_TREE);
    write(&dir, "asset.txt", "embedded\n");

    denied(&check(&dir, "exec.gos", Some("none")), "process::run");
    denied(&check(&dir, "read.gos", Some("none")), "fs::read_to_string");
    permitted(
        &check(&dir, "read.gos", None),
        "an in-tree read at confined",
    );
}

#[test]
fn confined_permits_an_in_tree_read_and_denies_one_that_leaves_the_tree() {
    let dir = workspace("confined-reads");
    write(&dir, "asset.txt", "embedded\n");
    write(&dir, "in.gos", READ_IN_TREE);
    write(&dir, "out.gos", READ_OUT_OF_TREE);
    std::fs::write(dir.join("..").join("outside.txt"), "secret\n").expect("write outside");

    permitted(&check(&dir, "in.gos", None), "an in-tree read");
    denied(&check(&dir, "out.gos", None), "fs::read_to_string");
}

#[cfg(unix)]
#[test]
fn a_symlink_out_of_the_tree_is_denied_at_its_target() {
    let dir = workspace("symlink-escape");
    let outside = dir.join("..").join("gos-comptime-outside.txt");
    std::fs::write(&outside, "secret\n").expect("write outside");
    std::os::unix::fs::symlink(&outside, dir.join("link.txt")).expect("symlink");
    write(&dir, "link.gos", READ_THROUGH_SYMLINK);

    denied(&check(&dir, "link.gos", None), "fs::read_to_string");
}

#[test]
fn full_restores_every_denied_capability() {
    let dir = workspace("full-escape");
    write(&dir, "write.gos", WRITE);
    permitted(
        &check(&dir, "write.gos", Some("full")),
        "a comptime write at --comptime-io=full",
    );
}

#[test]
fn codegen_is_unaffected_by_the_strictest_level() {
    let dir = workspace("codegen-none");
    write(&dir, "gen.gos", CODEGEN);
    permitted(
        &check(&dir, "gen.gos", Some("none")),
        "codegen! at --comptime-io=none",
    );

    let out = Command::new(gos_binary())
        .current_dir(&dir)
        .arg("run")
        .arg("--comptime-io=none")
        .arg("gen.gos")
        .output()
        .expect("run gos run");
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "forty-two");
}

#[test]
fn a_manifest_may_tighten_the_posture_and_may_never_loosen_it() {
    let dir = workspace("manifest-resolution");
    std::fs::create_dir_all(dir.join("src")).expect("create src");
    write(
        &dir,
        "project.toml",
        "[project]\nid = \"example.com/policy\"\nversion = \"0.1.0\"\ncomptime-io = \"full\"\n",
    );
    std::fs::write(dir.join("src").join("main.gos"), EXEC).expect("write entry");
    denied(&check(&dir, "src/main.gos", None), "process::run");

    write(
        &dir,
        "project.toml",
        "[project]\nid = \"example.com/policy\"\nversion = \"0.1.0\"\ncomptime-io = \"none\"\n",
    );
    std::fs::write(dir.join("src").join("read.gos"), READ_IN_TREE).expect("write entry");
    std::fs::write(dir.join("src").join("asset.txt"), "embedded\n").expect("write asset");
    denied(&check(&dir, "src/read.gos", None), "fs::read_to_string");
}
