//! Process-wide registry of binding-installed native functions.
//!
//! `gossamer-binding::install_all` populates this table once at
//! startup; both [`crate::Interpreter::new`] and [`crate::Vm::new`]
//! merge the snapshot into their per-instance globals so qualified
//! binding paths (e.g. `tuigoose::layout::rect`) resolve through
//! the same `Value::Native` lookup the interpreter uses for
//! built-in stdlib entries.

use std::sync::Mutex;

use crate::value::{NativeCall, Value};

static EXTERNAL_NATIVES: Mutex<Vec<(&'static str, Value)>> = Mutex::new(Vec::new());

/// Registers a binding-supplied native function under `name`.
///
/// Both segments of the name are stored: the binding crate
/// pre-leaks the qualified spelling so this entry point never
/// touches lifetime concerns.
pub fn register_external_native(name: &'static str, call: NativeCall) {
    let value = Value::native(name, call);
    let mut guard = EXTERNAL_NATIVES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    guard.push((name, value));
}

/// Returns a clone of every (name, native-value) pair currently
/// registered. Each `Value::Native` payload is `Arc`-backed, so
/// cloning is a refcount bump.
#[must_use]
pub fn external_natives_snapshot() -> Vec<(&'static str, Value)> {
    EXTERNAL_NATIVES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

/// Clears the registry. Used by tests that need to assert from
/// a fresh state.
pub fn clear_external_natives_for_test() {
    EXTERNAL_NATIVES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clear();
}
