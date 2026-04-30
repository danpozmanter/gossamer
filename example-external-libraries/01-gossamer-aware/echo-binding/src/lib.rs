//! Tiny binding crate exposing string helpers to Gossamer code.
//!
//! The `register_module!` macro emits both an interpreter thunk
//! (consumed by the bytecode VM) and a C-ABI thunk
//! (`gos_binding_echo__shout`, etc.) used by the compiled-mode
//! linker.

use gossamer_binding::register_module;

register_module!(
    binding,
    path: "echo",
    symbol_prefix: echo,
    doc: "String helpers exposed by the example echo-binding crate.",

    fn shout(s: String) -> String {
        s.to_uppercase()
    }

    fn sum(xs: Vec<i64>) -> i64 {
        xs.iter().sum()
    }

    fn count(xs: Vec<i64>) -> i64 {
        i64::try_from(xs.len()).unwrap_or(i64::MAX)
    }
);

/// Linker hook required by the runner template.
pub fn __bindings_force_link() {
    binding::force_link();
}
