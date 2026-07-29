// Regression tests covering shapes that previously crashed,
// mis-dispatched, or surfaced wrong values. Each `#[test]` runs
// a small Gossamer program through `gos run` (or `gos build`)
// and asserts the user-visible output. A regression in any of
// the underlying fixes turns the test red. Tests are named
// after the property under test, not after a bug number - the
// file location is the regression-guard context.


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
    let mut map = gossamer_lex::SourceMap::new();
    let file = map.add_file(path.display().to_string(), source.to_string());
    let source = gossamer_parse::autoderive::migrate_braced_struct_constructors(source, file)
        .expect("bug-regression source must parse for constructor migration");
    f.write_all(source.as_bytes()).unwrap();
    path
}

#[test]
fn nested_consts_run_in_vm_and_native() {
    let src = r#"
const OUTER: i64 = 5

fn main() {
    println!("top={}", OUTER)
    const OUTER: i64 = 40
    const STEP: i64 = OUTER + 2
    {
        const OUTER: String = "answer"
        println!("{}={}", OUTER, STEP)
    }
    println!("again={}", OUTER)
}
"#;
    let dir = fresh_dir("nested_consts");
    let path = write_source(&dir, "nested_consts", src);
    let scratch = dir.join("bin");
    std::fs::create_dir_all(&scratch).unwrap();
    let release_scratch = dir.join("bin-release");
    std::fs::create_dir_all(&release_scratch).unwrap();

    let vm = run_vm(&path);
    let bin = build_native(&path, &scratch).expect("native build");
    let native = run_native(&bin);
    let release_bin = build_native_release(&path, &release_scratch).expect("release build");
    let release_native = run_native(&release_bin);
    let _ = std::fs::remove_dir_all(&dir);

    assert_eq!(vm.2, Some(0), "vm stderr: {}", vm.1);
    assert_eq!(native.2, Some(0), "native stderr: {}", native.1);
    assert_eq!(
        release_native.2,
        Some(0),
        "release native stderr: {}",
        release_native.1
    );
    assert_eq!(vm.0, "top=5\nanswer=42\nagain=40\n");
    assert_eq!(native.0, vm.0);
    assert_eq!(release_native.0, vm.0);
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
fn heterogeneous_tuple_binding_is_iterable() {
    let src = r#"
fn main() {
    let t = (1, 3.4, "a")
    for i in t { println(i) }
}
"#;
    let dir = fresh_dir("tuple_iter");
    let path = write_source(&dir, "tuple_iter", src);
    let run = run_vm(&path);
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(run.2, Some(0), "stderr: {}", run.1);
    assert_eq!(run.0, "1\n3.4\na\n");
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
    // `process::exit(N)` after `println!` must flush the runtime's
    // stdout buffer before terminating; otherwise the buffered
    // output is dropped (`gos_rt_exit` previously skipped the
    // drain and called `std::process::exit` directly).
    let dir = fresh_dir("exit_flush");
    let src = write_source(
        &dir,
        "exit_flush",
        "use std::process\nfn main() {\n    println!(\"before exit\")\n    process::exit(2)\n}\n",
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
    let original = User { name: "alice", age: 30, active: true, tags: tags, address: Address { city: "denver", zip: "80205" } }
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
fn open_range_breaks_and_bounds_checks_match_across_tiers() {
    let break_src = r#"
fn main() {
    let mut total = 0
    for i in 0.. {
        total += i
        if i == 2 { break }
        if i > 20 { panic("break failed") }
    }
    let mut total2 = 0
    for i in .. {
        total2 += i
        if i == 2 { break }
        if i > 20 { panic("break failed") }
    }
    let mut finite = 0
    for i in ..3 { finite += i }
    println!("{} {} {}", total, total2, finite)
}
"#;
    let dir = fresh_dir("open_range_break");
    let path = write_source(&dir, "open_range_break", break_src);
    let vm = run_vm(&path);
    assert_eq!(vm.2, Some(0), "vm stderr: {}", vm.1);
    assert_eq!(vm.0, "3 3 3\n");
    let cl_dir = dir.join("cl");
    fs::create_dir_all(&cl_dir).unwrap();
    let bin = build_native(&path, &cl_dir).expect("cranelift build");
    let native = run_native(&bin);
    assert_eq!(native.2, Some(0), "native stderr: {}", native.1);
    assert_eq!(native.0, vm.0);

    let bounds_src =
        "fn main() {\n    let a = [1, 2, 3]\n    for i in .. { println(a[i]) }\n}\n";
    let bounds_path = write_source(&dir, "open_range_bounds", bounds_src);
    let vm_bounds = run_vm(&bounds_path);
    assert_ne!(vm_bounds.2, Some(0), "VM accepted index 3");
    assert!(vm_bounds.1.contains("out of bounds"), "{}", vm_bounds.1);
    let bounds_dir = dir.join("bounds-cl");
    fs::create_dir_all(&bounds_dir).unwrap();
    let bounds_bin = build_native(&bounds_path, &bounds_dir).expect("cranelift build");
    let native_bounds = run_native(&bounds_bin);
    let _ = fs::remove_dir_all(&dir);
    assert_ne!(native_bounds.2, Some(0), "native accepted index 3");
    assert!(
        native_bounds.1.contains("out of bounds"),
        "{}",
        native_bounds.1
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
