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

pub const SORT: StdModule = StdModule {
    path: "std::sort",
    summary: "Slice sorting and binary search.",
    items: &[
        StdItem {
            name: "sort",
            kind: StdItemKind::Function,
            doc: "Ascending unstable sort.",
        },
        StdItem {
            name: "sort_stable",
            kind: StdItemKind::Function,
            doc: "Ascending stable sort.",
        },
        StdItem {
            name: "binary_search",
            kind: StdItemKind::Function,
            doc: "Binary search on a sorted slice.",
        },
    ],
};

pub const MATH_RAND: StdModule = StdModule {
    path: "std::math::rand",
    summary: "Deterministic pseudo-random number generation.",
    items: &[StdItem {
        name: "Rng",
        kind: StdItemKind::Type,
        doc: "SplitMix64-based RNG.",
    }],
};

pub const MATH: StdModule = StdModule {
    path: "std::math",
    summary: "Mathematical constants and f64 functions (Go's math package shape).",
    items: &[
        StdItem {
            name: "PI",
            kind: StdItemKind::Const,
            doc: "Archimedes' constant π.",
        },
        StdItem {
            name: "E",
            kind: StdItemKind::Const,
            doc: "Euler's number e.",
        },
        StdItem {
            name: "SQRT_2",
            kind: StdItemKind::Const,
            doc: "√2.",
        },
        StdItem {
            name: "LN_2",
            kind: StdItemKind::Const,
            doc: "Natural log of 2.",
        },
        StdItem {
            name: "LN_10",
            kind: StdItemKind::Const,
            doc: "Natural log of 10.",
        },
        StdItem {
            name: "PHI",
            kind: StdItemKind::Const,
            doc: "Golden ratio φ.",
        },
        StdItem {
            name: "INF",
            kind: StdItemKind::Const,
            doc: "Positive infinity.",
        },
        StdItem {
            name: "abs",
            kind: StdItemKind::Function,
            doc: "Absolute value of x.",
        },
        StdItem {
            name: "sqrt",
            kind: StdItemKind::Function,
            doc: "Square root.",
        },
        StdItem {
            name: "cbrt",
            kind: StdItemKind::Function,
            doc: "Cube root.",
        },
        StdItem {
            name: "floor",
            kind: StdItemKind::Function,
            doc: "Largest integer ≤ x.",
        },
        StdItem {
            name: "ceil",
            kind: StdItemKind::Function,
            doc: "Smallest integer ≥ x.",
        },
        StdItem {
            name: "round",
            kind: StdItemKind::Function,
            doc: "Nearest integer, half away from zero.",
        },
        StdItem {
            name: "trunc",
            kind: StdItemKind::Function,
            doc: "Integer part of x.",
        },
        StdItem {
            name: "sin",
            kind: StdItemKind::Function,
            doc: "Sine (radians).",
        },
        StdItem {
            name: "cos",
            kind: StdItemKind::Function,
            doc: "Cosine (radians).",
        },
        StdItem {
            name: "tan",
            kind: StdItemKind::Function,
            doc: "Tangent (radians).",
        },
        StdItem {
            name: "asin",
            kind: StdItemKind::Function,
            doc: "Arcsine (radians).",
        },
        StdItem {
            name: "acos",
            kind: StdItemKind::Function,
            doc: "Arccosine (radians).",
        },
        StdItem {
            name: "atan",
            kind: StdItemKind::Function,
            doc: "Arctangent (radians).",
        },
        StdItem {
            name: "atan2",
            kind: StdItemKind::Function,
            doc: "Four-quadrant arctangent of y/x.",
        },
        StdItem {
            name: "exp",
            kind: StdItemKind::Function,
            doc: "e^x.",
        },
        StdItem {
            name: "exp2",
            kind: StdItemKind::Function,
            doc: "2^x.",
        },
        StdItem {
            name: "ln",
            kind: StdItemKind::Function,
            doc: "Natural logarithm.",
        },
        StdItem {
            name: "log2",
            kind: StdItemKind::Function,
            doc: "Base-2 logarithm.",
        },
        StdItem {
            name: "log10",
            kind: StdItemKind::Function,
            doc: "Base-10 logarithm.",
        },
        StdItem {
            name: "log",
            kind: StdItemKind::Function,
            doc: "Logarithm with given base.",
        },
        StdItem {
            name: "pow",
            kind: StdItemKind::Function,
            doc: "x raised to the power y.",
        },
        StdItem {
            name: "hypot",
            kind: StdItemKind::Function,
            doc: "Euclidean distance √(x²+y²).",
        },
        StdItem {
            name: "min_f64",
            kind: StdItemKind::Function,
            doc: "Lesser of two f64 values.",
        },
        StdItem {
            name: "max_f64",
            kind: StdItemKind::Function,
            doc: "Greater of two f64 values.",
        },
        StdItem {
            name: "min_i64",
            kind: StdItemKind::Function,
            doc: "Lesser of two i64 values.",
        },
        StdItem {
            name: "max_i64",
            kind: StdItemKind::Function,
            doc: "Greater of two i64 values.",
        },
        StdItem {
            name: "abs_i64",
            kind: StdItemKind::Function,
            doc: "Absolute value of an i64.",
        },
        StdItem {
            name: "fmod",
            kind: StdItemKind::Function,
            doc: "Floating-point remainder x%y.",
        },
        StdItem {
            name: "is_nan",
            kind: StdItemKind::Function,
            doc: "Reports whether x is NaN.",
        },
        StdItem {
            name: "is_inf",
            kind: StdItemKind::Function,
            doc: "Reports whether x is infinite.",
        },
        StdItem {
            name: "nan",
            kind: StdItemKind::Function,
            doc: "Returns the IEEE 754 NaN value.",
        },
        StdItem {
            name: "inf",
            kind: StdItemKind::Function,
            doc: "Returns ±infinity based on sign.",
        },
        StdItem {
            name: "copysign",
            kind: StdItemKind::Function,
            doc: "Magnitude of x with sign of y.",
        },
        StdItem {
            name: "dim",
            kind: StdItemKind::Function,
            doc: "max(x-y, 0) — Go's math.Dim.",
        },
    ],
};

