//! VM-native standalone range value and generic array literal tests.

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
    try_run_main(source).expect("main failed")
}

fn try_run_main(source: &str) -> Result<String, String> {
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
    result.map_err(|error| error.to_string())?;
    Ok(CAPTURED.with(|cell| cell.borrow().clone()))
}

#[test]
fn exclusive_range_value() {
    let src = r#"
fn main() {
    let r = 0..3
    println!("{:?}", r)
}
"#;
    assert_eq!(run_main(src), "0..3\n");
}

#[test]
fn inclusive_range_value() {
    let src = r#"
fn main() {
    let r = 1..=4
    println!("{:?}", r)
}
"#;
    assert_eq!(run_main(src), "1..=4\n");
}

#[test]
fn open_ranges_preserve_their_source_shape_without_realising() {
    let src = r#"
fn main() {
    println!("{} {} {}", ..10, 10.., ..)
}
"#;
    assert_eq!(run_main(src), "..10 10.. ..\n");
}

#[test]
fn open_range_can_be_bounded_before_collection() {
    let src = r#"
fn main() {
    let first = 10.. |> iter::take(3) |> iter::collect()
    println!("{:?}", first)
}
"#;
    assert_eq!(run_main(src), "[10, 11, 12]\n");
}

#[test]
fn open_range_matches_rust_overflow_profile() {
    let src = r#"
fn main() {
    let edge = 9223372036854775805.. |> iter::take(4) |> iter::collect()
    println!("{:?}", edge)
}
"#;
    if cfg!(debug_assertions) {
        let error = try_run_main(src).expect_err("debug open range must overflow");
        assert!(error.contains("attempt to add with overflow"), "{error}");
    } else {
        assert_eq!(
            try_run_main(src).expect("release open range wraps"),
            "[9223372036854775805, 9223372036854775806, 9223372036854775807, -9223372036854775808]\n"
        );
    }
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
