//! Integration tests for stdlib surface that previously had Rust-side
//! implementations but no Gossamer-callable bindings. Each test runs
//! a small `.gos` program through the full frontend + interpreter
//! pipeline and asserts on the captured stdout.

#![allow(clippy::needless_raw_string_hashes)]

use std::cell::RefCell;

use gossamer_hir::lower_source_file;
use gossamer_interp::{Interpreter, set_stdout_writer};
use gossamer_lex::SourceMap;
use gossamer_parse::parse_source_file;
use gossamer_resolve::resolve_source_file;
use gossamer_types::{TyCtxt, typecheck_source_file};

thread_local! {
    static CAPTURED: RefCell<String> = const { RefCell::new(String::new()) };
}

fn capture_writer(text: &str) {
    CAPTURED.with(|cell| cell.borrow_mut().push_str(text));
}

fn run(source: &str) -> String {
    let mut map = SourceMap::new();
    let file = map.add_file("test.gos", source.to_string());
    let (sf, parse_diags) = parse_source_file(source, file);
    assert!(parse_diags.is_empty(), "parse: {parse_diags:?}");
    let (resolutions, _resolve_diags) = resolve_source_file(&sf);
    let mut tcx = TyCtxt::new();
    let (table, _type_diags) = typecheck_source_file(&sf, &resolutions, &mut tcx);
    let program = lower_source_file(&sf, &resolutions, &table, &mut tcx);
    let mut interp = Interpreter::new();
    interp.load(&program);
    CAPTURED.with(|cell| cell.borrow_mut().clear());
    let prev = set_stdout_writer(capture_writer);
    let result = interp.call("main", Vec::new());
    set_stdout_writer(prev);
    result.expect("main returned an error");
    CAPTURED.with(|cell| cell.borrow().clone())
}

#[test]
fn strings_join_works_through_qualified_call() {
    let src = r#"
fn main() {
    let parts = ["a", "b", "c"]
    let joined = strings::join(parts, "-")
    println(joined)
}
"#;
    assert_eq!(run(src), "a-b-c\n");
}

#[test]
fn strings_pad_left_zero_pads_a_number() {
    let src = r#"
fn main() {
    let padded = strings::pad_left("42", 5, '0')
    println(padded)
}
"#;
    assert_eq!(run(src), "00042\n");
}

#[test]
fn strconv_parse_int_round_trips() {
    let src = r#"
fn main() {
    let parsed = strconv::parse_int("123")
    let n = parsed.unwrap_or(0)
    println(strconv::format_int(n))
}
"#;
    assert_eq!(run(src), "123\n");
}

#[test]
fn hashset_supports_insert_remove_and_contains() {
    let src = r#"
fn main() {
    let s = HashSet::new()
    HashSet::insert(s, 1)
    HashSet::insert(s, 2)
    HashSet::insert(s, 1)
    println(HashSet::len(s))
    println(HashSet::contains(s, 2))
    HashSet::remove(s, 1)
    println(HashSet::contains(s, 1))
    println(HashSet::len(s))
}
"#;
    assert_eq!(run(src), "2\ntrue\nfalse\n1\n");
}

#[test]
fn os_extra_helpers_query_filesystem() {
    let src = r#"
fn main() {
    let temp = os::temp_dir()
    let exists_temp = os::is_dir(temp)
    println(exists_temp)
    let not_a_file = os::is_file("/__definitely_not_a_real_path__")
    println(not_a_file)
}
"#;
    assert_eq!(run(src), "true\nfalse\n");
}

#[test]
fn path_helpers_decompose_a_unix_path() {
    let src = r#"
fn main() {
    let dir = path::parent("/tmp/foo/bar.txt")
    println(dir.unwrap_or(""))
    let base = path::file_name("/tmp/foo/bar.txt")
    println(base.unwrap_or(""))
    let stem = path::stem("/tmp/foo/bar.txt")
    println(stem.unwrap_or(""))
    let ext = path::ext("/tmp/foo/bar.txt")
    println(ext.unwrap_or(""))
}
"#;
    let out = run(src);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 4);
    assert!(lines[0].ends_with("foo"), "parent: {}", lines[0]);
    assert_eq!(lines[1], "bar.txt");
    assert_eq!(lines[2], "bar");
    assert_eq!(lines[3], ".txt");
}

#[test]
fn time_instant_elapsed_returns_nonnegative() {
    let src = r#"
fn main() {
    let t = Instant::now()
    let dt = elapsed_ms(t)
    if dt >= 0 { println("ok") } else { println("bad") }
}
"#;
    assert_eq!(run(src), "ok\n");
}

#[test]
fn utf8_count_runes_handles_unicode() {
    let src = r#"
fn main() {
    println(utf8::count_runes("café"))
}
"#;
    assert_eq!(run(src), "4\n");
}

#[test]
fn atomic_i64_supports_load_store_fetch_add() {
    let src = r#"
fn main() {
    let a = AtomicI64::new(10)
    println(AtomicI64::load(a))
    AtomicI64::store(a, 20)
    println(AtomicI64::load(a))
    let prev = AtomicI64::fetch_add(a, 5)
    println(prev)
    println(AtomicI64::load(a))
}
"#;
    assert_eq!(run(src), "10\n20\n20\n25\n");
}

#[test]
fn atomic_bool_compare_and_swap() {
    let src = r#"
fn main() {
    let b = AtomicBool::new(false)
    let did_swap = AtomicBool::compare_and_swap(b, false, true)
    println(did_swap)
    let again = AtomicBool::compare_and_swap(b, false, true)
    println(again)
    println(AtomicBool::load(b))
}
"#;
    assert_eq!(run(src), "true\nfalse\ntrue\n");
}

#[test]
fn once_call_runs_exactly_once() {
    let src = r#"
fn main() {
    let o = Once::new()
    println(Once::call(o))
    println(Once::call(o))
}
"#;
    assert_eq!(run(src), "true\nfalse\n");
}