pub const MATH_BITS: StdModule = StdModule {
    path: "std::math::bits",
    summary: "Integer bit-manipulation operations (Go's math/bits shape).",
    items: &[
        StdItem {
            name: "count_ones",
            kind: StdItemKind::Function,
            doc: "Number of set bits (popcount).",
        },
        StdItem {
            name: "count_zeros",
            kind: StdItemKind::Function,
            doc: "Number of clear bits.",
        },
        StdItem {
            name: "leading_zeros",
            kind: StdItemKind::Function,
            doc: "Leading zero bit count.",
        },
        StdItem {
            name: "trailing_zeros",
            kind: StdItemKind::Function,
            doc: "Trailing zero bit count.",
        },
        StdItem {
            name: "rotate_left",
            kind: StdItemKind::Function,
            doc: "Rotates x left by n bits.",
        },
        StdItem {
            name: "rotate_right",
            kind: StdItemKind::Function,
            doc: "Rotates x right by n bits.",
        },
        StdItem {
            name: "reverse_bits",
            kind: StdItemKind::Function,
            doc: "Reverses bit order of x.",
        },
        StdItem {
            name: "reverse_bytes",
            kind: StdItemKind::Function,
            doc: "Reverses byte order of x.",
        },
        StdItem {
            name: "len",
            kind: StdItemKind::Function,
            doc: "Minimum bits required to represent x.",
        },
        StdItem {
            name: "add",
            kind: StdItemKind::Function,
            doc: "x + y + carry; returns (sum, carry_out).",
        },
        StdItem {
            name: "sub",
            kind: StdItemKind::Function,
            doc: "x - y - borrow; returns (diff, borrow_out).",
        },
        StdItem {
            name: "mul",
            kind: StdItemKind::Function,
            doc: "Full 128-bit product; returns (hi, lo).",
        },
        StdItem {
            name: "div",
            kind: StdItemKind::Function,
            doc: "128-bit dividend / 64-bit divisor; returns (quotient, remainder).",
        },
    ],
};

pub const ITER: StdModule = StdModule {
    path: "std::iter",
    summary: "Sequence adapters over Vec<T>: map, filter, fold, zip, enumerate, chain, etc.",
    items: &[
        StdItem {
            name: "count",
            kind: StdItemKind::Function,
            doc: "Number of elements.",
        },
        StdItem {
            name: "take",
            kind: StdItemKind::Function,
            doc: "First n elements.",
        },
        StdItem {
            name: "skip",
            kind: StdItemKind::Function,
            doc: "All elements after the first n.",
        },
        StdItem {
            name: "zip",
            kind: StdItemKind::Function,
            doc: "Pairs elements from two sequences.",
        },
        StdItem {
            name: "enumerate",
            kind: StdItemKind::Function,
            doc: "Pairs each element with its index.",
        },
        StdItem {
            name: "chain",
            kind: StdItemKind::Function,
            doc: "Concatenates two sequences.",
        },
        StdItem {
            name: "flatten",
            kind: StdItemKind::Function,
            doc: "Flattens a Vec<Vec<T>> into Vec<T>.",
        },
        StdItem {
            name: "reversed",
            kind: StdItemKind::Function,
            doc: "Returns a reversed copy.",
        },
        StdItem {
            name: "dedup",
            kind: StdItemKind::Function,
            doc: "Removes consecutive duplicate elements.",
        },
        StdItem {
            name: "map",
            kind: StdItemKind::Function,
            doc: "Applies f to each element, returning a new Vec.",
        },
        StdItem {
            name: "filter",
            kind: StdItemKind::Function,
            doc: "Returns elements where f is true.",
        },
        StdItem {
            name: "fold",
            kind: StdItemKind::Function,
            doc: "Reduces a sequence with an accumulator.",
        },
        StdItem {
            name: "flat_map",
            kind: StdItemKind::Function,
            doc: "Maps f and flattens one level.",
        },
        StdItem {
            name: "any",
            kind: StdItemKind::Function,
            doc: "True if any element satisfies f.",
        },
        StdItem {
            name: "all",
            kind: StdItemKind::Function,
            doc: "True if every element satisfies f.",
        },
        StdItem {
            name: "sum",
            kind: StdItemKind::Function,
            doc: "Sum of i64 or f64 elements.",
        },
    ],
};

