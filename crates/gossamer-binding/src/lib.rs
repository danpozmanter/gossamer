//! Stable Rust-binding system for Gossamer libraries.
//!
//! A Rust crate that wants to expose Gossamer-callable functions
//! depends on this crate, declares its module(s) with the
//! [`register_module!`] macro, and lands its `Module` in the
//! global [`REGISTRY`] via `linkme`. The Gossamer toolchain
//! statically links the binding and its registry entries become
//! visible to `use` and to the runtime dispatcher.
//!
//! See `~/dev/contexts/lang/ffi.md` for the full design.

// `gossamer-binding` is the only workspace crate that needs
// `unsafe`: the compiled-mode export ABI in `native` materialises
// `*const c_char`, `*mut GosVec`, etc. from raw pointers handed
// in by the codegen. The unsafe is contained inside `native`;
// every other module keeps the workspace `forbid` posture by
// staying pure-safe.
#![deny(unsafe_code)]

pub mod conv;
mod macros;
pub mod native;
pub mod opaque;
pub mod registry;
mod sig;
pub mod types;

pub use crate::conv::{FromGos, ToGos};
pub use crate::opaque::Registry;
pub use crate::registry::{ItemFn, Module, NativeCall, REGISTRY, Signature};
pub use crate::sig::SigType;
pub use crate::types::Type;

/// Renders the C-ABI export symbol for a binding item.
///
/// Mirrors what the `register_module!` macro emits via the
/// `symbol_prefix:` parameter — `path::to::module` segments get
/// joined with `__`, and the item is appended after a final
/// `__`. Both the codegen and the macro use this scheme so the
/// codegen-emitted call resolves to the macro-emitted thunk at
/// link time.
///
/// Example:
/// `mangle_binding_symbol("tuigoose::layout", "rect")` →
/// `"gos_binding_tuigoose__layout__rect"`.
#[must_use]
pub fn mangle_binding_symbol(module_path: &str, item_name: &str) -> String {
    let mangled_path = module_path.replace("::", "__");
    format!("gos_binding_{mangled_path}__{item_name}")
}

pub use gossamer_interp::value::{NativeDispatch, RuntimeError, RuntimeResult, Value};

#[doc(hidden)]
pub use gossamer_interp::value;

#[doc(hidden)]
pub use linkme;
#[doc(hidden)]
pub use paste as __paste;

/// Returns every module registered via [`register_module!`].
///
/// The slice is populated at link time by `linkme`; it is empty
/// only when no binding crate is in the link graph.
#[must_use]
pub fn modules() -> Vec<&'static Module> {
    REGISTRY.iter().copied().collect()
}

/// Looks up a module by its declared path.
#[must_use]
pub fn module(path: &str) -> Option<&'static Module> {
    REGISTRY.iter().find(|m| m.path == path).copied()
}

/// Resolves an item by `module::name`.
#[must_use]
pub fn item(qualified: &str) -> Option<(&'static Module, &'static ItemFn)> {
    let (mod_path, item_name) = qualified.rsplit_once("::")?;
    let module = module(mod_path)?;
    module
        .items
        .iter()
        .find(|i| i.name == item_name)
        .map(|i| (module, i))
}

/// Installs every registered binding into the interpreter's
/// external-natives table.
///
/// Each item is registered under its fully-qualified
/// `module::item` spelling. Call this exactly once at runtime
/// startup, before constructing the first VM/Interpreter.
///
/// Returns the number of items installed (sum of `module.items.len()`
/// across [`REGISTRY`]).
///
/// Side effects:
/// - registers each item with the interpreter as a `Value::Native`
///   global under its fully-qualified `module::item` spelling.
/// - mirrors the registry into `gossamer_resolve::external` so the
///   resolver / type checker / `gos doc` can see binding metadata
///   without depending on this crate.
#[must_use]
pub fn install_all() -> usize {
    let mut count = 0;
    for module in REGISTRY.iter().copied() {
        for item in module.items {
            let qualified: &'static str =
                Box::leak(format!("{}::{}", module.path, item.name).into_boxed_str());
            gossamer_interp::register_external_native(qualified, item.call);
            // Also register under the bare leaf name so a path
            // expression like `rect(...)` (after
            // `use tuigoose::layout::rect`) resolves through the
            // interpreter's single-segment fallback. Stdlib does
            // the same in `gossamer_interp::builtins::install_module`.
            gossamer_interp::register_external_native(item.name, item.call);
            count += 1;
        }
    }
    populate_resolve_table();
    count
}

fn populate_resolve_table() {
    let modules = REGISTRY
        .iter()
        .copied()
        .map(|m| {
            let items = m
                .items
                .iter()
                .map(|item| gossamer_resolve::ExternalItem {
                    name: item.name.to_string(),
                    params: item.signature.params.iter().map(lower_type).collect(),
                    ret: lower_type(&item.signature.ret),
                    doc: item.doc.to_string(),
                })
                .collect();
            gossamer_resolve::ExternalModule {
                path: m.path.to_string(),
                doc: m.doc.to_string(),
                items,
            }
        })
        .collect();
    gossamer_resolve::set_external_modules(modules);
}

fn lower_type(t: &crate::types::Type) -> gossamer_resolve::BindingType {
    use crate::types::Type;
    use gossamer_resolve::BindingType as R;
    match t {
        Type::Unit => R::Unit,
        Type::Bool => R::Bool,
        Type::I64 => R::I64,
        Type::F64 => R::F64,
        Type::Char => R::Char,
        Type::String => R::String,
        Type::Tuple(ts) => R::Tuple(ts.iter().map(lower_type).collect()),
        Type::Vec(inner) => R::Vec(Box::new(lower_type(inner))),
        Type::Option(inner) => R::Option(Box::new(lower_type(inner))),
        Type::Result(ok, err) => R::Result(Box::new(lower_type(ok)), Box::new(lower_type(err))),
        Type::Opaque(name) => R::Opaque((*name).to_string()),
        Type::Any => R::Any,
    }
}
