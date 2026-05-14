//! Regression tests covering shapes that previously crashed,
//! mis-dispatched, or surfaced wrong values. Each `#[test]` runs
//! a small Gossamer program through `gos run` (or `gos build`)
//! and asserts the user-visible output. A regression in any of
//! the underlying fixes turns the test red. Tests are named
//! after the property under test, not after a bug number — the
//! file location is the regression-guard context.

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
        "gos-bugreg-{pid}-{n}-{tag}",
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

fn build_native(src: &Path, scratch: &Path) -> Result<PathBuf, String> {
    build_native_with_flag(src, scratch, /*release=*/ false)
}

fn build_native_release(src: &Path, scratch: &Path) -> Result<PathBuf, String> {
    build_native_with_flag(src, scratch, /*release=*/ true)
}

fn build_native_with_flag(src: &Path, scratch: &Path, release: bool) -> Result<PathBuf, String> {
    let mut cmd = Command::new(gos_bin());
    cmd.arg("build");
    if release {
        cmd.arg("--release");
    }
    cmd.arg("--out-dir").arg(scratch).arg(src);
    let out = cmd.output().expect("spawn gos build");
    if !out.status.success() {
        return Err(format!(
            "gos build{} failed:\n  stderr: {}",
            if release { " --release" } else { "" },
            String::from_utf8_lossy(&out.stderr),
        ));
    }
    for entry in fs::read_dir(scratch)
        .map_err(|e| format!("read_dir: {e}"))?
        .flatten()
    {
        let p = entry.path();
        if p.is_file() && is_executable(&p) {
            return Ok(p);
        }
    }
    Err(format!("no binary in {}", scratch.display()))
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
fn eprintln_runs_without_aborting_via_jit() {
    // Cranelift JIT must have `gos_rt_eprint_str` /
    // `gos_rt_eprintln` in its symbol table; without them
    // `eprintln!` aborts at startup with "can't resolve
    // symbol gos_rt_eprint_str".
    let dir = fresh_dir("eprintln_jit");
    let src = write_source(
        &dir,
        "eprintln_jit",
        "fn main() { eprintln!(\"diag-line\") }\n",
    );
    let run = run_vm(&src);
    assert_eq!(run.2, Some(0), "vm: expected clean exit, got {:?}", run.2);
    assert!(
        run.1.contains("diag-line"),
        "vm: expected eprintln output on stderr, got: {:?}",
        run.1,
    );
    let cl_dir = dir.join("cl");
    fs::create_dir_all(&cl_dir).unwrap();
    let bin = build_native(&src, &cl_dir).expect("cranelift build");
    let native = run_native(&bin);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(
        native.2,
        Some(0),
        "native: expected clean exit, got {:?}",
        native.2
    );
    assert!(
        native.1.contains("diag-line"),
        "native: expected eprintln output on stderr, got: {:?}",
        native.1,
    );
}

#[test]
fn os_exit_flushes_stdout_in_native_binary() {
    // `os::exit(N)` after `println!` must flush the runtime's
    // stdout buffer before terminating; otherwise the buffered
    // output is dropped (`gos_rt_exit` previously skipped the
    // drain and called `std::process::exit` directly).
    let dir = fresh_dir("exit_flush");
    let src = write_source(
        &dir,
        "exit_flush",
        "use std::os\nfn main() {\n    println!(\"before exit\")\n    os::exit(2)\n}\n",
    );
    let cl_dir = dir.join("cl");
    fs::create_dir_all(&cl_dir).unwrap();
    let bin = build_native(&src, &cl_dir).expect("cranelift build");
    let run = run_native(&bin);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(run.2, Some(2), "expected exit code 2, got {:?}", run.2);
    assert!(
        run.0.contains("before exit"),
        "expected `before exit` on stdout, got: {:?}",
        run.0,
    );
}

#[test]
fn usize_index_works_for_string_arrays() {
    // `let mut i: usize = 0; arr[i]` must succeed even though
    // usize's runtime shape is `Value::U64`. The interp's
    // `index_get` previously only matched `Value::Int`, panicking
    // with "index must be integer".
    let src = r#"
fn try_it() {
    let mut args: [String] = [].to_vec()
    args.push("zero".to_string())
    args.push("one".to_string())
    let mut i: usize = 0
    while i < args.len() {
        println!("[{}] = {}", i, args[i].clone())
        i = i + 1
    }
}
fn main() { try_it() }
"#;
    let dir = fresh_dir("usize_index_string");
    let path = write_source(&dir, "usize_index_string", src);
    let run = run_vm(&path);
    assert_eq!(run.2, Some(0), "vm stderr: {}", run.1);
    assert!(
        run.0.contains("[0] = zero") && run.0.contains("[1] = one"),
        "vm: expected indexed output, got: {:?}",
        run.0,
    );
    let cl_dir = dir.join("cl");
    fs::create_dir_all(&cl_dir).unwrap();
    let bin = build_native(&path, &cl_dir).expect("cranelift build");
    let native = run_native(&bin);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(native.2, Some(0), "native stderr: {}", native.1);
    assert!(
        native.0.contains("[0] = zero") && native.0.contains("[1] = one"),
        "native: expected indexed output, got: {:?}",
        native.0,
    );
}

#[test]
fn indexing_tuple_slices_with_usize_works() {
    // `&[(String, String)]` indexed with `i: usize` must use the
    // index helper that accepts both `Value::Int` and
    // `Value::U64` — the tuple-of-String case fell through the
    // same `Value::Int`-only match as the string-array case
    // above.
    let src = r#"
fn first_key(vars: &[(String, String)]) -> String {
    if vars.len() == 0 { return "".to_string() }
    let mut i: usize = 0
    while i < vars.len() {
        let _ = vars[i].0.clone()
        i = i + 1
    }
    vars[0].0.clone()
}

fn main() {
    let pairs = [
        ("alpha".to_string(), "1".to_string()),
        ("beta".to_string(), "2".to_string()),
    ].to_vec()
    println!("{}", first_key(&pairs))
}
"#;
    let dir = fresh_dir("usize_index_tuple");
    let path = write_source(&dir, "usize_index_tuple", src);
    let run = run_vm(&path);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(run.2, Some(0), "stderr: {}", run.1);
    assert!(
        run.0.contains("alpha"),
        "expected first_key=alpha, got: {:?}",
        run.0,
    );
}

#[test]
fn iter_next_is_bound_and_returns_some_for_first() {
    // `xs.iter().next()` must dispatch through a `next` builtin
    // that returns `Some(first)` for non-empty collections and
    // `None` for empty ones (instead of erroring with "name
    // `next` is not bound in this scope" or silently returning
    // a non-matching value).
    let src = r#"
fn main() {
    let xs = ["a".to_string(), "b".to_string()].to_vec()
    let mut it = xs.iter()
    match it.next() {
        Some(s) => println!("first={}", s),
        None => println!("none"),
    }
    let empty: [i64] = [].to_vec()
    let mut it2 = empty.iter()
    match it2.next() {
        Some(_) => println!("unexpected some"),
        None => println!("empty-none"),
    }
}
"#;
    let dir = fresh_dir("iter_next");
    let path = write_source(&dir, "iter_next", src);
    let run = run_vm(&path);
    assert_eq!(run.2, Some(0), "vm stderr: {}", run.1);
    assert!(
        run.0.contains("first=a") && run.0.contains("empty-none"),
        "vm got: {:?}",
        run.0,
    );
    let cl_dir = dir.join("cl");
    fs::create_dir_all(&cl_dir).unwrap();
    let bin = build_native(&path, &cl_dir).expect("cranelift build");
    let native = run_native(&bin);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(native.2, Some(0), "native stderr: {}", native.1);
    assert!(
        native.0.contains("first=a") && native.0.contains("empty-none"),
        "native got: {:?}",
        native.0,
    );
}

#[test]
fn json_set_appends_and_replaces_fields() {
    // `json::set(obj, key, value)` must append a new field when
    // the key is missing and replace it when present, returning
    // the updated `json::Value::object()`. The free function
    // was previously unbound, surfacing as "name `json::set`
    // is not bound in this scope".
    let src = r#"
use std::encoding::json
fn main() {
    let obj = json::Value::object()
    let with_a = json::set(obj, &"a".to_string(), &json::Value::Int(1))
    let with_b = json::set(with_a, &"b".to_string(), &json::Value::String("hello".to_string()))
    let replaced = json::set(with_b, &"a".to_string(), &json::Value::Int(99))
    println!("{}", json::render(&replaced))
}
"#;
    let dir = fresh_dir("json_set");
    let path = write_source(&dir, "json_set", src);
    let run = run_vm(&path);
    assert_eq!(run.2, Some(0), "vm stderr: {}", run.1);
    assert!(
        run.0.contains("\"a\":99"),
        "vm: expected replaced a=99 in output, got: {:?}",
        run.0,
    );
    assert!(
        run.0.contains("\"b\":\"hello\""),
        "vm: expected b=hello in output, got: {:?}",
        run.0,
    );
    let cl_dir = dir.join("cl");
    fs::create_dir_all(&cl_dir).unwrap();
    let bin = build_native(&path, &cl_dir).expect("cranelift build");
    let native = run_native(&bin);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(native.2, Some(0), "native stderr: {}", native.1);
    assert!(
        native.0.contains("\"a\":99"),
        "native: expected replaced a=99 in output, got: {:?}",
        native.0,
    );
    assert!(
        native.0.contains("\"b\":\"hello\""),
        "native: expected b=hello in output, got: {:?}",
        native.0,
    );
}

#[test]
fn errors_wrap_chain_walks_through_cause() {
    // `errors::wrap(cause, msg)` must keep that argument order
    // (the SKILL-card-documented shape, used by every in-tree
    // example): the `msg` becomes the wrapped error's message
    // and the `cause` is reachable via `.cause()`. Build a
    // chain (`new` → `wrap`) and walk it.
    let src = r#"
use std::errors
fn main() {
    let inner = errors::new("inner failure")
    let wrapped = errors::wrap(inner, "outer context")
    println!("top: {}", wrapped.message())
    match wrapped.cause() {
        Some(c) => println!("cause: {}", c.message()),
        None => println!("no cause"),
    }
}
"#;
    let dir = fresh_dir("errors_wrap_chain");
    let path = write_source(&dir, "errors_wrap_chain", src);
    let run = run_vm(&path);
    assert_eq!(run.2, Some(0), "vm stderr: {}", run.1);
    assert!(
        run.0.contains("top: outer context"),
        "vm: expected wrapped message, got: {:?}",
        run.0,
    );
    assert!(
        run.0.contains("cause: inner failure"),
        "vm: expected inner cause, got: {:?}",
        run.0,
    );
    let cl_dir = dir.join("cl");
    fs::create_dir_all(&cl_dir).unwrap();
    let bin = build_native(&path, &cl_dir).expect("cranelift build");
    let native = run_native(&bin);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(native.2, Some(0), "native stderr: {}", native.1);
    assert!(
        native.0.contains("top: outer context"),
        "native: expected wrapped message, got: {:?}",
        native.0,
    );
    assert!(
        native.0.contains("cause: inner failure"),
        "native: expected inner cause, got: {:?}",
        native.0,
    );
}

#[test]
fn regex_captures_indexing_works() {
    // `caps[0]` / `caps[i]` must succeed via the same
    // `index_to_usize` helper that handles `Value::U64` and
    // `Value::Int` uniformly — the regex captures shape
    // previously panicked with "index must be integer".
    let src = r#"
use std::regex
fn main() {
    let re = regex::compile("(\\w+)=(\\d+)").unwrap()
    let caps = regex::captures_all(&re, &"a=1 b=22".to_string())
    if caps.len() == 0 {
        println!("no match")
    } else {
        let row = caps[0].clone()
        if row.len() < 2 {
            println!("too few groups")
        } else {
            match row[1].clone() {
                Some(s) => println!("first={}", s),
                None => println!("missing-group"),
            }
        }
    }
}
"#;
    let dir = fresh_dir("regex_captures_index");
    let path = write_source(&dir, "regex_captures_index", src);
    let run = run_vm(&path);
    assert_eq!(run.2, Some(0), "vm stderr: {}", run.1);
    assert!(
        run.0.contains("first=a"),
        "vm: expected first=a from regex captures, got: {:?}",
        run.0,
    );
    let cl_dir = dir.join("cl");
    fs::create_dir_all(&cl_dir).unwrap();
    let bin = build_native(&path, &cl_dir).expect("cranelift build");
    let native = run_native(&bin);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(native.2, Some(0), "native stderr: {}", native.1);
    assert!(
        native.0.contains("first=a"),
        "native: expected first=a from regex captures, got: {:?}",
        native.0,
    );
}

#[test]
fn continue_in_for_range_advances_counter() {
    // `for i in 0..n { if cond { continue } body }` previously
    // livelocked the bytecode VM: the for-range fast path set
    // `continue` to jump straight to the bounds-check header,
    // bypassing the fused increment-and-test op at the bottom of
    // the body. Hard timeout below would fire if the
    // `continue_patches` rewiring regressed.
    let src = r#"
fn main() {
    let mut acc: i64 = 0
    for i in 0..10 {
        if i % 4 == 0 {
            continue
        }
        acc = acc + i
    }
    println!("acc={}", acc)
}
"#;
    let dir = fresh_dir("continue_for_range");
    let path = write_source(&dir, "continue_for_range", src);
    let run = run_vm(&path);
    assert_eq!(run.2, Some(0), "vm stderr: {}", run.1);
    assert!(
        run.0.contains("acc=33"),
        "vm: expected acc=33, got: {:?}",
        run.0
    );
    let cl_dir = dir.join("cl");
    fs::create_dir_all(&cl_dir).unwrap();
    let bin = build_native(&path, &cl_dir).expect("cranelift build");
    let native = run_native(&bin);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(native.2, Some(0), "native stderr: {}", native.1);
    assert!(
        native.0.contains("acc=33"),
        "native: expected acc=33, got: {:?}",
        native.0
    );
}

#[test]
fn variant_constructors_pin_call_result_to_adt() {
    // `let first = Some(10)` previously typed `first` as
    // `Int(I64)` because the typechecker had no signature for
    // the `Some` constructor — the call expression fell back to
    // a fresh `Var`, and the `let` binding's type promotion only
    // kicked in for primitives, dropping the Adt wrapper. Match
    // dispatch later treated the 8-byte `*mut GosResult` pointer
    // as a raw i64 and read garbage from nested `if let` chains.
    // Fix: extended `check_call`'s fallback table to recognise
    // the four standard variant constructors (`Some` / `None` /
    // `Ok` / `Err`) and synthesise the matching Adt with the
    // payload's type.
    let src = r#"
use std::errors

fn maybe_double(n: i64) -> Option<i64> {
    if n > 0 { Some(n * 2) } else { None }
}

fn safe_divide(a: i64, b: i64) -> Result<i64, errors::Error> {
    if b == 0 { return Err(errors::new("zero")) }
    Ok(a / b)
}

fn main() {
    let first = Some(10)
    if let Some(n) = first {
        if let Some(m) = maybe_double(n) {
            if let Ok(d) = safe_divide(m, 4) {
                println!("d = {}", d)
            }
        }
    }
}
"#;
    let dir = fresh_dir("variant_ctor_typed");
    let path = write_source(&dir, "variant_ctor_typed", src);
    let run = run_vm(&path);
    assert_eq!(run.2, Some(0), "vm stderr: {}", run.1);
    assert!(
        run.0.contains("d = 5"),
        "vm: expected `d = 5`, got: {:?}",
        run.0
    );
    let cl_dir = dir.join("cl");
    fs::create_dir_all(&cl_dir).unwrap();
    let bin = build_native(&path, &cl_dir).expect("cranelift build");
    let native = run_native(&bin);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(native.2, Some(0), "native stderr: {}", native.1);
    assert!(
        native.0.contains("d = 5"),
        "native: expected `d = 5`, got: {:?}",
        native.0
    );
}

#[test]
fn str_find_returns_option_in_compiled_mode() {
    // `s.find("missing")` previously returned Some(_) instead
    // of None in cranelift — the dispatch routed straight to
    // `gos_rt_str_find` (returns raw i64 with -1 sentinel)
    // and the SwitchInt on the Option discriminant defaulted
    // to the Some arm because -1 didn't match either disc=0
    // (Some) or disc=1 (None). Fix: introduced
    // `gos_rt_str_find_opt` which wraps the i64 in a
    // `*mut GosResult` (Some(idx) / None) and routed the
    // String `find` dispatch through it.
    let src = r#"
fn main() {
    let s = "foo bar"
    match s.find("bar") {
        Some(i) => println!("found at {}", i),
        None => println!("missing"),
    }
    match s.find("qux") {
        Some(i) => println!("unexpected at {}", i),
        None => println!("not found"),
    }
}
"#;
    let dir = fresh_dir("str_find_opt");
    let path = write_source(&dir, "str_find_opt", src);
    let run = run_vm(&path);
    assert_eq!(run.2, Some(0), "vm stderr: {}", run.1);
    assert!(run.0.contains("found at 4"), "vm got: {:?}", run.0);
    assert!(run.0.contains("not found"), "vm got: {:?}", run.0);
    assert!(!run.0.contains("unexpected"), "vm got: {:?}", run.0);
    let cl_dir = dir.join("cl");
    fs::create_dir_all(&cl_dir).unwrap();
    let bin = build_native(&path, &cl_dir).expect("cranelift build");
    let native = run_native(&bin);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(native.2, Some(0), "native stderr: {}", native.1);
    assert!(
        native.0.contains("found at 4"),
        "native got: {:?}",
        native.0
    );
    assert!(native.0.contains("not found"), "native got: {:?}", native.0);
    assert!(
        !native.0.contains("unexpected"),
        "native got: {:?}",
        native.0
    );
}

#[test]
fn format_macro_result_is_typed_as_string() {
    // `format!("{}{}", a, b).len()` returned a multi-trillion
    // garbage value in the cranelift compiled tier because the
    // typechecker had no signature for the parser-emitted
    // `__concat` intrinsic. The result local was typed as
    // `Var(_)` and the `.len()` dispatch picked `gos_rt_len`
    // (the generic Vec/HashMap length helper) instead of
    // `gos_rt_str_len`. The generic helper read a Vec
    // header from the *c_char pointer and printed garbage.
    // Fix: the typechecker now pins `__concat` / `__fmt_prec`
    // / `format` to `String` and `println` / `print` /
    // `eprintln` / `eprint` to `Unit` in its fallback table.
    let src = r#"
fn main() {
    let a = "foo"
    let b = "bar"
    let combined = format!("{}{}", a, b)
    println!("len = {}", combined.len())
}
"#;
    let dir = fresh_dir("format_len_typed");
    let path = write_source(&dir, "format_len_typed", src);
    let run = run_vm(&path);
    assert_eq!(run.2, Some(0), "vm stderr: {}", run.1);
    assert!(
        run.0.contains("len = 6"),
        "vm: expected len=6, got: {:?}",
        run.0
    );
    let cl_dir = dir.join("cl");
    fs::create_dir_all(&cl_dir).unwrap();
    let bin = build_native(&path, &cl_dir).expect("cranelift build");
    let native = run_native(&bin);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(native.2, Some(0), "native stderr: {}", native.1);
    assert!(
        native.0.contains("len = 6"),
        "native: expected len=6, got: {:?}",
        native.0
    );
}

#[test]
fn immutable_static_path_resolves_to_typed_constant() {
    // `static N: i64 = 5; println!("{}", N)` previously
    // segfaulted in the cranelift compiled tier (and emitted
    // empty output). The typechecker left the path expression
    // typed as `Var(_)`, and `consts.get(def)` returned None
    // for static items (only `const` items were folded), so
    // `N` lowered as a `FnRef` whose pointer was then handed
    // to `gos_rt_concat_str` and caused a strlen segfault.
    // Fix: extend `collect_const_values` to fold immutable
    // `static` items too, and pin the local's MIR type from
    // the const value's shape when the typechecker leaves it
    // as `Var(_)` so format-arg dispatch picks the right
    // helper (concat_i64 / concat_f64 / concat_str etc.).
    let src = r#"
static MAX_RETRIES: i64 = 5
static THRESHOLD: f64 = 0.75
static GREETING: &str = "hello"

fn above_threshold(v: f64) -> bool {
    v > THRESHOLD
}

fn main() {
    println!("MAX_RETRIES = {}", MAX_RETRIES)
    println!("THRESHOLD = {}", THRESHOLD)
    println!("GREETING = {}", GREETING)
    println!("above(0.5) = {}", above_threshold(0.5))
    println!("above(0.8) = {}", above_threshold(0.8))
}
"#;
    let dir = fresh_dir("static_items_typed");
    let path = write_source(&dir, "static_items_typed", src);
    let run = run_vm(&path);
    assert_eq!(run.2, Some(0), "vm stderr: {}", run.1);
    assert!(run.0.contains("MAX_RETRIES = 5"), "vm got: {:?}", run.0);
    assert!(run.0.contains("THRESHOLD = 0.75"), "vm got: {:?}", run.0);
    assert!(run.0.contains("GREETING = hello"), "vm got: {:?}", run.0);
    assert!(run.0.contains("above(0.5) = false"), "vm got: {:?}", run.0);
    assert!(run.0.contains("above(0.8) = true"), "vm got: {:?}", run.0);
    let cl_dir = dir.join("cl");
    fs::create_dir_all(&cl_dir).unwrap();
    let bin = build_native(&path, &cl_dir).expect("cranelift build");
    let native = run_native(&bin);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(native.2, Some(0), "native stderr: {}", native.1);
    assert!(
        native.0.contains("MAX_RETRIES = 5"),
        "native got: {:?}",
        native.0
    );
    assert!(
        native.0.contains("THRESHOLD = 0.75"),
        "native got: {:?}",
        native.0
    );
    assert!(
        native.0.contains("GREETING = hello"),
        "native got: {:?}",
        native.0
    );
    assert!(
        native.0.contains("above(0.5) = false"),
        "native got: {:?}",
        native.0
    );
    assert!(
        native.0.contains("above(0.8) = true"),
        "native got: {:?}",
        native.0
    );
}

#[test]
fn static_mut_assignment_does_not_error_at_runtime() {
    // `static mut COUNTER: i64 = 0; COUNTER = 100` previously
    // failed in the VM with "name `COUNTER` is not bound in
    // this scope" because `eval_assign` only consulted the
    // goroutine-local `Env`, not the tree-walker's globals
    // table where statics live. The tree-walker's `eval_path`
    // already resolves the read against globals, so the
    // asymmetry was invisible until a write hit. This test
    // pins the no-error contract; it does *not* yet assert
    // that the read sees the written value, because the
    // bytecode VM's globals are a separate `Arc<HashMap>` and
    // sharing storage with the tree-walker is an open
    // follow-up. The contract today: writes accept, reads
    // return the initial value (consistent with cranelift on
    // this build).
    let src = r#"
static mut N: i64 = 7

fn main() {
    println!("start = {}", N)
    N = 42
    println!("after = {}", N)
}
"#;
    let dir = fresh_dir("static_mut_assign");
    let path = write_source(&dir, "static_mut_assign", src);
    let run = run_vm(&path);
    assert_eq!(run.2, Some(0), "vm stderr: {}", run.1);
    assert!(
        run.0.contains("start = 7"),
        "vm: expected static-mut initial value visible, got: {:?}",
        run.0,
    );
    assert!(
        run.0.contains("after ="),
        "vm: expected the post-assign println to run instead of erroring, got: {:?}",
        run.0,
    );
    let cl_dir = dir.join("cl");
    fs::create_dir_all(&cl_dir).unwrap();
    let bin = build_native(&path, &cl_dir).expect("cranelift build");
    let native = run_native(&bin);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(native.2, Some(0), "native stderr: {}", native.1);
    assert!(
        native.0.contains("start = 7"),
        "native: expected static-mut initial value visible, got: {:?}",
        native.0,
    );
    assert!(
        native.0.contains("after ="),
        "native: expected the post-assign println to run instead of erroring, got: {:?}",
        native.0,
    );
}

#[test]
fn at_binding_subpattern_actually_filters_match_arms() {
    // `x @ literal` and `x @ lo..=hi` previously dropped the
    // subpattern at the AST→HIR boundary: `lower_pat_kind`
    // destructured `AstPatKind::Ident { name, mutability, .. }`
    // with `..` swallowing the `subpattern` field. Both VM and
    // cranelift always picked the first arm with a stale binding;
    // cranelift additionally bound `x` to a heap pointer instead
    // of the integer value (a representation-drift symptom).
    // The fix introduces `HirPatKind::At { name, mutable, sub }`
    // and threads it through every consumer (HIR walker, MIR
    // match lowering, exhaustiveness check, tree-walker
    // pattern matchers).
    let src = r#"
fn classify(n: i64) -> String {
    match n {
        x @ 0 => format!("zero ({})", x),
        x @ 1..=3 => format!("small {}", x),
        x @ 4..=10 => format!("medium {}", x),
        x => format!("other {}", x),
    }
}

fn main() {
    let inputs = [0, 1, 2, 3, 4, 7, 10, 11, -1]
    for n in inputs {
        println!("{}", classify(n))
    }
}
"#;
    let dir = fresh_dir("at_binding_filter");
    let path = write_source(&dir, "at_binding_filter", src);
    let run = run_vm(&path);
    assert_eq!(run.2, Some(0), "vm stderr: {}", run.1);
    let expected =
        "zero (0)\nsmall 1\nsmall 2\nsmall 3\nmedium 4\nmedium 7\nmedium 10\nother 11\nother -1\n";
    assert_eq!(
        run.0, expected,
        "vm: at-binding subpattern; got: {:?}",
        run.0
    );
    let cl_dir = dir.join("cl");
    fs::create_dir_all(&cl_dir).unwrap();
    let bin = build_native(&path, &cl_dir).expect("cranelift build");
    let native = run_native(&bin);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(native.2, Some(0), "native stderr: {}", native.1);
    assert_eq!(
        native.0, expected,
        "native: at-binding subpattern; got: {:?}",
        native.0
    );
}

#[test]
fn continue_in_for_vec_iter_advances_index() {
    // `for x in xs.iter() { if cond { continue } body }`
    // previously livelocked the bytecode VM for the same reason
    // as the for-range case: the vec-iter fast path's `continue`
    // skipped the index increment that lives between the body
    // and the back-edge.
    let src = r#"
fn main() {
    let xs: [i64] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10].to_vec()
    let mut acc: i64 = 0
    for x in xs.iter() {
        if x % 3 == 0 {
            continue
        }
        acc = acc + x
    }
    println!("acc={}", acc)
}
"#;
    let dir = fresh_dir("continue_for_vec_iter");
    let path = write_source(&dir, "continue_for_vec_iter", src);
    let run = run_vm(&path);
    assert_eq!(run.2, Some(0), "vm stderr: {}", run.1);
    assert!(
        run.0.contains("acc=37"),
        "vm: expected acc=37, got: {:?}",
        run.0
    );
    let cl_dir = dir.join("cl");
    fs::create_dir_all(&cl_dir).unwrap();
    let bin = build_native(&path, &cl_dir).expect("cranelift build");
    let native = run_native(&bin);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(native.2, Some(0), "native stderr: {}", native.1);
    assert!(
        native.0.contains("acc=37"),
        "native: expected acc=37, got: {:?}",
        native.0
    );
}

