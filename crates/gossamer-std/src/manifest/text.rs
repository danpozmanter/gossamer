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

pub const UTF8: StdModule = StdModule {
    path: "std::utf8",
    summary: "UTF-8 validation and scalar decoding.",
    items: &[
        StdItem {
            name: "is_valid",
            kind: StdItemKind::Function,
            doc: "Validates a byte slice as UTF-8.",
        },
        StdItem {
            name: "rune_count",
            kind: StdItemKind::Function,
            doc: "Counts Unicode scalar values.",
        },
    ],
};

pub const REGEX: StdModule = StdModule {
    path: "std::regex",
    summary: "Compiled regular expressions (Rust `regex` crate syntax; no backreferences or look-around).",
    items: &[
        StdItem {
            name: "Pattern",
            kind: StdItemKind::Type,
            doc: "Compiled pattern handle returned by `compile`.",
        },
        StdItem {
            name: "compile",
            kind: StdItemKind::Function,
            doc: "Parses a pattern into a reusable `Pattern` or returns an `Err`.",
        },
        StdItem {
            name: "is_match",
            kind: StdItemKind::Function,
            doc: "Returns whether the pattern matches anywhere in the text.",
        },
        StdItem {
            name: "find",
            kind: StdItemKind::Function,
            doc: "Returns the first match as `(start, end, text)`, or `None`.",
        },
        StdItem {
            name: "find_all",
            kind: StdItemKind::Function,
            doc: "Returns every non-overlapping match as `(start, end, text)`.",
        },
        StdItem {
            name: "captures",
            kind: StdItemKind::Function,
            doc: "Returns capture groups for the first match; index 0 is the full match.",
        },
        StdItem {
            name: "captures_all",
            kind: StdItemKind::Function,
            doc: "Returns capture groups for every match in the text.",
        },
        StdItem {
            name: "replace",
            kind: StdItemKind::Function,
            doc: "Replaces the first match with the given replacement (supports `$N`).",
        },
        StdItem {
            name: "replace_all",
            kind: StdItemKind::Function,
            doc: "Replaces every non-overlapping match.",
        },
        StdItem {
            name: "split",
            kind: StdItemKind::Function,
            doc: "Splits the text on every pattern match.",
        },
    ],
};

pub const FMT: StdModule = StdModule {
    path: "std::fmt",
    summary: "Formatted printing and string interpolation.",
    items: &[
        StdItem {
            name: "Display",
            kind: StdItemKind::Trait,
            doc: "Trait for human-readable string conversion.",
        },
        StdItem {
            name: "Debug",
            kind: StdItemKind::Trait,
            doc: "Trait for debugging-oriented string conversion.",
        },
        StdItem {
            name: "println",
            kind: StdItemKind::Macro,
            doc: "Prints to stdout followed by a newline.",
        },
        StdItem {
            name: "print",
            kind: StdItemKind::Macro,
            doc: "Prints to stdout without a trailing newline.",
        },
        StdItem {
            name: "eprintln",
            kind: StdItemKind::Macro,
            doc: "Prints to stderr followed by a newline.",
        },
        StdItem {
            name: "eprint",
            kind: StdItemKind::Macro,
            doc: "Prints to stderr without a trailing newline.",
        },
        StdItem {
            name: "format",
            kind: StdItemKind::Macro,
            doc: "Formats arguments into an owned `String`.",
        },
        StdItem {
            name: "write",
            kind: StdItemKind::Macro,
            doc: "Writes formatted output into a `Writer`.",
        },
        StdItem {
            name: "writeln",
            kind: StdItemKind::Macro,
            doc: "Writes formatted output into a `Writer` followed by a newline.",
        },
    ],
};

