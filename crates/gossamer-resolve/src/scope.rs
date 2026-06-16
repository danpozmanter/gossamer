//! Lexical-scope tree used by the resolver.
//! A [`ScopeStack`] is a LIFO stack of named bindings organised into two
//! namespaces (type and value). Items at module scope are registered up
//! front so that forward references work; nested block, function, and
//! pattern scopes shadow the module scope and each other.

#![forbid(unsafe_code)]

use std::collections::HashMap;

use gossamer_ast::NodeId;
use gossamer_lex::Symbol;

use crate::def_id::{DefId, DefKind};
use crate::resolutions::{FloatWidth, IntWidth, PrimitiveTy, Resolution};

/// Sentinel [`NodeId`] used for prelude-provided names that have no
/// corresponding `use` declaration in the source file.
pub(crate) const PRELUDE_SENTINEL: NodeId = NodeId::DUMMY;

/// True for bindings inserted by [`ScopeStack::with_prelude`].
/// Those are the only entries `insert_type` / `insert_value`
/// silently overwrite — a non-prelude collision stays an error.
fn is_prelude_binding(b: &Binding) -> bool {
    matches!(
        b.resolution,
        Resolution::Import { use_id } if use_id == PRELUDE_SENTINEL
    )
}

/// A single entry in the value or type namespace.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Binding {
    /// Resolved target of this name.
    pub resolution: Resolution,
}

impl Binding {
    pub(crate) const fn def(def: DefId, kind: DefKind) -> Self {
        Self {
            resolution: Resolution::Def { def, kind },
        }
    }

    pub(crate) const fn local(node: NodeId) -> Self {
        Self {
            resolution: Resolution::Local(node),
        }
    }

    pub(crate) const fn primitive(prim: PrimitiveTy) -> Self {
        Self {
            resolution: Resolution::Primitive(prim),
        }
    }

    pub(crate) const fn import(use_id: NodeId) -> Self {
        Self {
            resolution: Resolution::Import { use_id },
        }
    }
}

/// One layer in the [`ScopeStack`].
#[derive(Debug, Default, Clone)]
pub(crate) struct Scope {
    /// Names live in the type namespace (struct/enum/trait/alias/module/
    /// type-parameter/primitive).
    types: HashMap<Symbol, Binding>,
    /// Names live in the value namespace (fn/const/static/variant/local
    /// binding).
    values: HashMap<Symbol, Binding>,
}

impl Scope {
    /// Returns `false` when the slot is already occupied by a
    /// non-prelude binding (a real duplicate). Prelude bindings —
    /// inserted by `with_prelude` with `PRELUDE_SENTINEL` — get
    /// silently overwritten so user definitions can shadow them
    /// (e.g. `fn clamp(...)` overriding the new prelude `clamp`).
    pub(crate) fn insert_type(&mut self, name: impl Into<Symbol>, binding: Binding) -> bool {
        let sym = name.into();
        if let Some(existing) = self.types.get(&sym) {
            if !is_prelude_binding(existing) {
                return false;
            }
        }
        self.types.insert(sym, binding);
        true
    }

    pub(crate) fn insert_value(&mut self, name: impl Into<Symbol>, binding: Binding) -> bool {
        let sym = name.into();
        if let Some(existing) = self.values.get(&sym) {
            if !is_prelude_binding(existing) {
                return false;
            }
        }
        self.values.insert(sym, binding);
        true
    }

    pub(crate) fn shadow_value(&mut self, name: impl Into<Symbol>, binding: Binding) {
        self.values.insert(name.into(), binding);
    }

    pub(crate) fn lookup_type(&self, name: &str) -> Option<Binding> {
        self.types.get(&Symbol::intern(name)).copied()
    }

    pub(crate) fn lookup_value(&self, name: &str) -> Option<Binding> {
        self.values.get(&Symbol::intern(name)).copied()
    }
}

/// Stack of lexical scopes visible at a given program point.
#[derive(Debug, Default, Clone)]
pub(crate) struct ScopeStack {
    layers: Vec<Scope>,
}

impl ScopeStack {
    /// Builds a stack seeded with a single module-level scope containing
    /// every primitive type name and the stdlib prelude entries that are
    /// always in scope (Result, Option, their variants, `str`, ...).
    pub(crate) fn with_prelude() -> Self {
        let mut root = Scope::default();
        for (name, prim) in PRIMITIVE_TYPES {
            root.insert_type(*name, Binding::primitive(*prim));
        }
        for name in PRELUDE_TYPES {
            root.insert_type(*name, Binding::import(PRELUDE_SENTINEL));
        }
        for name in PRELUDE_VALUES {
            root.insert_value(*name, Binding::import(PRELUDE_SENTINEL));
        }
        Self { layers: vec![root] }
    }

