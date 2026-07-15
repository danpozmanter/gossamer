// Built-in callables exposed to interpreted programs.


use std::cell::RefCell;
use std::fmt::Write;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gossamer_ast::Ident;

use gossamer_std::compress::gzip as gzip_std;
use gossamer_std::env as env_std;
use gossamer_std::exec as exec_std;
use gossamer_std::fs as fs_std;
use gossamer_std::http as http_std;
use gossamer_std::json as json_std;
use gossamer_std::os as os_std;
use gossamer_std::signal as signal_std;
use gossamer_std::slog as slog_std;
use gossamer_std::time as time_std;

use crate::value::{
    DenseMap, JsonInner, MapKey, NativeDispatch, RuntimeError, RuntimeResult, SmolStr, Value,
    dense_map_with_capacity,
};

thread_local! {
    pub(crate) static PROGRAM_ARGS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    pub(crate) static PROGRAM_NAME: RefCell<String> = RefCell::new(String::from("gos"));
}

/// Overwrites the program-level argument list that `env::args()`
/// returns. Called by the CLI entrypoint before invoking `main`.
///
/// Wires both execution paths:
/// - The bytecode VM's `env::args()` builtin reads from the
///   `PROGRAM_ARGS` thread-local cell below.
/// - JIT-compiled `main` calls into the runtime's
///   `gos_rt_os_args`, which reads from a *different* static
///   inside `gossamer-runtime::c_abi`. Without this second wire,
///   benchmarks like `fasta` and `nbody` see an empty arg list
///   when their `main` JIT-compiles, fall back to the default N
///   (typically 1000), and silently produce undersized output.
///
/// The runtime side is wired by `crate::set_runtime_args` in
/// `lib.rs`, which is allowed to call into the FFI; this module
/// keeps `forbid(unsafe_code)`.
pub fn set_program_args(args: &[String]) {
    PROGRAM_ARGS.with(|cell| {
        let mut v = cell.borrow_mut();
        v.clear();
        v.extend_from_slice(args);
    });
    crate::set_runtime_args(args);
}

/// Sets the program name returned by `os::program_name()`. The CLI
/// calls this with the script path before invoking `main`.
pub fn set_program_name(name: &str) {
    PROGRAM_NAME.with(|cell| *cell.borrow_mut() = String::from(name));
    crate::set_runtime_program_name(name);
}

// ------------------------------------------------------------------
// Mutable cell backing for `flag::Set` API.
//
// `Set::string` / `int` / `uint` / `bool` return a `__Cell` struct
// that `*` dereferences in `interp.rs` via [`resolve_cell`].

pub(crate) type CellMap =
    std::collections::HashMap<(u64, String), std::sync::Arc<parking_lot::Mutex<Value>>>;

// HashMap::new is not const-callable on our MSRV; these
// thread-locals construct on first access and stay registry-style
// for the life of the thread.
#[allow(
    clippy::missing_const_for_thread_local,
    reason = "HashMap::new with default RandomState is not const on MSRV"
)]
mod thread_local_registries {
    use super::{CellMap, RefCell, SetState};

    thread_local! {
        pub(crate) static NEXT_SET_ID: RefCell<u64> = const { RefCell::new(1) };
        pub(crate) static SET_REGISTRY: RefCell<std::collections::HashMap<u64, SetState>> =
            RefCell::new(std::collections::HashMap::new());
        pub(crate) static CELL_REGISTRY: RefCell<CellMap> = RefCell::new(std::collections::HashMap::new());
        pub(crate) static STRUCT_LAYOUTS: RefCell<std::collections::HashMap<String, Vec<&'static str>>> =
            RefCell::new(std::collections::HashMap::new());
    }
}

pub(crate) use thread_local_registries::{
    CELL_REGISTRY, NEXT_SET_ID, SET_REGISTRY, STRUCT_LAYOUTS,
};

/// Installs the struct-field declaration-order table that
/// `__struct` consults when assembling a new `Value::Struct`.
/// Invoked by [`crate::Vm::load`] before any program code runs.
#[allow(
    clippy::implicit_hasher,
    reason = "stored verbatim in a RandomState-typed thread-local; generic hasher would force the thread-local to be generic too"
)]
pub fn set_struct_layouts(layouts: std::collections::HashMap<String, Vec<String>>) {
    // Intern each field name once, here at load time, so the per-construction
    // path in `builtin_struct_new` copies cached `&'static str` pointers with
    // no interning (and no global-intern lock) per struct value built.
    let interned: std::collections::HashMap<String, Vec<&'static str>> = layouts
        .into_iter()
        .map(|(name, fields)| {
            let interned_fields = fields
                .iter()
                .map(|f| crate::value::intern_type_name(f))
                .collect();
            (name, interned_fields)
        })
        .collect();
    STRUCT_LAYOUTS.with(|cell| *cell.borrow_mut() = interned);
}

