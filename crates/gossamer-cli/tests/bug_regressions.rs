//! Regression tests covering shapes that previously crashed,
//! mis-dispatched, or surfaced wrong values. Each `#[test]` runs
//! a small Gossamer program through `gos run` (or `gos build`)
//! and asserts the user-visible output. A regression in any of
//! the underlying fixes turns the test red. Tests are named
//! after the property under test, not after a bug number - the
//! file location is the regression-guard context.

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
fn generic_struct_f64_field_prints_as_float() {
    // Bug fixed in 0.10.0: a generic struct field whose concrete type
    // is `f64` (`Triple<A, B, C> { third: C }` with `C = f64`) printed
    // the value's IEEE-754 bit pattern as an integer under
    // `gos build --release`. Two root causes: (1) unsuffixed float
    // literals (`3.0`) were never defaulted to `f64` so the field's
    // inference var leaked, and (2) `deep_resolve` didn't recurse into
    // `Adt` substs so the receiver's type args stayed unresolved. Both
    // are fixed; the field now prints as the float `3`.
    let src = r#"
struct Triple<A, B, C> { first: A, second: B, third: C }
fn main() {
    let r = Triple { first: 1, second: "two", third: 3.0 }
    println!("{} {} {}", r.first, r.second, r.third)
}
"#;
    let dir = fresh_dir("generic_struct_f64");
    let path = write_source(&dir, "generic_struct_f64", src);
    let scratch = dir.join("bin");
    std::fs::create_dir_all(&scratch).unwrap();
    let bin = build_native_release(&path, &scratch).expect("release build");
    let out = run_native(&bin);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(out.2, Some(0), "stderr: {}", out.1);
    assert!(out.0.contains("1 two 3"), "got: {:?}", out.0);
}

#[test]
fn impl_method_self_field_iteration_binds_element_type() {
    // Bug fixed in 0.10.0: inside an `impl` method the `self` receiver
    // was bound to a fresh inference var instead of the impl's `Self`
    // type, so `for x in self.items` over a `[String]` field bound `x`
    // at the i64 default - printing element pointers as integers (the
    // auto-derived `to_json` serialised a `[String]` field as numbers).
    // `self` now binds to the concrete `Self` type.
    let src = r#"
struct U { tags: [String] }
impl U {
    fn dump(self) {
        for item in self.tags { println!("item={}", item) }
    }
}
fn main() {
    let mut t: [String] = []
    t.push("a")
    t.push("b")
    let u = U { tags: t }
    U::dump(u)
}
"#;
    let dir = fresh_dir("impl_self_field_iter");
    let path = write_source(&dir, "impl_self_field_iter", src);
    let scratch = dir.join("bin");
    std::fs::create_dir_all(&scratch).unwrap();
    let bin = build_native_release(&path, &scratch).expect("release build");
    let out = run_native(&bin);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(out.2, Some(0), "stderr: {}", out.1);
    assert!(
        out.0.contains("item=a") && out.0.contains("item=b"),
        "got: {:?}",
        out.0
    );
}

#[test]
fn native_bytecode_match_covers_pattern_shapes() {
    // The bytecode VM now lowers `match` directly (literals, ranges,
    // or-patterns, guards, enum-variant payloads, tuple/struct
    // destructure, `@`-bindings) instead of routing every arm
    // evaluation through the bundled tree-walker via
    // `Op::EvalDeferred`. This pins the VM output for a match that
    // exercises every natively-compiled pattern shape; a regression
    // in the test-and-branch lowering or the `VariantIs` /
    // `VariantField` / `StructIs` opcodes turns it red.
    let src = r#"
enum Shape { Circle(i64), Rect(i64, i64), Unit }
struct Point { x: i64, y: i64 }
fn classify(n: i64) -> String {
    match n {
        0 => "zero".to_string(),
        1 | 2 | 3 => "small".to_string(),
        4..=9 => "mid".to_string(),
        x if x < 0 => "neg".to_string(),
        big @ 100..=999 => format!("big:{}", big),
        _ => "huge".to_string(),
    }
}
fn area(s: &Shape) -> i64 {
    match s {
        Shape::Circle(r) => 3 * r * r,
        Shape::Rect(w, h) => w * h,
        Shape::Unit => 0,
    }
}
fn main() {
    for n in [0, 2, 7, -4, 500, 100000] {
        println!("{}={}", n, classify(n))
    }
    println!("{}", area(&Shape::Circle(2)))
    println!("{}", area(&Shape::Rect(3, 4)))
    println!("{}", area(&Shape::Unit))
    let p = Point { x: 5, y: 9 }
    match p { Point { x, y } => println!("pt {} {}", x, y) }
    let pair = (11, 22)
    match pair { (a, b) => println!("pair {} {}", a, b) }
}
"#;
    let dir = fresh_dir("native_match");
    let path = write_source(&dir, "native_match", src);
    let out = run_vm(&path);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(out.2, Some(0), "stderr: {}", out.1);
    let expect = "0=zero\n2=small\n7=mid\n-4=neg\n500=big:500\n100000=huge\n\
12\n12\n0\npt 5 9\npair 11 22\n";
    assert!(
        out.0.contains(expect),
        "native match output mismatch:\n{}",
        out.0
    );
}