    /// Pushes a fresh empty scope.
    pub(crate) fn push(&mut self) {
        self.layers.push(Scope::default());
    }

    /// Pops the top scope. Panics in debug builds if the stack is empty
    /// (callers must balance push/pop).
    pub(crate) fn pop(&mut self) {
        debug_assert!(self.layers.len() > 1, "cannot pop the module scope");
        self.layers.pop();
    }

    /// Returns a mutable handle to the top-of-stack scope for inserting
    /// new bindings.
    pub(crate) fn top_mut(&mut self) -> &mut Scope {
        let idx = self.layers.len() - 1;
        &mut self.layers[idx]
    }

    /// Returns a handle to the innermost module-level scope (the bottom
    /// of the stack). Used when registering top-level items up front.
    pub(crate) fn module_mut(&mut self) -> &mut Scope {
        &mut self.layers[0]
    }

    /// Searches from innermost to outermost for a type-namespace binding.
    pub(crate) fn lookup_type(&self, name: &str) -> Option<Binding> {
        for scope in self.layers.iter().rev() {
            if let Some(binding) = scope.lookup_type(name) {
                return Some(binding);
            }
        }
        None
    }

    /// Searches from innermost to outermost for a value-namespace binding.
    pub(crate) fn lookup_value(&self, name: &str) -> Option<Binding> {
        for scope in self.layers.iter().rev() {
            if let Some(binding) = scope.lookup_value(name) {
                return Some(binding);
            }
        }
        None
    }
}

const PRELUDE_TYPES: &[&str] = &[
    "str",
    "Result",
    "Option",
    "Vec",
    "HashMap",
    "HashSet",
    "BTreeMap",
    "BTreeSet",
    "VecDeque",
    "Box",
    "Arc",
    "Rc",
    "Weak",
    "Range",
    "Sender",
    "Receiver",
    // Sync primitives matched to Go's `sync` package: a
    // mutex (lock/unlock), a wait group (add/done/wait), a
    // heap-allocated `[i64]` for cross-goroutine writes, and
    // an `AtomicI64` for lock-free counters.
    "Mutex",
    "WaitGroup",
    "I64Vec",
    "U8Vec",
    "Atomic",
];