#[derive(Debug, Clone)]
pub(crate) struct SetState {
    pub(crate) name: String,
    pub(crate) flag_order: Vec<String>,
    pub(crate) last_flag: Option<String>,
    pub(crate) flags: std::collections::HashMap<String, FlagDef>,
}

#[derive(Debug, Clone)]
pub(crate) struct FlagDef {
    pub(crate) short: Option<char>,
    pub(crate) kind: FlagKind,
    pub(crate) help: String,
    pub(crate) default: Value,
}

#[derive(Debug, Clone)]
pub(crate) enum FlagKind {
    String,
    Int,
    Uint,
    Float,
    Bool,
    Duration,
    StringList,
}

pub(crate) fn make_cell(set_id: u64, flag_name: &str, default: Value) -> Value {
    let key = (set_id, flag_name.to_string());
    let cell = std::sync::Arc::new(parking_lot::Mutex::new(default));
    CELL_REGISTRY.with(|reg| {
        reg.borrow_mut().insert(key, cell);
    });
    Value::struct_(
        "__Cell",
        vec![
            ("__set_id", Value::Int(set_id as i64)),
            (
                "__flag_name",
                Value::String(SmolStr::from(flag_name.to_string())),
            ),
        ],
    )
}

/// Resolves a `__Cell` handle to its current value.
pub(crate) fn resolve_cell(set_id: u64, flag_name: &str) -> Option<Value> {
    CELL_REGISTRY.with(|reg| {
        reg.borrow()
            .get(&(set_id, flag_name.to_string()))
            .map(|arc| arc.lock().clone())
    })
}

/// Installs stdlib-shaped built-ins (`println`, `print`, `eprintln`,
/// `eprint`, `format`, `panic`, ...) into the given global table,
/// plus a curated set of no-op stubs that let real-world example
/// programs at least reach the end of `main` without crashing.
pub(crate) fn install(globals: &mut Vec<(&'static str, Value)>) {
    install_io_builtins(globals);
    install_http_builtins(globals);
    install_variant_builtins(globals);
    install_module_builtins(globals);
    install_flag_builtins(globals);
    install_method_helpers(globals);
    install_concurrency_builtins(globals);
    install_regex_builtins(globals);
    crate::stdlib_builtins::install(globals);
    #[cfg(not(target_arch = "wasm32"))]
    globals.push(("serve", native("serve", native_http_serve)));
}

/// Returns the process-wide cached builtin table (built once on
/// first call). Each `Value::Builtin` / `Value::Native` payload is
/// behind an `Arc`, so cloning the entries is a refcount bump per
/// builtin - cheap enough that `Vm::new` can iterate the cached slice
/// when populating its globals map. The single shared cache avoids
/// rebuilding all ~330 entries per VM construction.
pub(crate) fn cached() -> &'static [(&'static str, Value)] {
    use std::sync::OnceLock;
    static CACHE: OnceLock<Vec<(&'static str, Value)>> = OnceLock::new();
    CACHE.get_or_init(|| {
        let mut list = Vec::new();
        install(&mut list);
        list
    })
}

/// Every globally-registered builtin name (bare and qualified). The
/// resolver ships a checked-in table of the qualified stdlib paths so
/// `gos check` / the LSP can reject `module::nonexistent` calls before
/// runtime; a drift test compares that table against this list so it
/// never falls behind the runtime registry. Returns `&'static str`
/// since every key is a string literal or interned name.
#[must_use]
pub fn registered_names() -> Vec<&'static str> {
    cached().iter().map(|(name, _)| *name).collect()
}

/// Process-shared prelude `HashMap` of all built-in callables. Every
/// [`Vm`](crate::vm::Vm) `Arc::clone`s this map and consults it on
/// lookup miss against its own per-Vm overlay; no Vm copies the
/// prelude into its own storage. Late-registered binding natives
/// stay out of the prelude - they can land after Vm construction
/// and ride the per-Vm overlay instead.
pub(crate) fn prelude_globals()
-> std::sync::Arc<rustc_hash::FxHashMap<&'static str, crate::vm::Global>> {
    use std::sync::OnceLock;
    static PRELUDE: OnceLock<
        std::sync::Arc<rustc_hash::FxHashMap<&'static str, crate::vm::Global>>,
    > = OnceLock::new();
    std::sync::Arc::clone(PRELUDE.get_or_init(|| {
        let mut map = rustc_hash::FxHashMap::default();
        for (name, value) in cached() {
            map.insert(*name, crate::vm::Global::Value(value.clone()));
        }
        map.shrink_to_fit();
        std::sync::Arc::new(map)
    }))
}

