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

pub const DATABASE_SQL: StdModule = StdModule {
    path: "std::database::sql",
    summary: "Driver-pluggable SQL database access.",
    items: &[
        StdItem {
            name: "Driver",
            kind: StdItemKind::Trait,
            doc: "Database driver — opens connections.",
        },
        StdItem {
            name: "Conn",
            kind: StdItemKind::Type,
            doc: "Open database connection.",
        },
        StdItem {
            name: "Tx",
            kind: StdItemKind::Type,
            doc: "Active transaction handle.",
        },
        StdItem {
            name: "Stmt",
            kind: StdItemKind::Type,
            doc: "Prepared statement.",
        },
        StdItem {
            name: "Rows",
            kind: StdItemKind::Type,
            doc: "Result-set iterator.",
        },
        StdItem {
            name: "open",
            kind: StdItemKind::Function,
            doc: "Opens a database connection by driver name + URL.",
        },
    ],
};

pub const COLLECTIONS: StdModule = StdModule {
    path: "std::collections",
    summary: "Built-in container types.",
    items: &[
        StdItem {
            name: "Vec",
            kind: StdItemKind::Type,
            doc: "Growable contiguous sequence.",
        },
        StdItem {
            name: "VecDeque",
            kind: StdItemKind::Type,
            doc: "Double-ended queue backed by a ring buffer.",
        },
        StdItem {
            name: "HashMap",
            kind: StdItemKind::Type,
            doc: "Hash map backed by the swiss-table layout.",
        },
        StdItem {
            name: "BTreeMap",
            kind: StdItemKind::Type,
            doc: "Ordered map.",
        },
        StdItem {
            name: "HashSet",
            kind: StdItemKind::Type,
            doc: "Unordered set built on top of `HashMap`.",
        },
        StdItem {
            name: "BTreeSet",
            kind: StdItemKind::Type,
            doc: "Ordered set built on top of `BTreeMap`.",
        },
    ],
};
