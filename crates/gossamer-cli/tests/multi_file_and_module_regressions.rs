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

const PER_RUN_TIMEOUT: Duration = Duration::from_mins(1);

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
        .expect("spawn gos");
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

/// Writes a multi-file project: a `project.toml` carrying `id` plus each
/// `(relative_path, contents)` pair under a fresh scratch dir. Returns
/// the project root.
fn write_project(tag: &str, id: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = fresh_dir(tag);
    fs::write(
        dir.join("project.toml"),
        format!("[project]\nid = \"{id}\"\nversion = \"0.1.0\"\n"),
    )
    .unwrap();
    for (rel, contents) in files {
        let path = dir.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }
    dir
}

/// Runs `gos run .` at the project root (VM tier), resolving the entry itself.
fn project_run_vm(dir: &Path) -> (String, String, Option<i32>) {
    let child = Command::new(gos_bin())
        .arg("run")
        .arg(".")
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn gos");
    run_with_timeout(child)
}

/// Builds at the project root (Cranelift native) and runs the produced
/// `id_tail`-named binary from `target/debug`.
fn project_build_run(dir: &Path, id_tail: &str) -> (String, String, Option<i32>) {
    let build = Command::new(gos_bin())
        .arg("build")
        .current_dir(dir)
        .output()
        .expect("spawn gos build");
    assert!(
        build.status.success(),
        "gos build failed:\nstderr: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let mut bin = dir.join("target/debug").join(id_tail);
    if !std::env::consts::EXE_EXTENSION.is_empty() {
        bin.set_extension(std::env::consts::EXE_EXTENSION);
    }
    assert!(bin.is_file(), "expected binary at {}", bin.display());
    run_native(&bin)
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
    let mut bin_path = dir.join("target/debug/probe");
    if !std::env::consts::EXE_EXTENSION.is_empty() {
        bin_path.set_extension(std::env::consts::EXE_EXTENSION);
    }
    assert!(bin_path.is_file(), "expected probe binary at {bin_path:?}");
    let run = run_native(&bin_path);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(run.2, Some(0), "stderr: {}", run.1);
    assert!(run.0.trim() == "3", "expected stdout '3', got: {:?}", run.0);
}

#[test]
fn directory_modules_nest_and_carry_types() {
    // `src/<dir>/<dir>/mod.gos` is two module levels below the entry, so
    // every module the layout declares has to be nameable from the entry
    // for `deep::nest::*` to resolve. The struct pins the second half:
    // its structural `eq` is synthesized against the type's declaring
    // module, and `==` must work on both tiers.
    let dir = write_project(
        "nested-dir-modules",
        "example.com/nested",
        &[
            (
                "src/main.gos",
                "fn main() {\n    println!(\"{}\", deep::depth())\n    \
                 println!(\"{}\", deep::nest::nested())\n    \
                 let a = deep::nest::Nested { n: 5 }\n    \
                 let b = deep::nest::Nested { n: 5 }\n    \
                 println!(\"{} {}\", a.n, a == b)\n}\n",
            ),
            (
                "src/deep/mod.gos",
                "pub fn depth() -> i64 { self::nest::nested() - 41 }\n",
            ),
            (
                "src/deep/nest/mod.gos",
                "pub struct Nested { n: i64 }\npub fn nested() -> i64 { 42 }\n",
            ),
        ],
    );
    let expected = "1\n42\n5 true\n";
    let vm = project_run_vm(&dir);
    assert_eq!(vm.2, Some(0), "vm stderr: {}", vm.1);
    assert_eq!(vm.0, expected, "vm stdout");
    let native = project_build_run(&dir, "nested");
    assert_eq!(native.2, Some(0), "native stderr: {}", native.1);
    assert_eq!(native.0, expected, "native stdout");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn module_relative_paths_reach_child_and_sibling_modules() {
    // A path written inside a module is anchored at that module:
    // `self::child::item` and a bare `child::item` both name the child,
    // and `super::sibling::item` crosses to a sibling. All three have to
    // reach the same def the entry names as `mod::child::item`.
    let dir = write_project(
        "module-relative-paths",
        "example.com/relpaths",
        &[
            (
                "src/main.gos",
                "fn main() { println!(\"{}\", outer::all()) }\n",
            ),
            (
                "src/outer/mod.gos",
                "pub mod child {\n    pub fn value() -> i64 { 7 }\n}\n\
                 pub fn all() -> i64 {\n    self::child::value() + child::value() \
                 + super::other::value()\n}\n",
            ),
            ("src/other.gos", "pub fn value() -> i64 { 1 }\n"),
        ],
    );
    let expected = "15\n";
    let vm = project_run_vm(&dir);
    assert_eq!(vm.2, Some(0), "vm stderr: {}", vm.1);
    assert_eq!(vm.0, expected, "vm stdout");
    let native = project_build_run(&dir, "relpaths");
    assert_eq!(native.2, Some(0), "native stderr: {}", native.1);
    assert_eq!(native.0, expected, "native stdout");
    let _ = fs::remove_dir_all(&dir);
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

    // VM tier (`gos`).
    let run_out = Command::new(gos_bin())
        .arg("run")
        .arg(".")
        .current_dir(&dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("gos");
    assert!(
        run_out.status.success(),
        "gos failed:\nstderr: {}",
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
    let mut bin_path = dir.join("target/debug/chained");
    if !std::env::consts::EXE_EXTENSION.is_empty() {
        bin_path.set_extension(std::env::consts::EXE_EXTENSION);
    }
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
        r#"use std::fs
fn main() {{
    let p: String = "{p}".to_string()
    match fs::read_to_string(&p) {{
        Ok(s) => println!("ok len={{}} content={{}}", s.len(), s),
        Err(e) => println!("err: {{}}", e),
    }}
    println!("exists = {{}}", fs::exists(&p))
}}
"#,
        // Embed with forward slashes: a Windows path has backslashes, and
        // `\a` / `\g` / `\t` … are escape sequences inside a `.gos` string
        // literal, so a raw backslash path corrupts to a nonexistent file.
        // Windows filesystem APIs accept `/` separators.
        p = payload_path.display().to_string().replace('\\', "/"),
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
    let mut argv: Vec<String> = Vec::from([]).to_vec()
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
/// 1. `exec::run` had no compiled-tier binding - the call
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
    let args: Vec<String> = [
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
        "process aborted (likely the [..].to_vec() segfault) - stdout: {:?}, stderr: {:?}",
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
/// `f(closure_ptr, payload)`\n on x86_64 the closure's `v` param
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
/// `json::as_i64(v).unwrap_or(0)` - both pieces were broken in
/// compiled mode:
/// 1. `Vec<String>[idx] = s` assignment was a no-op (the projection
///    machinery treated the Vec as a flat array\n the data lives at
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
    let mut xs: Vec<String> = Vec::from([]).to_vec()
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
/// rather than a stack `[T; N]` aggregate\n reading element
/// 0/1 as `len`/`cap` produced terabyte-scale allocations
/// and aborted with `memory allocation of <huge> bytes
/// failed` or a plain segfault. Both `i64` and `String`
/// element shapes are exercised because they pick different
/// elem_bytes paths inside `gos_rt_vec_from_arr`.
#[test]
fn native_binary_literal_array_to_vec_does_not_segfault() {
    let src = r#"
fn main() {
    let xs: Vec<i64> = [10, 20, 30].to_vec()
    println!("i64 len={} 0={} 1={} 2={}", xs.len(), xs[0], xs[1], xs[2])
    let ys: Vec<String> = ["a".to_string(), "b".to_string(), "c".to_string()].to_vec()
    println!("str len={} 0={} 1={} 2={}", ys.len(), ys[0], ys[1], ys[2])
    let zs: Vec<String> = ["aa", "bb", "cc"].to_vec()
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
        "process aborted - stdout: {:?}, stderr: {:?}",
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
    // landed\n the callee's `gos_rt_vec_len` then read the first
    // tuple element as the length and segfaulted on the
    // subsequent index dispatch. Asserting on both `gos` and
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
    // observed and counted against each test's tally - otherwise
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
    let _ = testing::check_eq(&s.len(), &47, "length sanity")
}

#[cfg(test)]
#[test]
fn after_jit_string_eq() {
    let _ = testing::check_eq(&"hi".to_string(), &"hi".to_string(), "string eq")
}

#[cfg(test)]
#[test]
fn after_jit_check_true() {
    let _ = testing::check(true, "always true")
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
    // any method dispatched through `gos_rt_http_client_*` -
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
    // per-test Vm reset in the runner\n this test pins that.
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

#[cfg(test)] #[test] fn warm_a()  { let _ = testing::check(1 == 1, "a") }
#[cfg(test)] #[test] fn warm_b()  { let _ = testing::check(2 == 2, "b") }
#[cfg(test)] #[test] fn warm_c()  { let _ = testing::check(3 == 3, "c") }
#[cfg(test)] #[test] fn warm_d()  { let _ = testing::check(4 == 4, "d") }
#[cfg(test)] #[test] fn warm_e()  { let _ = testing::check(5 == 5, "e") }
#[cfg(test)] #[test] fn warm_f()  { let _ = testing::check(6 == 6, "f") }
#[cfg(test)] #[test] fn warm_g()  { let _ = testing::check(7 == 7, "g") }
#[cfg(test)] #[test] fn warm_h()  { let _ = testing::check(8 == 8, "h") }
#[cfg(test)] #[test] fn warm_i()  { let _ = testing::check(9 == 9, "i") }
#[cfg(test)] #[test] fn warm_j()  { let _ = testing::check(10 == 10, "j") }
#[cfg(test)] #[test] fn warm_k()  { let _ = testing::check(11 == 11, "k") }
#[cfg(test)] #[test] fn warm_l()  { let _ = testing::check(12 == 12, "l") }
#[cfg(test)] #[test] fn warm_m()  { let _ = testing::check(13 == 13, "m") }
#[cfg(test)] #[test] fn warm_n()  { let _ = testing::check(14 == 14, "n") }

#[cfg(test)]
#[test]
fn after_jit_array_iteration_works() {
    let raw = "[{\"role\":\"user\",\"content\":\"hi\"},{\"role\":\"assistant\",\"content\":\"there\"},{\"role\":\"user\",\"content\":\"again\"}]"
    let n = super::count_user_messages(&raw)
    let _ = testing::check_eq(&n, &2, "two user messages survive iteration")
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
/// `println(42)`, `format!("{}", "x")`) had its `LoadConst`
/// dropped - the call then read `Value::Void` from the
/// not-yet-written argument slot. The user-visible symptom was
/// `<void>` printed instead of the literal.
#[test]
fn peephole_does_not_drop_literal_const_loads_feeding_call_args() {
    let src = r#"
fn main() {
    println!("hello")
    println(42)
    println!("{}", "world")
    println!("{}", 7)
    let s = format!("{}", "ok")
    println(s)
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

#[test]
fn test_runner_failed_test_prints_call_chain_traceback() {
    // A `#[test]` that panics deep in a nested call chain must report
    // not only the panic message (byte-identical to before the VM
    // switch) but also the VM's preserved call stack, so a failure
    // points at the path that reached it rather than just the leaf.
    let src = r#"
fn deepest(n: i64) -> i64 {
    if n == 0 {
        panic!("boom at the bottom")
    }
    n
}

fn middle(n: i64) -> i64 {
    deepest(n - 1)
}

fn top() -> i64 {
    middle(1)
}

#[cfg(test)]
mod tb_tests {
    #[test]
    fn panics_in_nested_call() {
        let _ = super::top()
    }
}

fn main() {}
"#;
    let dir = fresh_dir("test-runner-traceback");
    let path = write_source(&dir, "runner_tb", src);
    let out = Command::new(gos_bin())
        .arg("test")
        .arg(&path)
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("gos test");
    let _ = fs::remove_dir_all(&dir);
    let stdout = String::from_utf8_lossy(&out.stdout);
    // A failing test makes the runner exit non-zero.
    assert!(
        !out.status.success(),
        "expected non-zero exit for a failing test:\n{stdout}",
    );
    // Message text is unchanged by the traceback addition.
    assert!(
        stdout.contains("FAIL panics_in_nested_call") && stdout.contains("boom at the bottom"),
        "panic message regressed:\n{stdout}",
    );
    // The call chain is rendered outermost-first, one frame per call.
    assert!(
        stdout.contains("call stack (outermost first):"),
        "no traceback header:\n{stdout}",
    );
    for frame in ["panics_in_nested_call", "top", "middle", "deepest"] {
        assert!(
            stdout.contains(&format!("at {frame}")),
            "traceback missing frame `{frame}`:\n{stdout}",
        );
    }
}

#[test]
fn cross_module_struct_field_access_resolves_on_all_tiers() {
    // `pub struct Rec` lives in src/util.gos\n src/main.gos uses `&util::Rec`
    // as a param annotation and `&mut util::Rec` for a writeback. Both are
    // type-path annotations that must resolve to the struct's Adt so that
    // field access lowers to a real Field projection on every tier.
    let dir = fresh_dir("cross-mod-struct");
    let src = dir.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(
        dir.join("project.toml"),
        "[project]\nid = \"example.com/xmod2\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    fs::write(
        src.join("util.gos"),
        concat!(
            "pub struct Rec { pub name: String, pub age: i64 }\n",
            "pub fn make(name: String, age: i64) -> Rec { Rec { name: name, age: age } }\n",
            "pub fn birthday(r: &mut util::Rec) { r.age += 1 }\n",
        ),
    )
    .unwrap();
    fs::write(
        src.join("main.gos"),
        concat!(
            "fn describe(r: &util::Rec) -> String {\n",
            "    format!(\"{} is {}\", r.name, r.age)\n",
            "}\n",
            "fn main() {\n",
            "    let mut r = util::make(\"ada\", 36)\n",
            "    util::birthday(&mut r)\n",
            "    println!(\"{}\", describe(&r))\n",
            "}\n",
        ),
    )
    .unwrap();

    let run_out = Command::new(gos_bin())
        .arg("run")
        .arg(".")
        .current_dir(&dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("gos");
    assert!(
        run_out.status.success(),
        "gos failed:\nstderr: {}",
        String::from_utf8_lossy(&run_out.stderr),
    );
    assert_eq!(
        String::from_utf8_lossy(&run_out.stdout).trim(),
        "ada is 37",
        "VM stdout mismatch",
    );

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
    let mut bin_path = dir.join("target/debug/xmod2");
    if !std::env::consts::EXE_EXTENSION.is_empty() {
        bin_path.set_extension(std::env::consts::EXE_EXTENSION);
    }
    assert!(bin_path.is_file(), "expected binary at {bin_path:?}");
    let nat = run_native(&bin_path);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(nat.2, Some(0), "native stderr: {}", nat.1);
    assert_eq!(
        nat.0.trim(),
        "ada is 37",
        "native stdout mismatch: {:?}",
        nat.0
    );
}

#[test]
fn cross_file_from_json_on_sibling_struct() {
    // A struct declared in a non-entry file, decoded via `from_json::<T>`
    // in a THIRD file, driven from `main`. The sibling auto-bundle nests
    // the struct inside `mod types { ... }`, so the serde synthesizer must
    // descend into inline modules to emit its per-type functions.
    let dir = write_project(
        "xfile-from-json",
        "example.com/fromjson",
        &[
            (
                "src/types.gos",
                "pub struct Point {\n    x: i64,\n    y: i64,\n    label: String,\n}\n",
            ),
            (
                "src/codec.gos",
                "use std::errors\n\
                 pub fn describe(text: &String) -> String {\n\
                 \x20   match from_json::<types::Point>(text) {\n\
                 \x20       Ok(p) => format!(\"{},{},{}\", p.x, p.y, p.label),\n\
                 \x20       Err(e) => format!(\"err: {}\", e),\n\
                 \x20   }\n\
                 }\n",
            ),
            (
                "src/main.gos",
                "fn main() { println!(\"{}\", codec::describe(&\"{\\\"x\\\":3,\\\"y\\\":4,\\\"label\\\":\\\"origin\\\"}\")) }\n",
            ),
        ],
    );
    let vm = project_run_vm(&dir);
    assert_eq!(vm.2, Some(0), "VM stderr: {}", vm.1);
    assert_eq!(vm.0.trim(), "3,4,origin", "VM stdout: {:?}", vm.0);
    let nat = project_build_run(&dir, "fromjson");
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(nat.2, Some(0), "native stderr: {}", nat.1);
    assert_eq!(nat.0.trim(), "3,4,origin", "native stdout: {:?}", nat.0);
}

#[test]
fn cross_file_to_json_derive_and_typeinfo_on_sibling_struct() {
    // `to_json::<T>`, an implicit `#[derive(Debug)]` / structural equality,
    // and `typeInfo::<T>()` over a struct declared in a non-entry file all
    // reach it through the module-nesting the sibling auto-bundle introduces.
    let dir = write_project(
        "xfile-derive",
        "example.com/derivemod",
        &[
            (
                "src/model.gos",
                "#[derive(Debug)]\n\
                 pub struct Rec {\n    id: i64,\n    name: String,\n}\n\
                 pub fn new(id: i64, name: String) -> Rec { Rec { id: id, name: name } }\n\
                 pub fn roundtrip(r: Rec) -> String {\n\
                 \x20   match to_json::<Rec>(r) {\n\
                 \x20       Ok(s) => s,\n\
                 \x20       Err(e) => format!(\"err: {}\", e),\n\
                 \x20   }\n\
                 }\n\
                 pub fn describe() -> String {\n\
                 \x20   let a = Rec { id: 1, name: \"x\" }\n\
                 \x20   let b = Rec { id: 1, name: \"x\" }\n\
                 \x20   let mut fields = \"\"\n\
                 \x20   for (n, t) in typeInfo::<Rec>() { fields += n\n fields += \":\"\n fields += t\n fields += \";\" }\n\
                 \x20   format!(\"{:?} eq={} fields={}\", a, a == b, fields)\n\
                 }\n",
            ),
            (
                "src/main.gos",
                "fn main() {\n\
                 \x20   println!(\"{}\", model::roundtrip(model::new(7, \"hi\")))\n\
                 \x20   println!(\"{}\", model::describe())\n\
                 }\n",
            ),
        ],
    );
    let expected =
        "{\"id\":7,\"name\":\"hi\"}\nRec { id: 1, name: \"x\" } eq=true fields=id:i64;name:String;";
    let vm = project_run_vm(&dir);
    assert_eq!(vm.2, Some(0), "VM stderr: {}", vm.1);
    assert_eq!(vm.0.trim(), expected, "VM stdout: {:?}", vm.0);
    let nat = project_build_run(&dir, "derivemod");
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(nat.2, Some(0), "native stderr: {}", nat.1);
    assert_eq!(nat.0.trim(), expected, "native stdout: {:?}", nat.0);
}

#[test]
fn cross_file_from_json_nested_struct_on_vm() {
    // The decoded struct itself carries a nested-struct field, and both
    // structs live in a non-entry file. The synthesizer must reach the
    // nested type through the module-nesting to emit its serde functions.
    let dir = write_project(
        "xfile-nested",
        "example.com/nestedmod",
        &[
            (
                "src/types.gos",
                "pub struct Inner {\n    status: String,\n}\n\
                 pub struct Outer {\n    is_error: bool,\n    inner: Inner,\n}\n",
            ),
            (
                "src/main.gos",
                "fn main() {\n\
                 \x20   match from_json::<types::Outer>(&\"{\\\"is_error\\\":false,\\\"inner\\\":{\\\"status\\\":\\\"ok\\\"}}\") {\n\
                 \x20       Ok(v) => println!(\"{} {}\", v.is_error, v.inner.status),\n\
                 \x20       Err(e) => println!(\"err: {}\", e),\n\
                 \x20   }\n\
                 }\n",
            ),
        ],
    );
    let vm = project_run_vm(&dir);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(vm.2, Some(0), "VM stderr: {}", vm.1);
    assert_eq!(vm.0.trim(), "false ok", "VM stdout: {:?}", vm.0);
}

#[test]
fn gos_test_discovers_tests_in_cross_referencing_files() {
    // A `#[test]` lives in a file whose top-level code imports a sibling
    // module's item, so the file does not typecheck in isolation - only
    // against the bundled whole-package source. Test discovery must parse
    // for `#[test]` names rather than fully checking each file alone, and
    // execution bundles siblings the same way `gos` / `gos build` do, so
    // the import resolves and the test runs.
    let dir = write_project(
        "gos-test-discovery",
        "example.com/testdisc",
        &[
            ("src/helper.gos", "pub fn base() -> i64 { 40 }\n"),
            (
                "src/main.gos",
                "use helper::base\n\
                 fn total() -> i64 { base() + 2 }\n\
                 fn main() { println!(\"{}\", total()) }\n\
                 #[cfg(test)]\n\
                 mod main_tests {\n\
                 \x20   use std::testing\n\
                 \x20   #[test]\n\
                 \x20   fn total_uses_sibling() {\n\
                 \x20       let _ = testing::check_eq(&super::total(), &42, \"40 + 2\")\n\
                 \x20   }\n\
                 }\n",
            ),
        ],
    );
    let out = Command::new(gos_bin())
        .arg("test")
        .current_dir(&dir)
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("gos test");
    let _ = fs::remove_dir_all(&dir);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "gos test should pass:\nstdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        stdout.contains("PASS total_uses_sibling") && stdout.contains("1 passed"),
        "expected the sibling-referencing test to be discovered and pass:\n{stdout}",
    );
}

#[test]
fn relative_entry_path_bundles_siblings() {
    // `gos run main.gos` from inside the project directory must bundle
    // sibling modules exactly like `gos run .` does. A bare relative
    // entry has an empty `parent()`, and an unabsolutized path made
    // the module scan read from the empty dir and silently bundle
    // nothing, so qualified sibling calls failed with GR0001.
    let dir = fresh_dir("relative-entry");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("project.toml"),
        "[project]\nid = \"example.com/relentry\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    fs::write(
        dir.join("util.gos"),
        "pub fn add(a: i64, b: i64) -> i64 { a + b }\n",
    )
    .unwrap();
    fs::write(
        dir.join("main.gos"),
        "fn main() { println!(\"{}\", util::add(1, 2)) }\n",
    )
    .unwrap();
    let child = Command::new(gos_bin())
        .arg("run")
        .arg("main.gos")
        .current_dir(&dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn gos");
    let out = run_with_timeout(child);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(out.2, Some(0), "stderr: {}", out.1);
    assert_eq!(out.0.trim(), "3", "expected stdout '3', got: {:?}", out.0);
}

/// Writes a dependency project at `root/<name>` with the given id and
/// lib source, returning its directory.
fn write_dep_project(root: &Path, name: &str, id: &str, lib: &str) -> PathBuf {
    let dir = root.join(name);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("project.toml"),
        format!("[project]\nid = \"{id}\"\nversion = \"0.1.0\"\n"),
    )
    .unwrap();
    fs::write(dir.join("lib.gos"), lib).unwrap();
    dir
}

fn write_app_project(root: &Path, deps: &str, main: &str) -> PathBuf {
    let dir = root.join("app");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("project.toml"),
        format!(
            "[project]\nid = \"example.com/app\"\nversion = \"0.1.0\"\n\n[dependencies]\n{deps}"
        ),
    )
    .unwrap();
    fs::write(dir.join("main.gos"), main).unwrap();
    dir
}

#[test]
fn path_dependency_links_at_run() {
    let root = fresh_dir("path-dep-run");
    write_dep_project(
        &root,
        "dep",
        "example.com/dep",
        "pub fn greet(name: &String) -> String { format!(\"hi {}\", name) }\n",
    );
    let app = write_app_project(
        &root,
        "dep = { path = \"../dep\" }\n",
        "use \"example.com/dep\" as dep\n\nfn main() { println!(\"{}\", dep::greet(&\"gos\")) }\n",
    );
    let out = project_run_vm(&app);
    let _ = fs::remove_dir_all(&root);
    assert_eq!(out.2, Some(0), "stderr: {}", out.1);
    assert_eq!(out.0.trim(), "hi gos", "stdout: {:?}", out.0);
}

#[test]
fn path_dependency_links_at_build() {
    let root = fresh_dir("path-dep-build");
    write_dep_project(
        &root,
        "dep",
        "example.com/dep",
        "pub fn add(a: i64, b: i64) -> i64 { a + b }\n",
    );
    let app = write_app_project(
        &root,
        "dep = { path = \"../dep\" }\n",
        "use \"example.com/dep\" as d\n\nfn main() { println!(\"{}\", d::add(20, 22)) }\n",
    );
    let out = project_build_run(&app, "app");
    let _ = fs::remove_dir_all(&root);
    assert_eq!(out.2, Some(0), "stderr: {}", out.1);
    assert_eq!(out.0.trim(), "42", "stdout: {:?}", out.0);
}

#[test]
fn check_rejects_unknown_path_dep_member() {
    let root = fresh_dir("path-dep-check");
    write_dep_project(
        &root,
        "dep",
        "example.com/dep",
        "pub fn greet() -> String { \"hi\" }\n",
    );
    let app = write_app_project(
        &root,
        "dep = { path = \"../dep\" }\n",
        "use \"example.com/dep\" as dep\n\nfn main() { println!(\"{}\", dep::nonexistent()) }\n",
    );
    let out = Command::new(gos_bin())
        .arg("check")
        .arg(".")
        .current_dir(&app)
        .output()
        .expect("spawn gos check");
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let _ = fs::remove_dir_all(&root);
    assert!(
        !out.status.success(),
        "check must reject a nonexistent dep member, stderr: {stderr}"
    );
    assert!(
        stderr.contains("nonexistent"),
        "diagnostic should name the missing member: {stderr}"
    );
}

#[test]
fn transitive_path_dependency_links_at_run() {
    let root = fresh_dir("path-dep-transitive");
    write_dep_project(
        &root,
        "base",
        "example.com/base",
        "pub fn two() -> i64 { 2 }\n",
    );
    let mid = root.join("mid");
    fs::create_dir_all(&mid).unwrap();
    fs::write(
        mid.join("project.toml"),
        "[project]\nid = \"example.com/mid\"\nversion = \"0.1.0\"\n\n[dependencies]\nbase = { path = \"../base\" }\n",
    )
    .unwrap();
    fs::write(
        mid.join("lib.gos"),
        "use \"example.com/base\" as base\n\npub fn double_two() -> i64 { base::two() * 2 }\n",
    )
    .unwrap();
    let app = write_app_project(
        &root,
        "mid = { path = \"../mid\" }\n",
        "use \"example.com/mid\" as mid\n\nfn main() { println!(\"{}\", mid::double_two()) }\n",
    );
    let out = project_run_vm(&app);
    let _ = fs::remove_dir_all(&root);
    assert_eq!(out.2, Some(0), "stderr: {}", out.1);
    assert_eq!(out.0.trim(), "4", "stdout: {:?}", out.0);
}

#[test]
fn same_fn_name_in_two_sibling_modules_runs() {
    // Two bundled modules may each define `add`; bare in-module
    // references bind to the module's own item, qualified calls to
    // the named module's. Previously a flat namespace made this a
    // GR0003 duplicate-definition error.
    let dir = write_project(
        "same-name-two-modules",
        "example.com/samename",
        &[
            (
                "alpha.gos",
                "pub fn add(a: i64, b: i64) -> i64 { a + b }\npub fn twice(a: i64) -> i64 { add(a, a) }\n",
            ),
            (
                "beta.gos",
                "pub fn add(a: i64, b: i64) -> i64 { (a + b) * 10 }\n",
            ),
            (
                "main.gos",
                "fn main() {\n    println!(\"{} {} {}\", alpha::add(1, 2), beta::add(1, 2), alpha::twice(4))\n}\n",
            ),
        ],
    );
    let run = project_run_vm(&dir);
    assert_eq!(run.2, Some(0), "vm stderr: {}", run.1);
    assert_eq!(run.0.trim(), "3 30 8", "vm stdout: {:?}", run.0);
    let build = project_build_run(&dir, "samename");
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(build.2, Some(0), "native stderr: {}", build.1);
    assert_eq!(build.0.trim(), "3 30 8", "native stdout: {:?}", build.0);
}

/// A library's own `impl` block reaches its private helpers. The
/// dependency is inlined as `mod lib { ... }`, so the methods have to be
/// recorded against the identity a receiver of that type carries.
#[test]
fn a_librarys_impl_reaches_its_own_private_method() {
    let root = fresh_dir("dep-private-method");
    write_dep_project(
        &root,
        "lib",
        "example.com/lib",
        "pub struct Point {\n    x: i64\n    y: i64\n}\n\n\
         impl Point {\n\
             pub fn new(x: i64, y: i64) -> Self { Point { x: x, y: y } }\n\
             pub fn public_dist(self) -> i64 { self.internal_dist() }\n\
             fn internal_dist(self) -> i64 { self.x + self.y }\n\
         }\n",
    );
    let app = write_app_project(
        &root,
        "lib = { path = \"../lib\" }\n",
        "use \"example.com/lib\" as lib\n\n\
         fn main() {\n\
             let point = lib::Point::new(1, 2)\n\
             println!(\"{}\", point.public_dist())\n\
         }\n",
    );
    let run = project_run_vm(&app);
    assert_eq!(run.2, Some(0), "vm stderr: {}", run.1);
    assert_eq!(run.0.trim(), "3", "vm stdout: {:?}", run.0);
    let build = project_build_run(&app, "app");
    let _ = fs::remove_dir_all(&root);
    assert_eq!(build.2, Some(0), "native stderr: {}", build.1);
    assert_eq!(build.0.trim(), "3", "native stdout: {:?}", build.0);
}

/// A diagnostic raised inside a path dependency names the dependency's
/// own file and its line there, not the consumer's entry file at the
/// offset the dependency happened to be inlined at.
#[test]
fn a_diagnostic_in_a_path_dependency_names_the_dependencys_file() {
    let root = fresh_dir("dep-diag-origin");
    write_dep_project(
        &root,
        "lib",
        "example.com/lib",
        "pub struct Point {\n    x: i64\n}\n\n\
         impl Point {\n\
             pub fn get(self) -> i64 { self.nosuchmethod() }\n\
         }\n",
    );
    let app = write_app_project(
        &root,
        "lib = { path = \"../lib\" }\n",
        "use \"example.com/lib\" as lib\n\n\
         fn main() { println!(\"{}\", lib::Point { x: 1 }.get()) }\n",
    );
    let out = project_run_vm(&app);
    let _ = fs::remove_dir_all(&root);
    assert_ne!(out.2, Some(0), "expected a failure, stdout: {:?}", out.0);
    assert!(
        out.1.contains("lib.gos:6:"),
        "diagnostic must point at the dependency's own file and line: {}",
        out.1
    );
    assert!(
        !out.1.contains("main.gos:"),
        "diagnostic must not be attributed to the consumer's entry: {}",
        out.1
    );
}

/// `use "id" as alias` reaches a dependency's associated functions, not
/// just its free functions. Items are registered under the inlined
/// module's real name, so a renaming alias has to be respelled before
/// name-keyed dispatch.
#[test]
fn a_renaming_alias_reaches_a_dependencys_associated_function() {
    let root = fresh_dir("dep-alias-assoc");
    write_dep_project(
        &root,
        "lib",
        "example.com/lib",
        "pub struct Point {\n    x: i64\n    y: i64\n}\n\n\
         impl Point {\n\
             pub fn new(x: i64, y: i64) -> Self { Point { x: x, y: y } }\n\
             pub fn sum(self) -> i64 { self.x + self.y }\n\
         }\n\n\
         pub fn answer() -> i64 { 42 }\n",
    );
    let app = write_app_project(
        &root,
        "lib = { path = \"../lib\" }\n",
        "use \"example.com/lib\" as mylib\n\n\
         fn main() {\n\
             println!(\"{} {}\", mylib::Point::new(1, 2).sum(), mylib::answer())\n\
         }\n",
    );
    let run = project_run_vm(&app);
    assert_eq!(run.2, Some(0), "vm stderr: {}", run.1);
    assert_eq!(run.0.trim(), "3 42", "vm stdout: {:?}", run.0);
    let build = project_build_run(&app, "app");
    let _ = fs::remove_dir_all(&root);
    assert_eq!(build.2, Some(0), "native stderr: {}", build.1);
    assert_eq!(build.0.trim(), "3 42", "native stdout: {:?}", build.0);
}

/// A member the dependency genuinely does not export is still rejected
/// at check time through an alias.
#[test]
fn a_phantom_member_through_an_alias_is_still_rejected() {
    let root = fresh_dir("dep-alias-phantom");
    write_dep_project(
        &root,
        "lib",
        "example.com/lib",
        "pub fn real() -> i64 { 1 }\n",
    );
    let app = write_app_project(
        &root,
        "lib = { path = \"../lib\" }\n",
        "use \"example.com/lib\" as mylib\n\n\
         fn main() { println!(\"{}\", mylib::nosuchfn()) }\n",
    );
    let out = project_run_vm(&app);
    let _ = fs::remove_dir_all(&root);
    assert_ne!(out.2, Some(0), "expected a failure, stdout: {:?}", out.0);
    assert!(
        out.1.contains("GR0001"),
        "a phantom member must be rejected at check time: {}",
        out.1
    );
}

/// An item without `pub` is private to the module that declares it, so
/// a consumer reaches the `pub` wrapper but never the helper behind it.
#[test]
fn a_dependencys_private_surface_stays_inside_the_dependency() {
    let root = fresh_dir("dep-private-surface");
    write_dep_project(
        &root,
        "lib",
        "example.com/lib",
        "pub struct Point {\n    x: i64\n    y: i64\n}\n\n\
         impl Point {\n\
             pub fn new(x: i64, y: i64) -> Self { Point { x: x, y: y } }\n\
             pub fn public_dist(self) -> i64 { self.internal_dist() }\n\
             fn internal_dist(self) -> i64 { self.x + self.y }\n\
         }\n\n\
         pub fn wrapper() -> i64 { helper() }\n\n\
         fn helper() -> i64 { 99 }\n",
    );
    let reachable = |body: &str| -> (String, String, Option<i32>) {
        let app = write_app_project(&root, "lib = { path = \"../lib\" }\n", body);
        project_run_vm(&app)
    };

    let ok = reachable(
        "use \"example.com/lib\"\n\n\
         fn main() {\n\
             println!(\"{} {}\", lib::wrapper(), lib::Point::new(1, 2).public_dist())\n\
         }\n",
    );
    assert_eq!(
        ok.2,
        Some(0),
        "public surface must stay reachable: {}",
        ok.1
    );
    assert_eq!(ok.0.trim(), "99 3", "stdout: {:?}", ok.0);

    let private_fn =
        reachable("use \"example.com/lib\"\n\nfn main() { println!(\"{}\", lib::helper()) }\n");
    assert_ne!(
        private_fn.2,
        Some(0),
        "private fn leaked: {:?}",
        private_fn.0
    );
    assert!(
        private_fn.1.contains("GR0008"),
        "expected a visibility error: {}",
        private_fn.1
    );

    let private_method = reachable(
        "use \"example.com/lib\"\n\n\
         fn main() { println!(\"{}\", lib::Point::new(1, 2).internal_dist()) }\n",
    );
    assert_ne!(
        private_method.2,
        Some(0),
        "private method leaked: {:?}",
        private_method.0
    );
    assert!(
        private_method.1.contains("GT0063"),
        "expected a visibility error: {}",
        private_method.1
    );

    let _ = fs::remove_dir_all(&root);
}

/// A nested module reaches the private items of the module that
/// contains it, which is what the documented `#[cfg(test)]` block does.
#[test]
fn a_nested_module_reaches_its_parents_private_items() {
    let dir = write_project(
        "inline-mod-private",
        "example.com/inline",
        &[(
            "main.gos",
            "struct P { x: i64 }\n\
             impl P {\n\
                 pub fn new(x: i64) -> Self { P { x: x } }\n\
                 fn secret(self) -> i64 { self.x * 3 }\n\
             }\n\n\
             fn hidden() -> i64 { 4 }\n\n\
             mod inner {\n\
                 pub fn reach() -> i64 { super::P::new(2).secret() + super::hidden() }\n\
             }\n\n\
             fn main() { println!(\"{}\", inner::reach()) }\n",
        )],
    );
    let run = project_run_vm(&dir);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(run.2, Some(0), "vm stderr: {}", run.1);
    assert_eq!(run.0.trim(), "10", "vm stdout: {:?}", run.0);
}

/// A descendant module keeps access to its parent's private items, and
/// the entry - which is not a descendant - does not.
#[test]
fn a_descendant_module_reaches_its_parents_private_items() {
    let dir = write_project(
        "nested-module-private",
        "example.com/nested",
        &[
            (
                "src/sub/mod.gos",
                "fn secret() -> i64 { 7 }\npub fn own() -> i64 { secret() }\n",
            ),
            (
                "src/sub/child.gos",
                "pub fn peek() -> i64 { super::secret() }\n",
            ),
            (
                "src/main.gos",
                "fn main() { println!(\"{} {}\", sub::own(), sub::child::peek()) }\n",
            ),
        ],
    );
    let run = project_run_vm(&dir);
    assert_eq!(run.2, Some(0), "vm stderr: {}", run.1);
    assert_eq!(run.0.trim(), "7 7", "vm stdout: {:?}", run.0);

    fs::write(
        dir.join("src/main.gos"),
        "fn main() { println!(\"{}\", sub::secret()) }\n",
    )
    .unwrap();
    let outside = project_run_vm(&dir);
    let _ = fs::remove_dir_all(&dir);
    assert_ne!(outside.2, Some(0), "private item leaked: {:?}", outside.0);
    assert!(
        outside.1.contains("GR0008"),
        "expected a visibility error: {}",
        outside.1
    );
}