pub const STRINGS: StdModule = StdModule {
    path: "std::strings",
    summary: "Polished `String` operations.",
    items: &[
        StdItem {
            name: "split",
            kind: StdItemKind::Function,
            doc: "Splits a string by a delimiter.",
        },
        StdItem {
            name: "splitn",
            kind: StdItemKind::Function,
            doc: "Splits a string into at most `n` parts.",
        },
        StdItem {
            name: "trim",
            kind: StdItemKind::Function,
            doc: "Removes leading and trailing whitespace.",
        },
        StdItem {
            name: "contains",
            kind: StdItemKind::Function,
            doc: "Returns whether the string contains a substring.",
        },
        StdItem {
            name: "find",
            kind: StdItemKind::Function,
            doc: "Returns the byte position of the first match.",
        },
        StdItem {
            name: "replace",
            kind: StdItemKind::Function,
            doc: "Replaces every occurrence of `from` with `to`.",
        },
        StdItem {
            name: "to_lower",
            kind: StdItemKind::Function,
            doc: "Lowercases every character.",
        },
        StdItem {
            name: "to_upper",
            kind: StdItemKind::Function,
            doc: "Uppercases every character.",
        },
        StdItem {
            name: "to_lowercase",
            kind: StdItemKind::Function,
            doc: "Alias for to_lower (Rust-style name).",
        },
        StdItem {
            name: "to_uppercase",
            kind: StdItemKind::Function,
            doc: "Alias for to_upper (Rust-style name).",
        },
        StdItem {
            name: "starts_with",
            kind: StdItemKind::Function,
            doc: "Returns whether the string starts with the given prefix.",
        },
        StdItem {
            name: "ends_with",
            kind: StdItemKind::Function,
            doc: "Returns whether the string ends with the given suffix.",
        },
        StdItem {
            name: "split_once",
            kind: StdItemKind::Function,
            doc: "Splits on the first occurrence of `sep`; returns Option<(String, String)>.",
        },
        StdItem {
            name: "rsplit_once",
            kind: StdItemKind::Function,
            doc: "Splits on the last occurrence of `sep`; returns Option<(String, String)>.",
        },
        StdItem {
            name: "count",
            kind: StdItemKind::Function,
            doc: "Counts non-overlapping occurrences of `needle`.",
        },
        StdItem {
            name: "strip_chars",
            kind: StdItemKind::Function,
            doc: "Trims any character in `cutset` from both ends.",
        },
        StdItem {
            name: "lstrip_chars",
            kind: StdItemKind::Function,
            doc: "Trims any character in `cutset` from the left end.",
        },
        StdItem {
            name: "rstrip_chars",
            kind: StdItemKind::Function,
            doc: "Trims any character in `cutset` from the right end.",
        },
        StdItem {
            name: "zfill",
            kind: StdItemKind::Function,
            doc: "Pads with `'0'` on the left until at least `width` wide.",
        },
        StdItem {
            name: "center",
            kind: StdItemKind::Function,
            doc: "Symmetric pad to `width` using the given pad character.",
        },
        StdItem {
            name: "slice",
            kind: StdItemKind::Function,
            doc: "Safe byte-range slice returning Result<String, errors::Error>.",
        },
    ],
};

pub const STRCONV: StdModule = StdModule {
    path: "std::strconv",
    summary: "Conversions between strings and primitive numeric types.",
    items: &[
        StdItem {
            name: "parse_i64",
            kind: StdItemKind::Function,
            doc: "Parses a decimal `i64`.",
        },
        StdItem {
            name: "parse_u64",
            kind: StdItemKind::Function,
            doc: "Parses a decimal `u64`.",
        },
        StdItem {
            name: "parse_f64",
            kind: StdItemKind::Function,
            doc: "Parses a decimal `f64`.",
        },
        StdItem {
            name: "parse_bool",
            kind: StdItemKind::Function,
            doc: "Parses `\"true\"` / `\"false\"` into a bool.",
        },
        StdItem {
            name: "format_i64",
            kind: StdItemKind::Function,
            doc: "Renders an `i64` as a decimal string.",
        },
        StdItem {
            name: "format_f64",
            kind: StdItemKind::Function,
            doc: "Renders an `f64` as a decimal string.",
        },
        // Shorter aliases — SKILL.md and Go's `strconv` use these.
        StdItem {
            name: "parse_int",
            kind: StdItemKind::Function,
            doc: "Alias for parse_i64.",
        },
        StdItem {
            name: "atoi",
            kind: StdItemKind::Function,
            doc: "Alias for parse_i64 (Go-style spelling).",
        },
        StdItem {
            name: "parse_float",
            kind: StdItemKind::Function,
            doc: "Alias for parse_f64.",
        },
        StdItem {
            name: "format_int",
            kind: StdItemKind::Function,
            doc: "Alias for format_i64.",
        },
        StdItem {
            name: "itoa",
            kind: StdItemKind::Function,
            doc: "Alias for format_i64 (Go-style spelling).",
        },
        StdItem {
            name: "format_float",
            kind: StdItemKind::Function,
            doc: "Alias for format_f64.",
        },
    ],
};