#[test]
fn compiled_match_on_inferred_tuple_binds_element_types() {
    // Bug fixed in 0.10.0: a `match` on a tuple whose element types
    // were left unresolved by inference (`let pair = (10, "hi")`)
    // bound each element through a pointer-shaped local, so the
    // `println!` arg dispatcher routed the `i64` element through
    // `gos_rt_concat_str` and strlen'd the integer value → segfault.
    // The match tuple-binding now recovers each element type from
    // the sub-pattern when the tuple's recorded type is loose.
    let src = r#"
fn main() {
    let pair = (10, "hi")
    match pair { (n, s) => println!("{} {}", n, s) }
}
"#;
    let dir = fresh_dir("compiled_match_tuple");
    let path = write_source(&dir, "compiled_match_tuple", src);
    let scratch = dir.join("bin");
    std::fs::create_dir_all(&scratch).unwrap();
    let bin = build_native_release(&path, &scratch).expect("release build");
    let out = run_native(&bin);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(out.2, Some(0), "stderr: {}", out.1);
    assert!(out.0.contains("10 hi"), "got: {:?}", out.0);
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
    // `Value::U64` - the tuple-of-String case fell through the
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
fn type_from_json_round_trips_struct_with_nested_address_and_tags() {
    // Every named struct in the program auto-derives the generic
    // `from_json::<Type>(text)` / `to_json::<Type>(value)` free
    // functions. The decoder must (a) accept a clean payload
    // and produce a typed struct readable via dot-access, (b) reject
    // type mismatches with a path-qualified error, and (c) reject
    // missing required fields. Tests all three.
    let src = r#"
use std::errors

struct Address {
    city: String,
    zip: String,
}

struct User {
    name: String,
    age: i64,
    active: bool,
    tags: [String],
    address: Address,
}

fn main() -> Result<(), errors::Error> {
    let mut tags: [String] = []
    tags.push("admin")
    let original = User {
        name: "alice",
        age: 30,
        active: true,
        tags: tags,
        address: Address { city: "denver", zip: "80205" },
    }
    let text = to_json::<User>(&original)?
    let back: User = from_json::<User>(&text)?
    println!("name={}", back.name)
    println!("age={}", back.age)
    println!("city={}", back.address.city)
    println!("tag0={}", back.tags[0])

    let bad = "{\"name\":\"bob\",\"age\":\"oops\",\"active\":false,\"tags\":[],\"address\":{\"city\":\"x\",\"zip\":\"0\"}}"
    match from_json::<User>(&bad) {
        Ok(_)  => println!("bad-passed"),
        Err(e) => println!("bad-rejected: {}", e),
    }

    let missing = "{\"name\":\"carol\",\"age\":40,\"active\":true,\"tags\":[]}"
    match from_json::<User>(&missing) {
        Ok(_)  => println!("missing-passed"),
        Err(e) => println!("missing-rejected: {}", e),
    }
    Ok(())
}
"#;
    let dir = fresh_dir("type_from_json_round_trip");
    let path = write_source(&dir, "from_json", src);
    let run = run_vm(&path);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(run.2, Some(0), "vm stderr: {}", run.1);
    assert!(run.0.contains("name=alice"), "missing name: {:?}", run.0);
    assert!(run.0.contains("age=30"), "missing age: {:?}", run.0);
    assert!(run.0.contains("city=denver"), "missing city: {:?}", run.0);
    assert!(run.0.contains("tag0=admin"), "missing tag: {:?}", run.0);
    assert!(
        run.0.contains("bad-rejected: ") && run.0.contains("field `age`"),
        "expected age-mismatch rejection: {:?}",
        run.0,
    );
    assert!(
        run.0.contains("missing-rejected: ") && run.0.contains("missing field `address`"),
        "expected missing-address rejection: {:?}",
        run.0,
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
    // `Value::Int` uniformly - the regex captures shape
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
    // the `Some` constructor - the call expression fell back to
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
    // of None in cranelift - the dispatch routed straight to
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
    // `?` inside a macro argument - e.g. `print!("{}", expr?)` -
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
    // second slot - the temporary `(Var, Var)` tuple's alloca
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
    // The exact value can't be 0 - that's the bug shape we're
    // guarding against.
    assert!(!out.0.contains("checksum=0\n"), "second slot dropped");
}

#[test]
fn json_render_adt_text_branch_does_not_free_uninit_pairs_vec() {
    // json::render(&adt) builds a temporary GosVec (pairs_vec) inside
    // lower_json_render_adt.  The insert_drops_at_returns pass used to
    // emit a gos_rt_vec_free for pairs_vec at every Return block -
    // including the text-mode arm where pairs_vec was never initialised.
    // That produced gos_rt_vec_free(stack_garbage) → segfault in
    // __GI___libc_free.  The fix: emit the free immediately in the JSON
    // arm and re-assign pairs_vec to 0 so the global drop pass skips it.
    let src = r#"
use std::encoding::json

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

#[test]
fn jit_pre_interns_array_index_label_string() {
    // Regression: a program that hits the bounds-check helper
    // path (any `arr[i]` with i64 index) would route through the
    // codegen helper that interns "array index" as the diagnostic
    // label. The pre-pass that pre-interns strings before the
    // parallel codegen phase missed this literal, so the first
    // bounds-checked array access in any body panicked with
    // `OfflineModule: declare_data called in parallel phase`.
    //
    // The fix: pre-intern "array index" alongside `""`, `" "`,
    // and `"<value>"` in the codegen prelude. spectral-norm's
    // `src[j]` access is the canonical trigger.
    let src = "fn main() {\n\
                   let xs: [i64; 4] = [1, 2, 3, 4]\n\
                   let mut sum: i64 = 0\n\
                   let n: i64 = 4\n\
                   let mut i: i64 = 0\n\
                   while i < n {\n\
                       sum += xs[i]\n\
                       i += 1\n\
                   }\n\
                   println!(\"{}\", sum)\n\
               }\n";
    let dir = fresh_dir("jit_array_index_pre_intern");
    let path = write_source(&dir, "jit_array_index_pre_intern", src);
    let run = run_vm(&path);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(
        run.2,
        Some(0),
        "vm: expected clean exit, got {:?}; stderr: {}",
        run.2,
        run.1,
    );
    assert!(
        !run.1.contains("declare_data called in parallel phase"),
        "vm: regressed parallel-phase declare_data panic; stderr: {}",
        run.1
    );
    assert_eq!(run.0.trim_end(), "10");
}

#[test]
fn local_var_shadowing_module_does_not_capture_qualified_path() {
    // Regression: a local binding whose name matches an imported
    // module silently captured every `mod_name::item(...)` call
    // through the VM-tier's tree-walker fallback. `eval_path`
    // looked up the head segment in the env first, returning the
    // local's value (a String), and `apply()` of a non-callable
    // degraded to Unit. The LLVM AOT tier resolved correctly; the
    // VM tier did not - a parity gap that broke askq's
    // `provider::provider_endpoint_and_auth(&cfg, &provider)` call
    // (the local `provider: String` captured the call).
    //
    // The fix: multi-segment paths bypass the env-first lookup.
    // A path's head can only resolve to a module / type / trait -
    // never a local binding.
    let dir = fresh_dir("local_shadow_mod_path");
    fs::write(
        dir.join("project.toml"),
        "[project]\nid = \"example.com/shadow\"\nversion = \"0.0.1\"\n",
    )
    .expect("write project.toml");
    let src_dir = dir.join("src");
    fs::create_dir_all(&src_dir).expect("mk src dir");
    fs::write(
        src_dir.join("main.gos"),
        "mod prov;\n\
         fn main() {\n\
             let prov = \"local-string\".to_string()\n\
             let s = prov::greet(&prov)\n\
             println!(\"{}\", s)\n\
         }\n",
    )
    .expect("write main.gos");
    fs::write(
        src_dir.join("prov.gos"),
        "pub fn greet(who: &String) -> String {\n\
             format!(\"hello, {}\", who)\n\
         }\n",
    )
    .expect("write prov.gos");
    let main_path = src_dir.join("main.gos");
    let run = run_vm(&main_path);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(
        run.2,
        Some(0),
        "vm: expected clean exit, got {:?}; stderr: {}",
        run.2,
        run.1
    );
    assert_eq!(
        run.0.trim_end(),
        "hello, local-string",
        "vm: expected greet to run, got stdout: {:?}",
        run.0,
    );
}

#[test]
fn aggregate_alloc_loop_reclaims_deterministically() {
    // Stress: a tight loop that allocates a heap aggregate every
    // iteration and discards it. The MIR drop pass must emit a
    // matching `gos_rt_aggr_free` per iteration. The test verifies:
    //   - the loop produces the correct numeric result (the drop
    //     pass does not free values still held in locals);
    //   - the process exits with status 0 (no segfault, no
    //     double-free in the drop pass);
    //   - all three tiers agree.
    let src = "struct Pair { a: i64, b: i64 }\n\
               fn make(i: i64) -> Pair { Pair { a: i, b: i * 2 } }\n\
               fn main() {\n\
                   let mut total: i64 = 0\n\
                   let mut i: i64 = 0\n\
                   while i < 10000 {\n\
                       let p = make(i)\n\
                       total += p.a + p.b\n\
                       i += 1\n\
                   }\n\
                   println!(\"{}\", total)\n\
               }\n";
    let expected = (0i64..10000).map(|i| i + i * 2).sum::<i64>();
    let dir = fresh_dir("tracing_gc_loop");
    let path = write_source(&dir, "tracing_gc_loop", src);

    // VM tier
    let run = {
        let child = Command::new(gos_bin())
            .arg("run")
            .arg(&path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn gos run");
        run_with_timeout(child)
    };
    assert_eq!(
        run.2,
        Some(0),
        "vm: expected clean exit, got {:?}; stderr: {}",
        run.2,
        run.1
    );
    assert_eq!(run.0.trim_end(), expected.to_string(), "vm output mismatch");

    // Debug LLVM tier
    let scratch = dir.join("bin");
    fs::create_dir_all(&scratch).unwrap();
    let bin = build_native(&path, &scratch).expect("build debug");
    let out = {
        let child = Command::new(&bin)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn debug binary");
        run_with_timeout(child)
    };
    assert_eq!(
        out.2,
        Some(0),
        "debug: expected clean exit, got {:?}; stderr: {}",
        out.2,
        out.1
    );
    assert_eq!(
        out.0.trim_end(),
        expected.to_string(),
        "debug output mismatch"
    );

    // Release LLVM tier
    let bin = build_native_release(&path, &scratch).expect("build release");
    let out = {
        let child = Command::new(&bin)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn release binary");
        run_with_timeout(child)
    };
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(
        out.2,
        Some(0),
        "release: expected clean exit, got {:?}; stderr: {}",
        out.2,
        out.1
    );
    assert_eq!(
        out.0.trim_end(),
        expected.to_string(),
        "release output mismatch"
    );
}

#[test]
fn aggregate_return_chain_outlives_callee_frame() {
    // Stresses the aggregate-return heap-copy discipline: every
    // iteration calls a function that builds an aggregate on the
    // callee's frame; codegen copies it to the heap at return so
    // the pointer outlives the popped frame, and the caller uses
    // both fields of the returned tuple. The just-returned
    // aggregate must stay intact until the caller consumes it.
    let src = "fn pair_of(i: i64) -> (i64, i64) {\n\
                   (i, i * 7)\n\
               }\n\
               fn main() {\n\
                   let mut sum: i64 = 0\n\
                   let mut i: i64 = 0\n\
                   while i < 5000 {\n\
                       let p = pair_of(i)\n\
                       sum += p.0 + p.1\n\
                       i += 1\n\
                   }\n\
                   println!(\"{}\", sum)\n\
               }\n";
    let expected = (0i64..5000).map(|i| i + i * 7).sum::<i64>();
    let dir = fresh_dir("tracing_gc_return_chain");
    let path = write_source(&dir, "tracing_gc_return_chain", src);

    let run = {
        let child = Command::new(gos_bin())
            .arg("run")
            .arg(&path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn gos run");
        run_with_timeout(child)
    };
    assert_eq!(
        run.2,
        Some(0),
        "vm: expected clean exit, got {:?}; stderr: {}",
        run.2,
        run.1
    );
    assert_eq!(
        run.0.trim_end(),
        expected.to_string(),
        "vm aggregate-return chain mismatch (rooted-return discipline broken?)"
    );

    let scratch = dir.join("bin");
    fs::create_dir_all(&scratch).unwrap();
    let bin = build_native_release(&path, &scratch).expect("build release");
    let out = {
        let child = Command::new(&bin)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn release binary");
        run_with_timeout(child)
    };
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(
        out.2,
        Some(0),
        "release: expected clean exit, got {:?}; stderr: {}",
        out.2,
        out.1
    );
    assert_eq!(
        out.0.trim_end(),
        expected.to_string(),
        "release aggregate-return chain mismatch"
    );
}

#[test]
fn autoderive_synthesizes_for_narrow_integer_fields() {
    // Structs whose fields use `i32` / `u8` / `i16` etc. must
    // still get `from_json` / `to_json` synthesized. Before the
    // fix, the FieldKind table only covered `i64`, so any narrow
    // integer caused the entire struct to be skipped and the
    // user's `from_json::<Type>(text)?` call surfaced as
    // `field access on non-struct ()` at runtime.
    let src = r#"
use std::errors

struct Counts {
    small: u8,
    medium: i32,
    big: i64,
}

fn main() -> Result<(), errors::Error> {
    let text = "{\"small\":255,\"medium\":-1,\"big\":9000000000}".to_string()
    let c = from_json::<Counts>(&text)?
    println!("small={} medium={} big={}", c.small, c.medium, c.big)
    Ok(())
}
"#;
    let dir = fresh_dir("autoderive_narrow_int");
    let path = write_source(&dir, "narrow", src);
    let run = run_vm(&path);
    assert_eq!(run.2, Some(0), "vm stderr: {}", run.1);
    assert_eq!(
        run.0.trim_end(),
        "small=255 medium=-1 big=9000000000",
        "narrow-int autoderive mismatch (vm); stdout: {:?}",
        run.0
    );

    let scratch = dir.join("bin");
    fs::create_dir_all(&scratch).unwrap();
    let bin = build_native_release(&path, &scratch).expect("build release");
    let out = run_native(&bin);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(out.2, Some(0), "release stderr: {}", out.1);
    assert_eq!(
        out.0.trim_end(),
        "small=255 medium=-1 big=9000000000",
        "narrow-int autoderive mismatch (llvm release); stdout: {:?}",
        out.0
    );
}

#[test]
fn write_file_with_vec_u8_preserves_embedded_nul() {
    // `os::write_file(path, &Vec<u8>)` must route through the
    // bytes-shaped runtime helper; the c-string helper would
    // truncate at the first NUL and silently corrupt binary
    // writes. Reads the file back to confirm every byte
    // survived round-trip on each tier.
    let dir = fresh_dir("write_bytes_nul");
    let tmp_path = dir.join("payload.bin");
    let tmp_str = tmp_path.display().to_string();
    let src = format!(
        r#"
use std::errors
use std::os

fn main() -> Result<(), errors::Error> {{
    let payload: [u8] = [72, 105, 0, 65, 66, 67, 10]
    os::write_file(&"{tmp}", &payload)?
    let back = os::read_file(&"{tmp}")?
    println!("len={{}}", back.len())
    println!("byte2={{}}", back[2])
    println!("byte3={{}}", back[3])
    println!("byte6={{}}", back[6])
    Ok(())
}}
"#,
        tmp = tmp_str.replace('\\', "\\\\"),
    );
    let path = write_source(&dir, "write_nul", &src);
    let run = run_vm(&path);
    assert_eq!(run.2, Some(0), "vm stderr: {}", run.1);
    let expected = "len=7\nbyte2=0\nbyte3=65\nbyte6=10";
    assert_eq!(
        run.0.trim_end(),
        expected,
        "binary write round-trip mismatch (vm); stdout: {:?}",
        run.0
    );

    let _ = fs::remove_file(&tmp_path);
    let scratch = dir.join("bin");
    fs::create_dir_all(&scratch).unwrap();
    let bin = build_native_release(&path, &scratch).expect("build release");
    let out = run_native(&bin);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(out.2, Some(0), "release stderr: {}", out.1);
    assert_eq!(
        out.0.trim_end(),
        expected,
        "binary write round-trip mismatch (llvm release); stdout: {:?}",
        out.0
    );
}

#[test]
fn yaml_autoderive_round_trips_struct_via_yaml() {
    // Every named struct also gets `from_yaml` / `to_yaml`
    // alongside the JSON pair. The methods route through
    // `yaml::to_json` / `yaml::from_json` and reuse the
    // JSON decoder's strict field-type checks.
    let src = r#"
use std::errors

struct AppCfg {
    name: String,
    port: i64,
    debug: bool,
}

fn main() -> Result<(), errors::Error> {
    let yaml = "name: gossamer\nport: 8080\ndebug: true\n".to_string()
    let cfg = from_yaml::<AppCfg>(&yaml)?
    println!("{} {} {}", cfg.name, cfg.port, cfg.debug)

    let back = to_yaml::<AppCfg>(cfg)?
    let again = from_yaml::<AppCfg>(&back)?
    println!("{} {}", again.name, again.port)
    Ok(())
}
"#;
    let dir = fresh_dir("yaml_autoderive");
    let path = write_source(&dir, "yaml_derive", src);
    let run = run_vm(&path);
    assert_eq!(run.2, Some(0), "vm stderr: {}", run.1);
    let expected = "gossamer 8080 true\ngossamer 8080";
    assert_eq!(
        run.0.trim_end(),
        expected,
        "yaml round-trip mismatch (vm); stdout: {:?}",
        run.0
    );

    let scratch = dir.join("bin");
    fs::create_dir_all(&scratch).unwrap();
    let bin = build_native_release(&path, &scratch).expect("build release");
    let out = run_native(&bin);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(out.2, Some(0), "release stderr: {}", out.1);
    assert_eq!(
        out.0.trim_end(),
        expected,
        "yaml round-trip mismatch (llvm release); stdout: {:?}",
        out.0
    );
}

#[test]
fn sync_map_round_trips_set_get_delete_across_tiers() {
    // `sync::Map` is a concurrent string-keyed map. set / get /
    // contains / delete / len must dispatch correctly on every
    // tier. The Option<String> returned by `.get` was previously
    // pinned to `i64` in the kind_dispatch fallback, surfacing
    // as `bar=<raw-pointer-as-number>` for the Some arm and
    // `Some(_)` being taken even for the None case.
    let src = r#"
use std::sync

fn main() {
    let m = sync::Map::new()
    m.set("alpha", "1")
    m.set("beta", "2")
    println!("len={}", m.len())
    match m.get("beta") {
        Some(v) => println!("beta={}", v),
        None => println!("beta missing"),
    }
    match m.get("nope") {
        Some(_) => println!("nope unexpected"),
        None => println!("nope=None"),
    }
    m.delete("alpha")
    println!("contains alpha: {}", m.contains("alpha"))
    println!("after-delete len={}", m.len())
}
"#;
    let dir = fresh_dir("sync_map");
    let path = write_source(&dir, "sync_map", src);
    let expected = "len=2\nbeta=2\nnope=None\ncontains alpha: false\nafter-delete len=1";
    let run = run_vm(&path);
    assert_eq!(run.2, Some(0), "vm stderr: {}", run.1);
    assert_eq!(
        run.0.trim_end(),
        expected,
        "sync::Map mismatch (vm); stdout: {:?}",
        run.0
    );

    let scratch = dir.join("bin");
    fs::create_dir_all(&scratch).unwrap();
    let bin = build_native_release(&path, &scratch).expect("build release");
    let out = run_native(&bin);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(out.2, Some(0), "release stderr: {}", out.1);
    assert_eq!(
        out.0.trim_end(),
        expected,
        "sync::Map mismatch (llvm release); stdout: {:?}",
        out.0
    );
}

#[test]
fn deref_assign_through_mut_i64_runs_under_llvm() {
    // Bug fixed in 0.10.0: `*s = expr` through `&mut i64`
    // segfaulted in the LLVM AOT tier because `&mut state` was
    // lowered as the i64 value instead of the slot address.
    // Three coordinated MIR + LLVM + cranelift changes close the
    // class. The reproducer is the LCG step the bench-game LCRNG
    // benches use.
    let src = r#"
fn lcg(s: &mut i64) -> i64 {
    *s = *s * 6364136223846793005 + 1442695040888963407
    (*s >> 33) & 0x7fffffff
}
fn main() {
    let mut state: i64 = 42
    let n = lcg(&mut state)
    println!("{}", n)
}
"#;
    let dir = fresh_dir("deref_assign_mut_i64");
    let path = write_source(&dir, "deref_assign_mut_i64", src);
    let scratch = dir.join("bin");
    std::fs::create_dir_all(&scratch).unwrap();
    let bin = build_native_release(&path, &scratch).expect("release build");
    let out = run_native(&bin);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(out.2, Some(0), "stderr: {}", out.1);
    // First LCG step on state=42: returns 1220265334.
    assert!(
        out.0.contains("1220265334"),
        "expected LCG output 1220265334, got: {:?}",
        out.0
    );
}

#[test]
fn mut_self_field_compound_assign_writes_back() {
    // Bug fixed in 0.10.0: `self.field += 1` in an `&mut self`
    // method silently dropped the mutation in the LLVM AOT tier.
    let src = r#"
struct Counter { n: i64 }
impl Counter {
    fn bump(&mut self) { self.n += 1 }
}
fn main() {
    let mut c = Counter { n: 0 }
    c.bump()
    c.bump()
    c.bump()
    println!("{}", c.n)
}
"#;
    let dir = fresh_dir("mut_self_compound_assign");
    let path = write_source(&dir, "mut_self_compound_assign", src);
    let scratch = dir.join("bin");
    std::fs::create_dir_all(&scratch).unwrap();
    let bin = build_native_release(&path, &scratch).expect("release build");
    let out = run_native(&bin);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(out.2, Some(0), "stderr: {}", out.1);
    assert_eq!(out.0.trim_end(), "3", "got {:?}", out.0);
}

#[test]
fn multi_dim_fixed_array_index_walks_inner_strides() {
    // Bug fixed in 0.10.0: `lower_place_address` did not advance
    // `current_ty` after a `Projection::Index`, so `arr[i][j]` over
    // `[[T; A]; B]` used the OUTER array's bounds for the inner
    // index. Iron Knight's 3D zobrist write hit this.
    let src = r#"
struct Z { pieces: [[[i64; 64]; 6]; 2] }
fn main() {
    let mut z = Z { pieces: [[[0; 64]; 6], [[0; 64]; 6]] }
    let mut s: i64 = 0
    while s < 2 {
        let mut p: i64 = 0
        while p < 6 {
            let mut sq: i64 = 0
            while sq < 64 {
                z.pieces[s][p][sq] = s * 1000 + p * 100 + sq
                sq += 1
            }
            p += 1
        }
        s += 1
    }
    println!("z[1][5][63]={}", z.pieces[1][5][63])
}
"#;
    let dir = fresh_dir("multi_dim_array");
    let path = write_source(&dir, "multi_dim_array", src);
    let scratch = dir.join("bin");
    std::fs::create_dir_all(&scratch).unwrap();
    let bin = build_native_release(&path, &scratch).expect("release build");
    let out = run_native(&bin);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(out.2, Some(0), "stderr: {}", out.1);
    assert!(
        out.0.contains("z[1][5][63]=1563"),
        "expected z[1][5][63]=1563 (1*1000+5*100+63), got: {:?}",
        out.0
    );
}

#[test]
fn env_args_empty_iter_does_not_segfault() {
    // Bug fixed in 0.10.0: `gos_rt_set_args` stored a null GosVec
    // pointer when `argc <= 1`, so iterating `env::args()` with
    // no user args segfaulted on the iterator's null-header walk.
    let src = r#"
use std::env
fn main() {
    let args = env::args()
    println!("len={}", args.len())
    for a in args {
        println!("{}", a)
    }
}
"#;
    let dir = fresh_dir("env_args_empty");
    let path = write_source(&dir, "env_args_empty", src);
    let scratch = dir.join("bin");
    std::fs::create_dir_all(&scratch).unwrap();
    let bin = build_native_release(&path, &scratch).expect("release build");
    let out = run_native(&bin);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(
        out.2,
        Some(0),
        "no-arg run must exit cleanly; stderr: {}",
        out.1
    );
    assert!(
        out.0.contains("len=0"),
        "expected empty args to report len=0, got: {:?}",
        out.0
    );
}

#[test]
fn vec_pop_on_typed_storage_shrinks_by_one() {
    // Bug fixed in 0.10.0: VM `builtin_pop` fell into the
    // `_ => empty_array` catch-all for `Value::IntArray` /
    // `Value::FloatVec` receivers, and the writeback then moved
    // the empty result into the receiver - clobbering every
    // element instead of removing only the last one.
    let src = r#"
fn main() {
    let mut xs: [i64] = [10, 20, 30, 40]
    let _ = xs.pop()
    println!("len={}", xs.len())
    println!("xs[0]={}", xs[0])
    println!("xs[2]={}", xs[2])
}
"#;
    let dir = fresh_dir("vec_pop_typed");
    let path = write_source(&dir, "vec_pop_typed", src);
    let run = run_vm(&path);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(run.2, Some(0), "vm stderr: {}", run.1);
    assert!(
        run.0.contains("len=3") && run.0.contains("xs[0]=10") && run.0.contains("xs[2]=30"),
        "expected len=3 + xs[0]=10 + xs[2]=30 after a single pop, got: {:?}",
        run.0
    );
}

#[test]
fn hashmap_keys_router_does_not_get_shadowed_by_json() {
    // Bug fixed in 0.10.0: `install_module("json", …)` unconditionally
    // pushed `("keys", builtin_json_keys)` AFTER the HashMap surface
    // registered `("keys", builtin_map_keys)`. The later json push
    // overrode the bare-name registry, so every `m.keys()` on a
    // HashMap silently dispatched to the JSON helper which returns
    // `None` for non-Struct receivers - surfacing as `ks.len() == 0`
    // even with multiple inserts. Receiver-routing wrapper now
    // dispatches by Value shape.
    let src = r#"
use std::collections::HashMap
fn main() {
    let mut m: HashMap<i64, i64> = HashMap::new()
    m.insert(1, 10)
    m.insert(2, 20)
    m.insert(3, 30)
    let ks = m.keys()
    println!("len={}", ks.len())
}
"#;
    let dir = fresh_dir("hashmap_keys_router");
    let path = write_source(&dir, "hashmap_keys_router", src);
    let run = run_vm(&path);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(run.2, Some(0), "vm stderr: {}", run.1);
    assert!(run.0.contains("len=3"), "expected 3 keys, got: {:?}", run.0);
}

#[test]
fn return_array_literal_coerces_to_slice() {
    // Bug fixed in 0.10.0: `fn f() -> [String] { return ["a", "b"] }`
    // lowered the array literal as a flat `Array<String; 2>` and
    // returned the stack-aggregate bytes through the slot that the
    // caller dereferenced as a `*mut GosVec` - len read as garbage
    // bits, then `for s in xs` ran zero iterations. The Return
    // path now coerces `Array<T; N>` → `Vec<T>` via
    // `gos_rt_vec_from_arr` whenever the declared return type is
    // `Vec(elem)` or `Slice(elem)` with matching `elem`.
    let src = r#"
fn cols() -> [String] {
    return ["id", "name", "value"]
}
fn main() {
    let xs = cols()
    println!("len={}", xs.len())
    for s in xs { println!("{}", s) }
}
"#;
    let dir = fresh_dir("return_array_to_slice");
    let path = write_source(&dir, "return_array_to_slice", src);
    let scratch = dir.join("bin");
    std::fs::create_dir_all(&scratch).unwrap();
    let bin = build_native_release(&path, &scratch).expect("release build");
    let out = run_native(&bin);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(out.2, Some(0), "stderr: {}", out.1);
    assert!(
        out.0.contains("len=3") && out.0.contains("id") && out.0.contains("value"),
        "expected len=3 + all 3 strings, got: {:?}",
        out.0
    );
}

#[test]
fn typed_int_array_get_falls_back_to_generic_array() {
    // Bug fixed in 0.10.0: when `fn slide(arr: [i64; 4]) -> i64`
    // was called from a loop body, the bytecode compiler tracked
    // `arr` as a `flat_int_local` (Value::IntArray) - but the
    // call-args ABI didn't always preserve that shape across the
    // boundary, so the second iteration saw a Value::Array of
    // boxed Value::Int instead and panicked with "IntArrayGetI64:
    // receiver lost flat invariant". The runtime fast path now
    // tolerates the generic Array shape (one discriminant match
    // per index) instead of aborting.
    let src = r#"
fn slide(arr: [i64; 4]) -> i64 {
    let mut sum: i64 = 0
    for i in 0..4 { sum += arr[i] }
    sum
}
fn main() {
    for k in 0..3 {
        let r = slide([1, 2, 3, 4])
        println!("k={} r={}", k, r)
    }
}
"#;
    let dir = fresh_dir("typed_int_array_get_fallback");
    let path = write_source(&dir, "typed_int_array_get_fallback", src);
    let run = run_vm(&path);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(run.2, Some(0), "vm stderr: {}", run.1);
    assert_eq!(
        run.0.matches("r=10").count(),
        3,
        "expected r=10 three times, got: {:?}",
        run.0
    );
}

#[test]
fn logical_and_or_short_circuit_in_compiled_tier() {
    // Bug fixed in 0.10.0: `lower_binary` evaluated both sides of
    // `&&` / `||` unconditionally in the MIR lowering, so a guarded
    // bounds check like `while j > 0 && arr[j - 1] < x` panicked
    // with `the index is -1` once j reached 0 - the RHS fired
    // even though the LHS was already false. The lowering now
    // branches on the LHS and evaluates the RHS only on the path
    // that needs it.
    let src = r#"
fn check_idx(arr: [i64; 4], j: i64) -> bool {
    arr[j - 1] < 100
}
fn main() {
    let arr: [i64; 4] = [1, 2, 3, 4]
    let mut j: i64 = 2
    while j > 0 && check_idx(arr, j) {
        j -= 1
    }
    println!("done j={}", j)
    let mut k: i64 = 0
    while k < 5 || k > 100 {
        k += 1
        if k > 3 { break }
    }
    println!("k={}", k)
}
"#;
    let dir = fresh_dir("logical_short_circuit");
    let path = write_source(&dir, "logical_short_circuit", src);
    let scratch = dir.join("bin");
    std::fs::create_dir_all(&scratch).unwrap();
    let bin = build_native_release(&path, &scratch).expect("release build");
    let out = run_native(&bin);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(out.2, Some(0), "stderr: {}", out.1);
    assert!(
        out.0.contains("done j=0") && out.0.contains("k=4"),
        "expected short-circuit semantics, got: {:?}",
        out.0
    );
}

#[test]
fn vec_of_struct_index_field_reads_and_writes_through_data_buffer() {
    // Bug fixed in 0.10.0: indexing a `Vec<Body>` (multi-slot struct
    // elements) in a place expression - `bodies[i].x` for a read or
    // `bodies[i].vx = v` for a write - built a flat `Projection::Index`
    // that strode off the `*mut GosVec` *header* instead of the data
    // buffer, so every element past index 0 read/wrote garbage. The
    // place lowerer now routes Vec-with-multi-slot-element indexing
    // through `gos_rt_vec_get_ptr` and binds the element address to a
    // `&elem` local so the appended `Field` projection auto-derefs and
    // lands inside the Vec's storage for both reads and writes.
    let src = r#"
struct Body { x: f64, y: f64, mass: f64 }
fn main() {
    let mut bs: [Body] = []
    bs.push(Body { x: 1.0, y: 2.0, mass: 10.0 })
    bs.push(Body { x: 4.0, y: 5.0, mass: 20.0 })
    bs[1].x = 9.0
    println!("{} {} {}", bs[0].x, bs[1].x, bs[1].mass)
}
"#;
    let dir = fresh_dir("vec_struct_index_field");
    let path = write_source(&dir, "vec_struct_index_field", src);
    let scratch = dir.join("bin");
    std::fs::create_dir_all(&scratch).unwrap();
    let bin = build_native_release(&path, &scratch).expect("release build");
    let out = run_native(&bin);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(out.2, Some(0), "stderr: {}", out.1);
    assert!(
        out.0.contains("1 9 20"),
        "expected element-1 field write + correct strides, got: {:?}",
        out.0
    );
}

#[test]
fn mut_fixed_struct_array_not_promoted_keeps_layout_across_calls() {
    // Bug fixed in 0.10.0: `let mut bodies: [Body; N]` was
    // unconditionally promoted to a heap `Vec<Body>` because the
    // binding was `mut` with an array literal. Passing `&bodies` to a
    // function declared `fn energy(b: &[Body; N])` then desynchronised
    // the element stride (the callee strode the GosVec header as inline
    // data) and produced NaN. The promotion now fires only when the
    // binding actually receives a growth method (push / pop / sort /
    // …); a fixed array that is merely indexed, field-mutated, or
    // passed to a `[T; N]` parameter keeps its inline layout.
    let src = r#"
struct Body { x: f64, vx: f64, mass: f64 }
fn total_momentum(b: &[Body; 2]) -> f64 {
    let mut p = 0.0
    for i in 0..2 { p += b[i].vx * b[i].mass }
    p
}
fn main() {
    let mut bodies: [Body; 2] = [
        Body { x: 1.0, vx: 0.1, mass: 10.0 },
        Body { x: 2.0, vx: 0.4, mass: 20.0 },
    ]
    bodies[0].vx = 0.5
    println!("{:.4}", total_momentum(&bodies))
}
"#;
    let dir = fresh_dir("mut_fixed_struct_array");
    let path = write_source(&dir, "mut_fixed_struct_array", src);
    let scratch = dir.join("bin");
    std::fs::create_dir_all(&scratch).unwrap();
    let bin = build_native_release(&path, &scratch).expect("release build");
    let out = run_native(&bin);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(out.2, Some(0), "stderr: {}", out.1);
    // 0.5*10 + 0.4*20 = 5 + 8 = 13.0
    assert!(
        out.0.contains("13.0000"),
        "expected fixed-array field mutation + correct stride, got: {:?}",
        out.0
    );
}

#[test]
fn mut_scalar_array_with_push_still_promotes_to_vec() {
    // Companion to the fixed-array regression: a `let mut xs =
    // [literal]` that *does* call a growth method must still promote to
    // a heap Vec so `push` / `sort` work.
    let src = r#"
fn main() {
    let mut xs = [3, 1, 2]
    xs.push(4)
    xs.sort()
    for x in &xs { print!("{} ", x) }
    println!("")
}
"#;
    let dir = fresh_dir("mut_scalar_array_push");
    let path = write_source(&dir, "mut_scalar_array_push", src);
    let scratch = dir.join("bin");
    std::fs::create_dir_all(&scratch).unwrap();
    let bin = build_native_release(&path, &scratch).expect("release build");
    let out = run_native(&bin);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(out.2, Some(0), "stderr: {}", out.1);
    assert!(
        out.0.contains("1 2 3 4"),
        "expected push + sort to work on promoted Vec, got: {:?}",
        out.0
    );
}

#[test]
fn sort_by_on_tuple_vec_orders_by_comparator() {
    // Bug fixed in 0.10.0: `xs.sort_by(|a, b| ...)` on a
    // `Vec<(String, i64)>` was a no-op / wrong-order because the
    // closure params `a` / `b` were left `Var` by inference and the
    // lift pass blanket-pinned every unresolved closure param to
    // i64. The lifted comparator body then computed `a.1`'s field
    // offset off a junk integer instead of the element pointer the
    // runtime sort hands it. The lift pass now skips the i64 pin for
    // params used through `TupleIndex` / `Field` / method-call
    // receivers - those are aggregates passed by pointer.
    let src = r#"
fn main() {
    let mut xs: [(String, i64)] = []
    xs.push(("c".to_string(), 3))
    xs.push(("a".to_string(), 1))
    xs.push(("b".to_string(), 2))
    xs.sort_by(|a, b| {
        if a.1 < b.1 { -1 }
        else if a.1 > b.1 { 1 }
        else { 0 }
    })
    for x in &xs {
        println!("{}={}", x.0.clone(), x.1)
    }
}
"#;
    let dir = fresh_dir("sort_by_tuple_vec");
    let path = write_source(&dir, "sort_by_tuple_vec", src);
    let scratch = dir.join("bin");
    std::fs::create_dir_all(&scratch).unwrap();
    let bin = build_native_release(&path, &scratch).expect("release build");
    let out = run_native(&bin);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(out.2, Some(0), "stderr: {}", out.1);
    let want = "a=1\nb=2\nc=3\n";
    assert!(
        out.0.contains(want),
        "expected ascending order by .1, got: {:?}",
        out.0
    );
}

#[test]
fn vec_of_enum_for_loop_dereferences_slot_pointer() {
    // Bug fixed in 0.10.0: `lower_for_vec` flagged any
    // `TyKind::Adt` element as "inline aggregate" and bound the loop
    // variable to the slot address directly. That's correct for
    // multi-slot user structs (whose inline storage starts at the
    // slot address), but enums and sentinel-handle structs occupy
    // exactly one 8-byte slot that *holds* a heap pointer. The loop
    // body needs the pointer value (one `gos_load` away), not the
    // slot address. Without the load, every `match e { … }` saw the
    // first 8 bytes of the heap allocation interpreted as the
    // pattern scrutinee - and fell through every variant arm.
    let src = r#"
enum Sv {
    SvInt(i64),
    SvText(String),
    SvNull,
}
enum Expr {
    EColumn(String, String),
    ELit(Sv),
}
fn show(e: &Expr) {
    match e {
        Expr::EColumn(t, c) => println!("Col({}, {})", t.clone(), c.clone()),
        Expr::ELit(v) => match v {
            Sv::SvInt(n) => println!("Lit(Int({}))", *n),
            Sv::SvText(s) => println!("Lit(Text({}))", s.clone()),
            Sv::SvNull => println!("Lit(Null)"),
        },
    }
}
fn main() {
    let mut xs: [Expr] = []
    xs.push(Expr::EColumn("t".to_string(), "id".to_string()))
    xs.push(Expr::ELit(Sv::SvInt(42)))
    xs.push(Expr::ELit(Sv::SvText("hello".to_string())))
    for e in &xs { show(e) }
}
"#;
    let dir = fresh_dir("vec_enum_for_loop");
    let path = write_source(&dir, "vec_enum_for_loop", src);
    let scratch = dir.join("bin");
    std::fs::create_dir_all(&scratch).unwrap();
    let bin = build_native_release(&path, &scratch).expect("release build");
    let out = run_native(&bin);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(out.2, Some(0), "stderr: {}", out.1);
    assert!(
        out.0.contains("Col(t, id)")
            && out.0.contains("Lit(Int(42))")
            && out.0.contains("Lit(Text(hello))"),
        "expected all three variants printed, got: {:?}",
        out.0
    );
}

#[test]
fn vec_of_multi_slot_struct_round_trips_all_fields() {
    // Bug fixed in 0.10.0: `type_slot_bytes` in MIR returned a flat
    // 8 bytes for every user-defined `Adt`, including multi-field
    // structs. `let xs: [Projection] = []` then created a Vec with
    // `elem_bytes = 8`, so a `push(Projection { a, b })` writing 16
    // bytes of inline storage truncated to the first field. The
    // first iteration of `for p in &xs` re-read garbage for `p.b`
    // and downstream `p.alias.len()` strlen'd a bogus pointer →
    // segfault (atlas_db's exec_project crash).
    let src = r#"
struct Projection {
    a: i64,
    b: i64,
}
fn main() {
    let mut xs: [Projection] = []
    xs.push(Projection { a: 1, b: 2 })
    xs.push(Projection { a: 3, b: 4 })
    for p in &xs {
        println!("a={} b={}", p.a, p.b)
    }
}
"#;
    let dir = fresh_dir("vec_multi_slot_struct");
    let path = write_source(&dir, "vec_multi_slot_struct", src);
    let scratch = dir.join("bin");
    std::fs::create_dir_all(&scratch).unwrap();
    let bin = build_native_release(&path, &scratch).expect("release build");
    let out = run_native(&bin);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(out.2, Some(0), "stderr: {}", out.1);
    assert!(
        out.0.contains("a=1 b=2") && out.0.contains("a=3 b=4"),
        "expected both fields per element, got: {:?}",
        out.0
    );
}

#[test]
fn regex_captures_all_option_string_match_reads_real_discriminant() {
    // Bug fixed in 0.10.0: `gos_rt_regex_captures_all` / `captures`
    // pushed a bare c-string pointer (or 0) per capture group, but the
    // source type of each group is `Option<String>`. When the element
    // typed as a concrete `Option<String>` (e.g. through a function
    // whose declared return is `[[Option<String>]]`), the compiled-tier
    // `match group { Some(k) => ..., None => ... }` reads the tagged-
    // union discriminant via `gos_rt_result_disc` off the pointer - a
    // raw c-string's first bytes are not a valid discriminant, so the
    // match fell through and printed nothing. Fix: the runtime now
    // pushes canonical `gos_rt_result_new(disc, payload)` Options and
    // the MIR pins the result element to `Option<String>`.
    let src = r#"
use std::regex
fn parse_pairs(line: String) -> [[Option<String>]] {
    let re = match regex::compile("(\\w+)=(\\w+)") { Ok(r) => r, Err(_) => { return [] } }
    regex::captures_all(&re, &line)
}
fn main() {
    for row in parse_pairs("addr=localhost port=8080") {
        match row[1] {
            Some(k) => match row[2] {
                Some(v) => println!("{} = {}", k, v),
                None => {}
            },
            None => {}
        }
    }
}
"#;
    let dir = fresh_dir("regex_captures_option");
    let path = write_source(&dir, "regex_captures_option", src);
    let scratch = dir.join("bin");
    std::fs::create_dir_all(&scratch).unwrap();
    let bin = build_native_release(&path, &scratch).expect("release build");
    let out = run_native(&bin);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(out.2, Some(0), "stderr: {}", out.1);
    assert!(
        out.0.contains("addr = localhost") && out.0.contains("port = 8080"),
        "expected both decoded pairs, got: {:?}",
        out.0
    );
}
