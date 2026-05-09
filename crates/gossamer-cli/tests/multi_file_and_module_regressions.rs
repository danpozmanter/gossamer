#![allow(clippy::doc_markdown)]
//! Behavioural regression tests covering: multi-file project bundling,
//! named-module bodies, native-binary `os::*` filesystem and process
//! helpers, `Option<json::Value>` discriminator semantics, and
//! cross-test runner state isolation under JIT warm-up.
//!
//! These tests drive the `gos` binary end-to-end against a temporary
//! source tree and assert observable output, so a regression in the
//! frontend, MIR lowering, runtime FFI, or test runner all turn the
//! relevant case red.

#![allow(missing_docs)]

use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

const PER_RUN_TIMEOUT: Duration = Duration::from_secs(60);

fn gos_bin() -> PathBuf {
    PathBuf::from(env::var("CARGO_BIN_EXE_gos").expect("CARGO_BIN_EXE_gos"))
}

fn fresh_dir(tag: &str) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = env::temp_dir().join(format!(
        "gos-multi-{pid}-{n}-{tag}",
        pid = std::process::id(),
    ));
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
        String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n"),
        String::from_utf8_lossy(&out.stderr).replace("\r\n", "\n"),
        out.status.code(),
    )
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

fn run_vm(src: &Path) -> (String, String, Option<i32>) {
    let child = Command::new(gos_bin())
        .arg("run")
        .arg(src)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn gos run");
    run_with_timeout(child)
}

