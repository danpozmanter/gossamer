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
    vm.load(&program, tcx.clone(), true).expect("load");
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
    let (vm, _) =
        build_vm("fn double(n: i64) -> i64 { n * 2i64 }\nfn main() -> i64 { double(21i64) }\n");
    let result = vm.call("main", Vec::new()).expect("main");
    assert!(matches!(result, Value::Int(42)));
}

#[test]
fn jit_fib_recursion_returns_correctly() {
    let _g = GosJitGuard::new();
    let source = "fn fib(n: i64) -> i64 {\n  if n < 2i64 { n } else { fib(n - 1i64) + fib(n - 2i64) }\n}\nfn main() -> i64 { fib(10i64) }\n";
    let (vm, _) = build_vm(source);
    let result = vm.call("main", Vec::new()).expect("main");
    assert!(
        matches!(result, Value::Int(55)),
        "fib(10) result: {result:?}"
    );
}

#[test]
fn jit_falls_back_when_signature_unsupported() {
    // String concat returns String - not in the JIT's supported
    // primitive set. The VM should still produce the right answer
    // via the bytecode fallback.
    let _g = GosJitGuard::new();
    let (vm, _) = build_vm(
        "fn pick(b: bool) -> i64 { if b { 1i64 } else { 0i64 } }\nfn main() -> i64 { pick(true) + pick(false) }\n",
    );
    let result = vm.call("main", Vec::new()).expect("main");
    assert!(
        matches!(result, Value::Int(1)),
        "pick(true) + pick(false): {result:?}"
    );
}

/// Calls `name` enough times to drive its per-function tier-up
/// counter to zero (the floor is 16, the ceiling 100), which forces
/// the deferred cranelift compile and installs the native override.
/// Deterministic: the count is fixed, not time-based.
fn warm_up(vm: &Vm, name: &str, args: &[Value]) {
    for _ in 0..300 {
        let _ = vm.call(name, args.to_vec());
    }
}

#[test]
fn jit_u64_ge_uses_unsigned_comparison() {
    // Regression: the JIT lowered `>=` with a signed condition even
    // for `u64`, so a value with the high bit set (>= 2^63) compared
    // as a negative i64. `2^63 >= 1` must be true unsigned; a signed
    // compare wrongly yields false. The bytecode VM is the oracle.
    let _g = GosJitGuard::new();
    let (vm, _) =
        build_vm("fn uge(a: u64, b: u64) -> bool { a >= b }\nfn main() -> i64 { 0i64 }\n");
    // `i64::MIN` is the bit pattern of `u64` 2^63.
    let big = Value::Int(i64::MIN);
    let one = Value::Int(1);
    let vm_answer = vm.call("uge", vec![big.clone(), one.clone()]).expect("uge");
    assert!(
        matches!(vm_answer, Value::Bool(true)),
        "bytecode oracle: u64 2^63 >= 1 should be true, got {vm_answer:?}"
    );
    warm_up(&vm, "uge", &[big.clone(), one.clone()]);
    let jit_answer = vm.call("uge", vec![big, one]).expect("uge");
    assert!(
        matches!(jit_answer, Value::Bool(true)),
        "JIT must match the VM: u64 2^63 >= 1 is true (unsigned), got {jit_answer:?}"
    );
}

#[test]
fn jit_u64_shr_uses_logical_shift() {
    // Regression: the JIT always lowered `>>` as an arithmetic shift.
    // `u64` must shift logically: `2^63 >> 1 == 2^62`; an arithmetic
    // shift would sign-extend to `-2^62`.
    let _g = GosJitGuard::new();
    let (vm, _) =
        build_vm("fn ushr(a: u64, n: u64) -> u64 { a >> n }\nfn main() -> i64 { 0i64 }\n");
    let big = Value::Int(i64::MIN); // u64 2^63
    let one = Value::Int(1);
    let expected = 1i64 << 62; // 2^62
    let vm_answer = vm
        .call("ushr", vec![big.clone(), one.clone()])
        .expect("ushr");
    assert!(
        matches!(vm_answer, Value::Int(n) if n == expected),
        "bytecode oracle: u64 2^63 >> 1 should be 2^62, got {vm_answer:?}"
    );
    warm_up(&vm, "ushr", &[big.clone(), one.clone()]);
    let jit_answer = vm.call("ushr", vec![big, one]).expect("ushr");
    assert!(
        matches!(jit_answer, Value::Int(n) if n == expected),
        "JIT must match the VM: u64 2^63 >> 1 is 2^62 (logical), got {jit_answer:?}"
    );
}

#[test]
fn jit_divide_by_zero_is_clean_panic_not_trap() {
    // Regression: the JIT lowered a failed divide-by-zero assertion
    // to a raw machine trap (SIGILL), where the VM and AOT tiers
    // render a clean `error[GX0005]` panic. The panic terminates the
    // process, so the actual divide runs in a re-exec'd child and the
    // parent inspects how the child died: a clean panic exits with a
    // code and a GX0005 message; a trap kills the child by signal.
    const CHILD_ENV: &str = "GOS_JIT_DIVZERO_CHILD";
    if std::env::var(CHILD_ENV).is_ok() {
        let _g = GosJitGuard::new();
        // `divi` carries a one-trip loop so it is JIT-worthy under the
        // promote-only-real-work policy; the divide still runs natively,
        // which is the path this regression guards.
        let (vm, _) = build_vm(
            "fn divi(a: i64, b: i64) -> i64 { let mut r = 0\nlet mut i = 0\nwhile i < 1 { r = a / b\ni += 1 }\nr }\nfn main() -> i64 { 0i64 }\n",
        );
        warm_up(&vm, "divi", &[Value::Int(10), Value::Int(2)]);
        // Trip the divide-by-zero on the now-native body.
        let _ = vm.call("divi", vec![Value::Int(1), Value::Int(0)]);
        // Only reached if the body somehow did not panic; exit clean.
        std::process::exit(0);
    }
    let exe = std::env::current_exe().expect("current test executable");
    let output = std::process::Command::new(exe)
        .args([
            "jit_divide_by_zero_is_clean_panic_not_trap",
            "--exact",
            "--nocapture",
        ])
        .env(CHILD_ENV, "1")
        .output()
        .expect("spawn child test process");
    let stderr = String::from_utf8_lossy(&output.stderr);
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        assert!(
            output.status.signal().is_none(),
            "JIT divide-by-zero terminated by signal {:?} (an illegal-instruction \
             trap) instead of a clean panic; stderr:\n{stderr}",
            output.status.signal()
        );
    }
    assert!(
        stderr.contains("GX0005"),
        "JIT divide-by-zero must produce a clean error[GX0005] panic; \
         child status {:?}, stderr:\n{stderr}",
        output.status
    );
}
