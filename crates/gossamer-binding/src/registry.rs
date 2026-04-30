//! Linkme-backed registry of every binding-exported module.
//!
//! Each binding crate uses [`crate::register_module!`] to drop a
//! `&'static Module` into [`REGISTRY`] at link time. The runner
//! walks the registry on startup and installs each item as a
//! `Value::Native` global keyed by `module::item`.

use linkme::distributed_slice;

use gossamer_interp::value::NativeCall as InterpNativeCall;

use crate::types::Type;

/// Function-pointer signature every binding item lowers to.
///
/// Re-exports `gossamer_interp::value::NativeCall` so binding
/// crates name a stable type from this crate alone.
pub type NativeCall = InterpNativeCall;

/// One Gossamer-callable function exported by a binding.
#[derive(Debug, Clone, Copy)]
pub struct ItemFn {
    /// Item name (un-prefixed; the module path is on `Module`).
    pub name: &'static str,
    /// Implementation pointer.
    pub call: NativeCall,
    /// Declared signature for the type checker.
    pub signature: Signature,
    /// One-line documentation rendered by `gos doc`.
    pub doc: &'static str,
}

/// Declared parameter / return types for an [`ItemFn`].
#[derive(Debug, Clone, Copy)]
pub struct Signature {
    /// Positional parameter types.
    pub params: &'static [Type],
    /// Return type.
    pub ret: Type,
}

/// One module exported by a binding crate.
///
/// Modules are flat — no nesting. A binding that wants nested
/// structure declares each sub-module as a separate `Module`
/// (e.g. `tuigoose::layout`, `tuigoose::widgets::block`). The
/// path is the canonical spelling Gossamer source uses with `use`.
#[derive(Debug, Clone, Copy)]
pub struct Module {
    /// Canonical path (e.g. `"tuigoose::layout"`).
    pub path: &'static str,
    /// One-line documentation rendered by `gos doc`.
    pub doc: &'static str,
    /// Items exported from this module.
    pub items: &'static [ItemFn],
}

/// Every `Module` registered via [`register_module!`].
///
/// The slice is populated at link time; a binary that doesn't
/// link any binding crate sees an empty slice.
#[distributed_slice]
pub static REGISTRY: [&'static Module] = [..];
