//! VM-native standalone range value + generic array literal tests
//! (destroy-tree-walker Phase 7). A `a..b` / `a..=b` used as a value
//! materialises the eager `Value::Array` of `Value::Int` the walker's
//! `eval_range` produced; generic array literals build natively.

use std::cell::RefCell;

use gossamer_hir::lower_source_file;
use gossamer_interp::{Vm, set_stdout_writer};
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

fn run_main(source: &str) -> String {
    let mut map = SourceMap::new();
    let file = map.add_file("range_value.gos", source.to_string());
    let (sf, parse_diags) = parse_source_file(source, file);
    assert!(parse_diags.is_empty(), "parse: {parse_diags:?}");
    let (resolutions, _resolve_diags) = resolve_source_file(&sf);
    let mut tcx = TyCtxt::new();
    let (table, _type_diags) = typecheck_source_file(&sf, &resolutions, &mut tcx);
    let program = lower_source_file(&sf, &resolutions, &table, &mut tcx);
    let mut vm = Vm::new();
    vm.load(&program, tcx, true).expect("load");
    CAPTURED.with(|cell| cell.borrow_mut().clear());
    let prev = set_stdout_writer(capture_writer);
    let result = vm.call("main", Vec::new());
    set_stdout_writer(prev);
    result.expect("main failed");
    CAPTURED.with(|cell| cell.borrow().clone())
}

#[test]
fn exclusive_range_value() {
    let src = r#"
fn main() {
    let r = 0..3
    println!("{:?}", r)
}
"#;
    assert_eq!(run_main(src), "[0, 1, 2]\n");
}

#[test]
fn inclusive_range_value() {
    let src = r#"
fn main() {
    let r = 1..=4
    println!("{:?}", r)
}
"#;
    assert_eq!(run_main(src), "[1, 2, 3, 4]\n");
}

#[test]
fn generic_string_array_and_repeat() {
    let src = r#"
fn main() {
    let xs = ["a", "b", "c"]
    let rep = [7; 3]
    println!("{:?} {:?}", xs, rep)
}
"#;
    assert_eq!(run_main(src), "[a, b, c] [7, 7, 7]\n");
}
