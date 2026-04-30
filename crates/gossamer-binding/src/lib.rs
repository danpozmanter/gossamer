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

/// Compiled-mode counterpart to [`install_all`].
///
/// Compiled binaries call binding items directly through the C-ABI
/// thunks emitted by `register_module!` — they don't go through
/// the interpreter's external-natives table or the resolver. This
/// function exists so the compiled-mode entry point has a single,
/// stable symbol to call. It's deliberately a no-op aside from
/// touching every `Module` to keep the `linkme` distributed-slice
/// entries alive across LTO.
pub fn install_all_for_compiled() {
    for module in REGISTRY.iter().copied() {
        let _ = module.path;
    }
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
    let mut leaf_groups: rustc_hash::FxHashMap<&'static str, Vec<&'static ItemFn>> =
        rustc_hash::FxHashMap::default();
    for module in REGISTRY.iter().copied() {
        for item in module.items {
            leaf_groups.entry(item.name).or_default().push(item);
        }
    }
    for module in REGISTRY.iter().copied() {
        for item in module.items {
            let qualified: &'static str =
                Box::leak(format!("{}::{}", module.path, item.name).into_boxed_str());
            gossamer_interp::register_external_native(qualified, item.call);
            count += 1;
        }
    }
    for (leaf, group) in &leaf_groups {
        if group.len() == 1 {
            // Unambiguous leaf — install the direct thunk.
            gossamer_interp::register_external_native(leaf, group[0].call);
        } else {
            // Ambiguous leaf — install an arity-aware dispatcher
            // that picks a candidate matching the call's argc.
            // Falls back to the first candidate when no arity
            // matches, so the binding's own arity check produces
            // the standard error message.
            let dispatcher = assign_ambig_dispatcher(group.clone());
            gossamer_interp::register_external_native(leaf, dispatcher);
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

/// Capacity of the ambiguous-leaf dispatcher pool. Each ambiguous
/// leaf consumes one slot; collisions across more than this many
/// distinct leaves panic. The number is generous given typical
/// binding-crate sizes (tuigoose has ~50 items across 7 modules).
const AMBIG_POOL_SIZE: usize = 64;

type AmbigGroup = Vec<&'static ItemFn>;

static AMBIG_SLOTS: parking_lot::RwLock<Vec<Option<AmbigGroup>>> =
    parking_lot::RwLock::new(Vec::new());

fn ambig_slots() -> parking_lot::RwLockReadGuard<'static, Vec<Option<AmbigGroup>>> {
    AMBIG_SLOTS.read()
}

fn ambig_slots_mut() -> parking_lot::RwLockWriteGuard<'static, Vec<Option<AmbigGroup>>> {
    let mut guard = AMBIG_SLOTS.write();
    if guard.len() < AMBIG_POOL_SIZE {
        guard.resize(AMBIG_POOL_SIZE, None);
    }
    guard
}

fn assign_ambig_dispatcher(group: AmbigGroup) -> gossamer_interp::value::NativeCall {
    let idx = {
        let mut guard = ambig_slots_mut();
        let mut chosen: Option<usize> = None;
        for (i, slot) in guard.iter_mut().enumerate() {
            if slot.is_none() {
                *slot = Some(group);
                chosen = Some(i);
                break;
            }
        }
        chosen.expect("ambiguous-leaf pool exhausted; raise AMBIG_POOL_SIZE")
    };
    AMBIG_DISPATCH_TABLE[idx]
}

fn ambig_call<const N: usize>(
    dispatch: &mut dyn gossamer_interp::value::NativeDispatch,
    args: &[gossamer_interp::value::Value],
) -> gossamer_interp::value::RuntimeResult<gossamer_interp::value::Value> {
    let Some(group) = ambig_slots().get(N).cloned().flatten() else {
        return Err(gossamer_interp::value::RuntimeError::Arity {
            expected: 0,
            found: args.len(),
        });
    };
    for item in &group {
        if item.signature.params.len() == args.len() {
            return (item.call)(dispatch, args);
        }
    }
    let first = group
        .first()
        .copied()
        .expect("ambig group must be non-empty");
    (first.call)(dispatch, args)
}

const AMBIG_DISPATCH_TABLE: [gossamer_interp::value::NativeCall; AMBIG_POOL_SIZE] = [
    ambig_call::<0>,
    ambig_call::<1>,
    ambig_call::<2>,
    ambig_call::<3>,
    ambig_call::<4>,
    ambig_call::<5>,
    ambig_call::<6>,
    ambig_call::<7>,
    ambig_call::<8>,
    ambig_call::<9>,
    ambig_call::<10>,
    ambig_call::<11>,
    ambig_call::<12>,
    ambig_call::<13>,
    ambig_call::<14>,
    ambig_call::<15>,
    ambig_call::<16>,
    ambig_call::<17>,
    ambig_call::<18>,
    ambig_call::<19>,
    ambig_call::<20>,
    ambig_call::<21>,
    ambig_call::<22>,
    ambig_call::<23>,
    ambig_call::<24>,
    ambig_call::<25>,
    ambig_call::<26>,
    ambig_call::<27>,
    ambig_call::<28>,
    ambig_call::<29>,
    ambig_call::<30>,
    ambig_call::<31>,
    ambig_call::<32>,
    ambig_call::<33>,
    ambig_call::<34>,
    ambig_call::<35>,
    ambig_call::<36>,
    ambig_call::<37>,
    ambig_call::<38>,
    ambig_call::<39>,
    ambig_call::<40>,
    ambig_call::<41>,
    ambig_call::<42>,
    ambig_call::<43>,
    ambig_call::<44>,
    ambig_call::<45>,
    ambig_call::<46>,
    ambig_call::<47>,
    ambig_call::<48>,
    ambig_call::<49>,
    ambig_call::<50>,
    ambig_call::<51>,
    ambig_call::<52>,
    ambig_call::<53>,
    ambig_call::<54>,
    ambig_call::<55>,
    ambig_call::<56>,
    ambig_call::<57>,
    ambig_call::<58>,
    ambig_call::<59>,
    ambig_call::<60>,
    ambig_call::<61>,
    ambig_call::<62>,
    ambig_call::<63>,
];

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
