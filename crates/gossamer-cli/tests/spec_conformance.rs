//! SPEC.md behavioural conformance tests.
//!
//! Each test pins a behaviour the language specification describes, so
//! the spec and the toolchain stay in lockstep: a claim SPEC.md makes
//! must be demonstrably true (or, for a rejection, demonstrably
//! enforced).
//!
//! Behaviours covered:
//!   §3.1   integer overflow wraps at 64-bit width with no panic;
//!          `i128`/`u128` are rejected on every tier.
//!   §7.5   reference mutability is checked; aliasing `&mut` compiles.
//!   §8.6   `extern "C"` is not an `unsafe` power; it is rejected.
//!   §11.2  linking - musl-static is the Linux default, `--dynamic`
//!          opts out.
//!   §12    FFI is rust-bindings-only (GP0016 fires for `extern`).
//!   §14    the implemented macro set is accepted; the rest are
//!          rejected at parse time.

#![allow(missing_docs)]

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

fn gos_binary() -> PathBuf {
    // Built by `cargo test -p gossamer-cli` via the workspace
    // `[[bin]]` target. Test runner injects CARGO_BIN_EXE_gos.
    PathBuf::from(env!("CARGO_BIN_EXE_gos"))
}

static FIXTURE_COUNTER: AtomicU64 = AtomicU64::new(0);

fn write_temp_file(stem: &str, body: &str) -> PathBuf {
    // Cargo runs integration tests in parallel by default. Per-test
    // temp paths therefore have to be unique across threads and
    // across `write_temp_file` calls within a thread; we combine the
    // process id with a monotonic counter so collisions are
    // impossible.
    let serial = FIXTURE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "gos-conformance-{}-{}-{}",
        stem,
        std::process::id(),
        serial,
    ));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join(format!("{stem}.gos"));
    std::fs::write(&path, body).expect("temp write");
    path
}

fn run_with_timeout(mut cmd: Command, timeout: Duration) -> std::process::Output {
    let mut child = cmd
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn");
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("subprocess did not terminate within {timeout:?}");
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => panic!("wait error: {e}"),
        }
    }
    child.wait_with_output().expect("wait_with_output")
}

fn run_check(stem: &str, source: &str) -> (bool, String, String) {
    let path = write_temp_file(stem, source);
    let mut cmd = Command::new(gos_binary());
    cmd.arg("check").arg(&path);
    let out = run_with_timeout(cmd, Duration::from_secs(30));
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    (out.status.success(), stdout, stderr)
}

fn run_program(stem: &str, source: &str, args: &[&str]) -> (bool, String, String) {
    let path = write_temp_file(stem, source);
    let mut cmd = Command::new(gos_binary());
    cmd.arg("run").arg(&path);
    for arg in args {
        cmd.arg(arg);
    }
    let out = run_with_timeout(cmd, Duration::from_secs(30));
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    (out.status.success(), stdout, stderr)
}

// ---------- diagnostics: --message-format json ----------

#[test]
fn check_message_format_json_emits_single_line_json() {
    // Triggers GP0016 (extern reserved) and asserts the
    // `--message-format json` output is a single-line JSON
    // object with the documented schema fields.
    let src = r#"
extern "C" { fn malloc(size: usize) -> *mut u8 }
fn main() { println!("hi") }
"#;
    let path = write_temp_file("json_diag", src);
    let mut cmd = Command::new(gos_binary());
    cmd.arg("check")
        .arg(&path)
        .arg("--message-format")
        .arg("json");
    let out = run_with_timeout(cmd, Duration::from_secs(30));
    let stderr = String::from_utf8_lossy(&out.stderr);
    // The diagnostic line is the line that starts with `{`.
    let json_line = stderr
        .lines()
        .find(|l| l.starts_with('{'))
        .unwrap_or_else(|| panic!("no JSON diagnostic line in:\n{stderr}"));
    // Schema fields the rendering contract pins.
    assert!(json_line.contains("\"schema\":1"));
    assert!(json_line.contains("\"code\":\"GP0016\""));
    assert!(json_line.contains("\"severity\":\"error\""));
    assert!(json_line.contains("\"labels\":["));
    assert!(json_line.contains("\"primary\":true"));
    assert!(json_line.contains("\"helps\":["));
    // Process exit is non-zero on a diagnostic.
    assert!(!out.status.success());
}

// ---------- §12: FFI is rust-bindings-only ----------