const PRELUDE_VALUES: &[&str] = &[
    "Ok",
    "Err",
    "Some",
    "None",
    "print",
    "println",
    "eprint",
    "eprintln",
    "format",
    "panic",
    "assert",
    "assert_eq",
    "todo",
    // 0.7.0 scalar QoL helpers. Routed by `builtin_min_dispatch`
    // / `builtin_max_dispatch` / `builtin_clamp` in the interp;
    // compiled tier resolves them through the same bare-name
    // path. Single-Vec callers still get the iter::min / iter::max
    // fall-through inside the dispatcher.
    "min",
    "max",
    "clamp",
    // Goroutine join handle: `spawn(f)` runs `f` on a goroutine and
    // returns a handle whose `.join()` blocks for the outcome. Bare
    // prelude name so a user `fn spawn` overrides it.
    "spawn",
    // `channel()` — typed goroutine channel constructor. Prelude so the
    // injected `time::after` / `time::tick` timer wrappers can build a
    // channel without the user importing `std::sync::channel`.
    "channel",
    // Compile-time intrinsics referenced by macro expansion
    // (`println!` → `println(__concat(…))`) and struct-literal
    // lowering (`Path { f: v }` → `__struct("Path", "f", v)`).
    // Both are resolved in the interpreter/codegen, not by user
    // code, but the resolver still traverses the expanded form.
    "__concat",
    "__fmt_prec",
    // Format-spec intrinsics emitted by `{:spec}` macro expansion:
    // `__fmt_pad` (width/align/fill), `__fmt_radix` (`{:x}`/`{:b}`/`{:o}`),
    // `__fmt_upper` (`{:X}`). Lowered through the stdlib free-call table.
    "__fmt_pad",
    "__fmt_radix",
    "__fmt_upper",
    "__struct",
    // LCG jump-ahead: routes to `gos_rt_lcg_jump`. Callable
    // from user code as `lcg_jump(state, ia, ic, im, n)`.
    // Used by multi-threaded fasta to seed each worker.
    "lcg_jump",
    "gos_rt_lcg_jump",
    // Leaf intrinsics for the injected `encoding::pem` real-struct
    // wrappers (gossamer-parse autoderive). Resolved here so the
    // synthesized wrapper bodies type-check; user code never names
    // them directly.
    "__gos_pem_decode_raw",
    "__gos_pem_decode_all_raw",
    "__gos_pem_encode_raw",
    "__gos_x509_parse_pem_raw",
    "__gos_fs_metadata_raw",
    "__gos_tar_read_raw",
    "__gos_zip_read_raw",
    // Leaf intrinsics for the injected `database::sql` real-struct
    // wrappers (gossamer-parse autoderive).
    "__gos_sql_open_raw",
    "__gos_sql_last_error_raw",
    "__gos_sql_drivers_raw",
    "__gos_sql_params_new_raw",
    "__gos_sql_params_push_null_raw",
    "__gos_sql_params_push_bool_raw",
    "__gos_sql_params_push_int_raw",
    "__gos_sql_params_push_float_raw",
    "__gos_sql_params_push_text_raw",
    "__gos_sql_params_push_blob_raw",
    "__gos_sql_conn_execute_raw",
    "__gos_sql_conn_query_raw",
    "__gos_sql_conn_begin_raw",
    "__gos_sql_conn_begin_with_raw",
    "__gos_sql_conn_ping_raw",
    "__gos_sql_conn_set_busy_timeout_raw",
    "__gos_sql_conn_interrupt_raw",
    "__gos_sql_conn_close_raw",
    "__gos_sql_rows_next_row_raw",
    "__gos_sql_rows_close_raw",
    "__gos_sql_rows_columns_raw",
    "__gos_sql_row_kind_raw",
    "__gos_sql_row_get_i64_raw",
    "__gos_sql_row_get_f64_raw",
    "__gos_sql_row_get_bool_raw",
    "__gos_sql_row_get_text_raw",
    "__gos_sql_row_get_blob_raw",
    "__gos_sql_row_width_raw",
    "__gos_sql_tx_commit_raw",
    "__gos_sql_tx_rollback_raw",
    "__gos_sql_tx_execute_raw",
    "__gos_sql_tx_savepoint_raw",
    "__gos_sql_tx_release_savepoint_raw",
    "__gos_sql_tx_rollback_to_savepoint_raw",
    "__gos_sql_tx_execute_params_raw",
    "__gos_sql_tx_query_params_raw",
    "__gos_sql_conn_prepare_raw",
    "__gos_sql_stmt_execute_raw",
    "__gos_sql_stmt_query_raw",
    "__gos_sql_stmt_close_raw",
    "__gos_sql_conn_copy_in_raw",
    "__gos_sql_conn_copy_out_run_raw",
    "__gos_sql_conn_copy_out_take_raw",
    "__gos_sql_conn_listen_raw",
    "__gos_sql_conn_unlisten_raw",
    "__gos_sql_conn_poll_notification_raw",
    "__gos_sql_notification_channel_raw",
    "__gos_sql_notification_payload_raw",
    "__gos_sql_notification_pid_raw",
    "__gos_sql_pool_new_raw",
    "__gos_sql_pool_get_raw",
    "__gos_sql_pool_live_raw",
    "__gos_sql_pool_idle_raw",
    "__gos_sql_pool_close_idle_raw",
    "__gos_sql_migrate_up_raw",
];

const PRIMITIVE_TYPES: &[(&str, PrimitiveTy)] = &[
    ("bool", PrimitiveTy::Bool),
    ("char", PrimitiveTy::Char),
    ("String", PrimitiveTy::String),
    ("i8", PrimitiveTy::Int(IntWidth::W8)),
    ("i16", PrimitiveTy::Int(IntWidth::W16)),
    ("i32", PrimitiveTy::Int(IntWidth::W32)),
    ("i64", PrimitiveTy::Int(IntWidth::W64)),
    ("i128", PrimitiveTy::Int(IntWidth::W128)),
    ("isize", PrimitiveTy::Int(IntWidth::Size)),
    ("u8", PrimitiveTy::UInt(IntWidth::W8)),
    ("u16", PrimitiveTy::UInt(IntWidth::W16)),
    ("u32", PrimitiveTy::UInt(IntWidth::W32)),
    ("u64", PrimitiveTy::UInt(IntWidth::W64)),
    ("u128", PrimitiveTy::UInt(IntWidth::W128)),
    ("usize", PrimitiveTy::UInt(IntWidth::Size)),
    ("f32", PrimitiveTy::Float(FloatWidth::W32)),
    ("f64", PrimitiveTy::Float(FloatWidth::W64)),
    ("Never", PrimitiveTy::Never),
    ("Unit", PrimitiveTy::Unit),
];