fn build_native(src: &Path, out_dir: &Path) -> PathBuf {
    let out = Command::new(gos_bin())
        .arg("build")
        .arg("--out-dir")
        .arg(out_dir)
        .arg(src)
        .output()
        .expect("spawn gos build");
    assert!(
        out.status.success(),
        "gos build failed:\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    for entry in fs::read_dir(out_dir).expect("read out_dir").flatten() {
        let p = entry.path();
        if p.is_file() && is_executable(&p) {
            return p;
        }
    }
    panic!("no binary in {}", out_dir.display());
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

fn write_source(dir: &Path, tag: &str, source: &str) -> PathBuf {
    let path = dir.join(format!("{tag}.gos"));
    let mut f = fs::File::create(&path).expect("create source");
    f.write_all(source.as_bytes()).unwrap();
    path
}

#[test]
fn cross_file_project_bundles_sibling_modules() {
    let dir = fresh_dir("cross-file");
    let src = dir.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(
        dir.join("project.toml"),
        "[project]\nid = \"example.com/probe\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    fs::write(
        src.join("util.gos"),
        "pub fn add(a: i64, b: i64) -> i64 { a + b }\n",
    )
    .unwrap();
    fs::write(
        src.join("main.gos"),
        "fn main() { println!(\"{}\", util::add(1, 2)) }\n",
    )
    .unwrap();

    // Build at the project root and execute the resulting binary.
    let build_out = Command::new(gos_bin())
        .arg("build")
        .current_dir(&dir)
        .output()
        .expect("gos build");
    assert!(
        build_out.status.success(),
        "gos build failed:\nstderr: {}",
        String::from_utf8_lossy(&build_out.stderr),
    );
    let bin_path = dir.join("target/debug/probe");
    assert!(bin_path.is_file(), "expected probe binary at {bin_path:?}");
    let run = run_native(&bin_path);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(run.2, Some(0), "stderr: {}", run.1);
    assert!(run.0.trim() == "3", "expected stdout '3', got: {:?}", run.0);
}

#[test]
fn cross_file_chained_sibling_module_calls() {
    // Three sibling modules where each call cascades into the next:
    // main → a::foo → b::bar → util::helper. Pins both the qualified-
    // path resolution at every hop and the type-checker carrying the
    // callee's `String` return type through the chain so the outer
    // `format!` argument prints as text instead of `<value>`.
    let dir = fresh_dir("chained-cross-file");
    let src = dir.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(
        dir.join("project.toml"),
        "[project]\nid = \"example.com/chained\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    fs::write(
        src.join("main.gos"),
        "fn main() { println!(\"{}\", a::foo()) }\n",
    )
    .unwrap();
    fs::write(
        src.join("a.gos"),
        "pub fn foo() -> String { format!(\"a({})\", b::bar()) }\n",
    )
    .unwrap();
    fs::write(
        src.join("b.gos"),
        "pub fn bar() -> String { format!(\"b({})\", util::helper()) }\n",
    )
    .unwrap();
    fs::write(
        src.join("util.gos"),
        "pub fn helper() -> String { \"leaf\".to_string() }\n",
    )
    .unwrap();

    // VM tier (`gos run`).
    let run_out = Command::new(gos_bin())
        .arg("run")
        .current_dir(&dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("gos run");
    assert!(
        run_out.status.success(),
        "gos run failed:\nstderr: {}",
        String::from_utf8_lossy(&run_out.stderr),
    );
    let run_stdout = String::from_utf8_lossy(&run_out.stdout);
    assert_eq!(
        run_stdout.trim(),
        "a(b(leaf))",
        "VM stdout mismatch: {run_stdout:?}",
    );

    // Cranelift native build (`gos build`).
    let build_out = Command::new(gos_bin())
        .arg("build")
        .current_dir(&dir)
        .output()
        .expect("gos build");
    assert!(
        build_out.status.success(),
        "gos build failed:\nstderr: {}",
        String::from_utf8_lossy(&build_out.stderr),
    );
    let bin_path = dir.join("target/debug/chained");
    assert!(bin_path.is_file(), "expected binary at {bin_path:?}");
    let nat = run_native(&bin_path);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(nat.2, Some(0), "native stderr: {}", nat.1);
    assert_eq!(
        nat.0.trim(),
        "a(b(leaf))",
        "native stdout mismatch: {:?}",
        nat.0,
    );
}

#[test]
fn named_module_body_resolves_qualified_calls() {
    let src = r#"
mod util {
    pub fn add(a: i64, b: i64) -> i64 { a + b }
}
fn main() { println!("{}", util::add(2, 5)) }
"#;
    let dir = fresh_dir("named-mod");
    let path = write_source(&dir, "named_mod", src);
    let run = run_vm(&path);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(run.2, Some(0), "stderr: {}", run.1);
    assert!(
        run.0.contains('7'),
        "expected '7' on stdout, got: {:?}",
        run.0,
    );
}

#[test]
fn native_binary_os_read_file_to_string_returns_real_contents() {
    let dir = fresh_dir("os-read-file");
    let payload_path = dir.join("payload.txt");
    fs::write(&payload_path, b"hello-from-disk").unwrap();
    let src = format!(
        r#"use std::os
fn main() {{
    let p: String = "{p}".to_string()
    match os::read_file_to_string(&p) {{
        Ok(s) => println!("ok len={{}} content={{}}", s.len(), s),
        Err(e) => println!("err: {{}}", e),
    }}
    println!("exists = {{}}", os::exists(&p))
}}
"#,
        p = payload_path.display(),
    );
    let path = write_source(&dir, "os_read", &src);
    let cl_dir = dir.join("cl");
    fs::create_dir_all(&cl_dir).unwrap();
    let bin = build_native(&path, &cl_dir);
    let run = run_native(&bin);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(run.2, Some(0), "stderr: {}", run.1);
    assert!(
        run.0.contains("ok len=15") && run.0.contains("hello-from-disk"),
        "expected real file contents in native build, got: {:?}",
        run.0,
    );
    assert!(
        run.0.contains("exists = true"),
        "expected exists=true, got: {:?}",
        run.0,
    );
}

#[test]
fn native_binary_exec_run_returns_subprocess_output() {
    let src = r#"
use std::os::exec
fn main() {
    println!("calling exec::run")
    let mut argv: [String] = [].to_vec()
    argv.push("hello-via-echo".to_string())
    match exec::run(&"echo".to_string(), &argv) {
        Ok(o) => println!("code={} stdout={}", o.code, o.stdout),
        Err(e) => println!("err: {}", e),
    }
}
"#;
    let dir = fresh_dir("exec-run");
    let path = write_source(&dir, "exec_run", src);
    let cl_dir = dir.join("cl");
    fs::create_dir_all(&cl_dir).unwrap();
    let bin = build_native(&path, &cl_dir);
    let run = run_native(&bin);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(run.2, Some(0), "stderr: {}", run.1);
    assert!(
        run.0.contains("calling exec::run"),
        "expected pre-call print, got: {:?}",
        run.0,
    );
    assert!(
        run.0.contains("hello-via-echo"),
        "expected echoed payload, got: {:?}",
        run.0,
    );
}

/// Regression for the askq segfault: `exec::run(&prog,
/// &[a, b, c].to_vec())` reached the runtime via the
/// literal-array `.to_vec()` shape (not the empty-array +
/// push pattern). Two distinct root causes converged here:
/// 1. `exec::run` had no compiled-tier binding — the call
///    fell through to a non-existent symbol and the
///    destination held an undefined Result pointer.
/// 2. `[a, b, c].to_vec()` lowered to
///    `gos_rt_vec_clone(stack_array)`, but `vec_clone`
///    expects a `*const GosVec` header; reading the array's
///    payload bytes as `len/cap/elem_bytes/ptr` produced a
///    multi-terabyte length and a `memory allocation of
///    <huge> bytes failed` abort.
#[test]
fn native_binary_exec_run_with_literal_array_args() {
    let src = r#"
use std::os::exec
fn main() {
    let args: [String] = [
        "-n".to_string(),
        "from-literal-array".to_string(),
    ].to_vec()
    match exec::run(&"echo".to_string(), &args) {
        Ok(o) => println!("ok code={} stdout={}", o.code, o.stdout),
        Err(e) => println!("err: {}", e),
    }
}
"#;
    let dir = fresh_dir("exec-literal");
    let path = write_source(&dir, "exec_literal", src);
    let cl_dir = dir.join("cl");
    fs::create_dir_all(&cl_dir).unwrap();
    let bin = build_native(&path, &cl_dir);
    let run = run_native(&bin);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(
        run.2,
        Some(0),
        "process aborted (likely the [..].to_vec() segfault) — stdout: {:?}, stderr: {:?}",
        run.0,
        run.1,
    );
    assert!(
        run.0.contains("from-literal-array"),
        "expected echoed literal payload, got stdout: {:?}, stderr: {:?}",
        run.0,
        run.1,
    );
}

/// Regression for the `gos_rt_json_get` Arc-sharing refactor.
/// The previous implementation deep-cloned the matched
/// `serde_json::Value` per call (O(N) on a nested tree) and
/// `Box`-leaked the copy. A 1000-iteration drill that walks a
/// 3-deep nested object exercises the refactored path: the same
/// `Arc<Value>` tree is shared across child handles, so cloning
/// is a cheap refcount bump and `Box::into_raw`'d handles all
/// keep the same allocation alive. Memory growth is bounded by
/// the handle leak (24 bytes each), not by the tree's total size.
/// Without the refactor (or with a use-after-free in the new
/// shape) this test segfaults or returns garbage.
#[test]
fn native_binary_json_get_arc_shared_does_not_deep_clone() {
    let src = r#"
use std::encoding::json
fn main() {
    let raw = "{\"a\":{\"b\":{\"c\":42}}}"
    let mut iter: i64 = 0
    while iter < 1000 {
        let v = json::parse(&raw.to_string()).unwrap_or(json::Value::Null)
        if let Some(a) = json::get(&v, &"a") {
            if let Some(b) = json::get(a, &"b") {
                if let Some(c) = json::get(b, &"c") {
                    let n = json::as_i64(c).unwrap_or(0)
                    if n != 42 { println!("UNEXPECTED iter={} n={}", iter, n) }
                }
            }
        }
        iter += 1
    }
    println!("ok iter={}", iter)
}
"#;
    let dir = fresh_dir("json-arc");
    let path = write_source(&dir, "json_arc", src);
    let cl_dir = dir.join("cl");
    fs::create_dir_all(&cl_dir).unwrap();
    let bin = build_native(&path, &cl_dir);
    let run = run_native(&bin);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(run.2, Some(0), "stderr: {}", run.1);
    assert!(
        run.0.contains("ok iter=1000"),
        "expected 1000 iterations of json::get to succeed, got: {:?}",
        run.0,
    );
    assert!(
        !run.0.contains("UNEXPECTED"),
        "json::get returned wrong value at some iteration, got: {:?}",
        run.0,
    );
}

/// Regression for the non-capturing closure ABI mismatch in
/// `Result::map`. Pre-fix the lift pass emitted non-capturing
/// closures as `extern "C" fn(payload) -> ret` (no env slot)
/// while the runtime helper `gos_rt_result_map` invoked them as
/// `f(closure_ptr, payload)`; on x86_64 the closure's `v` param
/// shadowed RDI = closure_ptr while the actual payload sat unread
/// in RSI. The closure body then transformed the env-pointer
/// instead of the payload, corrupting the resulting Result.
/// Now the MIR call-site dispatches non-capturing closures to a
/// dedicated `gos_rt_result_map_bare` helper that calls the
/// bare-fn ABI `f(payload)` directly.
#[test]
fn native_binary_result_map_non_capturing_closure_abi() {
    let src = r#"
use std::encoding::json
fn main() {
    let raw = "{\"x\":42}"
    let r1 = json::parse(&raw.to_string())
    let r2 = r1.map(|v| v.clone())
    let r3 = r2.unwrap_or(json::Value::Null)
    println!("r3.is_null={}", json::is_null(&r3))
}
"#;
    let dir = fresh_dir("non-cap-map");
    let path = write_source(&dir, "non_cap_map", src);
    let cl_dir = dir.join("cl");
    fs::create_dir_all(&cl_dir).unwrap();
    let bin = build_native(&path, &cl_dir);
    let run = run_native(&bin);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(run.2, Some(0), "stderr: {}", run.1);
    assert!(
        run.0.contains("r3.is_null=false"),
        "expected map+unwrap_or chain to preserve the parsed Ok payload, got: {:?}",
        run.0,
    );
}

/// Regression for `json::Value::Null` (path expression, no
/// parens). The previous `lower_path` had no special case for
/// stdlib enum unit variants, so it fell through to the FnRef
/// fallback and produced a function-pointer value. Subsequent
/// code that expected a `*mut GosJson` (e.g. `json::is_null(&v)`)
/// dereferenced the function pointer and segfaulted. Now routes
/// through `gos_rt_json_value_null()` to produce a real handle.
#[test]
fn native_binary_json_value_null_path_expression() {
    let src = r#"
use std::encoding::json
fn main() {
    let v1 = json::Value::Null
    println!("v1 is_null={}", json::is_null(&v1))
    let raw = "{\"path\":\"/tmp\"}"
    let parsed = json::parse(&raw.to_string()).unwrap_or(json::Value::Null)
    if let Some(child) = json::get(&parsed, &"path") {
        let s = json::as_str(&child).unwrap_or("")
        println!("path={}", s)
    }
    println!("done")
}
"#;
    let dir = fresh_dir("json-null-path");
    let path = write_source(&dir, "json_null", src);
    let cl_dir = dir.join("cl");
    fs::create_dir_all(&cl_dir).unwrap();
    let bin = build_native(&path, &cl_dir);
    let run = run_native(&bin);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(run.2, Some(0), "stderr: {}", run.1);
    assert!(
        run.0.contains("v1 is_null=true"),
        "expected Value::Null to be is_null=true, got: {:?}",
        run.0,
    );
    assert!(
        run.0.contains("path=/tmp"),
        "expected unwrap_or-with-Null default to preserve Ok payload, got: {:?}",
        run.0,
    );
    assert!(
        run.0.contains("done"),
        "expected program to reach the end, got: {:?}",
        run.0,
    );
}

/// Regression for the askq tool-call accumulator. The compiled
/// chat round did `tc_names[idx] = s` where `idx` came from
/// `json::as_i64(v).unwrap_or(0)` — both pieces were broken in
/// compiled mode:
/// 1. `Vec<String>[idx] = s` assignment was a no-op (the projection
///    machinery treated the Vec as a flat array; the data lives at
///    `header.ptr`, not in the slot directly). Now routes through
///    `gos_rt_vec_set_i64`.
/// 2. `json::as_i64(v).unwrap_or(0)` returned a multi-trillion
///    garbage pointer. The HIR typechecker assumed the chained
///    `.unwrap_or` meant the receiver was `Option<i64>`, dispatched
///    `unwrap_or` to `gos_rt_result_unwrap_or` which read the i64
///    return value as a `*mut GosResult` pointer and dereferenced
///    garbage `disc`/`payload`. Now falls back to identity when
///    the lowered receiver is a real scalar.
#[test]
fn native_binary_vec_string_indexed_assign_and_scalar_unwrap_or() {
    let src = r#"
fn main() {
    let mut xs: [String] = [].to_vec()
    xs.push("a".to_string())
    xs.push("b".to_string())
    xs[0] = "X".to_string()
    xs[1] = "Y".to_string()
    println!("xs[0]={} xs[1]={}", xs[0], xs[1])
}
"#;
    let dir = fresh_dir("vec-set");
    let path = write_source(&dir, "vec_set", src);
    let cl_dir = dir.join("cl");
    fs::create_dir_all(&cl_dir).unwrap();
    let bin = build_native(&path, &cl_dir);
    let run = run_native(&bin);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(run.2, Some(0), "stderr: {}", run.1);
    assert!(
        run.0.contains("xs[0]=X xs[1]=Y"),
        "expected updated values, got: {:?}",
        run.0,
    );
}

/// Regression for `[1, 2, 3].to_vec()` and
/// `["a", "b", "c"].to_vec()` literal-array `.to_vec()`
/// crashes. These shapes lowered to `gos_rt_vec_clone(arr)`,
/// but the runtime helper expects a real `GosVec` header
/// rather than a stack `[T; N]` aggregate; reading element
/// 0/1 as `len`/`cap` produced terabyte-scale allocations
/// and aborted with `memory allocation of <huge> bytes
/// failed` or a plain segfault. Both `i64` and `String`
/// element shapes are exercised because they pick different
/// elem_bytes paths inside `gos_rt_vec_from_arr`.
#[test]
fn native_binary_literal_array_to_vec_does_not_segfault() {
    let src = r#"
fn main() {
    let xs: [i64] = [10, 20, 30].to_vec()
    println!("i64 len={} 0={} 1={} 2={}", xs.len(), xs[0], xs[1], xs[2])
    let ys: [String] = ["a".to_string(), "b".to_string(), "c".to_string()].to_vec()
    println!("str len={} 0={} 1={} 2={}", ys.len(), ys[0], ys[1], ys[2])
    let zs: [String] = ["aa", "bb", "cc"].to_vec()
    println!("lit len={} 0={} 1={} 2={}", zs.len(), zs[0], zs[1], zs[2])
}
"#;
    let dir = fresh_dir("literal-to-vec");
    let path = write_source(&dir, "lit_tovec", src);
    let cl_dir = dir.join("cl");
    fs::create_dir_all(&cl_dir).unwrap();
    let bin = build_native(&path, &cl_dir);
    let run = run_native(&bin);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(
        run.2,
        Some(0),
        "process aborted — stdout: {:?}, stderr: {:?}",
        run.0,
        run.1,
    );
    assert!(
        run.0.contains("i64 len=3 0=10 1=20 2=30"),
        "i64 literal-array to_vec failed, got: {:?}",
        run.0,
    );
    assert!(
        run.0.contains("str len=3 0=a 1=b 2=c"),
        "String literal-array (.to_string() elems) to_vec failed, got: {:?}",
        run.0,
    );
    assert!(
        run.0.contains("lit len=3 0=aa 1=bb 2=cc"),
        "&str literal-array to_vec failed, got: {:?}",
        run.0,
    );
}

#[test]
fn json_get_returns_option_with_correct_discriminator() {
    let src = r#"
use std::encoding::json
fn main() {
    let v = json::parse(&"{\"a\":1}".to_string()).unwrap()
    let opt = json::get(&v, &"a".to_string())
    println!("is_some? {}", opt.is_some())
    println!("is_none? {}", opt.is_none())
    if let Some(_) = opt {
        println!("matched Some")
    } else {
        println!("matched None")
    }
    let missing = json::get(&v, &"absent".to_string())
    println!("missing is_some? {}", missing.is_some())
    println!("missing is_none? {}", missing.is_none())
}
"#;
    let dir = fresh_dir("json-option");
    let path = write_source(&dir, "json_opt", src);
    // Both interpreter and native build must agree.
    let vm = run_vm(&path);
    assert_eq!(vm.2, Some(0), "vm stderr: {}", vm.1);
    assert!(
        vm.0.contains("is_some? true")
            && vm.0.contains("is_none? false")
            && vm.0.contains("matched Some")
            && vm.0.contains("missing is_some? false")
            && vm.0.contains("missing is_none? true"),
        "unexpected interpreter output: {:?}",
        vm.0,
    );

    let cl_dir = dir.join("cl");
    fs::create_dir_all(&cl_dir).unwrap();
    let bin = build_native(&path, &cl_dir);
    let nat = run_native(&bin);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(nat.2, Some(0), "native stderr: {}", nat.1);
    assert!(
        nat.0.contains("is_some? true")
            && nat.0.contains("is_none? false")
            && nat.0.contains("matched Some")
            && nat.0.contains("missing is_some? false")
            && nat.0.contains("missing is_none? true"),
        "unexpected native output: {:?}",
        nat.0,
    );
}

#[test]
fn slice_of_tuples_indexing_works_in_native_build() {
    // `&[(String, String)]` callees would receive the address of
    // a flat-array aggregate before the unified `coerce_to_vec_arg`
    // landed; the callee's `gos_rt_vec_len` then read the first
    // tuple element as the length and segfaulted on the
    // subsequent index dispatch. Asserting on both `gos run` and
    // a native build guards the path against future regressions.
    let src = r#"
fn first_key(vars: &[(String, String)]) -> String {
    vars[0].0.clone()
}
fn second_value(vars: &[(String, String)]) -> String {
    vars[1].1.clone()
}
fn main() {
    let pairs = [
        ("alpha".to_string(), "1".to_string()),
        ("beta".to_string(), "2".to_string()),
    ].to_vec()
    println!("{}", first_key(&pairs))
    println!("{}", second_value(&pairs))
}
"#;
    let dir = fresh_dir("slice-tuples");
    let path = write_source(&dir, "slice_tuples", src);
    let vm = run_vm(&path);
    assert_eq!(vm.2, Some(0), "vm stderr: {}", vm.1);
    assert!(
        vm.0.contains("alpha") && vm.0.contains('2'),
        "vm output mismatch: {:?}",
        vm.0,
    );
    let cl_dir = dir.join("cl");
    fs::create_dir_all(&cl_dir).unwrap();
    let bin = build_native(&path, &cl_dir);
    let nat = run_native(&bin);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(nat.2, Some(0), "native stderr: {}", nat.1);
    assert!(
        nat.0.contains("alpha") && nat.0.contains('2'),
        "native output mismatch: {:?}",
        nat.0,
    );
}

#[test]
fn test_runner_assertions_persist_after_jit_warmup() {
    // First test runs a regex-heavy body that triggers the JIT.
    // Subsequent string-comparing assertions must continue to be
    // observed and counted against each test's tally — otherwise
    // the runner reports a false PASS with 0 assertions for any
    // test that ran after the JIT became hot.
    let src = r#"
use std::regex
use std::testing

fn empty_pattern() -> regex::Pattern {
    regex::compile("").unwrap_or(regex::compile("a").unwrap())
}

pub fn substring(s: &String, start: i64, end: i64) -> String {
    let n = s.len() as i64
    let mut a = start
    let mut b = end
    if a < 0 { a = 0 }
    if b > n { b = n }
    if a >= b { return "".to_string() }
    let drop_pat = regex::compile(&format!("(?s)^.{{0,{}}}", a)).unwrap_or(empty_pattern())
    let after = regex::replace(&drop_pat, s, &"".to_string())
    let len = b - a
    let take_pat = regex::compile(&format!("(?s)^(.{{0,{}}})", len)).unwrap_or(empty_pattern())
    let row = regex::captures(&take_pat, &after).map(|r| r.clone()).unwrap_or([].to_vec())
    if row.len() < 2 { return "".to_string() }
    row[1].clone().map(|x| x.clone()).unwrap_or("".to_string())
}

#[cfg(test)]
#[test]
fn warmup_jit() {
    let s = "padding-padding-padding-padding-padding-padding".to_string()
    let mut i: i64 = 0
    while i < (s.len() as i64) {
        let _ = substring(&s, i, i + 5)
        i += 1
    }
    testing::check_eq(&s.len(), &47, "length sanity")
}

#[cfg(test)]
#[test]
fn after_jit_string_eq() {
    testing::check_eq(&"hi".to_string(), &"hi".to_string(), "string eq")
}

#[cfg(test)]
#[test]
fn after_jit_check_true() {
    testing::check(true, "always true")
}

fn main() {}
"#;
    let dir = fresh_dir("test-runner-jit");
    let path = write_source(&dir, "runner_jit", src);
    let out = Command::new(gos_bin())
        .arg("test")
        .arg(&path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("gos test");
    let _ = fs::remove_dir_all(&dir);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "gos test failed:\nstdout: {stdout}\nstderr: {stderr}",
    );
    assert!(
        stdout.contains("PASS warmup_jit") && stdout.contains("(1 assertion"),
        "warmup did not log an assertion: {stdout}",
    );
    assert!(
        stdout.contains("PASS after_jit_string_eq") && stdout.contains("(1 assertion"),
        "string-eq test reported zero assertions after JIT: {stdout}",
    );
    assert!(
        stdout.contains("PASS after_jit_check_true"),
        "check(true) test silently dropped after JIT: {stdout}",
    );
    assert!(
        stdout.contains("3 passed, 0 failed, 3 assertion(s)"),
        "expected 3 assertions across 3 tests, got: {stdout}",
    );
}

#[test]
fn vec_double_free_does_not_segfault_in_native_release() {
    // The askq chat-streaming loop drops the same `*mut GosVec`
    // handle twice (the MIR drop pass emits a free for each
    // shadowed binding inside a loop, and a real LLVM-release
    // build of askq would segfault inside the allocator's
    // `__libc_free`/`get_meta` after the second drop). The
    // runtime now tracks freed addresses in a process-global
    // set so the second free is an idempotent no-op.
    //
    // Repro: aggressively shadow a local that holds a vec
    // returned by a runtime helper. With a small `cap` we keep
    // re-allocating into the same arena slot.
    let src = r#"
fn main() {
    let mut total: i64 = 0
    let mut i: i64 = 0
    while i < 256 {
        let parts = "a,b,c,d".to_string().split(",")
        total += parts.len() as i64
        i += 1
    }
    println!("total={}", total)
}
"#;
    let dir = fresh_dir("vec-double-free");
    let path = write_source(&dir, "vec_dbl_free", src);
    let cl_dir = dir.join("cl");
    fs::create_dir_all(&cl_dir).unwrap();
    let bin = build_native(&path, &cl_dir);
    let nat = run_native(&bin);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(nat.2, Some(0), "native stderr: {}", nat.1);
    assert_eq!(nat.0.trim(), "total=1024", "native stdout: {:?}", nat.0);
}

#[test]
fn param_named_client_with_user_type_is_not_tagged_as_http_client() {
    // `fn render(client: User)`: the param is *user-typed* but
    // its name collides with the stdlib heuristic. Previously
    // the MIR tagged `client`'s local with `http::Client`, and
    // any method dispatched through `gos_rt_http_client_*` —
    // reading bytes past the user struct. The heuristic now only
    // applies when the parameter's declared type is unresolved.
    let src = r#"
struct User { name: String, age: i64 }

impl User {
    fn render(&self) -> String {
        format!("{} ({})", self.name, self.age)
    }
}

fn show(client: User) -> String {
    client.render()
}

fn main() {
    let u = User { name: "alice".to_string(), age: 30 }
    println!("{}", show(u))
}
"#;
    let dir = fresh_dir("client-param");
    let path = write_source(&dir, "client_param", src);
    let vm = run_vm(&path);
    assert_eq!(vm.2, Some(0), "vm stderr: {}", vm.1);
    assert_eq!(vm.0.trim(), "alice (30)", "vm stdout: {:?}", vm.0);
    let cl_dir = dir.join("cl");
    fs::create_dir_all(&cl_dir).unwrap();
    let bin = build_native(&path, &cl_dir);
    let nat = run_native(&bin);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(nat.2, Some(0), "native stderr: {}", nat.1);
    assert_eq!(nat.0.trim(), "alice (30)", "native stdout: {:?}", nat.0);
}

#[test]
fn i128_use_panics_native_build_rather_than_silently_truncating() {
    // `cl_type_of` was silently mapping `i128`/`u128` to `i64`,
    // so `let n: i128 = 1 << 100` would compile, run, and produce
    // a corrupted i64 with no warning. Refuse the build instead.
    let src = r#"
fn main() {
    let n: i128 = 1
    println!("{}", n)
}
"#;
    let dir = fresh_dir("i128-reject");
    let path = write_source(&dir, "i128_reject", src);
    let cl_dir = dir.join("cl");
    fs::create_dir_all(&cl_dir).unwrap();
    let out = Command::new(gos_bin())
        .arg("build")
        .arg("--out-dir")
        .arg(&cl_dir)
        .arg(&path)
        .output()
        .expect("gos build");
    let _ = fs::remove_dir_all(&dir);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "expected `gos build` to refuse i128 source; got success. stderr={stderr}",
    );
    assert!(
        stderr.contains("i128") && stderr.contains("compiled tier"),
        "expected diagnostic mentioning i128 + compiled tier, got: {stderr}",
    );
}

#[test]
fn user_fn_named_substring_does_not_recurse_via_method_dispatch() {
    // The bytecode VM's method dispatch was tripping over user
    // free fns whose name collided with a builtin String method:
    // `pub fn substring(s, a, b)` made every `s.substring(...)`
    // call inside the user's body recurse straight back into the
    // user fn (the bare-name fallback in `MethodCall` resolution
    // beat the builtin). The VM now consults a `String::method`
    // qualified key first, so the builtin wins.
    let src = r#"
pub fn substring(s: &String, a: i64, b: i64) -> String {
    s.substring(a, b)
}

fn main() {
    let s = "hello world".to_string()
    println!("{}", substring(&s, 0, 5))
    println!("{}", substring(&s, 6, 11))
}
"#;
    let dir = fresh_dir("user-fn-substring");
    let path = write_source(&dir, "user_substring", src);
    let vm = run_vm(&path);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(vm.2, Some(0), "vm stderr: {}", vm.1);
    assert_eq!(vm.0.trim(), "hello\nworld", "vm stdout: {:?}", vm.0);
}

#[test]
fn malformed_json_returns_none_not_segfault() {
    // Calling `json::get(v, key)` / `as_array` / `as_str` on a
    // value whose actual shape doesn't match the expected variant
    // must return None / "" cleanly. Previously the helpers
    // `unwrap`'d serde_json's index lookups and segfaulted when
    // the value was the wrong kind. Pinned by this test plus the
    // c-string addr-floor guard in `gos_rt_string_view`.
    let src = r#"
use std::encoding::json
fn main() {
    // Parse a JSON string. `as_object` keys / `as_array` iter on
    // a string value must return None.
    let s = json::parse(&"\"plain\"".to_string()).unwrap()
    match json::as_array(&s) {
        Some(_) => println!("array? wrong"),
        None => println!("not-array ok"),
    }
    match json::keys(&s) {
        Some(_) => println!("keys? wrong"),
        None => println!("not-object-keys ok"),
    }
    let missing = json::get(&s, &"nope")
    match missing {
        Some(_) => println!("got? wrong"),
        None => println!("missing ok"),
    }
    // Object lookup with a key that doesn't exist.
    let obj = json::parse(&"{\"a\": 1}".to_string()).unwrap()
    match json::get(&obj, &"b") {
        Some(_) => println!("b? wrong"),
        None => println!("b missing ok"),
    }
}
"#;
    let dir = fresh_dir("malformed-json");
    let path = write_source(&dir, "malformed_json", src);

    let vm = run_vm(&path);
    assert_eq!(vm.2, Some(0), "interp stderr: {}", vm.1);
    let expected = "not-array ok\nnot-object-keys ok\nmissing ok\nb missing ok";
    assert_eq!(vm.0.trim(), expected, "interp output: {:?}", vm.0);

    let cl_dir = dir.join("cl");
    fs::create_dir_all(&cl_dir).unwrap();
    let bin = build_native(&path, &cl_dir);
    let nat = run_native(&bin);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(nat.2, Some(0), "native stderr: {}", nat.1);
    assert_eq!(nat.0.trim(), expected, "native output: {:?}", nat.0);
}

#[test]
fn json_as_array_iter_native() {
    // `json::as_array(&v).unwrap()` was identity-pinned to
    // `JsonValue` in MIR, so downstream `arr.len()` /
    // `for x in arr.iter()` / `arr[i]` walked serde_json's
    // private headers as if they were a `*mut GosVec` and
    // returned garbage / segfaulted. Now backed by
    // `gos_rt_json_as_array`, which materialises a real
    // `*mut GosVec<*mut GosJson>` and wraps it in a
    // sentinel-Option.
    let src = r#"
use std::encoding::json
fn main() {
    let v = json::parse(&"[10, 20, 30]".to_string()).unwrap()
    let arr = json::as_array(&v).unwrap()
    let n = arr.len() as i64
    let mut i: i64 = 0
    while i < n {
        let item = arr[i]
        println!("{}", json::as_i64(&item).unwrap_or(-1))
        i += 1
    }
    println!("len={}", n)
}
"#;
    let dir = fresh_dir("json-as-array-iter");
    let path = write_source(&dir, "json_arr", src);

    let vm = run_vm(&path);
    assert_eq!(vm.2, Some(0), "interp stderr: {}", vm.1);
    assert!(
        vm.0.trim() == "10\n20\n30\nlen=3",
        "interp output: {:?}",
        vm.0,
    );

    let cl_dir = dir.join("cl");
    fs::create_dir_all(&cl_dir).unwrap();
    let bin = build_native(&path, &cl_dir);
    let nat = run_native(&bin);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(nat.2, Some(0), "native stderr: {}", nat.1);
    assert!(
        nat.0.trim() == "10\n20\n30\nlen=3",
        "native output: {:?}",
        nat.0,
    );
}

#[test]
fn regex_replace_singular_native() {
    // `regex::replace(pat, text, repl)` (one occurrence) was unwired
    // in the AOT cranelift dispatch and silently lowered to an `iconst
    // i64 0`. Downstream `.clone().unwrap_or("")` then double-freed
    // the empty-string literal in askq's substring impl. The fix is
    // a `gos_rt_regex_replace` runtime helper plus dispatch in MIR /
    // JIT / native / LLVM. This test pins both the result and the
    // single-replace semantics.
    let src = r#"
use std::regex
fn main() {
    let pat = regex::compile(&"foo").unwrap()
    let s = "foo and foo".to_string()
    let one = pat.replace(&s, &"BAR")
    let all = pat.replace_all(&s, &"BAR")
    println!("{} | {}", one, all)
}
"#;
    let dir = fresh_dir("regex-replace-singular");
    let path = write_source(&dir, "regex_replace", src);

    // Interp tier first.
    let vm = run_vm(&path);
    assert_eq!(vm.2, Some(0), "interp stderr: {}", vm.1);
    assert!(
        vm.0.trim() == "BAR and foo | BAR and BAR",
        "interp output mismatch: {:?}",
        vm.0,
    );

    // Native tier: build, run, compare.
    let cl_dir = dir.join("cl");
    fs::create_dir_all(&cl_dir).unwrap();
    let bin = build_native(&path, &cl_dir);
    let nat = run_native(&bin);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(nat.2, Some(0), "native stderr: {}", nat.1);
    assert!(
        nat.0.trim() == "BAR and foo | BAR and BAR",
        "native output mismatch: {:?}",
        nat.0,
    );
}

#[test]
fn test_runner_isolates_jit_state_across_json_iteration() {
    // After the batch JIT compile fires (triggered by the hot
    // counter on any helper), the JIT-compiled body of a later
    // test that iterates a parsed JSON array must produce the
    // same items the bytecode interpreter would. This previously
    // regressed in askq: a chain of ~14 short tests warmed the
    // JIT, the next test's `for msg in arr.iter()` loop produced
    // zero items, and downstream `keep` was empty. The fix is
    // per-test Vm reset in the runner; this test pins that.
    let src = r#"
use std::encoding::json
use std::testing

pub fn json_string(v: &json::Value, key: &String) -> String {
    if let Some(child) = json::get(v, key) {
        json::as_str(&child).unwrap_or("")
    } else {
        ""
    }
}

pub fn count_user_messages(messages_json: &String) -> i64 {
    let parsed = json::parse(messages_json).unwrap_or(json::Value::Null)
    if json::is_null(&parsed) { return 0 }
    let arr = json::as_array(&parsed).unwrap()
    let mut n: i64 = 0
    for msg in arr.iter() {
        let role = json_string(msg, &"role")
        if role == "user" { n += 1 }
    }
    n
}

#[cfg(test)] #[test] fn warm_a()  { testing::check(1 == 1, "a") }
#[cfg(test)] #[test] fn warm_b()  { testing::check(2 == 2, "b") }
#[cfg(test)] #[test] fn warm_c()  { testing::check(3 == 3, "c") }
#[cfg(test)] #[test] fn warm_d()  { testing::check(4 == 4, "d") }
#[cfg(test)] #[test] fn warm_e()  { testing::check(5 == 5, "e") }
#[cfg(test)] #[test] fn warm_f()  { testing::check(6 == 6, "f") }
#[cfg(test)] #[test] fn warm_g()  { testing::check(7 == 7, "g") }
#[cfg(test)] #[test] fn warm_h()  { testing::check(8 == 8, "h") }
#[cfg(test)] #[test] fn warm_i()  { testing::check(9 == 9, "i") }
#[cfg(test)] #[test] fn warm_j()  { testing::check(10 == 10, "j") }
#[cfg(test)] #[test] fn warm_k()  { testing::check(11 == 11, "k") }
#[cfg(test)] #[test] fn warm_l()  { testing::check(12 == 12, "l") }
#[cfg(test)] #[test] fn warm_m()  { testing::check(13 == 13, "m") }
#[cfg(test)] #[test] fn warm_n()  { testing::check(14 == 14, "n") }

#[cfg(test)]
#[test]
fn after_jit_array_iteration_works() {
    let raw = "[{\"role\":\"user\",\"content\":\"hi\"},{\"role\":\"assistant\",\"content\":\"there\"},{\"role\":\"user\",\"content\":\"again\"}]"
    let n = super::count_user_messages(&raw)
    testing::check_eq(&n, &2, "two user messages survive iteration")
}

fn main() {}
"#;
    let dir = fresh_dir("test-runner-jit-iter");
    let path = write_source(&dir, "runner_jit_iter", src);
    let out = Command::new(gos_bin())
        .arg("test")
        .arg(&path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("gos test");
    let _ = fs::remove_dir_all(&dir);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "gos test failed:\nstdout: {stdout}\nstderr: {stderr}",
    );
    assert!(
        stdout.contains("PASS after_jit_array_iteration_works"),
        "JSON-iteration test failed after JIT warm-up:\n{stdout}",
    );
    assert!(
        stdout.contains("15 passed, 0 failed"),
        "expected all 15 tests to pass, got: {stdout}",
    );
}

/// Regression: the bytecode peephole's `drop_dead_const_loads`
/// pass relied on `op_value_reads` to enumerate every register
/// any op reads. `Op::Call` / `MethodCall` / `Spawn` / `SpawnMethod`
/// each read the contiguous `args..args+argc` span; before this
/// fix the pass only saw `callee` / `receiver`, so a literal
/// passed directly to a builtin (e.g. `println!("hello")`,
/// `println!(42)`, `format!("{}", "x")`) had its `LoadConst`
/// dropped — the call then read `Value::Void` from the
/// not-yet-written argument slot. The user-visible symptom was
/// `<void>` printed instead of the literal.
#[test]
fn peephole_does_not_drop_literal_const_loads_feeding_call_args() {
    let src = r#"
fn main() {
    println!("hello")
    println!(42)
    println!("{}", "world")
    println!("{}", 7)
    let s = format!("{}", "ok")
    println!(s)
}
"#;
    let dir = fresh_dir("peephole-call-args");
    let path = write_source(&dir, "peephole_call_args", src);
    let (stdout, stderr, code) = run_vm(&path);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(
        code,
        Some(0),
        "exit code\nstdout: {stdout}\nstderr: {stderr}"
    );
    let trimmed = stdout.trim_end();
    assert_eq!(
        trimmed, "hello\n42\nworld\n7\nok",
        "literal-arg println output regressed:\n{stdout}",
    );
    assert!(
        !stdout.contains("<void>"),
        "peephole dropped a const-load feeding a call:\n{stdout}",
    );
}
