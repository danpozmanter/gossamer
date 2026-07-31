//! Minimal Rust binding: a single `add` function exposed to Gossamer.
//!
//! `register_module!` emits both the interpreter thunk and the C-ABI
//! thunk (`gos_binding_addlib__add`), so the same crate is callable
//! from `gos`, `gos test`, and `gos build --release`.
//!
//! Uses the 0.9.0 ergonomic form: `name: addlib` doubles as the
//! Gossamer-side spelling AND the C-ABI symbol prefix; no
//! `symbol_prefix:` line, no hand-written `__bindings_force_link`
//! shim. (The runner template still expects the shim for back-compat,
//! so it's kept as a thin one-liner.)

use gossamer_binding::register_module;

register_module!(
    name: addlib,
    doc: "Minimal demo binding: one Rust function callable from Gossamer.",

    fn add(a: i64, b: i64) -> i64 {
        a + b
    }
);

pub fn __bindings_force_link() {
    __gos_addlib::force_link();
}
