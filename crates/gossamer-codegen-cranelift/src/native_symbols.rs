//! Process-wide registry of native binding C-ABI symbols.
//!
//! Two registration paths feed this registry, both consumed by
//! [`super::compile_to_jit`] at JIT-finalize time so user code
//! that calls a binding from a JIT-compiled body resolves to the
//! real entry point:
//!
//! - [`NATIVE_SYMBOLS`] — a `linkme::distributed_slice` populated
//!   at link time by the `register_module!` macro. This is the
//!   normal path: each binding item lands one entry, no runtime
//!   call is required.
//! - [`register_native_symbol`] / [`native_symbols_snapshot`] — a
//!   `Mutex<Vec>` populated at runtime. Kept for backward
//!   compatibility with bindings that call into the legacy
//!   per-module `force_link()` thunk, plus tests.
//!
//! Without this registry, cranelift JIT falls back to its default
//! `dlsym` lookup. `pub extern "C"` symbols statically linked into
//! a Cargo binary are not in the dynamic symbol table by default,
//! so dlsym fails and finalize panics with
//! `can't resolve symbol gos_binding_<...>`.

#![allow(unsafe_code)]

use std::sync::Mutex;

use linkme::distributed_slice;

/// One registered C-ABI binding entry-point. Used by the runtime
/// `Mutex<Vec>` registry path.
#[derive(Clone, Copy)]
pub struct NativeSymbol {
    /// Mangled symbol name as the codegen emits the call.
    pub name: &'static str,
    /// Address of the `extern "C"` thunk.
    pub addr: *const u8,
}

// SAFETY: `addr` points at an `extern "C" fn` with `'static` linkage —
// the binding crate retains the function for the entire process
// lifetime via `linkme`. The pointer is read-only from any thread.
unsafe impl Send for NativeSymbol {}
unsafe impl Sync for NativeSymbol {}

/// Link-time entry advertising one `extern "C"` binding thunk.
///
/// Stored in [`NATIVE_SYMBOLS`] via `linkme::distributed_slice`.
/// `addr_fn` is a thunk that returns the `*const u8` address of
/// the C-ABI export — using a fn pointer rather than a raw
/// pointer keeps the entry constructible in a `static`
/// initializer without unsafe.
pub struct NativeSymbolEntry {
    /// Mangled symbol name as the codegen emits the call.
    pub name: &'static str,
    /// Resolves the address of the `extern "C"` thunk at runtime.
    pub addr_fn: fn() -> *const u8,
}

/// Link-time registry of every binding's C-ABI thunk. Populated
/// by `gossamer_binding::register_module!`; read by
/// [`super::compile_to_jit`].
#[distributed_slice]
pub static NATIVE_SYMBOLS: [NativeSymbolEntry] = [..];

static REGISTRY: Mutex<Vec<NativeSymbol>> = Mutex::new(Vec::new());

/// Registers a binding's C-ABI symbol so the JIT can resolve it.
///
/// Idempotent: re-registering an existing name overwrites the
/// previous address (last-writer-wins). Bindings call this once
/// per item from their `__bindings_force_link()` shim.
pub fn register_native_symbol(name: &'static str, addr: *const u8) {
    let mut guard = REGISTRY
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(entry) = guard.iter_mut().find(|s| s.name == name) {
        entry.addr = addr;
    } else {
        guard.push(NativeSymbol { name, addr });
    }
}

/// Returns a copy of every registered symbol. Each `Copy` entry is
/// trivial to clone; the snapshot is taken under the mutex.
#[must_use]
pub fn native_symbols_snapshot() -> Vec<NativeSymbol> {
    REGISTRY
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    extern "C" fn dummy() {}
    extern "C" fn dummy_two() {}

    #[test]
    fn register_then_snapshot_round_trips() {
        register_native_symbol("gos_binding_test__round_trip", dummy as *const u8);
        let snap = native_symbols_snapshot();
        assert!(
            snap.iter()
                .any(|s| s.name == "gos_binding_test__round_trip")
        );
    }

    #[test]
    fn re_register_overwrites_address() {
        register_native_symbol("gos_binding_test__overwrite", dummy as *const u8);
        register_native_symbol("gos_binding_test__overwrite", dummy_two as *const u8);
        let snap = native_symbols_snapshot();
        let entry = snap
            .iter()
            .find(|s| s.name == "gos_binding_test__overwrite")
            .expect("registered entry visible in snapshot");
        assert_eq!(entry.addr, dummy_two as *const u8);
    }
}