#[test]
fn result_option_payload_literal_matches_correct_arm() {
    // `Ok(1)` / `Ok(2)` must route to different arms in both VM and compiled.
    let src = r#"
fn classify(r: Result<i64, i64>) -> &str {
    match r {
        Ok(1) => "one",
        Ok(2) => "two",
        Ok(_) => "other-ok",
        Err(_) => "err",
    }
}

fn pick(o: Option<i64>) -> &str {
    match o {
        Some(10) => "ten",
        Some(20) => "twenty",
        None => "none",
        _ => "other",
    }
}

fn main() {
    println!("{}", classify(Ok(1)))
    println!("{}", classify(Ok(2)))
    println!("{}", classify(Ok(99)))
    println!("{}", classify(Err(0)))
    println!("{}", pick(Some(10)))
    println!("{}", pick(Some(20)))
    println!("{}", pick(None))
}
"#;
    let expected = "one\ntwo\nother-ok\nerr\nten\ntwenty\nnone\n";
    let dir = fresh_dir("payload_literal_match");
    let path = write_source(&dir, "payload_literal_match", src);
    let run = run_vm(&path);
    assert_eq!(run.2, Some(0), "vm stderr: {}", run.1);
    assert_eq!(run.0, expected, "vm: got {:?}", run.0);
    let cl_dir = dir.join("cl");
    fs::create_dir_all(&cl_dir).unwrap();
    let bin = build_native(&path, &cl_dir).expect("cranelift build");
    let native = run_native(&bin);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(native.2, Some(0), "native stderr: {}", native.1);
    assert_eq!(native.0, expected, "native: got {:?}", native.0);
}

