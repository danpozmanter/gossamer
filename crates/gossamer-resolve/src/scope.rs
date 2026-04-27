//! Lexical-scope tree used by the resolver.
//! A [`ScopeStack`] is a LIFO stack of named bindings organised into two
//! namespaces (type and value). Items at module scope are registered up
//! front so that forward references work; nested block, function, and
//! pattern scopes shadow the module scope and each other.

#![forbid(unsafe_code)]

use std::collections::HashMap;

use gossamer_ast::NodeId;

use crate::def_id::{DefId, DefKind};
use crate::resolutions::{FloatWidth, IntWidth, PrimitiveTy, Resolution};

/// Sentinel [`NodeId`] used for prelude-provided names that have no
/// corresponding `use` declaration in the source file.
pub(crate) const PRELUDE_SENTINEL: NodeId = NodeId::DUMMY;

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
    types: HashMap<String, Binding>,
    /// Names live in the value namespace (fn/const/static/variant/local
    /// binding).
    values: HashMap<String, Binding>,
}

impl Scope {
    pub(crate) fn insert_type(&mut self, name: impl Into<String>, binding: Binding) -> bool {
        self.types.insert(name.into(), binding).is_none()
    }

    pub(crate) fn insert_value(&mut self, name: impl Into<String>, binding: Binding) -> bool {
        self.values.insert(name.into(), binding).is_none()
    }

    pub(crate) fn shadow_value(&mut self, name: impl Into<String>, binding: Binding) {
        self.values.insert(name.into(), binding);
    }

    pub(crate) fn lookup_type(&self, name: &str) -> Option<Binding> {
        self.types.get(name).copied()
    }

    pub(crate) fn lookup_value(&self, name: &str) -> Option<Binding> {
        self.values.get(name).copied()
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
    // Compile-time intrinsics referenced by macro expansion
    // (`println!` → `println(__concat(…))`) and struct-literal
    // lowering (`Path { f: v }` → `__struct("Path", "f", v)`).
    // Both are resolved in the interpreter/codegen, not by user
    // code, but the resolver still traverses the expanded form.
    "__concat",
    "__struct",
    // LCG jump-ahead: routes to `gos_rt_lcg_jump`. Callable
    // from user code as `lcg_jump(state, ia, ic, im, n)`.
    // Used by multi-threaded fasta to seed each worker.
    "lcg_jump",
    "gos_rt_lcg_jump",
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
