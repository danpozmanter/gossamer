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

pub const CONTAINER_QUEUE: StdModule = StdModule {
    path: "std::collections::queue",
    summary: "FIFO queue over Vec<i64>. Re-bind shape: `let q = queue::push(q, v)`.",
    items: &[
        StdItem {
            name: "push",
            kind: StdItemKind::Function,
            doc: "Append an i64 to the back; returns the new queue.",
        },
        StdItem {
            name: "pop",
            kind: StdItemKind::Function,
            doc: "Drop the front element; returns the new queue.",
        },
        StdItem {
            name: "peek",
            kind: StdItemKind::Function,
            doc: "Front element, or 0 if empty.",
        },
        StdItem {
            name: "len",
            kind: StdItemKind::Function,
            doc: "Element count.",
        },
    ],
};

pub const CONTAINER_STACK: StdModule = StdModule {
    path: "std::collections::stack",
    summary: "LIFO stack over Vec<i64>. Re-bind shape: `let s = stack::push(s, v)`.",
    items: &[
        StdItem {
            name: "push",
            kind: StdItemKind::Function,
            doc: "Push an i64 onto the top; returns the new stack.",
        },
        StdItem {
            name: "pop",
            kind: StdItemKind::Function,
            doc: "Drop the top; returns the new stack.",
        },
        StdItem {
            name: "peek",
            kind: StdItemKind::Function,
            doc: "Top element, or 0 if empty.",
        },
        StdItem {
            name: "len",
            kind: StdItemKind::Function,
            doc: "Element count.",
        },
    ],
};

pub const CONTAINER_DEQUE: StdModule = StdModule {
    path: "std::collections::deque",
    summary: "Double-ended queue over Vec<i64>. Re-bind shape on every mutator.",
    items: &[
        StdItem {
            name: "push_back",
            kind: StdItemKind::Function,
            doc: "Append to the back.",
        },
        StdItem {
            name: "push_front",
            kind: StdItemKind::Function,
            doc: "Prepend to the front.",
        },
        StdItem {
            name: "pop_back",
            kind: StdItemKind::Function,
            doc: "Drop the back.",
        },
        StdItem {
            name: "pop_front",
            kind: StdItemKind::Function,
            doc: "Drop the front.",
        },
        StdItem {
            name: "peek_front",
            kind: StdItemKind::Function,
            doc: "Front element, or 0 if empty.",
        },
        StdItem {
            name: "peek_back",
            kind: StdItemKind::Function,
            doc: "Back element, or 0 if empty.",
        },
        StdItem {
            name: "len",
            kind: StdItemKind::Function,
            doc: "Element count.",
        },
    ],
};

pub const CONTAINER_ORDERED_SET: StdModule = StdModule {
    path: "std::collections::ordered_set",
    summary: "Sorted set of i64 with binary-search lookups. Re-bind shape on every mutator.",
    items: &[
        StdItem {
            name: "insert",
            kind: StdItemKind::Function,
            doc: "Insert (sorted, no duplicates).",
        },
        StdItem {
            name: "remove",
            kind: StdItemKind::Function,
            doc: "Remove a value.",
        },
        StdItem {
            name: "contains",
            kind: StdItemKind::Function,
            doc: "Membership test.",
        },
        StdItem {
            name: "len",
            kind: StdItemKind::Function,
            doc: "Element count.",
        },
    ],
};

pub const CONTAINER_ORDERED_MAP: StdModule = StdModule {
    path: "std::collections::ordered_map",
    summary: "Sorted key/value map (i64 -> i64) backed by a flat pair Vec. Re-bind on every mutator.",
    items: &[
        StdItem {
            name: "insert",
            kind: StdItemKind::Function,
            doc: "Set key => value.",
        },
        StdItem {
            name: "remove",
            kind: StdItemKind::Function,
            doc: "Remove a key.",
        },
        StdItem {
            name: "get",
            kind: StdItemKind::Function,
            doc: "Lookup; returns 0 if absent.",
        },
        StdItem {
            name: "contains_key",
            kind: StdItemKind::Function,
            doc: "Key-membership test.",
        },
        StdItem {
            name: "len",
            kind: StdItemKind::Function,
            doc: "Pair count.",
        },
    ],
};

pub const CONTAINER_ORDERED_VEC: StdModule = StdModule {
    path: "std::collections::ordered_vec",
    summary: "Sorted-on-insert Vec<i64> with binary-search lookups.",
    items: &[
        StdItem {
            name: "insert",
            kind: StdItemKind::Function,
            doc: "Insert at the unique sorted position.",
        },
        StdItem {
            name: "remove_at",
            kind: StdItemKind::Function,
            doc: "Remove the element at index `i`.",
        },
        StdItem {
            name: "contains",
            kind: StdItemKind::Function,
            doc: "Return true iff `value` is present.",
        },
        StdItem {
            name: "index_of",
            kind: StdItemKind::Function,
            doc: "Return the index of `value`, or -1.",
        },
        StdItem {
            name: "peek_min",
            kind: StdItemKind::Function,
            doc: "Smallest element, or 0.",
        },
        StdItem {
            name: "peek_max",
            kind: StdItemKind::Function,
            doc: "Largest element, or 0.",
        },
        StdItem {
            name: "len",
            kind: StdItemKind::Function,
            doc: "Element count.",
        },
    ],
};

pub const CONTAINER_HEAP: StdModule = StdModule {
    path: "std::collections::heap",
    summary: "Binary min-heap (priority queue) over Vec<i64>. Re-bind shape: `let h = heap::push(h, v)`.",
    items: &[
        StdItem {
            name: "push",
            kind: StdItemKind::Function,
            doc: "Push an i64 onto the min-heap; returns the new heap.",
        },
        StdItem {
            name: "pop",
            kind: StdItemKind::Function,
            doc: "Drop the root from the heap; returns the new heap (use `peek` first to read the value).",
        },
        StdItem {
            name: "peek",
            kind: StdItemKind::Function,
            doc: "Smallest element of the heap, or 0 if empty.",
        },
        StdItem {
            name: "len",
            kind: StdItemKind::Function,
            doc: "Element count.",
        },
    ],
};

pub const SORT: StdModule = StdModule {
    path: "std::sort",
    summary: "Explicit stable ordering and sorted-sequence search, the deliberate counterpart to Vec's unstable inherent `sort`.",
    items: &[
        StdItem {
            name: "sort_stable",
            kind: StdItemKind::Function,
            doc: "`sort_stable(xs: Vec<T>) -> Vec<T>` - a fresh ascending sequence in which equal elements keep their input order. `xs.sort()` sorts in place and is unstable; this is the spelling that guarantees stability. Example: `let ordered = sort::sort_stable(#[3, 1, 2])`.",
        },
        StdItem {
            name: "binary_search",
            kind: StdItemKind::Function,
            doc: "`binary_search(xs: Vec<T>, target: T) -> Option<i64>` - index of a matching element in an already-ascending sequence, `None` when absent. Example: `sort::binary_search(#[1, 3, 5], 3)` is `Some(1)`.",
        },
        StdItem {
            name: "partition_point",
            kind: StdItemKind::Function,
            doc: "`partition_point(xs: Vec<T>, pivot: T) -> i64` - the count of elements strictly less than `pivot` in an already-ascending sequence, which is also the insertion index that keeps it sorted. Example: `sort::partition_point(#[1, 2, 6, 8], 5)` is `2`.",
        },
    ],
};