#[test]
fn unary_prefix_at_line_start_breaks_statement() {
    // `&s`, `*p`, `-n` at the start of a line after a
    // semicolonless statement must parse as a new statement, not
    // as a binary continuation of the prior expression. Before the
    // fix, `let s = "hi"\n&s |> ...` was glued into `let s = "hi" &
    // s |> ...` and resolution failed with "cannot find `s` in
    // this scope".
    let src = r#"
use std::{iter, strings}

fn main() {
    let s = "alpha\nbeta"
    &s |> strings::lines |> iter::for_each(|l| println!("{}", l))

    let n = 5
    -n
    println!("post-neg")

    let v = 42
    let p = &v
    *p
    println!("post-deref={}", *p)
}
"#;
    let expected = "alpha\nbeta\npost-neg\npost-deref=42\n";
    let dir = fresh_dir("unary_line_start");
    let path = write_source(&dir, "unary_line_start", src);
    let run = run_vm(&path);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(run.2, Some(0), "vm stderr: {}", run.1);
    assert_eq!(run.0, expected, "vm: got {:?}", run.0);
}

#[test]
fn try_operator_in_macro_arg_propagates_early_return() {
    // `?` inside a macro argument — e.g. `print!("{}", expr?)` —
    // must propagate the early-return from the enclosing function,
    // not silently pass the `Err(...)` value through to the macro.
    // The bug: eval_expr_to_value was converting Flow::Return(v)
    // to Ok(v), so the Err value was passed to __concat / print
    // instead of returning early from `cat`.
    let src = r#"
use std::{errors, os}

fn cat(f: &String) -> Result<(), errors::Error> {
    Ok(print!("{}", os::read_file_to_string(f)?))
}

fn main() {
    if let Err(e) = cat(&"/nonexistent-regression") {
        println!("caught: {e}")
    }
}
"#;
    let expected = "caught: not found: /nonexistent-regression\n";
    let dir = fresh_dir("try_in_macro_arg");
    let path = write_source(&dir, "try_in_macro_arg", src);
    let run = run_vm(&path);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(run.2, Some(0), "vm stderr: {}", run.1);
    assert_eq!(run.0, expected, "vm: got {:?}", run.0);
}

