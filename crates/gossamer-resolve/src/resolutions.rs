//! Side table mapping `NodeId`s to their resolved meaning.

#![forbid(unsafe_code)]

use std::collections::HashMap;

use gossamer_ast::NodeId;

use crate::def_id::{DefId, DefKind};

/// Where a named reference points after resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Resolution {
    /// A local binding introduced by a `let`, pattern, or parameter. The
    /// `NodeId` identifies the binding occurrence.
    Local(NodeId),
    /// A named item defined in the current crate.
    Def {
        /// Stable identifier of the definition.
        def: DefId,
        /// Kind of definition the id refers to.
        kind: DefKind,
    },
    /// A built-in primitive type (`i32`, `bool`, ...).
    Primitive(PrimitiveTy),
    /// A name imported by a `use` declaration. The leading segment still
    /// needs to be looked up externally; this resolution simply records
    /// that the name is an imported alias.
    Import {
        /// `NodeId` of the `use` declaration that introduced the name.
        use_id: NodeId,
    },
    /// The name could not be resolved; a diagnostic has been produced.
    Err,
}

/// Built-in primitive types recognised directly by the resolver.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PrimitiveTy {
    /// `bool`.
    Bool,
    /// `char`.
    Char,
    /// The owned `String` type used for text.
    String,
    /// A signed integer type with width in bits (8, 16, 32, 64, 128) or
    /// `0` to signal `isize`.
    Int(IntWidth),
    /// An unsigned integer type with width in bits (8, 16, 32, 64, 128) or
    /// `0` to signal `usize`.
    UInt(IntWidth),
    /// A floating-point type (`f32` or `f64`).
    Float(FloatWidth),
    /// The never type `!`.
    Never,
    /// The unit type `()` reached via `Self::Unit` is expressed as the
    /// `TypeKind::Unit` variant rather than a primitive path, but the
    /// resolver still emits this variant if user code writes `Unit`.
    Unit,
}

/// Width tag for integer primitive types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IntWidth {
    /// Pointer-sized (`isize` / `usize`).
    Size,
    /// 8-bit (`i8` / `u8`).
    W8,
    /// 16-bit.
    W16,
    /// 32-bit.
    W32,
    /// 64-bit.
    W64,
    /// 128-bit.
    W128,
}

/// Width tag for floating-point primitive types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FloatWidth {
    /// 32-bit IEEE-754 binary32.
    W32,
    /// 64-bit IEEE-754 binary64.
    W64,
}

/// Side table produced by [`crate::resolve_source_file`].
#[derive(Debug, Default, Clone)]
pub struct Resolutions {
    entries: HashMap<NodeId, Resolution>,
    bindings: HashMap<NodeId, DefId>,
    definitions: HashMap<DefId, DefKind>,
}

impl Resolutions {
    /// Returns an empty resolution table.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Records that `path_node` resolves to `resolution`.
    pub fn insert(&mut self, path_node: NodeId, resolution: Resolution) {
        self.entries.insert(path_node, resolution);
    }

    /// Looks up the resolution for a path node. Returns `None` if the
    /// node was never touched by the resolver (e.g. because it appears
    /// outside of a resolved position) or has no resolution recorded.
    #[must_use]
    pub fn get(&self, path_node: NodeId) -> Option<Resolution> {
        self.entries.get(&path_node).copied()
    }

    /// Returns `true` when at least one resolution has been recorded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Returns the number of recorded resolutions.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Records the [`DefId`] allocated for an item or binding node.
    pub fn insert_definition(&mut self, node: NodeId, def: DefId, kind: DefKind) {
        self.bindings.insert(node, def);
        self.definitions.insert(def, kind);
    }

    /// Returns the [`DefId`] assigned to a definition node, if any.
    #[must_use]
    pub fn definition_of(&self, node: NodeId) -> Option<DefId> {
        self.bindings.get(&node).copied()
    }

    /// Returns the [`DefKind`] associated with a definition id, if any.
    #[must_use]
    pub fn kind_of(&self, def: DefId) -> Option<DefKind> {
        self.definitions.get(&def).copied()
    }

    /// Returns every `(NodeId, Resolution)` pair in a deterministic order
    /// (sorted by node id). Useful for snapshot comparisons.
    #[must_use]
    pub fn sorted_entries(&self) -> Vec<(NodeId, Resolution)> {
        let mut pairs: Vec<(NodeId, Resolution)> =
            self.entries.iter().map(|(k, v)| (*k, *v)).collect();
        pairs.sort_by_key(|(node, _)| node.as_u32());
        pairs
    }
}
