//! End-to-end run-pass tests for the tree-walking interpreter.
//! Each test parses a small Gossamer program, runs it through the
//! full frontend pipeline, and evaluates the `main` function. The
//! `println` built-in is captured into an in-memory buffer so the
//! tests can compare stdout against a literal expected string.

#![allow(clippy::needless_raw_string_hashes)]

use std::cell::RefCell;

use gossamer_hir::lower_source_file;
use gossamer_interp::{Interpreter, Value, set_stdout_writer};
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

fn run_program(source: &str) -> String {
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

fn call_and_return(source: &str, entry: &str, args: Vec<Value>) -> Value {
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
    interp.call(entry, args).expect("call failed")
}

#[test]
fn hello_world_prints_greeting() {
    let output = run_program("fn main() { println(\"hello, world\") }\n");
    assert_eq!(output, "hello, world\n");
}

#[test]
fn arithmetic_program_prints_sum() {
    let output = run_program("fn main() { println(1i64 + 2i64 + 3i64) }\n");
    assert_eq!(output, "6\n");
}

#[test]
fn if_else_branch_selects_correct_arm() {
    let source = r#"
fn main() {
    let x = 3i64
    let y = if x > 1i64 { "big" } else { "small" }
    println(y)
}
"#;
    assert_eq!(run_program(source), "big\n");
}

#[test]
fn while_loop_counts_down_to_zero() {
    let source = r#"
fn main() {
    let mut n = 3i64
    while n > 0i64 {
        println(n)
        n = n - 1i64
    }
}
"#;
    assert_eq!(run_program(source), "3\n2\n1\n");
}

#[test]
fn recursive_function_returns_expected_value() {
    let source = r#"
fn factorial(n: i64) -> i64 {
    if n <= 1i64 { 1i64 } else { n * factorial(n - 1i64) }
}
"#;
    let result = call_and_return(source, "factorial", vec![Value::Int(5)]);
    assert!(matches!(result, Value::Int(120)));
}

#[test]
fn closure_captures_outer_binding() {
    let source = r#"
fn main() {
    let base = 10i64
    let add_base = |x: i64| x + base
    println(add_base(5i64))
}
"#;
    assert_eq!(run_program(source), "15\n");
}

#[test]
fn match_on_bool_selects_literal_arm() {
    let source = r#"
fn describe(b: bool) -> String {
    match b {
        true => "yes",
        false => "no",
    }
}
"#;
    let yes = call_and_return(source, "describe", vec![Value::Bool(true)]);
    let no = call_and_return(source, "describe", vec![Value::Bool(false)]);
    assert!(matches!(yes, Value::String(ref s) if s.as_str() == "yes"));
    assert!(matches!(no, Value::String(ref s) if s.as_str() == "no"));
}

#[test]
fn tuple_destructuring_in_let_binds_components() {
    let source = r#"
fn main() {
    let pair = (1i64, 2i64)
    let (a, b) = pair
    println(a + b)
}
"#;
    assert_eq!(run_program(source), "3\n");
}

#[test]
fn early_return_short_circuits_function() {
    let source = r#"
fn first_positive(a: i64, b: i64) -> i64 {
    if a > 0i64 { return a }
    b
}
"#;
    let result = call_and_return(
        source,
        "first_positive",
        vec![Value::Int(5), Value::Int(99)],
    );
    assert!(matches!(result, Value::Int(5)));
}

#[test]
fn loop_with_break_returns_value() {
    let source = r#"
fn main() {
    let mut n = 0i64
    let x = loop {
        if n >= 3i64 { break n * 2i64 }
        n = n + 1i64
    }
    println(x)
}
"#;
    assert_eq!(run_program(source), "6\n");
}

#[test]
fn println_joins_multiple_arguments_with_spaces() {
    let output = run_program("fn main() { println(\"a\", 1i64, true) }\n");
    assert_eq!(output, "a 1 true\n");
}

#[test]
fn logical_and_short_circuits_false_side() {
    let source = r#"
fn main() {
    let f = false
    let t = true
    println(f && t)
    println(t && t)
    println(f || t)
}
"#;
    assert_eq!(run_program(source), "false\ntrue\ntrue\n");
}