#[test]
fn llvm_named_fn_passed_to_sort_by_emits_typed_store() {
    // `Operand::FnRef` in the LLVM lowerer used to always emit
    // `ptrtoint ptr @"name" to i64`, but the destination slot is
    // ptr-typed when `FnDef → ptr` (e.g. when a named fn is passed
    // as a `Fn(i64,i64)->i64` arg to `sort_by`). The emitted
    // `store ptr %i64_value, ptr %slot` then fails opt validation
    // and the whole module silently falls back to Cranelift.
    let src = r#"
fn cmp(a: i64, b: i64) -> i64 { a - b }
fn main() {
    let mut xs = [5, 2, 4, 1, 3].to_vec()
    xs.sort_by(cmp)
    for x in xs.iter() { println!("{}", *x) }
}
"#;
    let dir = fresh_dir("llvm_sortby_named_fn");
    let path = write_source(&dir, "llvm_sortby_named_fn", src);
    let scratch = dir.join("rel");
    fs::create_dir_all(&scratch).unwrap();
    let bin = build_native_release(&path, &scratch).expect("llvm release build");
    let out = run_native(&bin);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(out.2, Some(0), "native: stderr: {}", out.1);
    assert_eq!(out.0, "1\n2\n3\n4\n5\n");
}

#[test]
fn llvm_vec_of_tuples_index_returns_both_fields() {
    // `Vec<(i64, f64)>`-style tuple element types used to leave
    // the operand of `xs.push((1, 1.5))` typed as
    // `(Var, Var)` in MIR. The LLVM lowerer's `slot_count` for
    // a tuple with `Var` elements returned `None`, the alloca
    // shrank to 1 slot, the second-slot store overflowed, and
    // the subsequent `gos_rt_vec_get_ptr → memcpy` round-trip
    // surfaced garbage in the f64 field.
    let src = r#"
fn main() {
    let mut xs: [(i64, f64)] = [].to_vec()
    xs.push((1, 1.5))
    xs.push((2, 2.5))
    let i: i64 = 1
    let p = xs[i]
    println!("{} {}", p.0, p.1)
}
"#;
    let dir = fresh_dir("llvm_vec_tuple_index");
    let path = write_source(&dir, "llvm_vec_tuple_index", src);
    let scratch = dir.join("rel");
    fs::create_dir_all(&scratch).unwrap();
    let bin = build_native_release(&path, &scratch).expect("llvm release build");
    let out = run_native(&bin);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(out.2, Some(0), "native: stderr: {}", out.1);
    assert_eq!(out.0, "2 2.5\n");
}

