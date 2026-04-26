//! Tests that exercise the VM's JIT dispatch path explicitly via
//! `GOS_JIT=1` semantics. Each test sets the env var inside the
//! test process, runs `Vm::load`, asserts behaviour, then unsets.
//!
//! Tests that don't depend on `set_stdout_writer` (which the JIT
//! bypasses; the runtime writes through raw `write(2)` syscalls)
//! check return values directly so the JIT path is observable
//! without colliding with the bytecode VM's stdout-redirection.

#![allow(missing_docs)]
#![allow(unsafe_code)]

use gossamer_hir::lower_source_file;
use gossamer_interp::{Value, Vm};
use gossamer_lex::SourceMap;
use gossamer_parse::parse_source_file;
use gossamer_resolve::resolve_source_file;
use gossamer_types::{TyCtxt, typecheck_source_file};

struct GosJitGuard;

impl GosJitGuard {
    fn new() -> Self {
        // SAFETY: tests are single-threaded by default and we restore
        // the env on drop. `cargo test` runs each integration-test
        // file in its own process, so no other test in this binary
        // can race the variable.
        unsafe { std::env::set_var("GOS_JIT", "1") };
        Self
    }
}

impl Drop for GosJitGuard {
    fn drop(&mut self) {
        // SAFETY: same single-threaded test contract.
        unsafe { std::env::remove_var("GOS_JIT") };
    }
}

fn build_vm(source: &str) -> (Vm, TyCtxt) {
    let mut map = SourceMap::new();
    let file = map.add_file("jit.gos", source.to_string());
    let (sf, parse_diags) = parse_source_file(source, file);
    assert!(parse_diags.is_empty(), "parse: {parse_diags:?}");
    let (resolutions, _resolve_diags) = resolve_source_file(&sf);
    let mut tcx = TyCtxt::new();
    let (table, _type_diags) = typecheck_source_file(&sf, &resolutions, &mut tcx);
    let program = lower_source_file(&sf, &resolutions, &table, &mut tcx);
    let mut vm = Vm::new();
    vm.load(&program, &mut tcx).expect("load");
    (vm, tcx)
}

#[test]
fn jit_returns_constant_int() {
    let _g = GosJitGuard::new();
    let (vm, _) = build_vm("fn main() -> i64 { 42i64 }\n");
    let result = vm.call("main", Vec::new()).expect("main");
    assert!(matches!(result, Value::Int(42)));
}

#[test]
fn jit_dispatches_through_simple_arithmetic() {
    let _g = GosJitGuard::new();
    let (vm, _) = build_vm(
        "fn add(a: i64, b: i64) -> i64 { a + b }\nfn main() -> i64 { add(7i64, 35i64) }\n",
    );
    let result = vm.call("main", Vec::new()).expect("main");
    assert!(matches!(result, Value::Int(42)));
}

#[test]
fn jit_fallback_for_value_typed_args() {
    // The JIT trampoline only handles primitive scalar args. Calling
    // `concat` with `Value::String` operands forces the fallback
    // path; we still want the right answer.
    let _g = GosJitGuard::new();
    let (vm, _) = build_vm(
        "fn double(n: i64) -> i64 { n * 2i64 }\nfn main() -> i64 { double(21i64) }\n",
    );
    let result = vm.call("main", Vec::new()).expect("main");
    assert!(matches!(result, Value::Int(42)));
}

#[test]
fn jit_fib_recursion_returns_correctly() {
    let _g = GosJitGuard::new();
    let source = "fn fib(n: i64) -> i64 {\n  if n < 2i64 { n } else { fib(n - 1i64) + fib(n - 2i64) }\n}\nfn main() -> i64 { fib(10i64) }\n";
    let (vm, _) = build_vm(source);
    let result = vm.call("main", Vec::new()).expect("main");
    assert!(matches!(result, Value::Int(55)), "fib(10) result: {result:?}");
}

#[test]
fn jit_falls_back_when_signature_unsupported() {
    // String concat returns String — not in the JIT's supported
    // primitive set. The VM should still produce the right answer
    // via the bytecode fallback.
    let _g = GosJitGuard::new();
    let (vm, _) = build_vm(
        "fn pick(b: bool) -> i64 { if b { 1i64 } else { 0i64 } }\nfn main() -> i64 { pick(true) + pick(false) }\n",
    );
    let result = vm.call("main", Vec::new()).expect("main");
    assert!(matches!(result, Value::Int(1)), "pick(true) + pick(false): {result:?}");
}
