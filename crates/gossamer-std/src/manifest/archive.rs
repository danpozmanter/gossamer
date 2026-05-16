#![allow(
    unused_imports,
    dead_code,
    unreachable_pub,
    missing_docs,
    clippy::wildcard_imports,
    clippy::too_many_lines,
    clippy::too_many_arguments,
    clippy::similar_names,
    clippy::many_single_char_names,
    clippy::items_after_statements,
    clippy::cast_lossless,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::doc_markdown,
    clippy::option_if_let_else,
    clippy::match_same_arms,
    clippy::if_not_else,
    clippy::single_match_else,
    clippy::needless_pass_by_value,
    clippy::manual_let_else,
    clippy::redundant_else,
    clippy::collapsible_if,
    clippy::collapsible_else_if,
    clippy::map_unwrap_or,
    clippy::struct_excessive_bools,
    clippy::module_name_repetitions,
    clippy::unnecessary_wraps,
    clippy::large_enum_variant,
    clippy::if_same_then_else,
    clippy::single_match,
    clippy::useless_conversion,
    clippy::needless_borrows_for_generic_args,
    clippy::let_and_return,
    clippy::needless_collect,
    clippy::elidable_lifetime_names,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::must_use_candidate,
    clippy::missing_const_for_fn,
    clippy::needless_range_loop,
    clippy::ptr_arg,
    clippy::ptr_as_ptr,
    clippy::redundant_closure,
    clippy::redundant_closure_for_method_calls,
    clippy::semicolon_if_nothing_returned,
    clippy::single_call_fn,
    clippy::unused_self,
    clippy::range_plus_one,
    clippy::missing_safety_doc,
    clippy::not_unsafe_ptr_arg_deref,
    clippy::cast_ptr_alignment,
    clippy::manual_assert,
    clippy::manual_string_new,
    clippy::match_bool,
    clippy::nonminimal_bool,
    clippy::redundant_pattern_matching,
    clippy::useless_let_if_seq
)]
//! Static manifest of every registered stdlib module.
//! Each stdlib milestone extends this table with
//! the modules it adds. Entries are listed in phase-introduction order
//! so a `gos doc` walk renders modules in the same sequence as the
//! implementation plan.

#![forbid(unsafe_code)]
use crate::registry::{StdItem, StdItemKind, StdModule};

use super::*;

pub const ARCHIVE_ZIP: StdModule = StdModule {
    path: "std::archive::zip",
    summary: "ZIP archive reader and writer.",
    items: &[
        StdItem {
            name: "ZipEntry",
            kind: StdItemKind::Type,
            doc: "name + decompressed data + is_dir flag.",
        },
        StdItem {
            name: "read",
            kind: StdItemKind::Function,
            doc: "Reads all file entries from a zip stored in `data`.",
        },
        StdItem {
            name: "write",
            kind: StdItemKind::Function,
            doc: "Builds an in-memory zip from (name, data) pairs.",
        },
    ],
};

pub const ARCHIVE_TAR: StdModule = StdModule {
    path: "std::archive::tar",
    summary: "Unix tar reader and writer (USTAR / PAX-aware decode).",
    items: &[
        StdItem {
            name: "TarEntry",
            kind: StdItemKind::Type,
            doc: "name + data + size + mode.",
        },
        StdItem {
            name: "read",
            kind: StdItemKind::Function,
            doc: "Reads all entries from a tar archive.",
        },
        StdItem {
            name: "write",
            kind: StdItemKind::Function,
            doc: "Builds a tar archive from (name, data) pairs.",
        },
    ],
};