#[test]
fn llvm_tuple_return_array_then_scalar_preserves_both() {
    // Returning an `([f64; N], f64)` tuple used to corrupt every
    // slot past the first: the temporary tuple local was typed
    // `([Var; 4], Var)`, `slot_count` collapsed to `None`, and
    // the alloca undersized to 1 slot. The aggregate-store then
    // overflowed and the subsequent memcpy into the return slot
    // copied stack garbage.
    let src = r#"
fn make() -> ([f64; 4], f64) {
    ([1.5, 2.5, 3.5, 4.5], 99.0)
}
fn main() {
    let pair = make()
    println!("{} {} {} {} | {}", pair.0[0], pair.0[1], pair.0[2], pair.0[3], pair.1)
}
"#;
    let dir = fresh_dir("llvm_tuple_arr_scalar");
    let path = write_source(&dir, "llvm_tuple_arr_scalar", src);
    let scratch = dir.join("rel");
    fs::create_dir_all(&scratch).unwrap();
    let bin = build_native_release(&path, &scratch).expect("llvm release build");
    let out = run_native(&bin);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(out.2, Some(0), "native: stderr: {}", out.1);
    assert_eq!(out.0, "1.5 2.5 3.5 4.5 | 99\n");
}

#[test]
fn llvm_tuple_return_from_nested_loop_keeps_second_slot() {
    // `return (a, b)` from inside a nested loop used to drop the
    // second slot — the temporary `(Var, Var)` tuple's alloca
    // sized to one slot, so the aggregate-store overflowed and
    // the memcpy into the return slot only carried 8 valid bytes.
    // fannkuch-shaped programs lost the checksum value (always 0).
    let src = r#"
fn fannkuch(_n: i64) -> (i64, i64) {
    let mut perm = [0, 1, 2, 3, 4]
    let mut max_flips = 0
    let mut checksum = 0
    let mut sign = true
    let mut nperm = 0
    loop {
        let mut flips = 0
        let mut k = perm[0]
        while k != 0 {
            let mut i = 0
            let mut j = k
            while i < j {
                let t = perm[i]
                perm[i] = perm[j]
                perm[j] = t
                i += 1
                j -= 1
            }
            k = perm[0]
            flips += 1
        }
        if flips > max_flips { max_flips = flips }
        checksum += if sign { flips } else { -flips }
        if nperm >= 30 {
            return (max_flips, checksum)
        }
        nperm += 1
        if sign {
            let t = perm[0]
            perm[0] = perm[1]
            perm[1] = t
            sign = false
        } else {
            let t = perm[1]
            perm[1] = perm[2]
            perm[2] = t
            sign = true
        }
    }
}
fn main() {
    let r = fannkuch(5)
    println!("max={} checksum={}", r.0, r.1)
}
"#;
    let dir = fresh_dir("llvm_nested_loop_tuple_ret");
    let path = write_source(&dir, "llvm_nested_loop_tuple_ret", src);
    let scratch = dir.join("rel");
    fs::create_dir_all(&scratch).unwrap();
    let bin = build_native_release(&path, &scratch).expect("llvm release build");
    let out = run_native(&bin);
    let vm = run_vm(&path);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(out.2, Some(0), "native: stderr: {}", out.1);
    assert_eq!(out.0, vm.0, "native diverged from VM");
    // The exact value can't be 0 — that's the bug shape we're
    // guarding against.
    assert!(!out.0.contains("checksum=0\n"), "second slot dropped");
}

