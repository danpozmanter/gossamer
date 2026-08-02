//! Method dispatch on indexed-into-struct-field receivers.
//!
//! Regression coverage: when a method receiver is shaped like
//! `record.field[k]` (Index expression whose base is a Field
//! expression on a Path) and typeck leaves the chained
//! expression types as inference variables, the MIR `len`
//! dispatch used to fall through to the generic `gos_rt_len`
//! arm - which interprets the runtime value as a Vec/Slice/Array
//! header. For a `String` element, that yields garbage from a
//! C-string pointer reinterpreted as a length-prefixed buffer.
//!
//! The fix walks the HIR Path -> Field chain through the local
//! table and `struct_field_tys` so the element type survives
//! typeck's Var fall-through and the dispatch lands on
//! `gos_rt_str_len`.

#![allow(missing_docs)]

use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

const PER_RUN_TIMEOUT: Duration = Duration::from_mins(1);

fn gos_bin() -> PathBuf {
    PathBuf::from(env::var("CARGO_BIN_EXE_gos").expect("CARGO_BIN_EXE_gos"))
}

fn fresh_dir(tag: &str) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = env::temp_dir().join(format!("gos-ifd-{pid}-{n}-{tag}", pid = std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

fn run_with_timeout(mut child: std::process::Child) -> (String, String, Option<i32>) {
    let deadline = Instant::now() + PER_RUN_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    break;
                }
                std::thread::sleep(Duration::from_millis(40));
            }
            Err(_) => break,
        }
    }
    let out = child.wait_with_output().expect("wait_with_output");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code(),
    )
}

fn run_vm(src: &Path) -> (String, String, Option<i32>) {
    let child = Command::new(gos_bin())
        .arg(src)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn gos");
    run_with_timeout(child)
}

#[cfg(unix)]
fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(p).is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(p: &Path) -> bool {
    p.extension()
        .and_then(|s| s.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("exe"))
}

fn build_native(src: &Path, release: bool, scratch: &Path) -> Result<PathBuf, String> {
    let mut cmd = Command::new(gos_bin());
    cmd.arg("build");
    if release {
        cmd.arg("--release");
    }
    cmd.arg("--out-dir").arg(scratch).arg(src);
    let out = cmd.output().expect("spawn gos build");
    if !out.status.success() {
        return Err(format!(
            "gos build {flag} failed:\n  stderr: {}",
            String::from_utf8_lossy(&out.stderr),
            flag = if release { "--release" } else { "" },
        ));
    }
    let mut binaries = Vec::new();
    for entry in fs::read_dir(scratch)
        .map_err(|e| format!("read_dir: {e}"))?
        .flatten()
    {
        let p = entry.path();
        if p.is_file() && is_executable(&p) {
            binaries.push(p);
        }
    }
    binaries
        .into_iter()
        .next()
        .ok_or_else(|| format!("no binary in {}", scratch.display()))
}

fn run_native(bin: &Path) -> (String, String, Option<i32>) {
    let child = Command::new(bin)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn binary");
    run_with_timeout(child)
}

fn assert_three_tier_stdout(tag: &str, source: &str, expected: &str) {
    let dir = fresh_dir(tag);
    let src = dir.join(format!("{tag}.gos"));
    let mut f = fs::File::create(&src).expect("write src");
    f.write_all(source.as_bytes()).unwrap();
    drop(f);

    let vm = run_vm(&src);
    let cl_dir = dir.join("cl");
    fs::create_dir_all(&cl_dir).unwrap();
    let cl_bin = build_native(&src, false, &cl_dir).expect("cranelift build");
    let cl = run_native(&cl_bin);
    let ll_dir = dir.join("ll");
    fs::create_dir_all(&ll_dir).unwrap();
    let ll_bin = build_native(&src, true, &ll_dir).expect("llvm build");
    let ll = run_native(&ll_bin);

    let _ = fs::remove_dir_all(&dir);

    for (name, run) in [("vm", &vm), ("cranelift", &cl), ("llvm", &ll)] {
        assert_eq!(
            run.0.trim_end(),
            expected.trim_end(),
            "[{tag}/{name}] stdout disagrees with expected.\n\
             expected:\n{expected}\n\
             got stdout:\n{stdout}\n\
             stderr:\n{stderr}\n\
             exit: {code:?}",
            stdout = run.0,
            stderr = run.1,
            code = run.2,
        );
    }
}

#[test]
fn string_len_on_indexed_struct_vec_field_dispatches_correctly() {
    let src = r#"
use std::errors

struct Bag {
    pub content: String,
    pub count: i64,
    pub items: Vec<String>,
}

fn build() -> Result<Bag, errors::Error> {
    let mut v: Vec<String> = Vec::from([]).to_vec()
    v.push("hello world".to_string())
    Ok(Bag { content: "ok".to_string(), count: 1, items: v })
}

fn main() {
    let r = build().unwrap_or(Bag { content: "".to_string(), count: 0, items: [].to_vec() })
    let k: i64 = 0
    println!("len={}", r.items[k].len())
}
"#;
    assert_three_tier_stdout("idx_field_str_len_unwrap_or", src, "len=11");
}

#[test]
fn string_len_on_indexed_struct_vec_field_via_question_mark() {
    let src = r#"
use std::errors

struct Bag {
    pub items: Vec<String>,
}

fn build() -> Result<Bag, errors::Error> {
    let mut v: Vec<String> = Vec::from([]).to_vec()
    v.push("hi there".to_string())
    Ok(Bag { items: v })
}

fn run() -> Result<(), errors::Error> {
    let r = build()?
    let k: i64 = 0
    println!("len={}", r.items[k].len())
    Ok(())
}

fn main() {
    run().unwrap_or(())
}
"#;
    assert_three_tier_stdout("idx_field_str_len_qmark", src, "len=8");
}

#[test]
fn user_methods_on_indexed_vec_elements_match_across_tiers() {
    // Regression for #124. Native lowering previously emitted a call to an
    // undeclared `@is_halted` symbol for the shared method and failed to
    // write a mutable method receiver back through `amplifiers[index]`.
    let src = r#"
struct Amplifier {
    pub halted: bool,
    pub output: i64,
}

impl Amplifier {
    fn run(&mut self, input: i64) {
        self.output = input + 1
        self.halted = true
    }

    fn is_halted(self) -> bool {
        self.halted
    }
}

fn main() {
    let mut amplifiers: Vec<Amplifier> = Vec::from([
        Amplifier { halted: false, output: 0 },
        Amplifier { halted: false, output: 0 },
    ])
    let index = 1
    amplifiers[index].run(41)
    println!("halted={} output={}", amplifiers[index].is_halted(), amplifiers[index].output)
}
"#;
    assert_three_tier_stdout(
        "user_methods_on_indexed_vec_elements",
        src,
        "halted=true output=42",
    );
}
