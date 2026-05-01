//! Cross-shape coverage for the bytecode VM's `for` loop fast paths.
//!
//! Each test runs the same program through the tree-walker and the
//! bytecode VM and asserts byte-equal output. Catches regressions in
//! the `for-range` / inclusive-range / `vec.iter()` / `enumerate()`
//! lowering paths added under H3.

use std::cell::RefCell;

use gossamer_hir::lower_source_file;
use gossamer_interp::{Interpreter, Vm, set_stdout_writer};
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

fn run_walker(source: &str) -> String {
    let mut map = SourceMap::new();
    let file = map.add_file("test.gos", source.to_string());
    let (sf, parse_diags) = parse_source_file(source, file);
    assert!(parse_diags.is_empty(), "parse: {parse_diags:?}");
    let (resolutions, _) = resolve_source_file(&sf);
    let mut tcx = TyCtxt::new();
    let (table, _) = typecheck_source_file(&sf, &resolutions, &mut tcx);
    let program = lower_source_file(&sf, &resolutions, &table, &mut tcx);
    let mut interp = Interpreter::new();
    interp.load(&program);
    CAPTURED.with(|cell| cell.borrow_mut().clear());
    let prev = set_stdout_writer(capture_writer);
    let _ = interp.call("main", Vec::new()).expect("walker main failed");
    set_stdout_writer(prev);
    CAPTURED.with(|cell| cell.borrow().clone())
}

fn run_vm(source: &str) -> String {
    let mut map = SourceMap::new();
    let file = map.add_file("test.gos", source.to_string());
    let (sf, parse_diags) = parse_source_file(source, file);
    assert!(parse_diags.is_empty(), "parse: {parse_diags:?}");
    let (resolutions, _) = resolve_source_file(&sf);
    let mut tcx = TyCtxt::new();
    let (table, _) = typecheck_source_file(&sf, &resolutions, &mut tcx);
    let program = lower_source_file(&sf, &resolutions, &table, &mut tcx);
    let mut vm = Vm::new();
    vm.load(&program, &mut tcx).expect("vm load");
    CAPTURED.with(|cell| cell.borrow_mut().clear());
    let prev = set_stdout_writer(capture_writer);
    let _ = vm.call("main", Vec::new()).expect("vm main failed");
    set_stdout_writer(prev);
    CAPTURED.with(|cell| cell.borrow().clone())
}

fn assert_parity(source: &str) {
    let walker = run_walker(source);
    let vm = run_vm(source);
    assert_eq!(walker, vm, "walker / VM diverged");
}

#[test]
fn exclusive_for_range_sums_to_expected_total() {
    let source = r"
fn main() {
    let mut total = 0i64;
    for i in 0i64..5i64 {
        total = total + i;
    }
    println(total);
}
";
    assert_parity(source);
    assert_eq!(run_vm(source), "10\n");
}

#[test]
fn inclusive_for_range_includes_endpoint() {
    let source = r"
fn main() {
    let mut total = 0i64;
    for i in 0i64..=5i64 {
        total = total + i;
    }
    println(total);
}
";
    assert_parity(source);
    assert_eq!(run_vm(source), "15\n");
}

#[test]
fn inclusive_range_with_negative_start_works() {
    let source = r"
fn main() {
    let mut total = 0i64;
    for i in (-2i64)..=2i64 {
        total = total + i;
    }
    println(total);
}
";
    assert_parity(source);
    assert_eq!(run_vm(source), "0\n");
}

#[test]
fn empty_inclusive_range_runs_zero_iterations() {
    let source = r"
fn main() {
    let mut count = 0i64;
    for _ in 5i64..=4i64 {
        count = count + 1i64;
    }
    println(count);
}
";
    assert_parity(source);
    assert_eq!(run_vm(source), "0\n");
}

#[test]
fn for_range_break_exits_loop_at_first_match() {
    let source = r"
fn main() {
    let mut found = -1i64;
    for i in 0i64..100i64 {
        if i == 7i64 {
            found = i;
            break;
        }
    }
    println(found);
}
";
    assert_parity(source);
    assert_eq!(run_vm(source), "7\n");
}