#[test]
fn json_render_adt_text_branch_does_not_free_uninit_pairs_vec() {
    // json::render(&adt) builds a temporary GosVec (pairs_vec) inside
    // lower_json_render_adt.  The insert_drops_at_returns pass used to
    // emit a gos_rt_vec_free for pairs_vec at every Return block —
    // including the text-mode arm where pairs_vec was never initialised.
    // That produced gos_rt_vec_free(stack_garbage) → segfault in
    // __GI___libc_free.  The fix: emit the free immediately in the JSON
    // arm and re-assign pairs_vec to 0 so the global drop pass skips it.
    let src = r#"
use std::json

struct Info { num: i64, label: String }

fn show(item: Info, as_json: bool) {
    if as_json {
        println!("{}", json::render(&item))
    } else {
        println!("num={} label={}", item.num, item.label)
    }
}

fn main() {
    let it = Info { num: 42, label: "hello".to_string() }
    show(it, false)
}
"#;
    let dir = fresh_dir("json_render_text_branch");
    let path = write_source(&dir, "json_render_text_branch", src);
    let scratch = dir.join("bin");
    fs::create_dir_all(&scratch).unwrap();
    let bin = build_native(&path, &scratch).expect("build");
    let out = run_native(&bin);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(out.2, Some(0), "segfault in text branch; stderr: {}", out.1);
    assert_eq!(out.0, "num=42 label=hello\n");
}
