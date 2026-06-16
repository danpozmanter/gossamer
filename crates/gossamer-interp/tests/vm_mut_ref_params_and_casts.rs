//! Bytecode-VM regression tests for the two Task-23 divergences:
//! write-through `&mut Vec<T>` / `&mut [T]` parameters (the
//! write-back cell protocol) and the full GT0005 cast whitelist
//! (f32 / bool / char shapes that previously fell back to the
//! walker's no-op). Expected outputs are byte-identical to the
//! LLVM tier's.

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

fn run_vm_main(source: &str) -> String {
    let mut map = SourceMap::new();
    let file = map.add_file("test.gos", source.to_string());
    let (sf, parse_diags) = parse_source_file(source, file);
    assert!(parse_diags.is_empty(), "parse: {parse_diags:?}");
    let (resolutions, _resolve_diags) = resolve_source_file(&sf);
    let mut tcx = TyCtxt::new();
    let (table, type_diags) = typecheck_source_file(&sf, &resolutions, &mut tcx);
    assert!(type_diags.is_empty(), "typecheck: {type_diags:?}");
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
fn mut_vec_param_write_reaches_the_caller() {
    let output = run_vm_main(
        r#"
fn set_first(v: &mut Vec<i64>) {
    v[0] = 99
}

fn main() {
    let mut a: Vec<i64> = [1, 2, 3]
    set_first(&mut a)
    println!("{} {} {}", a[0], a[1], a[2])
}
"#,
    );
    assert_eq!(output, "99 2 3\n");
}

#[test]
fn mut_vec_param_forwarding_and_early_return_write_through() {
    let output = run_vm_main(
        r#"
fn set_first(v: &mut Vec<i64>) {
    v[0] = 99
}

fn nested(v: &mut Vec<i64>) {
    set_first(v)
    v[1] = 7
}

fn early(v: &mut Vec<i64>, stop: bool) {
    v[2] = 42
    if stop { return }
    v[2] = 43
}

fn main() {
    let mut c: Vec<i64> = [0, 0, 0]
    nested(&mut c)
    println!("{} {} {}", c[0], c[1], c[2])
    early(&mut c, true)
    println!("{}", c[2])
    early(&mut c, false)
    println!("{}", c[2])
}
"#,
    );
    assert_eq!(output, "99 7 0\n42\n43\n");
}

#[test]
fn mut_vec_param_push_grows_the_caller_vec() {
    let output = run_vm_main(
        r#"
fn push_one(v: &mut Vec<i64>) {
    v.push(5)
}

fn main() {
    let mut e: Vec<i64> = [1]
    push_one(&mut e)
    println!("{} {}", e.len(), e[1])
}
"#,
    );
    assert_eq!(output, "2 5\n");
}

#[test]
fn mut_slice_param_swap_writes_through() {
    let output = run_vm_main(
        r#"
fn swap_ends(v: &mut [i64]) {
    let n = v.len()
    v.swap(0, n - 1)
}

fn main() {
    let mut f: Vec<i64> = [10, 20, 30]
    swap_ends(&mut f)
    println!("{} {} {}", f[0], f[1], f[2])
}
"#,
    );
    assert_eq!(output, "30 20 10\n");
}

#[test]
fn mut_vec_field_place_argument_writes_through() {
    let output = run_vm_main(
        r#"
struct Holder { data: Vec<i64> }

fn set_first(v: &mut Vec<i64>) {
    v[0] = 99
}

fn main() {
    let mut h = Holder { data: [0, 0] }
    set_first(&mut h.data)
    println!("{}", h.data[0])
}
"#,
    );
    assert_eq!(output, "99\n");
}

#[test]
fn closure_with_mut_vec_param_writes_through() {
    let output = run_vm_main(
        r#"
fn apply_mut(f: Fn(&mut Vec<i64>) -> (), v: &mut Vec<i64>) {
    f(v)
}

fn main() {
    let setter = |v: &mut Vec<i64>| { v[0] = 9 }
    let mut a: Vec<i64> = [1, 2]
    setter(&mut a)
    println!("{}", a[0])
    let mut b: Vec<i64> = [1, 2]
    apply_mut(setter, &mut b)
    println!("{}", b[0])
}
"#,
    );
    assert_eq!(output, "9\n9\n");
}

#[test]
fn scalar_casts_follow_the_whitelist_on_the_vm() {
    let output = run_vm_main(
        r#"
fn main() {
    let f: f32 = 3.9
    println!("{}", f as i64)
    println!("{}", true as i64)
    println!("{}", false as i64)
    println!("{}", 'A' as i64)
    println!("{}", 65 as u8 as char)
    println!("{}", 300.7 as u8)
    println!("{}", 1e20 as i64)
    let nan = 0.0 / 0.0
    println!("{}", nan as i64)
    println!("{}", -3.9 as i64)
    println!("{}", 'A' as u8)
    println!("{}", true as u8)
    println!("{}", (0 - 1) as u64)
}
"#,
    );
    assert_eq!(
        output,
        "3\n1\n0\n65\nA\n300\n9223372036854775807\n0\n-3\n65\n1\n18446744073709551615\n"
    );
}

#[test]
fn casts_inside_capturing_closures_resolve_on_the_vm() {
    let output = run_vm_main(
        r#"
fn main() {
    let c = 'z'
    let f = || c as i64
    println!("{}", f())
}
"#,
    );
    assert_eq!(output, "122\n");
}

#[test]
fn fixed_array_mut_param_keeps_copy_semantics() {
    // Fixed `[T; N]` arrays copy at the call boundary on every
    // tier (the compiled backends pass a stack copy); the VM must
    // not write through for them or it would *create* a
    // divergence. Pin the uniform behaviour.
    let output = run_vm_main(
        r#"
fn set_first(v: &mut [i64]) {
    v[0] = 99
}

fn main() {
    let mut a = [1, 2, 3]
    set_first(&mut a)
    println!("{}", a[0])
}
"#,
    );
    assert_eq!(output, "1\n");
}
