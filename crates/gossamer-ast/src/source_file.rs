//! Top-level source file, `use` declarations, and project/module targets.

#![forbid(unsafe_code)]

use std::fmt;

use gossamer_lex::{FileId, Span};

use crate::common::Ident;
use crate::items::{Attrs, Item};
use crate::node_id::NodeId;
use crate::printer::Printer;

/// A parsed `.gos` source file.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SourceFile {
    /// File this source file was parsed from.
    pub file: FileId,
    /// File-level inner attributes (`#![...]`).
    pub attrs: Attrs,
    /// `use` declarations in source order.
    pub uses: Vec<UseDecl>,
    /// Items in source order.
    pub items: Vec<Item>,
}

impl SourceFile {
    /// Constructs a new source file with the given contents.
    #[must_use]
    pub fn new(file: FileId, uses: Vec<UseDecl>, items: Vec<Item>) -> Self {
        Self {
            file,
            attrs: Attrs::default(),
            uses,
            items,
        }
    }
}

impl PartialEq for SourceFile {
    fn eq(&self, other: &Self) -> bool {
        self.attrs == other.attrs && self.uses == other.uses && self.items == other.items
    }
}

impl fmt::Display for SourceFile {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut printer = Printer::new();
        printer.print_source_file(self);
        out.write_str(&printer.finish())
    }
}

/// A single `use` declaration.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct UseDecl {
    /// Unique id within the enclosing source file.
    pub id: NodeId,
    /// Source range covered by this declaration.
    pub span: Span,
    /// What is being imported.
    pub target: UseTarget,
    /// Optional `as name` renaming of the imported target.
    pub alias: Option<Ident>,
    /// Optional `{ item1, item2 as x, ... }` brace-list after the target.
    pub list: Option<Vec<UseListEntry>>,
}

impl UseDecl {
    /// Constructs a simple `use target` declaration with no alias and no brace list.
    #[must_use]
    pub fn simple(id: NodeId, span: Span, target: UseTarget) -> Self {
        Self {
            id,
            span,
            target,
            alias: None,
            list: None,
        }
    }
}

impl PartialEq for UseDecl {
    fn eq(&self, other: &Self) -> bool {
        self.target == other.target && self.alias == other.alias && self.list == other.list
    }
}

/// What a `use` declaration points at.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum UseTarget {
    /// A bare module path within the current project.
    Module(ModulePath),
    /// A string-quoted project identifier, optionally followed by `::module_path`.
    Project {
        /// Project identifier as written in the string literal.
        id: String,
        /// Optional module path inside that project.
        module: Option<ModulePath>,
    },
}

/// A `::`-separated module path `a::b::c`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ModulePath {
    /// Segments in order.
    pub segments: Vec<Ident>,
}

impl ModulePath {
    /// Constructs a module path from an iterator of segment names.
    #[must_use]
    pub fn from_names<I, S>(segments: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            segments: segments.into_iter().map(Ident::new).collect(),
        }
    }
}

/// One entry in a `use target::{ ... }` brace list.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct UseListEntry {
    /// Name being imported.
    pub name: Ident,
    /// Optional `as rename`.
    pub alias: Option<Ident>,
}

impl UseListEntry {
    /// Constructs an entry with no rename.
    #[must_use]
    pub fn simple(name: impl Into<String>) -> Self {
        Self {
            name: Ident::new(name),
            alias: None,
        }
    }

    /// Constructs an entry with `as rename`.
    #[must_use]
    pub fn aliased(name: impl Into<String>, alias: impl Into<String>) -> Self {
        Self {
            name: Ident::new(name),
            alias: Some(Ident::new(alias)),
        }
    }
}
