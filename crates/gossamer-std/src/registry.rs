//! Registry of stdlib modules and the items each module exports.
//! Until `gossamer-std/std/*.gos` source files can be compiled by the
//! Gossamer toolchain (which depends on the bytecode VM gaining ADT
//! support, ), the stdlib lives here as a manifest backed by
//! Rust-side runtime helpers. The interpreter and bytecode VM consult
//! this table to install built-in functions; the type checker can use
//! it to validate that imported names exist.

#![forbid(unsafe_code)]

/// Top-level stdlib module description.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StdModule {
    /// Path Gossamer source code uses (e.g. `"std::fmt"`, `"fmt"`).
    pub path: &'static str,
    /// Brief one-line summary used in `gos doc` output.
    pub summary: &'static str,
    /// Items exported from this module.
    pub items: &'static [StdItem],
}

/// Single item (function, type, constant) exported from a stdlib
/// module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StdItem {
    /// Item name as imported.
    pub name: &'static str,
    /// Kind tag used by the type checker / `gos doc`.
    pub kind: StdItemKind,
    /// One-line documentation.
    pub doc: &'static str,
}

/// Classification for a stdlib item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StdItemKind {
    /// Plain function.
    Function,
    /// User-facing type (struct or enum).
    Type,
    /// Trait declaration.
    Trait,
    /// Macro / built-in compiler intrinsic exposed as a callable.
    Macro,
    /// Module-level constant.
    Const,
}

/// Returns every registered stdlib module.
#[must_use]
pub fn modules() -> &'static [StdModule] {
    crate::manifest::ALL_MODULES
}

/// Looks up a module by canonical path.
#[must_use]
pub fn module(path: &str) -> Option<&'static StdModule> {
    modules().iter().find(|m| m.path == path)
}

/// Resolves an item by its `module::name` canonical spelling.
#[must_use]
pub fn item(qualified: &str) -> Option<(&'static StdModule, &'static StdItem)> {
    let mut parts: Vec<&str> = qualified.split("::").collect();
    let last = parts.pop()?;
    let path = parts.join("::");
    let module = module(&path)?;
    module
        .items
        .iter()
        .find(|i| i.name == last)
        .map(|i| (module, i))
}