pub const UNICODE: StdModule = StdModule {
    path: "std::unicode",
    summary: "Unicode general-category predicates, casing, normalization, and segmentation.",
    items: &[
        StdItem {
            name: "is_letter",
            kind: StdItemKind::Function,
            doc: "True if r is in general-category group L.",
        },
        StdItem {
            name: "is_digit",
            kind: StdItemKind::Function,
            doc: "True if r is a decimal digit (category Nd).",
        },
        StdItem {
            name: "is_number",
            kind: StdItemKind::Function,
            doc: "True if r is any numeric (Nd|Nl|No).",
        },
        StdItem {
            name: "is_space",
            kind: StdItemKind::Function,
            doc: "True if r is whitespace (Z* plus HT/LF/VT/FF/CR/NEL).",
        },
        StdItem {
            name: "is_upper",
            kind: StdItemKind::Function,
            doc: "True if r is category Lu.",
        },
        StdItem {
            name: "is_lower",
            kind: StdItemKind::Function,
            doc: "True if r is category Ll.",
        },
        StdItem {
            name: "is_title",
            kind: StdItemKind::Function,
            doc: "True if r is category Lt.",
        },
        StdItem {
            name: "is_punct",
            kind: StdItemKind::Function,
            doc: "True if r is in general-category group P.",
        },
        StdItem {
            name: "is_symbol",
            kind: StdItemKind::Function,
            doc: "True if r is in general-category group S.",
        },
        StdItem {
            name: "is_mark",
            kind: StdItemKind::Function,
            doc: "True if r is in general-category group M.",
        },
        StdItem {
            name: "is_print",
            kind: StdItemKind::Function,
            doc: "True if r is printable (not Cc/Cf/Cs/Co/Cn).",
        },
        StdItem {
            name: "is_graphic",
            kind: StdItemKind::Function,
            doc: "True if r is graphic (printable and not whitespace).",
        },
        StdItem {
            name: "is_control",
            kind: StdItemKind::Function,
            doc: "True if r is category Cc.",
        },
        StdItem {
            name: "is_assigned",
            kind: StdItemKind::Function,
            doc: "True if r is an assigned code point (not Cn).",
        },
        StdItem {
            name: "to_upper",
            kind: StdItemKind::Function,
            doc: "Simple uppercase mapping for one rune.",
        },
        StdItem {
            name: "to_lower",
            kind: StdItemKind::Function,
            doc: "Simple lowercase mapping for one rune.",
        },
        StdItem {
            name: "to_title",
            kind: StdItemKind::Function,
            doc: "Simple titlecase mapping for one rune.",
        },
        StdItem {
            name: "simple_fold",
            kind: StdItemKind::Function,
            doc: "Next rune in Unicode case-folding cycle.",
        },
        StdItem {
            name: "combining_class",
            kind: StdItemKind::Function,
            doc: "Canonical combining class (0-254) for r.",
        },
        StdItem {
            name: "to_upper_str",
            kind: StdItemKind::Function,
            doc: "Full uppercase mapping for a string (ss -> SS).",
        },
        StdItem {
            name: "to_lower_str",
            kind: StdItemKind::Function,
            doc: "Full lowercase mapping for a string.",
        },
        StdItem {
            name: "fold_case",
            kind: StdItemKind::Function,
            doc: "Simple case-folded comparison form for a string.",
        },
        StdItem {
            name: "nfc",
            kind: StdItemKind::Function,
            doc: "Normalize a string to NFC (canonical composition).",
        },
        StdItem {
            name: "nfd",
            kind: StdItemKind::Function,
            doc: "Normalize a string to NFD (canonical decomposition).",
        },
        StdItem {
            name: "nfkc",
            kind: StdItemKind::Function,
            doc: "Normalize a string to NFKC (compat composition).",
        },
        StdItem {
            name: "nfkd",
            kind: StdItemKind::Function,
            doc: "Normalize a string to NFKD (compat decomposition).",
        },
        StdItem {
            name: "is_nfc",
            kind: StdItemKind::Function,
            doc: "True if a string is already in NFC.",
        },
        StdItem {
            name: "is_nfd",
            kind: StdItemKind::Function,
            doc: "True if a string is already in NFD.",
        },
        StdItem {
            name: "is_nfkc",
            kind: StdItemKind::Function,
            doc: "True if a string is already in NFKC.",
        },
        StdItem {
            name: "is_nfkd",
            kind: StdItemKind::Function,
            doc: "True if a string is already in NFKD.",
        },
        StdItem {
            name: "graphemes",
            kind: StdItemKind::Function,
            doc: "UAX #29 extended grapheme clusters of a string.",
        },
        StdItem {
            name: "grapheme_count",
            kind: StdItemKind::Function,
            doc: "Number of UAX #29 grapheme clusters in a string.",
        },
        StdItem {
            name: "words",
            kind: StdItemKind::Function,
            doc: "UAX #29 Unicode words in a string (skips punct/whitespace).",
        },
        StdItem {
            name: "word_bounds",
            kind: StdItemKind::Function,
            doc: "UAX #29 word boundaries (includes punct + whitespace runs).",
        },
        StdItem {
            name: "word_count",
            kind: StdItemKind::Function,
            doc: "Number of UAX #29 words in a string.",
        },
        StdItem {
            name: "sentences",
            kind: StdItemKind::Function,
            doc: "UAX #29 Unicode sentences in a string.",
        },
        StdItem {
            name: "sentence_count",
            kind: StdItemKind::Function,
            doc: "Number of UAX #29 sentences in a string.",
        },
    ],
};