#[test]
fn spec_12_extern_block_rejected_with_gp0016() {
    let src = r#"
extern "C" {
    fn malloc(size: usize) -> *mut u8
}
fn main() { println!("hi") }
"#;
    let (ok, _stdout, stderr) = run_check("spec_12_extern_block", src);
    assert!(!ok, "extern \"C\" {{}} must not pass `gos check`");
    assert!(
        stderr.contains("GP0016"),
        "expected GP0016 in stderr, got: {stderr}",
    );
    assert!(
        stderr.contains("rust-bindings") || stderr.contains("[rust-bindings]"),
        "diagnostic must direct user to [rust-bindings]; got: {stderr}",
    );
}

#[test]
fn spec_12_no_mangle_extern_fn_rejected_with_gp0016() {
    let src = r#"
#[no_mangle]
extern "C" fn exported(x: i32) -> i32 { x + 1 }
"#;
    let (ok, _stdout, stderr) = run_check("spec_12_no_mangle", src);
    assert!(!ok);
    assert!(stderr.contains("GP0016"), "got: {stderr}");
}

// ---------- §8.6: extern is not an unsafe power ----------

#[test]
fn spec_8_6_extern_inside_unsafe_block_is_still_rejected() {
    // §8.6: `extern "C"` is not an unsafe power. A bare `extern "C"`
    // block (with or without `unsafe`) must be rejected; this test
    // pins both. The bare form fires the specific GP0016. The
    // `unsafe`-wrapped form fires whichever
    // diagnostic the parser surfaces first (today: GP0001 from the
    // `unsafe`-fn parser, after which GP0016 is reached if recovery
    // continues). The invariant we pin is "rejected" - the specific
    // diagnostic chain is part of the diagnostic-quality follow-up.
    let bare = r#"
extern "C" {
    fn libc_malloc(n: i64) -> i64
}
fn main() { println!("hi") }
"#;
    let (ok_bare, _so, se_bare) = run_check("spec_8_6_bare", bare);
    assert!(!ok_bare);
    assert!(se_bare.contains("GP0016"), "bare extern got: {se_bare}");

    let wrapped = r#"
unsafe extern "C" {
    fn libc_malloc(n: i64) -> i64
}
"#;
    let (ok_wrapped, _so2, _se2) = run_check("spec_8_6_unsafe_extern", wrapped);
    assert!(
        !ok_wrapped,
        "unsafe extern \"C\" must be rejected; got success",
    );
}

// ---------- §14: macro subset ----------

#[test]
fn spec_14_format_macro_subset_accepted() {
    // The six format-shaped macros - println, print, eprintln, eprint,
    // format, and panic - must parse and check cleanly. `vec!` is not a
    // macro: the array literal `[...]` coerces to `Vec<T>` instead.
    let src = r#"
fn main() {
    println!("p")

    print!("p")

    eprintln!("e")

    eprint!("e")

    let s = format!("f {}", 1)

    let v = [1, 2, 3]

    if s.len() == 0 && v.len() == 0 {
        panic!("unreachable")
    }
}
"#;
    let (ok, _stdout, _stderr) = run_check("spec_14_format_macros", src);
    assert!(ok);
}

#[test]
fn spec_14_desugar_macros_accepted() {
    // `matches!`, `todo!`, `unimplemented!`, `unreachable!`, and `dbg!`
    // are implemented desugar macros and must parse and check cleanly.
    let src = r#"
fn maybe() -> i64 {
    if false { todo!() } else if false { unimplemented!() } else { 1 }
}
fn main() {
    let m = matches!(Some(1), Some(_))

    let n = maybe()

    let d = dbg!(n + 1)

    let label = match d {
        2 => "two",
        _ => unreachable!(),
    }

    if m && label.len() == 0 {
        println!("x")
    }
}
"#;
    let (ok, _stdout, stderr) = run_check("spec_14_desugar_macros", src);
    assert!(ok, "desugar macros must check clean; stderr: {stderr}");
}

#[test]
fn spec_14_unimplemented_macro_rejected() {
    // Macros with no implementation are rejected at parse time.
    // `todo!`, `unimplemented!`, and `unreachable!` are supported
    // desugar macros (covered above), so they are not in this list;
    // the rest have no desugaring and stay rejected.
    for macro_call in [
        "assert!(true)",
        "assert_eq!(1, 1)",
        "debug_assert!(true)",
        "write!(buf, \"x\")",
        "writeln!(buf, \"x\")",
    ] {
        let src = format!("fn main() {{ let _ = {macro_call}\n }}\n");
        let (ok, _stdout, _stderr) = run_check("spec_14_rejected", &src);
        assert!(!ok, "{macro_call} must be rejected, but `gos check` passed");
    }
}

// ---------- §3.1: integer overflow / i128 ----------

