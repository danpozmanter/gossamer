//! Bytecode-VM run-pass tests.
//! Mirrors the interpreter run-pass corpus against the register-based
//! bytecode VM so the two implementations are observed to agree.

#![allow(clippy::needless_raw_string_hashes)]

use std::cell::RefCell;

use gossamer_hir::lower_source_file;
use gossamer_interp::{Value, Vm, set_stdout_writer};
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

fn build_vm(source: &str) -> Vm {
    let mut map = SourceMap::new();
    let file = map.add_file("test.gos", source.to_string());
    let (sf, parse_diags) = parse_source_file(source, file);
    assert!(parse_diags.is_empty(), "parse: {parse_diags:?}");
    let (resolutions, _resolve_diags) = resolve_source_file(&sf);
    let mut tcx = TyCtxt::new();
    let (table, _type_diags) = typecheck_source_file(&sf, &resolutions, &mut tcx);
    let program = lower_source_file(&sf, &resolutions, &table, &mut tcx);
    let mut vm = Vm::new();
    vm.load(&program, &mut tcx).expect("load");
    vm
}

fn run_vm_main(source: &str) -> String {
    let vm = build_vm(source);
    CAPTURED.with(|cell| cell.borrow_mut().clear());
    let prev = set_stdout_writer(capture_writer);
    let result = vm.call("main", Vec::new());
    set_stdout_writer(prev);
    result.expect("main failed");
    CAPTURED.with(|cell| cell.borrow().clone())
}

#[test]
fn vm_prints_hello() {
    let output = run_vm_main("fn main() { println(\"hello\") }\n");
    assert_eq!(output, "hello\n");
}

#[test]
fn vm_evaluates_arithmetic_expression() {
    let vm = build_vm(
        "fn add(a: i64, b: i64) -> i64 { a + b }\nfn main() { println(add(1i64, 2i64)) }\n",
    );
    CAPTURED.with(|cell| cell.borrow_mut().clear());
    let prev = set_stdout_writer(capture_writer);
    vm.call("main", Vec::new()).expect("main failed");
    set_stdout_writer(prev);
    let output = CAPTURED.with(|cell| cell.borrow().clone());
    assert_eq!(output, "3\n");
}

#[test]
fn vm_if_else_picks_correct_branch() {
    let source = r#"
fn pick(n: i64) -> i64 {
    if n > 0i64 { n } else { -n }
}
"#;
    let vm = build_vm(source);
    match vm.call("pick", vec![Value::Int(-5)]).unwrap() {
        Value::Int(v) => assert_eq!(v, 5),
        other => panic!("unexpected result: {other:?}"),
    }
    match vm.call("pick", vec![Value::Int(7)]).unwrap() {
        Value::Int(v) => assert_eq!(v, 7),
        other => panic!("unexpected result: {other:?}"),
    }
}

#[test]
fn vm_while_loop_counts_down() {
    let source = r#"
fn main() {
    let mut n = 3i64
    while n > 0i64 {
        println(n)
        n = n - 1i64
    }
}
"#;
    assert_eq!(run_vm_main(source), "3\n2\n1\n");
}

#[test]
fn vm_loop_with_break_returns_value() {
    let source = r#"
fn main() {
    let mut n = 0i64
    let r = loop {
        if n >= 3i64 { break n * 2i64 }
        n = n + 1i64
    }
    println(r)
}
"#;
    assert_eq!(run_vm_main(source), "6\n");
}

#[test]
fn vm_handles_recursive_call() {
    let source = r#"
fn factorial(n: i64) -> i64 {
    if n <= 1i64 { 1i64 } else { n * factorial(n - 1i64) }
}
"#;
    let vm = build_vm(source);
    let result = vm.call("factorial", vec![Value::Int(6)]).unwrap();
    match result {
        Value::Int(v) => assert_eq!(v, 720),
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn vm_short_circuits_logical_operators() {
    let source = r#"
fn main() {
    let f = false
    let t = true
    println(f && t)
    println(t && t)
    println(f || t)
}
"#;
    assert_eq!(run_vm_main(source), "false\ntrue\ntrue\n");
}

#[test]
fn vm_arithmetic_agrees_with_tree_walker() {
    use gossamer_interp::Interpreter;
    let source = r#"
fn compute(a: i64, b: i64) -> i64 {
    (a + b) * (a - b) + a * b
}
"#;
    let mut map = SourceMap::new();
    let file = map.add_file("test.gos", source.to_string());
    let (sf, _) = parse_source_file(source, file);
    let (resolutions, _) = resolve_source_file(&sf);
    let mut tcx = TyCtxt::new();
    let (table, _) = typecheck_source_file(&sf, &resolutions, &mut tcx);
    let program = lower_source_file(&sf, &resolutions, &table, &mut tcx);

    let mut vm = Vm::new();
    vm.load(&program, &mut tcx).unwrap();
    let mut tree = Interpreter::new();
    tree.load(&program);

    for (a, b) in [(1, 2), (3, 4), (10, 3), (-5, 7)] {
        let args = vec![Value::Int(a), Value::Int(b)];
        let vm_result = vm.call("compute", args.clone()).unwrap();
        let tree_result = tree.call("compute", args).unwrap();
        assert!(
            matches!((&vm_result, &tree_result), (Value::Int(x), Value::Int(y)) if x == y),
            "mismatch on ({a}, {b}): vm={vm_result:?} tree={tree_result:?}"
        );
    }
}