pub const UTF16: StdModule = StdModule {
    path: "std::utf16",
    summary: "UTF-16 encoding/decoding and surrogate pair helpers.",
    items: &[
        StdItem {
            name: "is_surrogate",
            kind: StdItemKind::Function,
            doc: "True iff r falls in the surrogate range U+D800..U+DFFF.",
        },
        StdItem {
            name: "rune_len",
            kind: StdItemKind::Function,
            doc: "Number of UTF-16 code units needed to encode r (1 or 2).",
        },
        StdItem {
            name: "encode_rune",
            kind: StdItemKind::Function,
            doc: "Encodes r as 1 or 2 u16 code units.",
        },
        StdItem {
            name: "decode_surrogate_pair",
            kind: StdItemKind::Function,
            doc: "Decodes a high+low surrogate pair to a char.",
        },
        StdItem {
            name: "append_rune",
            kind: StdItemKind::Function,
            doc: "Appends the UTF-16 encoding of r to a Vec<u16>.",
        },
        StdItem {
            name: "encode",
            kind: StdItemKind::Function,
            doc: "Encodes a []char to Vec<u16>.",
        },
        StdItem {
            name: "decode",
            kind: StdItemKind::Function,
            doc: "Decodes a []u16 to Vec<char>, replacing surrogates with U+FFFD.",
        },
        StdItem {
            name: "encode_string",
            kind: StdItemKind::Function,
            doc: "Encodes a String directly to Vec<u16>.",
        },
        StdItem {
            name: "decode_to_string",
            kind: StdItemKind::Function,
            doc: "Decodes a []u16 to String.",
        },
    ],
};