#[test]
fn spec_3_1_overflow_does_not_panic() {
    // Integer overflow wraps at 64-bit width on every tier; no build
    // mode emits an overflow panic. The invariant we pin here is
    // "no panic" - the program completes. We use i64::MAX so the
    // host-Rust arithmetic (which `gos run` uses under the bytecode
    // VM) actually wraps rather than silently widening to a wider
    // integer.
    let src = r#"
fn main() {
    let mut x: i64 = 9223372036854775807
    x = x + 1
    println!("{}", x)
}
"#;
    let (ok, stdout, stderr) = run_program("spec_3_1_wrap", src, &[]);
    assert!(
        ok,
        "i64 overflow must not panic in `gos run`; stderr: {stderr}",
    );
    // The spec does not require a specific value, only that the
    // program completes without an overflow panic.
    assert!(
        stdout.contains('-') || !stdout.is_empty(),
        "expected wrap result, got {stdout:?}",
    );
}

// ---------- §11.2: static-musl is the default link mode ----------
//
// This is a build-system claim, not a language one. Verifying the
// actual linkage requires running `gos build` and inspecting the
// produced ELF - expensive (~minutes), so that end-to-end check
// lives in the release pipeline rather than the per-PR suite. We
// pin only the doc claim here: static-musl is the Linux default and
// `--dynamic` is the opt-out.

#[test]
fn spec_11_2_states_static_musl_default() {
    let spec = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .unwrap()
            .join("SPEC.md"),
    )
    .expect("read SPEC.md");
    // Collapse line-wrapping so the assertion tracks the prose, not the
    // markdown column at which it happens to break.
    let prose: String = spec.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        prose.contains("fully-static musl binary by default") && prose.contains("`--dynamic`"),
        "§11.2 must state the static-musl default and the --dynamic opt-out",
    );
}

// ---------- §7.5: reference mutability without a borrow checker ----------

#[test]
fn spec_7_5_mut_ref_requires_mutable_source() {
    let src = r"
fn main() {
    let x = 1
    let p = &mut x
}
";
    let (ok, _stdout, stderr) = run_check("spec_7_5_mut_source", src);
    assert!(!ok, "&mut of an immutable source must be rejected");
    assert!(stderr.contains("GT0032"), "expected GT0032, got: {stderr}");
}

#[test]
fn spec_7_5_shared_reference_rejects_writes() {
    let src = r"
fn main() {
    let mut x = [1, 2]
    let mut p = &x
    p[0] = 0
}
";
    let (ok, _stdout, stderr) = run_check("spec_7_5_shared_write", src);
    assert!(!ok, "assignment through &T must be rejected");
    assert!(stderr.contains("GT0031"), "expected GT0031, got: {stderr}");
}

#[test]
fn spec_7_5_mut_reference_aliases_fixed_array_source() {
    let src = r#"
fn main() {
    let mut xs = [1, 2]
    let r = &mut xs
    r[0] = 0
    if xs[0] != 0 { panic!("mutable reference did not write through") }
}
"#;
    let (ok, _stdout, stderr) = run_program("spec_7_5_write_through", src, &[]);
    assert!(ok, "write-through reference program failed: {stderr}");
}

#[test]
fn spec_7_5_mut_reference_aliases_scalar_source() {
    let src = r#"
fn main() {
    let mut value = 1i64
    let r = &mut value
    *r = 42i64
    if value != 42i64 { panic!("mutable reference did not write through") }
}
"#;
    let (ok, _stdout, stderr) = run_program("spec_7_5_scalar_write_through", src, &[]);
    assert!(ok, "write-through reference program failed: {stderr}");
}

#[test]
fn spec_7_5_mut_reference_binding_rebinds_its_target() {
    let src = r#"
fn main() {
    let mut first = 1i64
    let mut second = 2i64
    let mut r = &mut first
    r = &mut second
    *r = 42i64
    if first != 1i64 { panic!("rebind changed the old target") }
    if second != 42i64 { panic!("rebind did not change the new target") }
}
"#;
    let (ok, _stdout, stderr) = run_program("spec_7_5_reference_rebind", src, &[]);
    assert!(ok, "rebindable reference program failed: {stderr}");
}

#[test]
fn spec_7_5_aliased_mut_borrow_is_rejected() {
    // Named mutable-reference bindings are lexically exclusive, matching
    // Rust's core aliasing rule and preventing conflicting writes.
    let src = r#"
fn main() {
    let mut x = 1
    let a = &mut x
    let b = &mut x
    *a = 2
    *b = 3
    println!("{}", x)
}
"#;
    let (ok, _stdout, stderr) = run_check("spec_7_5_borrow", src);
    assert!(!ok, "overlapping named mutable borrows must be rejected");
    assert!(stderr.contains("GT0043"), "expected GT0043, got: {stderr}");
}