// ---------------------------------------------------------------------------
// 0.4.0 surface — HTTP/2, websocket, sse, router, middleware, static files,
// proxy, native client, chunked transfer, archives, extended compress,
// XML / base32 / ascii85, FNV, big-int, insecure / cipher crypto.
// ---------------------------------------------------------------------------

pub const MATH_BIG: StdModule = StdModule {
    path: "std::math::big",
    summary: "Arbitrary-precision integers (num-bigint).",
    items: &[
        StdItem {
            name: "Int",
            kind: StdItemKind::Type,
            doc: "Arbitrary-precision signed integer.",
        },
        StdItem {
            name: "Uint",
            kind: StdItemKind::Type,
            doc: "Arbitrary-precision unsigned integer.",
        },
        StdItem {
            name: "factorial",
            kind: StdItemKind::Function,
            doc: "Computes n! as an Int.",
        },
    ],
};

/// `std::option` — data-last combinators that thread through `|>`.
pub const OPTION: StdModule = StdModule {
    path: "std::option",
    summary: "Data-last Option combinators for pipeline chaining: map, filter, default, and_then, etc.",
    items: &[
        StdItem {
            name: "and_then",
            kind: StdItemKind::Function,
            doc: "Chains a fallible step: Some(v) -> f(v), None stays None.",
        },
        StdItem {
            name: "default",
            kind: StdItemKind::Function,
            doc: "Unwraps with a fallback value for None.",
        },
        StdItem {
            name: "default_with",
            kind: StdItemKind::Function,
            doc: "Unwraps with a lazily computed fallback for None.",
        },
        StdItem {
            name: "filter",
            kind: StdItemKind::Function,
            doc: "Keeps Some(v) only when the predicate holds.",
        },
        StdItem {
            name: "flatten",
            kind: StdItemKind::Function,
            doc: "Collapses Option<Option<T>> one level.",
        },
        StdItem {
            name: "is_none",
            kind: StdItemKind::Function,
            doc: "True for None.",
        },
        StdItem {
            name: "is_some",
            kind: StdItemKind::Function,
            doc: "True for Some.",
        },
        StdItem {
            name: "iter",
            kind: StdItemKind::Function,
            doc: "Zero-or-one element sequence view.",
        },
        StdItem {
            name: "map",
            kind: StdItemKind::Function,
            doc: "Transforms the Some payload, None stays None.",
        },
        StdItem {
            name: "or",
            kind: StdItemKind::Function,
            doc: "First Some of self and the alternative.",
        },
        StdItem {
            name: "or_else",
            kind: StdItemKind::Function,
            doc: "First Some of self and a lazily built alternative.",
        },
        StdItem {
            name: "zip",
            kind: StdItemKind::Function,
            doc: "Pairs two Somes into Some((a, b)).",
        },
    ],
};

/// `std::result` — data-last combinators that thread through `|>`.
pub const RESULT: StdModule = StdModule {
    path: "std::result",
    summary: "Data-last Result combinators for pipeline chaining: map, map_err, default_with, etc.",
    items: &[
        StdItem {
            name: "and_then",
            kind: StdItemKind::Function,
            doc: "Chains a fallible step on the Ok payload.",
        },
        StdItem {
            name: "default",
            kind: StdItemKind::Function,
            doc: "Unwraps Ok with a fallback value for Err.",
        },
        StdItem {
            name: "default_with",
            kind: StdItemKind::Function,
            doc: "Consumes the result, handling Err with a callback.",
        },
        StdItem {
            name: "err",
            kind: StdItemKind::Function,
            doc: "Err payload as an Option.",
        },
        StdItem {
            name: "is_err",
            kind: StdItemKind::Function,
            doc: "True for Err.",
        },
        StdItem {
            name: "is_ok",
            kind: StdItemKind::Function,
            doc: "True for Ok.",
        },
        StdItem {
            name: "map",
            kind: StdItemKind::Function,
            doc: "Transforms the Ok payload, Err passes through.",
        },
        StdItem {
            name: "map_err",
            kind: StdItemKind::Function,
            doc: "Transforms the Err payload, Ok passes through.",
        },
        StdItem {
            name: "ok",
            kind: StdItemKind::Function,
            doc: "Ok payload as an Option.",
        },
        StdItem {
            name: "or_else",
            kind: StdItemKind::Function,
            doc: "Recovers from Err with a fallback computation.",
        },
    ],
};
