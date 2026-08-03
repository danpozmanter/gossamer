//! Interactive REPL.
//!
//! Kept in its own module so `main.rs` stays under the 2000-line
//! hard limit defined in `GUIDELINES.md`.

use std::collections::{BTreeMap, HashSet};
use std::io::Write as _;

use anyhow::{Result, anyhow};
use gossamer_parse::builtin_macros::{BUILTIN_MACROS, BuiltinMacro};
use gossamer_std::registry::{StdItem, StdItemKind, StdModule};
use regex::Regex;

use crate::paths::repl_history_path;

const REPL_HELP_TEXT: &str = "\
REPL commands

  %help
    Show this list of REPL commands.
  %info (%i) [pattern] [-d|--details] [-a|--all] [-p N|--page N]
    List language and standard-library matches; use -d for documentation.
  %explain (%e) NAME [-d|--details]
    Inspect a persistent `let` binding; use -d for methods and capability.
  %bindings (%b) [regex] [-a|--all] [-p N|--page N]
    Show persistent `let` bindings.
  %drop NAME
    End a persistent binding's lexical lifetime and remove it.
  %declarations (%d) [regex] [-a|--all] [-p N|--page N]
    Show persistent declarations.
  %history (%h) [regex]
    Search inputs from this and previous sessions.
  %clear-history
    Delete all saved inputs and clear up/down history.
  %reset (%r)
    Clear persistent bindings and declarations.
  %quit (%q)
    Exit the REPL.

Expressions print their value. Declarations and `let` bindings persist.

Listings show 20 entries at a time. Use `--page N` for another page or
`-a`/`--all` for all results; either option may appear before or after a
pattern.

Up/down cycles history.";

const REPL_FALLBACK_COLUMNS: usize = 80;

fn repl_output_width() -> usize {
    crate::style::terminal_width(REPL_FALLBACK_COLUMNS, 24)
}

fn wrap_repl_output(text: &str) -> String {
    let width = repl_output_width();
    text.lines()
        .flat_map(|line| wrap_repl_line(line, width))
        .collect::<Vec<_>>()
        .join("\n")
}

fn wrap_repl_line(line: &str, width: usize) -> Vec<String> {
    if line.chars().count() <= width {
        return vec![line.to_string()];
    }
    let indent_len = line.chars().take_while(|ch| ch.is_whitespace()).count();
    let indent = " ".repeat(indent_len.min(width.saturating_sub(1)));
    let continuation_len = indent_len.min(width.saturating_sub(1));
    let continuation = " ".repeat(continuation_len);
    let mut lines = Vec::new();
    let mut current = indent;
    for word in line.split_whitespace() {
        let separator = usize::from(!current.trim().is_empty());
        if current.chars().count() + separator + word.chars().count() > width
            && !current.trim().is_empty()
        {
            lines.push(std::mem::take(&mut current));
            current.push_str(&continuation);
        }
        if !current.trim().is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

fn print_repl_output(text: &str) {
    let wrapped = wrap_repl_output(text);
    for line in wrapped.lines() {
        println!("{}", style_repl_output_line(line));
    }
}

fn style_repl_output_line(line: &str) -> String {
    let trimmed = line.trim_start();
    if trimmed.is_empty() {
        return String::new();
    }
    if !line.starts_with(char::is_whitespace) {
        return crate::style::repl_meta_heading(line);
    }
    if trimmed.starts_with('%') {
        return crate::style::repl_meta_accent(line);
    }
    crate::style::repl_meta_detail(line)
}

fn print_repl_error(message: &str) {
    eprintln!("{}", crate::style::repl_error(message));
}

struct PreludeBuiltinHelp {
    name: &'static str,
    signature: &'static str,
    doc: &'static str,
}

struct CoreMethodHelp {
    owner: &'static str,
    name: &'static str,
    kind: &'static str,
    signature: &'static str,
    doc: &'static str,
}

#[derive(Clone, Debug)]
struct CoreMethodEntry {
    owner: String,
    name: String,
    kind: &'static str,
    signature: String,
    doc: String,
}

pub(crate) fn core_method_names(owner: &str) -> Vec<&'static str> {
    CORE_METHODS
        .iter()
        .filter(|method| method.owner == owner && method.kind == "method")
        .map(|method| method.name)
        .collect()
}

// These prelude functions are runtime builtins rather than stdlib-manifest
// exports. Every parser-recognized macro is sourced from BUILTIN_MACROS below.
const PRELUDE_BUILTINS: &[PreludeBuiltinHelp] = &[
    PreludeBuiltinHelp {
        name: "assert",
        signature: "assert(condition: bool, message?: String)",
        doc: "Panics when condition is false.",
    },
    PreludeBuiltinHelp {
        name: "assert_eq",
        signature: "assert_eq(left, right, message?: String)",
        doc: "Panics when left and right are not equal.",
    },
];

// Core receiver and associated methods are runtime builtins, not stdlib module
// exports. Keep them visible to REPL discovery so working calls such as
// `"123".parse()` are not hidden from `%help` and `%info`.
const CORE_METHODS: &[CoreMethodHelp] = &[
    CoreMethodHelp {
        owner: "Buffer",
        name: "new",
        kind: "assoc",
        signature: "fn new() -> bytes::Buffer",
        doc: "Creates an empty byte buffer.",
    },
    CoreMethodHelp {
        owner: "Buffer",
        name: "with_capacity",
        kind: "assoc",
        signature: "fn with_capacity(capacity: i64) -> bytes::Buffer",
        doc: "Creates an empty byte buffer with capacity reserved.",
    },
    CoreMethodHelp {
        owner: "Buffer",
        name: "push",
        kind: "method",
        signature: "fn push(&mut self, byte: u8) -> ()",
        doc: "Appends one byte.",
    },
    CoreMethodHelp {
        owner: "Buffer",
        name: "write_str",
        kind: "method",
        signature: "fn write_str(&mut self, text: String) -> ()",
        doc: "Appends a string's UTF-8 bytes.",
    },
    CoreMethodHelp {
        owner: "Buffer",
        name: "clear",
        kind: "method",
        signature: "fn clear(&mut self) -> ()",
        doc: "Clears the buffer in place.",
    },
    CoreMethodHelp {
        owner: "Buffer",
        name: "len",
        kind: "method",
        signature: "fn len(&self) -> i64",
        doc: "Returns the number of buffered bytes.",
    },
    CoreMethodHelp {
        owner: "Buffer",
        name: "is_empty",
        kind: "method",
        signature: "fn is_empty(&self) -> bool",
        doc: "Returns true when the buffer has no bytes.",
    },
    CoreMethodHelp {
        owner: "Buffer",
        name: "to_string",
        kind: "method",
        signature: "fn to_string(&self) -> String",
        doc: "Decodes the buffered bytes with lossy UTF-8 replacement.",
    },
    CoreMethodHelp {
        owner: "Builder",
        name: "new",
        kind: "assoc",
        signature: "fn new() -> bytes::Builder",
        doc: "Creates an empty string builder.",
    },
    CoreMethodHelp {
        owner: "Builder",
        name: "with_capacity",
        kind: "assoc",
        signature: "fn with_capacity(capacity: i64) -> bytes::Builder",
        doc: "Creates an empty string builder with capacity reserved.",
    },
    CoreMethodHelp {
        owner: "Builder",
        name: "write",
        kind: "method",
        signature: "fn write(&mut self, text: String) -> ()",
        doc: "Appends text.",
    },
    CoreMethodHelp {
        owner: "Builder",
        name: "write_char",
        kind: "method",
        signature: "fn write_char(&mut self, ch: char) -> ()",
        doc: "Appends one Unicode scalar.",
    },
    CoreMethodHelp {
        owner: "Builder",
        name: "len",
        kind: "method",
        signature: "fn len(&self) -> i64",
        doc: "Returns the accumulated byte length.",
    },
    CoreMethodHelp {
        owner: "Builder",
        name: "as_str",
        kind: "method",
        signature: "fn as_str(&self) -> String",
        doc: "Returns the accumulated text.",
    },
    CoreMethodHelp {
        owner: "Builder",
        name: "build",
        kind: "method",
        signature: "fn build(&self) -> String",
        doc: "Returns the accumulated text.",
    },
    CoreMethodHelp {
        owner: "String",
        name: "new",
        kind: "assoc",
        signature: "fn new() -> String",
        doc: "Creates an empty owned string.",
    },
    CoreMethodHelp {
        owner: "String",
        name: "with_capacity",
        kind: "assoc",
        signature: "fn with_capacity(capacity: i64) -> String",
        doc: "Creates an empty string with capacity reserved.",
    },
    CoreMethodHelp {
        owner: "String",
        name: "from",
        kind: "assoc",
        signature: "fn from<T: Display>(value: T) -> String",
        doc: "Converts a Display value into a string.",
    },
    CoreMethodHelp {
        owner: "String",
        name: "from_utf8",
        kind: "assoc",
        signature: "fn from_utf8(bytes: Vec<u8>) -> Result<String, errors::Error>",
        doc: "Decodes UTF-8 bytes into a string.",
    },
    CoreMethodHelp {
        owner: "String",
        name: "parse",
        kind: "method",
        signature: "fn parse<T>(self: String) -> Result<T, errors::Error>",
        doc: "Parses the string into the expected result type.",
    },
    CoreMethodHelp {
        owner: "String",
        name: "len",
        kind: "method",
        signature: "fn len(self: String) -> i64",
        doc: "Returns the byte length of the string.",
    },
    CoreMethodHelp {
        owner: "String",
        name: "is_empty",
        kind: "method",
        signature: "fn is_empty(self: String) -> bool",
        doc: "Returns true when the string has zero bytes.",
    },
    CoreMethodHelp {
        owner: "String",
        name: "clear",
        kind: "method",
        signature: "fn clear(self: &mut String) -> ()",
        doc: "Clears the string in place.",
    },
    CoreMethodHelp {
        owner: "String",
        name: "truncate",
        kind: "method",
        signature: "fn truncate(self: &mut String, len: i64) -> ()",
        doc: "Truncates the string at a valid UTF-8 boundary.",
    },
    CoreMethodHelp {
        owner: "String",
        name: "push",
        kind: "method",
        signature: "fn push(self: &mut String, ch: char) -> ()",
        doc: "Appends a Unicode scalar.",
    },
    CoreMethodHelp {
        owner: "String",
        name: "push_char",
        kind: "method",
        signature: "fn push_char(self: &mut String, ch: char) -> ()",
        doc: "Appends a Unicode scalar.",
    },
    CoreMethodHelp {
        owner: "String",
        name: "push_byte",
        kind: "method",
        signature: "fn push_byte(self: &mut String, byte: i64) -> ()",
        doc: "Appends the byte as a Unicode scalar.",
    },
    CoreMethodHelp {
        owner: "String",
        name: "push_str",
        kind: "method",
        signature: "fn push_str(self: &mut String, text: String) -> ()",
        doc: "Appends string contents.",
    },
    CoreMethodHelp {
        owner: "String",
        name: "clone",
        kind: "method",
        signature: "fn clone(self: String) -> String",
        doc: "Returns a copy of the string.",
    },
    CoreMethodHelp {
        owner: "String",
        name: "to_string",
        kind: "method",
        signature: "fn to_string(self: String) -> String",
        doc: "Returns the string unchanged.",
    },
    CoreMethodHelp {
        owner: "String",
        name: "as_bytes",
        kind: "method",
        signature: "fn as_bytes(self: String) -> Vec<u8>",
        doc: "Returns the UTF-8 bytes of the string.",
    },
    CoreMethodHelp {
        owner: "Vec",
        name: "new",
        kind: "assoc",
        signature: "fn new<T>() -> Vec<T>",
        doc: "Creates an empty vector.",
    },
    CoreMethodHelp {
        owner: "Vec",
        name: "from",
        kind: "assoc",
        signature: "fn from<T, const N: usize>(values: [T; N]) -> Vec<T>",
        doc: "Creates a growable vector by moving values from a fixed-size array.",
    },
    CoreMethodHelp {
        owner: "Vec",
        name: "with_capacity",
        kind: "assoc",
        signature: "fn with_capacity<T>(capacity: i64) -> Vec<T>",
        doc: "Creates an empty vector with capacity reserved.",
    },
    CoreMethodHelp {
        owner: "Vec",
        name: "clone",
        kind: "method",
        signature: "fn clone<T>(self: Vec<T>) -> Vec<T>",
        doc: "Returns a copy of the vector.",
    },
    CoreMethodHelp {
        owner: "Vec",
        name: "push",
        kind: "method",
        signature: "fn push<T>(self: &mut Vec<T>, value: T) -> ()",
        doc: "Appends a value to the end of the vector.",
    },
    CoreMethodHelp {
        owner: "Vec",
        name: "pop",
        kind: "method",
        signature: "fn pop<T>(self: &mut Vec<T>) -> Option<T>",
        doc: "Removes and returns the last value when present.",
    },
    CoreMethodHelp {
        owner: "Vec",
        name: "insert",
        kind: "method",
        signature: "fn insert<T>(self: &mut Vec<T>, index: i64, value: T) -> Result<(), errors::Error>",
        doc: "Inserts a value at an index.",
    },
    CoreMethodHelp {
        owner: "Vec",
        name: "remove",
        kind: "method",
        signature: "fn remove<T>(self: &mut Vec<T>, index: i64) -> Result<T, errors::Error>",
        doc: "Removes and returns the value at an index.",
    },
    CoreMethodHelp {
        owner: "Vec",
        name: "clear",
        kind: "method",
        signature: "fn clear<T>(self: &mut Vec<T>) -> ()",
        doc: "Removes all values.",
    },
    CoreMethodHelp {
        owner: "Vec",
        name: "extend",
        kind: "method",
        signature: "fn extend<T>(self: &mut Vec<T>, values: Vec<T>) -> ()",
        doc: "Appends all values from another vector.",
    },
    CoreMethodHelp {
        owner: "Vec",
        name: "extend_from_slice",
        kind: "method",
        signature: "fn extend_from_slice<T>(self: &mut Vec<T>, values: Vec<T>) -> ()",
        doc: "Appends all values from another vector.",
    },
    CoreMethodHelp {
        owner: "Vec",
        name: "truncate",
        kind: "method",
        signature: "fn truncate<T>(self: &mut Vec<T>, len: i64) -> ()",
        doc: "Shortens the vector to at most len values.",
    },
    CoreMethodHelp {
        owner: "Vec",
        name: "reserve",
        kind: "method",
        signature: "fn reserve<T>(self: &mut Vec<T>, capacity: i64) -> ()",
        doc: "Ensures at least the requested total capacity.",
    },
    CoreMethodHelp {
        owner: "Vec",
        name: "reserve_exact",
        kind: "method",
        signature: "fn reserve_exact<T>(self: &mut Vec<T>, capacity: i64) -> ()",
        doc: "Ensures at least the requested total capacity without extra growth.",
    },
    CoreMethodHelp {
        owner: "Vec",
        name: "len",
        kind: "method",
        signature: "fn len<T>(self: Vec<T>) -> i64",
        doc: "Returns the number of values.",
    },
    CoreMethodHelp {
        owner: "Vec",
        name: "capacity",
        kind: "method",
        signature: "fn capacity<T>(self: Vec<T>) -> i64",
        doc: "Returns the current vector capacity.",
    },
    CoreMethodHelp {
        owner: "Vec",
        name: "is_empty",
        kind: "method",
        signature: "fn is_empty<T>(self: Vec<T>) -> bool",
        doc: "Returns true when the vector has no values.",
    },
    CoreMethodHelp {
        owner: "Vec",
        name: "slice",
        kind: "method",
        signature: "fn slice<T>(self: Vec<T>, start: i64, end: i64) -> Result<Vec<T>, errors::Error>",
        doc: "Returns a checked sub-slice copy.",
    },
    CoreMethodHelp {
        owner: "Vec",
        name: "first",
        kind: "method",
        signature: "fn first<T>(self: Vec<T>) -> Option<T>",
        doc: "Returns the first value when present.",
    },
    CoreMethodHelp {
        owner: "Vec",
        name: "last",
        kind: "method",
        signature: "fn last<T>(self: Vec<T>) -> Option<T>",
        doc: "Returns the last value when present.",
    },
    CoreMethodHelp {
        owner: "Vec",
        name: "get",
        kind: "method",
        signature: "fn get<T>(self: Vec<T>, index: i64) -> Option<T>",
        doc: "Returns the value at an index when present.",
    },
    CoreMethodHelp {
        owner: "Vec",
        name: "contains",
        kind: "method",
        signature: "fn contains<T>(self: Vec<T>, value: T) -> bool",
        doc: "Returns true when the vector contains the value.",
    },
    CoreMethodHelp {
        owner: "Vec",
        name: "index_of",
        kind: "method",
        signature: "fn index_of<T>(self: Vec<T>, value: T) -> Option<i64>",
        doc: "Returns the first matching index when present.",
    },
    CoreMethodHelp {
        owner: "Vec",
        name: "count_of",
        kind: "method",
        signature: "fn count_of<T>(self: Vec<T>, value: T) -> i64",
        doc: "Counts values equal to the argument.",
    },
    CoreMethodHelp {
        owner: "Vec",
        name: "sort",
        kind: "method",
        signature: "fn sort<T>(self: &mut Vec<T>) -> ()",
        doc: "Sorts the vector in place.",
    },
    CoreMethodHelp {
        owner: "Vec",
        name: "sort_by",
        kind: "method",
        signature: "fn sort_by<T>(self: &mut Vec<T>, cmp: fn(T, T) -> i64) -> ()",
        doc: "Sorts the vector in place with a comparator.",
    },
    CoreMethodHelp {
        owner: "Vec",
        name: "sort_by_key",
        kind: "method",
        signature: "fn sort_by_key<T, K>(self: &mut Vec<T>, f: fn(T) -> K) -> ()",
        doc: "Sorts the vector in place by a derived key.",
    },
    CoreMethodHelp {
        owner: "Vec",
        name: "reverse",
        kind: "method",
        signature: "fn reverse<T>(self: &mut Vec<T>) -> ()",
        doc: "Reverses the vector in place.",
    },
    CoreMethodHelp {
        owner: "Vec",
        name: "fill",
        kind: "method",
        signature: "fn fill<T>(self: &mut [T], value: T) -> ()",
        doc: "Clones a value into every existing element without resizing.",
    },
    CoreMethodHelp {
        owner: "Vec",
        name: "rev",
        kind: "method",
        signature: "fn rev<T>(self: Vec<T>) -> Vec<T>",
        doc: "Returns a reversed vector.",
    },
    CoreMethodHelp {
        owner: "Vec",
        name: "collect",
        kind: "method",
        signature: "fn collect<T>(self: Vec<T>) -> Vec<T>",
        doc: "Materializes the sequence as a vector.",
    },
    CoreMethodHelp {
        owner: "Vec",
        name: "to_vec",
        kind: "method",
        signature: "fn to_vec<T>(self: Vec<T>) -> Vec<T>",
        doc: "Materializes the sequence as a vector.",
    },
    CoreMethodHelp {
        owner: "Vec",
        name: "dedup",
        kind: "method",
        signature: "fn dedup<T>(self: Vec<T>) -> Vec<T>",
        doc: "Removes adjacent duplicate values.",
    },
    CoreMethodHelp {
        owner: "Vec",
        name: "take",
        kind: "method",
        signature: "fn take<T>(self: Vec<T>, n: i64) -> Vec<T>",
        doc: "Returns the first n values.",
    },
    CoreMethodHelp {
        owner: "Vec",
        name: "skip",
        kind: "method",
        signature: "fn skip<T>(self: Vec<T>, n: i64) -> Vec<T>",
        doc: "Drops the first n values.",
    },
    CoreMethodHelp {
        owner: "Vec",
        name: "step_by",
        kind: "method",
        signature: "fn step_by<T>(self: Vec<T>, step: i64) -> Vec<T>",
        doc: "Returns every nth value.",
    },
    CoreMethodHelp {
        owner: "Vec",
        name: "chain",
        kind: "method",
        signature: "fn chain<T>(self: Vec<T>, other: Vec<T>) -> Vec<T>",
        doc: "Concatenates this sequence with another sequence.",
    },
    CoreMethodHelp {
        owner: "Vec",
        name: "zip",
        kind: "method",
        signature: "fn zip<T, U>(self: Vec<T>, other: Vec<U>) -> Vec<(T, U)>",
        doc: "Pairs values with another sequence.",
    },
    CoreMethodHelp {
        owner: "Vec",
        name: "windows",
        kind: "method",
        signature: "fn windows<T>(self: Vec<T>, size: i64) -> Vec<Vec<T>>",
        doc: "Returns overlapping fixed-size windows.",
    },
    CoreMethodHelp {
        owner: "Vec",
        name: "chunks",
        kind: "method",
        signature: "fn chunks<T>(self: Vec<T>, size: i64) -> Vec<Vec<T>>",
        doc: "Groups values into fixed-size chunks.",
    },
    CoreMethodHelp {
        owner: "Vec",
        name: "pairwise",
        kind: "method",
        signature: "fn pairwise<T>(self: Vec<T>) -> Vec<(T, T)>",
        doc: "Returns adjacent value pairs.",
    },
    CoreMethodHelp {
        owner: "Vec",
        name: "flatten",
        kind: "method",
        signature: "fn flatten<T>(self: Vec<Vec<T>>) -> Vec<T>",
        doc: "Flattens one level of nested vectors.",
    },
    CoreMethodHelp {
        owner: "Vec",
        name: "swap",
        kind: "method",
        signature: "fn swap<T>(self: &mut Vec<T>, a: i64, b: i64) -> Result<(), errors::Error>",
        doc: "Swaps two vector positions.",
    },
    CoreMethodHelp {
        owner: "Vec",
        name: "join",
        kind: "method",
        signature: "fn join<T>(self: Vec<T>, sep: String) -> String",
        doc: "Joins displayable values with a separator.",
    },
    CoreMethodHelp {
        owner: "Vec",
        name: "map",
        kind: "method",
        signature: "fn map<T, U>(self: Vec<T>, f: fn(T) -> U) -> Vec<U>",
        doc: "Maps every value through a closure.",
    },
    CoreMethodHelp {
        owner: "Vec",
        name: "filter",
        kind: "method",
        signature: "fn filter<T>(self: Vec<T>, f: fn(T) -> bool) -> Vec<T>",
        doc: "Keeps values accepted by a predicate.",
    },
    CoreMethodHelp {
        owner: "Vec",
        name: "fold",
        kind: "method",
        signature: "fn fold<T, A>(self: Vec<T>, init: A, f: fn(A, T) -> A) -> A",
        doc: "Reduces values with an accumulator.",
    },
    CoreMethodHelp {
        owner: "Vec",
        name: "for_each",
        kind: "method",
        signature: "fn for_each<T>(self: Vec<T>, f: fn(T) -> ()) -> ()",
        doc: "Runs a closure for each value.",
    },
    CoreMethodHelp {
        owner: "Vec",
        name: "any",
        kind: "method",
        signature: "fn any<T>(self: Vec<T>, f: fn(T) -> bool) -> bool",
        doc: "Returns true if any value matches.",
    },
    CoreMethodHelp {
        owner: "Vec",
        name: "all",
        kind: "method",
        signature: "fn all<T>(self: Vec<T>, f: fn(T) -> bool) -> bool",
        doc: "Returns true if all values match.",
    },
    CoreMethodHelp {
        owner: "Vec",
        name: "find",
        kind: "method",
        signature: "fn find<T>(self: Vec<T>, f: fn(T) -> bool) -> Option<T>",
        doc: "Returns the first matching value.",
    },
    CoreMethodHelp {
        owner: "Vec",
        name: "position",
        kind: "method",
        signature: "fn position<T>(self: Vec<T>, f: fn(T) -> bool) -> Option<i64>",
        doc: "Returns the first matching index.",
    },
    CoreMethodHelp {
        owner: "Vec",
        name: "count",
        kind: "method",
        signature: "fn count<T>(self: Vec<T>) -> i64",
        doc: "Counts values.",
    },
    CoreMethodHelp {
        owner: "Vec",
        name: "enumerate",
        kind: "method",
        signature: "fn enumerate<T>(self: Vec<T>) -> Vec<(i64, T)>",
        doc: "Pairs each value with its index.",
    },
    CoreMethodHelp {
        owner: "Vec",
        name: "sum",
        kind: "method",
        signature: "fn sum<T>(self: Vec<T>) -> T",
        doc: "Sums numeric values.",
    },
    CoreMethodHelp {
        owner: "Vec",
        name: "min",
        kind: "method",
        signature: "fn min<T>(self: Vec<T>) -> Option<T>",
        doc: "Returns the minimum value when present.",
    },
    CoreMethodHelp {
        owner: "Vec",
        name: "max",
        kind: "method",
        signature: "fn max<T>(self: Vec<T>) -> Option<T>",
        doc: "Returns the maximum value when present.",
    },
    CoreMethodHelp {
        owner: "Vec",
        name: "min_by_key",
        kind: "method",
        signature: "fn min_by_key<T, K>(self: Vec<T>, f: fn(T) -> K) -> Option<T>",
        doc: "Returns the minimum value by derived key.",
    },
    CoreMethodHelp {
        owner: "Vec",
        name: "max_by_key",
        kind: "method",
        signature: "fn max_by_key<T, K>(self: Vec<T>, f: fn(T) -> K) -> Option<T>",
        doc: "Returns the maximum value by derived key.",
    },
    CoreMethodHelp {
        owner: "HashMap",
        name: "new",
        kind: "assoc",
        signature: "fn new<K, V>() -> HashMap<K, V>",
        doc: "Creates an empty hash map.",
    },
    CoreMethodHelp {
        owner: "HashMap",
        name: "with_capacity",
        kind: "assoc",
        signature: "fn with_capacity<K, V>(capacity: i64) -> HashMap<K, V>",
        doc: "Creates an empty hash map with capacity reserved.",
    },
    CoreMethodHelp {
        owner: "HashMap",
        name: "from",
        kind: "assoc",
        signature: "fn from<K, V, const N: usize>(entries: {K: V} | [(K, V); N]) -> HashMap<K, V>",
        doc: "Creates a hash map from a map literal or key-value tuple array.",
    },
    CoreMethodHelp {
        owner: "HashMap",
        name: "insert",
        kind: "method",
        signature: "fn insert<K, V>(self: &mut HashMap<K, V>, key: K, value: V) -> Option<V>",
        doc: "Inserts a pair and returns the previous value when present.",
    },
    CoreMethodHelp {
        owner: "HashMap",
        name: "get",
        kind: "method",
        signature: "fn get<K, V>(self: HashMap<K, V>, key: K) -> Option<V>",
        doc: "Returns the value for a key when present.",
    },
    CoreMethodHelp {
        owner: "HashMap",
        name: "get_or",
        kind: "method",
        signature: "fn get_or<K, V>(self: HashMap<K, V>, key: K, default: V) -> V",
        doc: "Returns the value for a key or a default.",
    },
    CoreMethodHelp {
        owner: "HashMap",
        name: "or_insert",
        kind: "method",
        signature: "fn or_insert<K, V>(self: &mut HashMap<K, V>, key: K, default: V) -> V",
        doc: "Returns the existing value or inserts a default.",
    },
    CoreMethodHelp {
        owner: "HashMap",
        name: "remove",
        kind: "method",
        signature: "fn remove<K, V>(self: &mut HashMap<K, V>, key: K) -> Option<V>",
        doc: "Removes a key and returns its previous value when present.",
    },
    CoreMethodHelp {
        owner: "HashMap",
        name: "pop",
        kind: "method",
        signature: "fn pop<K, V>(self: &mut HashMap<K, V>, key: K) -> Option<V>",
        doc: "Removes and returns the value for a key when present.",
    },
    CoreMethodHelp {
        owner: "HashMap",
        name: "contains_key",
        kind: "method",
        signature: "fn contains_key<K, V>(self: HashMap<K, V>, key: K) -> bool",
        doc: "Returns true when the map contains a key.",
    },
    CoreMethodHelp {
        owner: "HashMap",
        name: "contains",
        kind: "method",
        signature: "fn contains<K, V>(self: HashMap<K, V>, key: K) -> bool",
        doc: "Alias for contains_key.",
    },
    CoreMethodHelp {
        owner: "HashMap",
        name: "len",
        kind: "method",
        signature: "fn len<K, V>(self: HashMap<K, V>) -> i64",
        doc: "Returns the number of entries.",
    },
    CoreMethodHelp {
        owner: "HashMap",
        name: "is_empty",
        kind: "method",
        signature: "fn is_empty<K, V>(self: HashMap<K, V>) -> bool",
        doc: "Returns true when the map has no entries.",
    },
    CoreMethodHelp {
        owner: "HashMap",
        name: "keys",
        kind: "method",
        signature: "fn keys<K, V>(self: HashMap<K, V>) -> Vec<K>",
        doc: "Returns all keys.",
    },
    CoreMethodHelp {
        owner: "HashMap",
        name: "values",
        kind: "method",
        signature: "fn values<K, V>(self: HashMap<K, V>) -> Vec<V>",
        doc: "Returns all values.",
    },
    CoreMethodHelp {
        owner: "HashMap",
        name: "iter",
        kind: "method",
        signature: "fn iter<K, V>(self: HashMap<K, V>) -> Vec<(K, V)>",
        doc: "Returns key-value pairs.",
    },
    CoreMethodHelp {
        owner: "BTreeMap",
        name: "new",
        kind: "assoc",
        signature: "fn new<K, V>() -> BTreeMap<K, V>",
        doc: "Creates an empty ordered map.",
    },
    CoreMethodHelp {
        owner: "BTreeMap",
        name: "insert",
        kind: "method",
        signature: "fn insert<K, V>(self: &mut BTreeMap<K, V>, key: K, value: V) -> Option<V>",
        doc: "Inserts a pair and returns the previous value when present.",
    },
    CoreMethodHelp {
        owner: "BTreeMap",
        name: "get",
        kind: "method",
        signature: "fn get<K, V>(self: BTreeMap<K, V>, key: K) -> Option<V>",
        doc: "Returns the value for a key when present.",
    },
    CoreMethodHelp {
        owner: "BTreeMap",
        name: "get_or",
        kind: "method",
        signature: "fn get_or<K, V>(self: BTreeMap<K, V>, key: K, default: V) -> V",
        doc: "Returns the value for a key or a default.",
    },
    CoreMethodHelp {
        owner: "BTreeMap",
        name: "contains_key",
        kind: "method",
        signature: "fn contains_key<K, V>(self: BTreeMap<K, V>, key: K) -> bool",
        doc: "Returns true when the ordered map contains a key.",
    },
    CoreMethodHelp {
        owner: "BTreeMap",
        name: "contains",
        kind: "method",
        signature: "fn contains<K, V>(self: BTreeMap<K, V>, key: K) -> bool",
        doc: "Alias for contains_key.",
    },
    CoreMethodHelp {
        owner: "BTreeMap",
        name: "len",
        kind: "method",
        signature: "fn len<K, V>(self: BTreeMap<K, V>) -> i64",
        doc: "Returns the number of entries.",
    },
    CoreMethodHelp {
        owner: "BTreeMap",
        name: "keys",
        kind: "method",
        signature: "fn keys<K, V>(self: BTreeMap<K, V>) -> Vec<K>",
        doc: "Returns ordered keys.",
    },
    CoreMethodHelp {
        owner: "HashSet",
        name: "new",
        kind: "assoc",
        signature: "fn new<T>() -> HashSet<T>",
        doc: "Creates an empty hash set.",
    },
    CoreMethodHelp {
        owner: "HashSet",
        name: "from",
        kind: "assoc",
        signature: "fn from<T, const N: usize>(values: [T; N]) -> HashSet<T>",
        doc: "Creates a hash set from a collection, removing duplicate values.",
    },
    CoreMethodHelp {
        owner: "HashSet",
        name: "insert",
        kind: "method",
        signature: "fn insert<T>(self: &mut HashSet<T>, value: T) -> bool",
        doc: "Adds a value to the set.",
    },
    CoreMethodHelp {
        owner: "HashSet",
        name: "remove",
        kind: "method",
        signature: "fn remove<T>(self: &mut HashSet<T>, value: T) -> bool",
        doc: "Removes a value from the set.",
    },
    CoreMethodHelp {
        owner: "HashSet",
        name: "contains",
        kind: "method",
        signature: "fn contains<T>(self: HashSet<T>, value: T) -> bool",
        doc: "Returns true when the set contains a value.",
    },
    CoreMethodHelp {
        owner: "HashSet",
        name: "union",
        kind: "method",
        signature: "fn union<T>(self: HashSet<T>, other: HashSet<T>) -> HashSet<T>",
        doc: "Returns the union of two sets.",
    },
    CoreMethodHelp {
        owner: "HashSet",
        name: "intersection",
        kind: "method",
        signature: "fn intersection<T>(self: HashSet<T>, other: HashSet<T>) -> HashSet<T>",
        doc: "Returns the intersection of two sets.",
    },
    CoreMethodHelp {
        owner: "HashSet",
        name: "difference",
        kind: "method",
        signature: "fn difference<T>(self: HashSet<T>, other: HashSet<T>) -> HashSet<T>",
        doc: "Returns values present only in the receiver.",
    },
    CoreMethodHelp {
        owner: "HashSet",
        name: "symmetric_difference",
        kind: "method",
        signature: "fn symmetric_difference<T>(self: HashSet<T>, other: HashSet<T>) -> HashSet<T>",
        doc: "Returns values present in exactly one set.",
    },
    CoreMethodHelp {
        owner: "HashSet",
        name: "len",
        kind: "method",
        signature: "fn len<T>(self: HashSet<T>) -> i64",
        doc: "Returns the number of values.",
    },
    CoreMethodHelp {
        owner: "HashSet",
        name: "is_empty",
        kind: "method",
        signature: "fn is_empty<T>(self: HashSet<T>) -> bool",
        doc: "Returns true when the set has no values.",
    },
    CoreMethodHelp {
        owner: "HashSet",
        name: "clear",
        kind: "method",
        signature: "fn clear<T>(self: &mut HashSet<T>) -> ()",
        doc: "Removes every value.",
    },
    CoreMethodHelp {
        owner: "HashSet",
        name: "iter",
        kind: "method",
        signature: "fn iter<T>(self: HashSet<T>) -> Vec<T>",
        doc: "Returns a deterministic snapshot suitable for iterator methods.",
    },
    CoreMethodHelp {
        owner: "HashSet",
        name: "to_vec",
        kind: "method",
        signature: "fn to_vec<T>(self: HashSet<T>) -> Vec<T>",
        doc: "Returns the values in deterministic order.",
    },
    CoreMethodHelp {
        owner: "HashSet",
        name: "is_subset",
        kind: "method",
        signature: "fn is_subset<T>(self: HashSet<T>, other: HashSet<T>) -> bool",
        doc: "Returns true when every value is present in the other set.",
    },
    CoreMethodHelp {
        owner: "HashSet",
        name: "is_superset",
        kind: "method",
        signature: "fn is_superset<T>(self: HashSet<T>, other: HashSet<T>) -> bool",
        doc: "Returns true when the set contains every value from the other set.",
    },
    CoreMethodHelp {
        owner: "HashSet",
        name: "is_disjoint",
        kind: "method",
        signature: "fn is_disjoint<T>(self: HashSet<T>, other: HashSet<T>) -> bool",
        doc: "Returns true when the sets have no values in common.",
    },
    CoreMethodHelp {
        owner: "VecDeque",
        name: "new",
        kind: "assoc",
        signature: "fn new<T>() -> VecDeque<T>",
        doc: "Creates an empty double-ended queue.",
    },
    CoreMethodHelp {
        owner: "VecDeque",
        name: "push_back",
        kind: "method",
        signature: "fn push_back<T>(self: &mut VecDeque<T>, value: T) -> ()",
        doc: "Appends a value to the back.",
    },
    CoreMethodHelp {
        owner: "VecDeque",
        name: "push_front",
        kind: "method",
        signature: "fn push_front<T>(self: &mut VecDeque<T>, value: T) -> ()",
        doc: "Appends a value to the front.",
    },
    CoreMethodHelp {
        owner: "VecDeque",
        name: "pop_front",
        kind: "method",
        signature: "fn pop_front<T>(self: &mut VecDeque<T>) -> Option<T>",
        doc: "Removes and returns the front value when present.",
    },
    CoreMethodHelp {
        owner: "VecDeque",
        name: "pop_back",
        kind: "method",
        signature: "fn pop_back<T>(self: &mut VecDeque<T>) -> Option<T>",
        doc: "Removes and returns the back value when present.",
    },
    CoreMethodHelp {
        owner: "VecDeque",
        name: "peek_front",
        kind: "method",
        signature: "fn peek_front<T>(self: VecDeque<T>) -> Option<T>",
        doc: "Returns the front value without removing it.",
    },
    CoreMethodHelp {
        owner: "VecDeque",
        name: "peek_back",
        kind: "method",
        signature: "fn peek_back<T>(self: VecDeque<T>) -> Option<T>",
        doc: "Returns the back value without removing it.",
    },
    CoreMethodHelp {
        owner: "Result",
        name: "map",
        kind: "method",
        signature: "fn map<T, U, E>(self: Result<T, E>, f: fn(T) -> U) -> Result<U, E>",
        doc: "Maps Ok through a closure and leaves Err unchanged.",
    },
    CoreMethodHelp {
        owner: "Result",
        name: "map_err",
        kind: "method",
        signature: "fn map_err<T, E, F>(self: Result<T, E>, f: fn(E) -> F) -> Result<T, F>",
        doc: "Maps Err through a closure and leaves Ok unchanged.",
    },
    CoreMethodHelp {
        owner: "Result",
        name: "is_ok",
        kind: "method",
        signature: "fn is_ok<T, E>(self: Result<T, E>) -> bool",
        doc: "Returns true for Ok.",
    },
    CoreMethodHelp {
        owner: "Result",
        name: "is_err",
        kind: "method",
        signature: "fn is_err<T, E>(self: Result<T, E>) -> bool",
        doc: "Returns true for Err.",
    },
];

#[allow(
    clippy::cognitive_complexity,
    clippy::too_many_lines,
    reason = "REPL loop bundles input, completion, history, and graceful-exit handling"
)]
pub(crate) fn cmd_repl(verbose: bool) -> Result<()> {
    use rustyline::error::ReadlineError;
    use rustyline::history::FileHistory;
    use rustyline::{ColorMode, CompletionType, Config, EditMode, Editor, EventHandler, KeyEvent};

    use crate::repl_helper::{GosReplHelper, ReplEnterHandler};

    println!(
        "gos {version} REPL [{arch}-{os}]\n\
         %help for commands · Enter continues until braces close · Ctrl-D or %q exits",
        version = env!("CARGO_PKG_VERSION"),
        arch = std::env::consts::ARCH,
        os = std::env::consts::OS,
    );

    let mut transcript: Vec<String> = Vec::new();
    let mut declarations: Vec<String> = Vec::new();
    let mut lets: Vec<String> = Vec::new();
    let mut bindings: Vec<ReplBinding> = Vec::new();
    let mut input_no = 1u32;

    let config = Config::builder()
        .edit_mode(EditMode::Emacs)
        .color_mode(ColorMode::Enabled)
        .completion_type(CompletionType::List)
        .auto_add_history(false)
        .build();
    let mut editor: Editor<GosReplHelper, FileHistory> =
        Editor::with_config(config).map_err(|e| anyhow!("repl init: {e}"))?;
    editor.set_helper(Some(GosReplHelper::new()));
    editor.bind_sequence(
        KeyEvent::from('\r'),
        EventHandler::Conditional(Box::new(ReplEnterHandler)),
    );
    let history_path = repl_history_path();
    if let Some(path) = &history_path {
        let _ = editor.load_history(path);
    }
    transcript.extend(editor.history().iter().cloned());

    let tty = std::io::IsTerminal::is_terminal(&std::io::stdout());
    if tty {
        crate::style::force_enable();
    }
    loop {
        let prompt = if tty {
            "\x1b[32m>>>\x1b[0m ".to_string()
        } else {
            ">>> ".to_string()
        };
        let line = match editor.readline(&prompt) {
            Ok(line) => line,
            Err(ReadlineError::Interrupted) => {
                eprintln!("KeyboardInterrupt");
                continue;
            }
            Err(ReadlineError::Eof) => {
                if let Some(path) = &history_path {
                    let _ = editor.save_history(path);
                }
                println!();
                return Ok(());
            }
            Err(err) => {
                print_repl_error(&format!("repl: {err}"));
                return Ok(());
            }
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        // History searches must run against earlier inputs only. Recording
        // `%history pattern` first would make it match its own pattern.
        if let Some(rest) = trimmed.strip_prefix('%') {
            let (command, arg) = split_meta_command(rest.trim());
            if matches!(command, "history" | "h") {
                match render_repl_history(&transcript, arg) {
                    Ok(entries) => {
                        for entry in entries {
                            println!("{}", crate::style::repl_meta_accent(&entry));
                        }
                    }
                    Err(message) => print_repl_error(&message),
                }
                let _ = editor.add_history_entry(trimmed);
                transcript.push(trimmed.to_string());
                continue;
            }
            // Clearing is deliberately handled before recording the current
            // meta-command. It clears both the in-memory editor navigation
            // and the persistent transcript, so `%h` immediately after it is
            // empty and a later REPL session cannot resurrect old entries.
            if command == "clear-history" {
                if !arg.is_empty() {
                    print_repl_error("usage: %clear-history");
                    continue;
                }
                if let Err(err) = editor.clear_history() {
                    print_repl_error(&format!("clear history: {err}"));
                    continue;
                }
                transcript.clear();
                if let Some(path) = &history_path
                    && let Err(err) = std::fs::remove_file(path)
                    && err.kind() != std::io::ErrorKind::NotFound
                {
                    print_repl_error(&format!("clear history: {err}"));
                    continue;
                }
                println!("history cleared");
                continue;
            }
        }
        let _ = editor.add_history_entry(trimmed);
        transcript.push(trimmed.to_string());

        // Meta-commands first.
        if let Some(rest) = trimmed.strip_prefix('%') {
            let rest = rest.trim();
            let (command, arg) = split_meta_command(rest);
            match command {
                "quit" | "q" => {
                    if let Some(path) = &history_path {
                        let _ = editor.save_history(path);
                    }
                    return Ok(());
                }
                "bindings" | "b" => {
                    let options = match parse_listing_options("bindings", arg) {
                        Ok(options) => options,
                        Err(message) => {
                            print_repl_error(&message);
                            continue;
                        }
                    };
                    if bindings.is_empty() {
                        println!(
                            "{}",
                            crate::style::repl_meta_detail("    no `let` bindings yet")
                        );
                    } else {
                        let pattern = if options.pattern.is_empty() {
                            None
                        } else {
                            match compile_search_regex("bindings", &options.pattern) {
                                Ok(pattern) => Some(pattern),
                                Err(message) => {
                                    print_repl_error(&message);
                                    continue;
                                }
                            }
                        };
                        let lines = render_repl_bindings(&declarations, &lets, &bindings);
                        let matches = lines
                            .iter()
                            .filter(|line| pattern.as_ref().is_none_or(|re| re.is_match(line)))
                            .collect::<Vec<_>>();
                        if matches.is_empty() {
                            println!(
                                "{}",
                                crate::style::repl_meta_detail(&format!(
                                    "    no bindings match `{}`",
                                    options.pattern
                                ))
                            );
                            continue;
                        }
                        for line in paginate_listing(matches, &options, "%b") {
                            println!("{}", crate::style::repl_meta_heading(&line));
                        }
                    }
                    continue;
                }
                "drop" => {
                    let mut words = arg.split_whitespace();
                    let Some(name) = words.next() else {
                        print_repl_error("usage: %drop NAME");
                        continue;
                    };
                    if words.next().is_some() {
                        print_repl_error("usage: %drop NAME");
                        continue;
                    }
                    let Some((candidate_lets, candidate_bindings)) =
                        prepare_repl_drop(&lets, &bindings, name)
                    else {
                        print_repl_error(&format!("no persistent binding named `{name}`"));
                        continue;
                    };
                    let probe_body = format!("{}()\n", render_repl_setup(&candidate_lets));
                    let entry = format!("__irepl_drop_{input_no}");
                    let probe = format!(
                        "{}\nfn {entry}() {{\n    {probe_body}}}\n",
                        declarations.join("\n"),
                    );
                    match build_and_call(&probe, &entry) {
                        Ok(_) => {
                            lets = candidate_lets;
                            bindings = candidate_bindings;
                            if let Some(helper) = editor.helper_mut() {
                                helper.forget_binding(name);
                            }
                            println!(
                                "{}",
                                crate::style::repl_meta_accent(&format!("dropped `{name}`"))
                            );
                        }
                        Err(message) => print_repl_error(&format!(
                            "cannot drop `{name}` while later bindings depend on it:\n{message}"
                        )),
                    }
                    continue;
                }
                "declarations" | "decls" | "d" => {
                    let options = match parse_listing_options("declarations", arg) {
                        Ok(options) => options,
                        Err(message) => {
                            print_repl_error(&message);
                            continue;
                        }
                    };
                    if declarations.is_empty() {
                        println!(
                            "{}",
                            crate::style::repl_meta_detail("    no declarations yet")
                        );
                    } else {
                        let pattern = if options.pattern.is_empty() {
                            None
                        } else {
                            match compile_search_regex("declarations", &options.pattern) {
                                Ok(pattern) => Some(pattern),
                                Err(message) => {
                                    print_repl_error(&message);
                                    continue;
                                }
                            }
                        };
                        let matches = declarations
                            .iter()
                            .filter(|line| pattern.as_ref().is_none_or(|re| re.is_match(line)))
                            .collect::<Vec<_>>();
                        if matches.is_empty() {
                            println!(
                                "{}",
                                crate::style::repl_meta_detail(&format!(
                                    "    no declarations match `{}`",
                                    options.pattern
                                ))
                            );
                            continue;
                        }
                        for line in paginate_listing(matches, &options, "%d") {
                            println!("{}", crate::style::repl_meta_heading(&line));
                        }
                    }
                    continue;
                }
                "reset" | "r" => {
                    declarations.clear();
                    lets.clear();
                    bindings.clear();
                    if let Some(helper) = editor.helper_mut() {
                        helper.reset_session();
                    }
                    println!("{}", crate::style::repl_meta_accent("session cleared"));
                    continue;
                }
                "help" => {
                    if arg.is_empty() {
                        print_repl_output(REPL_HELP_TEXT);
                    } else {
                        print_repl_error("usage: %help");
                    }
                    continue;
                }
                "info" | "i" => {
                    let options = match parse_listing_options("info", arg) {
                        Ok(options) => options,
                        Err(message) => {
                            print_repl_error(&message);
                            continue;
                        }
                    };
                    let result = if options.details {
                        repl_info(&options.pattern)
                    } else {
                        repl_info_listing(&options.pattern)
                    }
                    .map(|text| paginate_info(&text, &options));
                    match result {
                        Ok(text) => print_repl_output(&text),
                        Err(msg) => print_repl_error(&msg),
                    }
                    continue;
                }
                "explain" | "e" => {
                    let options = match parse_listing_options("explain", arg) {
                        Ok(options) => options,
                        Err(message) => {
                            print_repl_error(&message);
                            continue;
                        }
                    };
                    if options.pattern.is_empty() {
                        print_repl_error("usage: %explain NAME [-d|--details]");
                        continue;
                    }
                    let result = if options.details {
                        repl_binding_info(&declarations, &lets, &bindings, &options.pattern)
                    } else {
                        repl_binding_listing(&declarations, &lets, &bindings, &options.pattern)
                    };
                    match result {
                        Some(Ok(text)) => print_repl_output(&text),
                        Some(Err(msg)) => print_repl_error(&msg),
                        None if catalog_has_exact_match(normalize_query(&options.pattern)) => {
                            match repl_info_matches(&options.pattern, options.details) {
                                Ok(text) => print_repl_output(&text),
                                Err(msg) => print_repl_error(&msg),
                            }
                        }
                        None => print_repl_error(&format!(
                            "no persistent binding named `{}`",
                            options.pattern
                        )),
                    }
                    continue;
                }
                _ => {
                    print_repl_error(&format!("unknown meta-command: %{rest}"));
                    continue;
                }
            }
        }

        let is_declaration = input_is_declaration(trimmed);

        if is_declaration {
            declarations.push(trimmed.to_string());
            match rebuild_session(&declarations) {
                Ok(()) => {
                    if verbose {
                        println!("    added {} declarations", declarations.len());
                    }
                }
                Err(msg) => {
                    declarations.pop();
                    print_repl_error(&format!("    {msg}"));
                }
            }
            input_no += 1;
            continue;
        }

        if trimmed.starts_with("let ") {
            let candidate = trimmed.to_string();
            let mut new_binding = match repl_binding_from_let_source(&candidate) {
                Ok(binding) => binding,
                Err(msg) => {
                    print_repl_error(&format!("    {msg}"));
                    input_no += 1;
                    continue;
                }
            };
            let probe_body = format!("{}{candidate}\n    ()\n", render_repl_setup(&lets));
            let probe = format!(
                "{}\nfn __irepl_{n}() {{\n    {body}}}\n",
                declarations.join("\n"),
                n = input_no,
                body = probe_body,
            );
            match build_and_call(&probe, &format!("__irepl_{input_no}")) {
                Ok(_) => {
                    new_binding.source_index = lets.len();
                    if let Some(helper) = editor.helper_mut() {
                        let names: Vec<&str> = new_binding
                            .vars
                            .iter()
                            .map(|var| var.name.as_str())
                            .collect();
                        helper.observe_let(&candidate, &names);
                    }
                    update_repl_bindings(&mut bindings, new_binding);
                    lets.push(candidate.clone());
                    if verbose {
                        println!("    binding added ({} total)", bindings.len());
                    }
                }
                Err(msg) => {
                    print_repl_error(&format!("    {msg}"));
                }
            }
            input_no += 1;
            continue;
        }

        // Assignments and collection mutation calls must be replayed with the
        // preceding bindings so their effects survive into later inputs.
        let user_mutating_methods = collect_repl_mut_self_method_names(&declarations);
        if input_mutates_binding(trimmed, &user_mutating_methods) {
            let probe_body = format!("{}{trimmed}", render_repl_setup(&lets));
            let probe = format!(
                "{}\nfn __irepl_{n}() {{\n    {body}}}\n",
                declarations.join("\n"),
                n = input_no,
                body = probe_body,
            );
            match build_and_call(&probe, &format!("__irepl_{input_no}")) {
                Ok(value) => {
                    if matches!(value, gossamer_interp::Value::Unit) {
                        lets.push(trimmed.to_string());
                    } else {
                        print_repl_result(&value);
                        // The call is a tail expression in the probe so its
                        // Result can be displayed. On later inputs it becomes
                        // a statement, where an unused Result is rightly a
                        // type error. Replay it with an explicit discard so
                        // its receiver mutation persists without poisoning
                        // every subsequent binding and `%b` inspection.
                        lets.push(format!("let _ = {trimmed}"));
                    }
                }
                Err(msg) => print_repl_error(&format!("error: {msg}")),
            }
            input_no += 1;
            continue;
        }

        let let_body = render_repl_setup(&lets);
        let program_source = format!(
            "{}\nfn __irepl_{n}() {{ {lets}{expr}\n}}\n",
            declarations.join("\n"),
            n = input_no,
            lets = let_body,
            expr = trimmed,
        );
        match build_and_call(&program_source, &format!("__irepl_{input_no}")) {
            Ok(value) => {
                if !matches!(value, gossamer_interp::Value::Unit) {
                    print_repl_result(&value);
                }
            }
            Err(msg) => {
                print_repl_error(&format!("error: {msg}"));
            }
        }
        input_no += 1;
    }
}

/// REPL results use source-like representation, while explicit `print` and
/// `println` retain `Display` formatting. This keeps a bare string distinct
/// from an identifier and applies recursively to aggregate values.
fn render_repl_value(value: &gossamer_interp::Value) -> String {
    value.repr()
}

struct ReplValueType {
    rendered: String,
    references: Vec<gossamer_types::Mutbl>,
    method_owner: Option<String>,
    fixed_array: bool,
}

impl ReplValueType {
    fn from_ty(tcx: &gossamer_types::TyCtxt, ty: gossamer_types::Ty) -> Self {
        let rendered = gossamer_types::render_public_ty(tcx, ty);
        let mut references = Vec::new();
        let mut current = ty;
        while let Some(gossamer_types::TyKind::Ref { mutability, inner }) = tcx.kind(current) {
            references.push(*mutability);
            current = *inner;
        }
        let (method_owner, fixed_array) = match tcx.kind(current) {
            Some(gossamer_types::TyKind::Array { .. }) => (Some("Array".to_string()), true),
            Some(gossamer_types::TyKind::Slice(_)) => (Some("Slice".to_string()), true),
            Some(gossamer_types::TyKind::Vec(_)) => (Some("Vec".to_string()), false),
            Some(gossamer_types::TyKind::String) => (Some("String".to_string()), false),
            Some(gossamer_types::TyKind::HashMap { .. }) => (Some("HashMap".to_string()), false),
            Some(gossamer_types::TyKind::Iterator(_)) => (Some("Iterator".to_string()), false),
            Some(gossamer_types::TyKind::Sender(_)) => (Some("Sender".to_string()), false),
            Some(gossamer_types::TyKind::Receiver(_)) => (Some("Receiver".to_string()), false),
            Some(gossamer_types::TyKind::JoinHandle(_)) => (Some("JoinHandle".to_string()), false),
            Some(gossamer_types::TyKind::Duration) => (Some("Duration".to_string()), false),
            Some(gossamer_types::TyKind::Instant) => (Some("Instant".to_string()), false),
            Some(gossamer_types::TyKind::Adt { def, .. }) => {
                (tcx.def_name(*def).map(str::to_string), false)
            }
            _ => (None, false),
        };
        Self {
            rendered,
            references,
            method_owner,
            fixed_array,
        }
    }

    fn unknown() -> Self {
        Self {
            rendered: "<unknown>".to_string(),
            references: Vec::new(),
            method_owner: None,
            fixed_array: false,
        }
    }
}

fn render_repl_binding_value(value: &gossamer_interp::Value, ty: &ReplValueType) -> String {
    let mut rendered = render_repl_value(value);
    for mutability in ty.references.iter().rev() {
        rendered = format!("{}{rendered}", mutability.prefix());
    }
    rendered
}

fn print_repl_result(value: &gossamer_interp::Value) {
    println!("{}", render_repl_value(value));
    std::io::stdout()
        .flush()
        .expect("flush REPL expression result");
}

fn render_repl_setup(lets: &[String]) -> String {
    if lets.is_empty() {
        String::new()
    } else {
        let replay = lets
            .iter()
            .map(|line| suppress_replayed_prints(line))
            .collect::<Vec<_>>()
            .join("\n    ");
        format!("{replay}\n    ")
    }
}

fn suppress_replayed_prints(input: &str) -> String {
    input
        .replace("println!(", "format!(")
        .replace("eprintln!(", "format!(")
        .replace("print!(", "format!(")
        .replace("eprint!(", "format!(")
        .replace("println(", "__repl_discard(")
        .replace("eprintln(", "__repl_discard(")
        .replace("print(", "__repl_discard(")
        .replace("eprint(", "__repl_discard(")
}

#[derive(Clone)]
struct ReplBinding {
    vars: Vec<ReplBindingVar>,
    source_index: usize,
}

#[derive(Clone)]
struct ReplBindingVar {
    name: String,
    mutable: bool,
}

fn update_repl_bindings(bindings: &mut Vec<ReplBinding>, new_binding: ReplBinding) {
    if !new_binding.vars.is_empty() {
        for binding in bindings.iter_mut() {
            binding.vars.retain(|var| {
                !new_binding
                    .vars
                    .iter()
                    .any(|new_var| new_var.name == var.name)
            });
        }
        bindings.retain(|binding| !binding.vars.is_empty());
    }
    bindings.push(new_binding);
}

fn prepare_repl_drop(
    lets: &[String],
    bindings: &[ReplBinding],
    name: &str,
) -> Option<(Vec<String>, Vec<ReplBinding>)> {
    let target_index = bindings
        .iter()
        .find(|binding| binding.vars.iter().any(|var| var.name == name))?
        .source_index;

    let surviving_vars = bindings
        .iter()
        .filter(|binding| binding.source_index >= target_index)
        .flat_map(|binding| binding.vars.iter())
        .filter(|var| var.name != name)
        .cloned()
        .collect::<Vec<_>>();
    let scoped_source = lets[target_index..].join("\n    ");
    let replacement = match surviving_vars.as_slice() {
        [] => format!("{{\n    {scoped_source}\n}}"),
        [var] => format!(
            "let {mutability}{name} = {{\n    {scoped_source}\n    {name}\n}}",
            mutability = if var.mutable { "mut " } else { "" },
            name = var.name,
        ),
        vars => {
            let pattern = vars
                .iter()
                .map(|var| {
                    if var.mutable {
                        format!("mut {}", var.name)
                    } else {
                        var.name.clone()
                    }
                })
                .collect::<Vec<_>>()
                .join(", ");
            let values = vars
                .iter()
                .map(|var| var.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            format!("let ({pattern}) = {{\n    {scoped_source}\n    ({values})\n}}")
        }
    };

    let mut new_lets = lets[..target_index].to_vec();
    new_lets.push(replacement);
    let mut new_bindings = bindings.to_vec();
    for binding in &mut new_bindings {
        binding.vars.retain(|var| var.name != name);
        if binding.source_index >= target_index {
            binding.source_index = target_index;
        }
    }
    new_bindings.retain(|binding| !binding.vars.is_empty());
    Some((new_lets, new_bindings))
}

fn render_repl_bindings(
    declarations: &[String],
    lets: &[String],
    bindings: &[ReplBinding],
) -> Vec<String> {
    let let_body = render_repl_setup(lets);
    let mut lines = Vec::new();
    for binding in bindings {
        for var in &binding.vars {
            let entry = format!("__irepl_binding_{}", lines.len());
            let source = format!(
                "{}\nfn {entry}() {{ {lets}{name} }}\n",
                declarations.join("\n"),
                lets = let_body,
                name = var.name,
            );
            let (value, ty) = match build_and_call_with_type_for_inspection(&source, &entry) {
                Ok((value, ty)) => (render_repl_binding_value(&value, &ty), ty.rendered),
                Err(msg) => (
                    format!("<error: {}>", msg.lines().next().unwrap_or("unknown")),
                    "<unknown>".to_string(),
                ),
            };
            let prefix = if var.mutable { "mut " } else { "" };
            lines.push(format!("{prefix}{}: {ty} = {value}", var.name));
        }
    }
    lines
}

fn repl_binding_info(
    declarations: &[String],
    lets: &[String],
    bindings: &[ReplBinding],
    name: &str,
) -> Option<std::result::Result<String, String>> {
    let var = bindings
        .iter()
        .flat_map(|binding| &binding.vars)
        .find(|var| var.name == name)?;
    Some(resolve_repl_binding(declarations, lets, name).map(|(_, ty)| {
        let can_mutate = binding_can_mutate(var, &ty);
        let capability = match (var.mutable, ty.references.as_slice(), can_mutate) {
            (_, [], true) => "mutable binding",
            (_, [], false) => "immutable binding",
            (_, _, true) => "mutable referent",
            (_, _, false) => "shared referent",
        };
        let mut out = format!("{} [binding]\n  type: {}\n  capability: {capability}\n", var.name, ty.rendered);
        let Some(ref owner) = ty.method_owner else {
            out.push_str(&format!(
                "\nNo cataloged methods for this binding's type.\nExample: let copy = {}",
                var.name
            ));
            return out;
        };
        if ty.fixed_array {
            out.push_str(&format!(
                "  method surface: fixed array (array and slice methods only; mutable methods require writable access)\n  Example: let first = {}[0]\n",
                var.name
            ));
        }
        let methods = available_repl_binding_methods(&ty, owner, can_mutate);
        let mut found = false;
        for method in methods {
            found = true;
            let signature = signature_suffix(&method.signature, &method.name);
            out.push_str(&format!(
                "{}.{}{signature} [method]\n    {}\n    Builtin\n    Example: {}.{}({})\n",
                var.name,
                method.name,
                method.doc,
                var.name,
                method.name,
                signature_argument_names(signature).join(", ")
            ));
        }
        if !found {
            out.push_str(&format!(
                "\nNo methods are available with this binding's capability.\nExample: let copy = {}",
                var.name
            ));
        }
        out.trim_end().to_string()
    }))
}

fn repl_binding_listing(
    declarations: &[String],
    lets: &[String],
    bindings: &[ReplBinding],
    name: &str,
) -> Option<std::result::Result<String, String>> {
    let var = bindings
        .iter()
        .flat_map(|binding| &binding.vars)
        .find(|var| var.name == name)?;
    Some(
        resolve_repl_binding(declarations, lets, name).map(|(_value, ty)| {
            let prefix = if var.mutable { "mut " } else { "" };
            let mut out = format!("{prefix}{name}: {} [binding]\n", ty.rendered);
            let Some(ref owner) = ty.method_owner else {
                return out.trim_end().to_string();
            };
            let can_mutate = binding_can_mutate(var, &ty);
            for method in available_repl_binding_methods(&ty, owner, can_mutate) {
                let signature = signature_suffix(&method.signature, &method.name);
                out.push_str(&format!("{name}.{}{signature} [method]\n", method.name));
            }
            out.trim_end().to_string()
        }),
    )
}

fn binding_can_mutate(var: &ReplBindingVar, ty: &ReplValueType) -> bool {
    if ty.references.is_empty() {
        var.mutable
    } else {
        ty.references
            .iter()
            .all(|mutability| *mutability == gossamer_types::Mutbl::Mut)
    }
}

fn available_repl_binding_methods(
    ty: &ReplValueType,
    owner: &str,
    can_mutate: bool,
) -> Vec<CoreMethodEntry> {
    core_method_entries()
        .into_iter()
        .filter(|method| {
            method.kind == "method"
                && method.owner == owner
                && (!ty.fixed_array || !gossamer_types::is_vec_only_sequence_method(&method.name))
                && (can_mutate || !gossamer_types::is_mutating_method_name(&method.name))
        })
        .collect()
}

fn resolve_repl_binding(
    declarations: &[String],
    lets: &[String],
    name: &str,
) -> std::result::Result<(gossamer_interp::Value, ReplValueType), String> {
    let let_body = render_repl_setup(lets);
    let entry = "__irepl_binding_info";
    let source = format!(
        "{}\nfn {entry}() {{ {lets}{name} }}\n",
        declarations.join("\n"),
        lets = let_body,
    );
    build_and_call_with_type_for_inspection(&source, entry)
}

fn repl_binding_from_let_source(input: &str) -> std::result::Result<ReplBinding, String> {
    use gossamer_ast::{ExprKind, ItemKind, StmtKind};

    // End the input before the synthetic closing brace so a trailing line
    // comment cannot consume it.
    let source = format!("fn __irepl_binding_names() {{ {input}\n}}\n");
    let mut map = gossamer_lex::SourceMap::new();
    let file = map.add_file("irepl-binding-names".to_string(), source.clone());
    let (sf, diags) = gossamer_parse::parse_source_file(&source, file);
    if !diags.is_empty() {
        return Err(format_parse_diags(&diags, &map, file));
    }
    let Some(item) = sf.items.first() else {
        return Err(repl_let_shape_error());
    };
    let ItemKind::Fn(decl) = &item.kind else {
        return Err(repl_let_shape_error());
    };
    let Some(body) = &decl.body else {
        return Err(repl_let_shape_error());
    };
    let ExprKind::Block(block) = &body.kind else {
        return Err(repl_let_shape_error());
    };
    if block.stmts.is_empty() {
        return Err(repl_let_shape_error());
    }
    let mut vars = Vec::new();
    let mut saw_let = false;
    for stmt in &block.stmts {
        let StmtKind::Let { pattern, init, .. } = &stmt.kind else {
            continue;
        };
        saw_let = true;
        if init.is_none() {
            return Err(repl_let_initializer_error());
        }
        collect_repl_pattern_bindings(pattern, &mut vars);
    }
    if !saw_let {
        return Err(repl_let_shape_error());
    }
    Ok(ReplBinding {
        vars,
        source_index: 0,
    })
}

fn repl_let_shape_error() -> String {
    "1 REPL input error:\n  malformed `let` input: expected one or more `let PAT = EXPR` statements"
        .to_string()
}

fn repl_let_initializer_error() -> String {
    "1 REPL input error:\n  malformed `let` input: missing `=` initializer; write `let PAT = EXPR`"
        .to_string()
}

fn collect_repl_pattern_bindings(pattern: &gossamer_ast::Pattern, out: &mut Vec<ReplBindingVar>) {
    use gossamer_ast::PatternKind;

    match &pattern.kind {
        PatternKind::Ident {
            mutability,
            name,
            subpattern,
        } => {
            out.push(ReplBindingVar {
                name: name.name.clone(),
                mutable: mutability.is_mutable(),
            });
            if let Some(subpattern) = subpattern {
                collect_repl_pattern_bindings(subpattern, out);
            }
        }
        PatternKind::Tuple(patterns) => {
            for pattern in patterns {
                collect_repl_pattern_bindings(pattern, out);
            }
        }
        PatternKind::Or(patterns) => {
            if let Some(pattern) = patterns.first() {
                collect_repl_pattern_bindings(pattern, out);
            }
        }
        PatternKind::Slice {
            prefix,
            rest,
            suffix,
        } => {
            for pattern in prefix {
                collect_repl_pattern_bindings(pattern, out);
            }
            if let Some(rest) = rest {
                collect_repl_pattern_bindings(rest, out);
            }
            for pattern in suffix {
                collect_repl_pattern_bindings(pattern, out);
            }
        }
        PatternKind::Struct { fields, .. } => {
            for field in fields {
                match &field.pattern {
                    Some(pattern) => collect_repl_pattern_bindings(pattern, out),
                    None => out.push(ReplBindingVar {
                        name: field.name.name.clone(),
                        mutable: false,
                    }),
                }
            }
        }
        PatternKind::TupleStruct { elems, .. } => {
            for pattern in elems {
                collect_repl_pattern_bindings(pattern, out);
            }
        }
        PatternKind::Ref { inner, .. } => collect_repl_pattern_bindings(inner, out),
        _ => {}
    }
}

fn split_meta_command(input: &str) -> (&str, &str) {
    input
        .split_once(char::is_whitespace)
        .map_or((input, ""), |(command, arg)| (command, arg.trim()))
}

fn input_is_declaration(input: &str) -> bool {
    let input = strip_leading_outer_attributes(input);
    let input = input
        .strip_prefix("pub ")
        .or_else(|| input.strip_prefix("pub(crate) "))
        .unwrap_or(input);
    input.starts_with("fn ")
        || input.starts_with("struct ")
        || input.starts_with("enum ")
        || input.starts_with("impl ")
        || input.starts_with("trait ")
        || input.starts_with("use ")
        || input.starts_with("const ")
        || input.starts_with("static ")
        || input.starts_with("type ")
}

/// Removes complete outer attributes from the beginning of a REPL input.
///
/// The REPL classifies declarations before rebuilding the accumulated source.
/// An attributed item still starts with `#`, so without this step it is
/// mistaken for an expression and wrapped in the synthetic REPL function.
fn strip_leading_outer_attributes(mut input: &str) -> &str {
    loop {
        input = input.trim_start();
        if !input.starts_with("#[") {
            return input;
        }

        let mut depth = 0usize;
        let mut quote = None;
        let mut escaped = false;
        let mut end = None;

        for (offset, ch) in input.char_indices() {
            if let Some(delimiter) = quote {
                if escaped {
                    escaped = false;
                } else if ch == '\\' {
                    escaped = true;
                } else if ch == delimiter {
                    quote = None;
                }
                continue;
            }

            match ch {
                '"' | '\'' => quote = Some(ch),
                '[' => depth += 1,
                ']' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        end = Some(offset + ch.len_utf8());
                        break;
                    }
                }
                _ => {}
            }
        }

        let Some(end) = end else {
            return input;
        };
        input = &input[end..];
    }
}

fn repl_info(arg: &str) -> std::result::Result<String, String> {
    let normalized = normalize_query(arg);
    if matches!(normalized, "std" | "std::") {
        return Ok(render_stdlib_dir());
    }
    if normalized.is_empty() {
        return repl_info_listing("");
    }
    let query = info_search_query(arg);
    if matching_modules(&query).is_empty() {
        if let Some(namespace) = canonical_stdlib_namespace(normalized) {
            let children = stdlib_namespace_children(&namespace);
            return Ok(render_stdlib_namespace_dir(&namespace, &children));
        }
    }
    // The catalog listing is the canonical module rendering. Omitting module
    // help here prevents `%i gzip` from printing the same module twice while
    // retaining matching items, methods, and types from the search.
    repl_info_matches(arg, true)
}

fn repl_info_listing(arg: &str) -> std::result::Result<String, String> {
    repl_info_matches(arg, false)
}

fn repl_info_matches(arg: &str, details: bool) -> std::result::Result<String, String> {
    if arg.is_empty() {
        return Ok(render_module_matches(
            gossamer_std::registry::modules(),
            details,
        ));
    }
    if let Some(pattern) = regex_argument(arg)? {
        let matches = render_catalog_matches(&pattern, details);
        return Ok(if matches == "no catalog matches" {
            format!("nothing found for `{arg}`")
        } else {
            matches
        });
    }

    let query = info_search_query(arg);
    let matches = render_catalog_query_matches(&query, details);
    if matches.is_empty() {
        Ok(format!("nothing found for `{arg}`"))
    } else {
        Ok(matches)
    }
}

fn stdlib_namespace_children(namespace: &str) -> Vec<StdModule> {
    let prefix = format!("{namespace}::");
    gossamer_std::registry::modules()
        .iter()
        .copied()
        .filter(|module| {
            module
                .path
                .strip_prefix(&prefix)
                .is_some_and(|path| !path.contains("::"))
        })
        .collect()
}

fn canonical_stdlib_namespace(query: &str) -> Option<String> {
    let canonical = if query.starts_with("std::") {
        query.to_string()
    } else {
        format!("std::{query}")
    };
    (!stdlib_namespace_children(&canonical).is_empty()).then_some(canonical)
}

fn render_repl_history(
    transcript: &[String],
    arg: &str,
) -> std::result::Result<Vec<String>, String> {
    let pattern = if arg.is_empty() {
        None
    } else {
        Some(compile_search_regex("history", arg)?)
    };
    Ok(transcript
        .iter()
        .filter(|entry| pattern.as_ref().is_none_or(|regex| regex.is_match(entry)))
        .cloned()
        .collect())
}

fn compile_search_regex(command: &str, query: &str) -> std::result::Result<Regex, String> {
    Regex::new(query).map_err(|error| format!("invalid {command} regex `{query}`: {error}"))
}

const REPL_PAGE_SIZE: usize = 20;

struct ListingOptions {
    pattern: String,
    all: bool,
    page: usize,
    details: bool,
}

fn parse_listing_options(command: &str, arg: &str) -> std::result::Result<ListingOptions, String> {
    let mut all = false;
    let mut page = 1usize;
    let mut details = false;
    let mut pattern = Vec::new();
    let mut words = arg.split_whitespace();
    while let Some(word) = words.next() {
        match word {
            "-a" | "--all" => all = true,
            "-d" | "--details" => details = true,
            "--page" | "-p" => {
                let Some(value) = words.next() else {
                    return Err(format!("usage: %{command} [pattern] [-a|--all] [--page N]"));
                };
                page = value
                    .parse()
                    .ok()
                    .filter(|page: &usize| *page > 0)
                    .ok_or_else(|| format!("%{command}: page must be a positive integer"))?;
            }
            _ => pattern.push(word),
        }
    }
    if all && page != 1 {
        return Err(format!("%{command}: use either -a or --page N"));
    }
    Ok(ListingOptions {
        pattern: pattern.join(" "),
        all,
        page,
        details,
    })
}

fn paginate_listing<T: std::fmt::Display + ?Sized>(
    entries: Vec<&T>,
    options: &ListingOptions,
    command: &str,
) -> Vec<String> {
    let total = entries.len();
    if options.all || total <= REPL_PAGE_SIZE {
        return entries.into_iter().map(ToString::to_string).collect();
    }
    let start = (options.page - 1).saturating_mul(REPL_PAGE_SIZE);
    if start >= total {
        return vec![format!(
            "no results on page {} ({} total)",
            options.page, total
        )];
    }
    let end = (start + REPL_PAGE_SIZE).min(total);
    let mut page: Vec<String> = entries[start..end]
        .iter()
        .map(ToString::to_string)
        .collect();
    let pattern = if options.pattern.is_empty() {
        String::new()
    } else {
        format!(" {}", options.pattern)
    };
    if end < total {
        page.push(format!(
            "({}-{} of {}) Use `{command}{pattern} -p {}` or `{command}{pattern} -a`.",
            start + 1,
            end,
            total,
            options.page + 1,
        ));
    }
    page
}

fn paginate_info(text: &str, options: &ListingOptions) -> String {
    let entries: Vec<&str> = text
        .split("\n\n")
        .filter(|entry| !entry.is_empty())
        .collect();
    let separator = if options.details { "\n\n" } else { "\n" };
    paginate_listing(entries, options, "%i").join(separator)
}

fn regex_argument(arg: &str) -> std::result::Result<Option<Regex>, String> {
    if !(arg.starts_with('/') && arg.ends_with('/') && arg.len() >= 2) {
        return Ok(None);
    }
    Regex::new(&arg[1..arg.len() - 1])
        .map(Some)
        .map_err(|e| format!("invalid regex `{arg}`: {e}"))
}

fn render_catalog_matches(pattern: &Regex, details: bool) -> String {
    let mut entries = Vec::new();
    for builtin in BUILTIN_MACROS {
        if pattern.is_match(builtin.name)
            || pattern.is_match(builtin.signature)
            || pattern.is_match(builtin.doc)
        {
            let mut entry = String::new();
            push_catalog_match(
                &mut entry,
                builtin.name,
                "macro",
                builtin.signature,
                builtin.doc,
                None,
                details,
            );
            entries.push(entry);
        }
    }
    for builtin in PRELUDE_BUILTINS {
        if pattern.is_match(builtin.name)
            || pattern.is_match(builtin.signature)
            || pattern.is_match(builtin.doc)
        {
            let mut entry = String::new();
            push_catalog_match(
                &mut entry,
                builtin.name,
                "builtin",
                builtin.signature,
                builtin.doc,
                None,
                details,
            );
            entries.push(entry);
        }
    }
    for owner in all_core_namespaces() {
        if pattern.is_match(&owner) {
            let mut entry = String::new();
            push_catalog_match(
                &mut entry,
                &owner,
                "type",
                "",
                "Built-in receiver and associated methods.",
                Some("Builtin"),
                details,
            );
            entries.push(entry);
        }
    }
    for method in core_method_entries() {
        let path = format!("{}::{}", method.owner, method.name);
        if pattern.is_match(&path)
            || pattern.is_match(&method.signature)
            || pattern.is_match(&method.doc)
        {
            let mut entry = String::new();
            push_core_method_match(&mut entry, &method, details);
            entries.push(entry);
        }
    }
    for module in gossamer_std::registry::modules() {
        if module_matches_regex(pattern, module) {
            let mut entry = String::new();
            push_module_match(&mut entry, module, details);
            entries.push(entry);
        }
        for item in module.items {
            if item_matches_regex(pattern, module, item) {
                let mut entry = String::new();
                push_item_match(&mut entry, module, item, details);
                entries.push(entry);
            }
        }
    }
    render_catalog_entries(entries, "no catalog matches")
}

fn render_catalog_query_matches(query: &str, details: bool) -> String {
    let mut entries = Vec::new();
    for builtin in matching_builtin_macros(query) {
        let mut entry = String::new();
        push_catalog_match(
            &mut entry,
            builtin.name,
            "macro",
            builtin.signature,
            builtin.doc,
            None,
            details,
        );
        entries.push(entry);
    }
    for builtin in matching_prelude_builtins(query) {
        let mut entry = String::new();
        push_catalog_match(
            &mut entry,
            builtin.name,
            "builtin",
            builtin.signature,
            builtin.doc,
            None,
            details,
        );
        entries.push(entry);
    }
    for owner in matching_core_namespaces(query) {
        let mut entry = String::new();
        push_catalog_match(
            &mut entry,
            &owner,
            "type",
            "",
            "Built-in receiver and associated methods.",
            Some("Builtin"),
            details,
        );
        entries.push(entry);
        for method in core_method_entries()
            .into_iter()
            .filter(|method| method.owner == owner)
        {
            let mut entry = String::new();
            push_core_method_match(&mut entry, &method, details);
            entries.push(entry);
        }
    }
    for method in matching_core_methods(query) {
        let mut entry = String::new();
        push_core_method_match(&mut entry, &method, details);
        entries.push(entry);
    }
    for module in matching_modules(query) {
        let mut entry = String::new();
        push_module_match(&mut entry, &module, details);
        entries.push(entry);
        // A matching module name is a namespace query, so include its public
        // contents. A qualified item query does not enter this branch and
        // remains focused on the requested symbol.
        for item in module.items {
            let mut entry = String::new();
            push_item_match(&mut entry, &module, item, details);
            entries.push(entry);
        }
    }
    for (module, item) in matching_items(query) {
        let mut entry = String::new();
        push_item_match(&mut entry, &module, &item, details);
        entries.push(entry);
    }
    render_catalog_entries(entries, "")
}

fn render_module_matches(modules: &[StdModule], details: bool) -> String {
    let mut entries = Vec::new();
    for module in modules {
        let mut entry = String::new();
        push_module_match(&mut entry, module, details);
        entries.push(entry);
    }
    render_catalog_entries(entries, "")
}

fn push_catalog_match(
    out: &mut String,
    path: &str,
    kind: &str,
    signature: &str,
    description: &str,
    defined_in: Option<&str>,
    details: bool,
) {
    out.push_str(path);
    if !signature.is_empty() {
        if let Some(suffix) = signature.strip_prefix(path) {
            out.push_str(suffix.trim_start());
        } else {
            out.push_str(signature);
        }
    }
    out.push_str(&format!(" [{}]\n", catalog_kind_label(kind)));
    if details {
        out.push_str(&format!("    {description}\n"));
        let defined_in = defined_in
            .filter(|location| !location.is_empty())
            .unwrap_or("Builtin");
        push_catalog_origin(out, defined_in);
        out.push_str(&format!(
            "    Example: {}\n",
            catalog_example(path, kind, signature)
        ));
    }
}

fn push_catalog_origin(out: &mut String, defined_in: &str) {
    if defined_in == "Builtin" {
        out.push_str("    Builtin\n");
    } else {
        out.push_str(&format!("    Defined in: {defined_in}\n"));
    }
}

fn catalog_kind_label(kind: &str) -> &str {
    if kind == "assoc" {
        "associated function"
    } else {
        kind
    }
}

fn catalog_example(path: &str, kind: &str, signature: &str) -> String {
    match path {
        "HashMap::from" => {
            return "let empty: HashMap<String, i64> = HashMap::from({}); let map: HashMap<String, i64> = HashMap::from({\"one\": 1, \"two\": 2}); let also = HashMap::from([(\"one\", 1), (\"two\", 2)])".to_string();
        }
        "HashSet::from" => {
            return "let set: HashSet<i64> = HashSet::from([1, 2, 2, 3])".to_string();
        }
        "Vec::from" => {
            return "let values: Vec<i64> = Vec::from([1, 2, 3])".to_string();
        }
        _ => {}
    }

    match kind {
        "module" => return format!("use {path}"),
        "type" => return format!("fn use_value(value: {path}) {{ }}"),
        "trait" => return format!("fn use_value<T: {path}>(value: T) {{ }}"),
        "const" => return format!("let value = {path}"),
        _ => {}
    }

    let args = signature_argument_names(signature).join(", ");
    if kind == "method" {
        let (owner, name) = path.rsplit_once("::").unwrap_or(("", path));
        return format!("{}.{}({args})", example_receiver(owner), name);
    }
    format!("{path}({args})")
}

fn example_receiver(owner: &str) -> &'static str {
    match owner.rsplit("::").next().unwrap_or(owner) {
        "String" | "str" => "\"text\"",
        "Vec" | "Slice" | "Array" => "values",
        "HashMap" | "BTreeMap" => "map",
        "HashSet" => "set",
        "VecDeque" => "queue",
        "Option" => "option",
        "Result" => "result",
        "Iterator" | "Range" => "iter",
        _ => "value",
    }
}

fn signature_argument_names(signature: &str) -> Vec<&str> {
    let Some(open) = signature.find('(') else {
        return Vec::new();
    };
    let mut depth = 0usize;
    let mut close = None;
    for (offset, ch) in signature[open + 1..].char_indices() {
        match ch {
            '(' | '[' | '{' | '<' => depth += 1,
            ')' if depth == 0 => {
                close = Some(open + 1 + offset);
                break;
            }
            ')' | ']' | '}' | '>' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    let Some(close) = close else {
        return Vec::new();
    };
    split_top_level_parameters(&signature[open + 1..close])
        .into_iter()
        .filter_map(|parameter| {
            let parameter = parameter.trim();
            let name = parameter
                .split_once(':')
                .map_or(parameter, |(name, _)| name);
            let name = name.trim().trim_end_matches('?');
            (!matches!(name, "self" | "&self" | "&mut self") && !name.is_empty()).then_some(name)
        })
        .collect()
}

fn split_top_level_parameters(parameters: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;
    for (offset, ch) in parameters.char_indices() {
        match ch {
            '(' | '[' | '{' | '<' => depth += 1,
            ')' | ']' | '}' | '>' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                parts.push(&parameters[start..offset]);
                start = offset + ch.len_utf8();
            }
            _ => {}
        }
    }
    if start < parameters.len() {
        parts.push(&parameters[start..]);
    }
    parts
}

fn signature_suffix<'a>(signature: &'a str, name: &str) -> &'a str {
    signature
        .strip_prefix(&format!("fn {name}"))
        .or_else(|| signature.strip_prefix(name))
        .unwrap_or(signature)
        .trim_start()
}

fn push_module_match(out: &mut String, module: &StdModule, details: bool) {
    push_catalog_match(
        out,
        module.path,
        "module",
        "",
        module.summary,
        Some(module.path),
        details,
    );
}

fn push_item_match(out: &mut String, module: &StdModule, item: &StdItem, details: bool) {
    let signature = gossamer_types::stdlib_function_signature(module.path, item.name)
        .map(|signature| signature_suffix(signature, item.name).to_string())
        .unwrap_or_default();
    push_catalog_match(
        out,
        &format!("{}::{}", module.path, item.name),
        item_kind_label(item.kind),
        &signature,
        item.doc,
        Some(module.path),
        details,
    );
}

fn push_core_method_match(out: &mut String, method: &CoreMethodEntry, details: bool) {
    let signature = signature_suffix(&method.signature, &method.name);
    push_catalog_match(
        out,
        &format!("{}::{}", method.owner, method.name),
        method.kind,
        signature,
        &method.doc,
        Some("Builtin"),
        details,
    );
}

fn render_catalog_entries(mut entries: Vec<String>, empty: &str) -> String {
    if entries.is_empty() {
        return empty.to_string();
    }
    entries.sort_unstable();
    entries.dedup();
    entries.join("\n").trim_end().to_string()
}

fn render_stdlib_dir() -> String {
    let mut entries = Vec::new();
    for namespace in stdlib_namespaces() {
        let mut entry = String::new();
        push_catalog_entry(
            &mut entry,
            &namespace,
            "module",
            "Standard-library namespace.",
        );
        entries.push(entry);
    }
    for module in gossamer_std::registry::modules() {
        let mut entry = String::new();
        push_catalog_entry(&mut entry, module.path, "module", module.summary);
        entries.push(entry);
    }
    entries.sort_unstable();
    entries.concat().trim_end().to_string()
}

fn stdlib_namespaces() -> Vec<String> {
    let modules = gossamer_std::registry::modules();
    let mut namespaces = modules
        .iter()
        .filter_map(|module| module.path.rsplit_once("::").map(|(parent, _)| parent))
        .filter(|parent| *parent != "std")
        .filter(|parent| !modules.iter().any(|module| module.path == *parent))
        .map(str::to_string)
        .collect::<Vec<_>>();
    namespaces.sort_unstable();
    namespaces.dedup();
    namespaces
}

fn render_stdlib_namespace_dir(namespace: &str, modules: &[StdModule]) -> String {
    let mut entries = Vec::with_capacity(modules.len() + 1);
    let mut namespace_entry = String::new();
    push_catalog_entry(
        &mut namespace_entry,
        namespace,
        "module",
        "Standard-library namespace.",
    );
    entries.push(namespace_entry);
    for module in modules {
        let mut entry = String::new();
        push_catalog_entry(&mut entry, module.path, "module", module.summary);
        entries.push(entry);
    }
    entries.sort_unstable();
    entries.concat().trim_end().to_string()
}

fn push_catalog_entry(out: &mut String, path: &str, kind: &str, description: &str) {
    out.push_str(&format!(
        "{path} [{kind}]\n  {description}\n  Example: {}\n\n",
        catalog_example(path, kind, "")
    ));
}

fn core_method_entries() -> Vec<CoreMethodEntry> {
    let mut entries = BTreeMap::<(String, String), CoreMethodEntry>::new();
    for method in CORE_METHODS {
        insert_core_method_entry(
            &mut entries,
            CoreMethodEntry {
                owner: method.owner.to_string(),
                name: method.name.to_string(),
                kind: method.kind,
                signature: method.signature.to_string(),
                doc: method.doc.to_string(),
            },
        );
    }
    add_data_last_std_methods(&mut entries, "Option", "std::option");
    add_data_last_std_methods(&mut entries, "Iterator", "std::iter");
    for registered in gossamer_interp::registered_names() {
        if let Some((owner, name)) = registered_core_method_path(registered) {
            if owner == "Iterator" && !gossamer_types::is_iterator_method(&name) {
                continue;
            }
            let kind = if runtime_assoc_name(&name) {
                "assoc"
            } else {
                "method"
            };
            // A runtime registration is authoritative evidence that the
            // option exists. Keep it visible even while its richer checker
            // signature metadata is being filled in. An empty signature is
            // truthful and renders as a method name, unlike the old `...`
            // placeholder that pretended to know an argument contract.
            let signature = runtime_core_method_signature(&owner, &name, kind).unwrap_or_default();
            let doc = runtime_core_method_doc(&owner, &name)
                .map_or_else(|| format!("Built-in {kind} on {owner}."), str::to_string);
            insert_core_method_entry(
                &mut entries,
                CoreMethodEntry {
                    owner: owner.clone(),
                    name: name.clone(),
                    kind,
                    signature,
                    doc,
                },
            );
        }
    }
    // Arrays and slices inherit only the canonical slice surface, not every
    // non-resizing Vec convenience or eager iterator combinator.
    let shared_sequence_methods: Vec<CoreMethodEntry> = entries
        .values()
        .filter(|method| {
            method.owner == "Vec"
                && method.kind == "method"
                && gossamer_types::is_slice_sequence_method(&method.name)
        })
        .cloned()
        .collect();
    for method in shared_sequence_methods {
        for owner in ["Array", "Slice"] {
            let mut derived = method.clone();
            derived.owner = owner.to_string();
            derived.signature = sequence_owner_signature(&derived.signature, owner);
            insert_core_method_entry(&mut entries, derived);
        }
    }
    insert_core_method_entry(
        &mut entries,
        CoreMethodEntry {
            owner: "Array".to_string(),
            name: "clone".to_string(),
            kind: "method",
            signature: "fn clone<T, const N: i64>(self: &[T; N]) -> [T; N]".to_string(),
            doc: "Returns a fixed-size copy of the array.".to_string(),
        },
    );
    entries.into_values().collect()
}

fn sequence_owner_signature(signature: &str, owner: &str) -> String {
    let shared_receiver = if owner == "Array" { "&[T; N]" } else { "&[T]" };
    let mutable_receiver = if owner == "Array" {
        "&mut [T; N]"
    } else {
        "&mut [T]"
    };
    signature
        .replace("self: &mut Vec<T>", &format!("self: {mutable_receiver}"))
        .replace("self: Vec<T>", &format!("self: {shared_receiver}"))
}

/// Exposes a data-last standard module as receiver methods without maintaining
/// a second signature or documentation table. Constructors whose final
/// parameter is not the collection value are excluded automatically.
fn add_data_last_std_methods(
    entries: &mut BTreeMap<(String, String), CoreMethodEntry>,
    owner: &str,
    module_path: &str,
) {
    let Some(module) = gossamer_std::registry::modules()
        .iter()
        .find(|module| module.path == module_path)
    else {
        return;
    };
    for item in module.items {
        if item.kind != StdItemKind::Function {
            continue;
        }
        if owner == "Iterator" && !gossamer_types::is_iterator_method(item.name) {
            continue;
        }
        let Some(signature) = data_last_method_signature(owner, module_path, item.name) else {
            continue;
        };
        insert_core_method_entry(
            entries,
            CoreMethodEntry {
                owner: owner.to_string(),
                name: item.name.to_string(),
                kind: "method",
                signature,
                doc: if owner == "Iterator" {
                    iterator_method_doc(item.name)
                        .unwrap_or(item.doc)
                        .to_string()
                } else {
                    item.doc.to_string()
                },
            },
        );
    }
}

fn data_last_method_signature(owner: &str, module_path: &str, name: &str) -> Option<String> {
    let row = gossamer_types::stdlib_function_signature(module_path, name)?;
    let shape = gossamer_types::stdlib_function_shape(module_path, name)?;
    let (receiver, leading) = shape.params.split_last()?;
    let receiver_matches = match owner {
        "Option" => receiver.ty.starts_with("Option<"),
        "Iterator" => receiver.ty.starts_with("Vec<"),
        _ => false,
    };
    if !receiver_matches {
        return None;
    }
    let name_start = row.find(name)?;
    let open = row[name_start..].find('(')? + name_start;
    let generics = row.get(name_start + name.len()..open)?;
    if owner == "Iterator" {
        if name == "chain" {
            return Some(
                "fn chain<T>(self: Iterator<T>, other: Iterator<T>) -> Iterator<T>".to_string(),
            );
        }
        if name == "zip" {
            return Some(
                "fn zip<A, B>(self: Iterator<A>, other: Iterator<B>) -> Iterator<(A, B)>"
                    .to_string(),
            );
        }
    }
    let receiver_ty = if owner == "Iterator" {
        receiver.ty.replacen("Vec<", "Iterator<", 1)
    } else {
        receiver.ty.to_string()
    };
    let mut params = vec![format!("self: {receiver_ty}")];
    if owner == "Iterator" && name == "fold" && leading.len() == 2 {
        params.push(format!("{}: {}", leading[1].name, leading[1].ty));
        params.push(format!("{}: {}", leading[0].name, leading[0].ty));
    } else {
        params.extend(
            leading
                .iter()
                .map(|param| format!("{}: {}", param.name, param.ty)),
        );
    }
    let return_ty = if owner == "Iterator" {
        match name {
            "take" | "skip" | "step_by" | "filter" | "rev" => {
                shape.return_ty.replacen("Vec<", "Iterator<", 1)
            }
            "enumerate" => "Iterator<(i64, T)>".to_string(),
            "chain" => "Iterator<T>".to_string(),
            "zip" => "Iterator<(A, B)>".to_string(),
            "map" => "Iterator<U>".to_string(),
            _ => shape.return_ty.to_string(),
        }
    } else {
        shape.return_ty.to_string()
    };
    Some(format!(
        "fn {name}{generics}({}) -> {}",
        params.join(", "),
        return_ty
    ))
}

fn iterator_method_doc(name: &str) -> Option<&'static str> {
    match name {
        "take" => Some("Returns a lazy iterator over at most the first n values."),
        "skip" => Some("Returns a lazy iterator that skips the first n values."),
        "step_by" => Some("Returns a lazy iterator yielding every step-th value."),
        "enumerate" => Some("Returns a lazy iterator of index and value pairs."),
        "chain" => Some("Returns a lazy iterator followed by another iterator."),
        "zip" => Some("Returns a lazy iterator pairing values from two iterators."),
        "map" => Some("Returns a lazy iterator that applies a function to each value."),
        "filter" => Some("Returns a lazy iterator containing values accepted by a predicate."),
        "rev" => Some("Returns a lazy iterator in reverse order."),
        _ => None,
    }
}

fn runtime_core_method_signature(owner: &str, name: &str, kind: &str) -> Option<String> {
    // These runtime-backed handle constructors are registered by the
    // interpreter rather than the stdlib function catalog. Keep their public
    // contracts here so `%info` never fabricates an ellipsis signature.
    if matches!(
        owner,
        "i8" | "i16" | "i32" | "i64" | "isize" | "u8" | "u16" | "u32" | "u64" | "usize"
    ) && matches!(name, "wrapping_add" | "wrapping_mul")
    {
        return Some(format!("fn {name}(self: {owner}, rhs: {owner}) -> {owner}"));
    }
    if let Some(signature) = match (owner, name) {
        ("Arc", "new") => Some("fn new<T>(value: T) -> Arc<T>"),
        ("Rc", "new") => Some("fn new<T>(value: T) -> Rc<T>"),
        ("Box", "new") => Some("fn new<T>(value: T) -> Box<T>"),
        ("AtomicBool", "new") => Some("fn new(value: bool) -> AtomicBool"),
        ("AtomicI32", "new") => Some("fn new(value: i64) -> AtomicI32"),
        ("AtomicI64", "new") => Some("fn new(value: i64) -> AtomicI64"),
        ("AtomicU64", "new") => Some("fn new(value: i64) -> AtomicU64"),
        ("Barrier", "new") => Some("fn new(parties: i64) -> Barrier"),
        ("Mutex", "new") => Some("fn new<T>(value: T) -> Mutex<T>"),
        ("RwLock", "new") => Some("fn new<T>(value: T) -> RwLock<T>"),
        ("Once", "new") => Some("fn new() -> Once"),
        ("WaitGroup", "new") => Some("fn new() -> WaitGroup"),
        ("Map" | "sync::Map", "new") => Some("fn new() -> sync::Map"),
        ("Map" | "sync::Map", "insert") => {
            Some("fn insert(self: &sync::Map, key: String, value: String) -> ()")
        }
        ("Map" | "sync::Map", "get") => {
            Some("fn get(self: &sync::Map, key: String) -> Option<String>")
        }
        ("Map" | "sync::Map", "remove") => Some("fn remove(self: &sync::Map, key: String) -> ()"),
        ("Map" | "sync::Map", "len") => Some("fn len(self: &sync::Map) -> i64"),
        ("Map" | "sync::Map", "contains_key") => {
            Some("fn contains_key(self: &sync::Map, key: String) -> bool")
        }
        ("Map" | "sync::Map", "keys") => Some("fn keys(self: &sync::Map) -> Vec<String>"),
        ("Errors" | "validate::Errors", "new") => Some("fn new() -> validate::Errors"),
        ("FieldError" | "validate::FieldError", "new") => {
            Some("fn new(path: String, message: String, code: String) -> validate::FieldError")
        }
        ("http::Client", "new") => Some("fn new() -> http::Client"),
        ("http::Client", "builder") => Some("fn builder() -> http::ClientBuilder"),
        _ => None,
    } {
        return Some(signature.to_string());
    }
    if owner == "String" && kind == "method" {
        if let Some(shape) = gossamer_types::stdlib_function_shape("std::strings", name) {
            let mut params = vec!["self: String".to_string()];
            params.extend(
                shape
                    .params
                    .iter()
                    .skip(1)
                    .map(|param| format!("{}: {}", param.name, param.ty)),
            );
            return Some(format!(
                "fn {name}({}) -> {}",
                params.join(", "),
                shape.return_ty
            ));
        }
        if let Some(signature) = match name {
            "byte_at" => Some("fn byte_at(self: String, index: i64) -> i64"),
            "byte_len" => Some("fn byte_len(self: String) -> i64"),
            "substring" => Some("fn substring(self: String, start: i64, end: i64) -> String"),
            _ => None,
        } {
            return Some(signature.to_string());
        }
    }
    None
}

#[allow(
    clippy::too_many_lines,
    reason = "flat metadata table keeps REPL core-method docs auditable"
)]
fn runtime_core_method_doc(owner: &str, name: &str) -> Option<&'static str> {
    if matches!(owner, "Map" | "sync::Map") {
        return match name {
            "new" => Some("Creates an empty concurrent string map."),
            "insert" => Some("Associates a string key with a string value."),
            "get" => Some("Returns the value for a key, or None when absent."),
            "remove" => Some("Removes a key and its value when present."),
            "len" => Some("Returns the number of entries."),
            "contains_key" => Some("Reports whether the map contains a key."),
            "keys" => Some("Returns a snapshot of the current keys."),
            _ => None,
        };
    }
    if matches!(
        owner,
        "i8" | "i16" | "i32" | "i64" | "isize" | "u8" | "u16" | "u32" | "u64" | "usize"
    ) {
        return match name {
            "wrapping_add" => {
                Some("Adds with two's-complement wrapping at this integer type's width.")
            }
            "wrapping_mul" => {
                Some("Multiplies with two's-complement wrapping at this integer type's width.")
            }
            _ => None,
        };
    }
    match (owner, name) {
        ("String", "byte_at") => Some("Returns the byte at an index, or -1 when out of range."),
        ("String", "byte_len") => Some("Returns the byte length of the string."),
        ("String", "bytes") => Some("Returns the UTF-8 bytes of the string."),
        ("String", "center") => Some("Pads both sides to the requested display width."),
        ("String", "chars") => Some("Returns the Unicode scalar values of the string."),
        ("String", "contains") => Some("Returns whether the string contains a substring."),
        ("String", "contains_any") => Some("Returns whether any character in the set appears."),
        ("String", "count") => Some("Counts non-overlapping substring occurrences."),
        ("String", "ends_with") => Some("Returns whether the string ends with a suffix."),
        ("String", "equal_fold") => Some("Compares strings with Unicode case folding."),
        ("String", "find") => Some("Returns the first byte index of a match."),
        ("String", "find_any") => Some("Returns the first byte index of any character in a set."),
        ("String", "index_rune") => Some("Returns the first byte index of a character."),
        ("String", "lines") => Some("Splits the string into lines."),
        ("String", "pad_left") => Some("Left-pads to the requested display width."),
        ("String", "pad_right") => Some("Right-pads to the requested display width."),
        ("String", "repeat") => Some("Repeats the string count times."),
        ("String", "replace") => Some("Replaces every occurrence of one pattern with another."),
        ("String", "replacen") => Some("Replaces at most n occurrences of a pattern."),
        ("String", "rfind") => Some("Returns the last byte index of a match."),
        ("String", "rfind_any") => Some("Returns the last byte index of any character in a set."),
        ("String", "rsplit_once") => Some("Splits once at the last matching separator."),
        ("String", "slice") => Some("Returns a checked byte-range slice."),
        ("String", "split") => Some("Splits on every matching separator."),
        ("String", "split_once") => Some("Splits once at the first matching separator."),
        ("String", "split_whitespace") => Some("Splits on runs of whitespace."),
        ("String", "splitn") => Some("Splits into at most n parts."),
        ("String", "starts_with") => Some("Returns whether the string starts with a prefix."),
        ("String", "strip_prefix") => Some("Removes a prefix when present."),
        ("String", "strip_suffix") => Some("Removes a suffix when present."),
        ("String", "substring") => Some("Returns a clamped character-range substring."),
        ("String", "to_bool") => Some("Parses exactly true or false to Option<bool>."),
        ("String", "to_f64") => Some("Parses the full string to Option<f64>."),
        ("String", "to_i64") => Some("Parses the full string to Option<i64>."),
        ("String", "to_lowercase") => Some("Lowercases every character."),
        ("String", "to_title") => Some("Title-cases the first letter of each word."),
        ("String", "to_uppercase") => Some("Uppercases every character."),
        ("String", "trim") => Some("Removes leading and trailing whitespace."),
        ("String", "trim_end") => Some("Removes trailing whitespace."),
        ("String", "trim_end_matches") => Some("Removes trailing characters from a set."),
        ("String", "trim_matches") => Some("Removes characters from a set at both ends."),
        ("String", "trim_start") => Some("Removes leading whitespace."),
        ("String", "trim_start_matches") => Some("Removes leading characters from a set."),
        ("Vec", "chain") => Some("Concatenates this sequence with another sequence."),
        ("Vec", "chunks") => Some("Groups values into fixed-size chunks."),
        ("Vec", "collect") => Some("Materializes the sequence as a vector."),
        ("Vec", "count") => Some("Counts values, or values accepted by a predicate."),
        ("Vec", "dedup") => Some("Removes adjacent duplicate values."),
        ("Vec", "enumerate") => Some("Pairs each value with its index."),
        ("Vec", "flatten") => Some("Flattens one level of nested vectors."),
        ("Vec", "for_each") => Some("Runs a closure for each value."),
        ("Vec", "max_by_key") => Some("Returns the maximum value by derived key."),
        ("Vec", "min_by_key") => Some("Returns the minimum value by derived key."),
        ("Vec", "pairwise") => Some("Returns adjacent value pairs."),
        ("Vec", "skip") => Some("Drops the first n values."),
        ("Vec", "step_by") => Some("Returns every nth value."),
        ("Vec", "take") => Some("Returns the first n values."),
        ("Vec", "windows") => Some("Returns overlapping fixed-size windows."),
        ("Vec", "zip") => Some("Pairs values with another sequence."),
        ("HashMap", "clear") => Some("Removes all entries."),
        ("HashMap", "inc") => Some("Increments an i64 counter value."),
        ("HashMap", "inc_at") => Some("Increments counters from a substring key range."),
        ("HashMap", "inc_batch") => Some("Increments counters for a batch of keys."),
        ("HashSet", "clear") => Some("Removes all values from the set."),
        ("HashSet", "is_disjoint") => Some("Returns true when two sets share no values."),
        ("HashSet", "is_empty") => Some("Returns true when the set has no values."),
        ("HashSet", "is_subset") => Some("Returns true when every value is in the other set."),
        ("HashSet", "is_superset") => Some("Returns true when the other set is a subset."),
        ("HashSet", "iter") => Some("Returns the set values as a vector."),
        ("HashSet", "len") => Some("Returns the number of values."),
        ("HashSet", "to_vec") => Some("Returns the set values as a vector."),
        ("VecDeque", "is_empty") => Some("Returns true when the deque has no values."),
        ("VecDeque", "len") => Some("Returns the number of values."),
        ("Option", "filter") => Some("Keeps Some only when a predicate accepts it."),
        ("Option", "flatten") => Some("Flattens a nested Option."),
        ("Option", "iter") => Some("Returns a zero-or-one element vector."),
        ("Option", "or") => Some("Returns the receiver when Some, otherwise a fallback."),
        ("Option", "or_else") => Some("Calls a fallback closure only when None."),
        ("Option", "unwrap_or") => Some("Returns the payload or a fallback value."),
        ("Option", "unwrap_or_else") => Some("Calls a fallback closure only when None."),
        ("Option", "zip") => Some("Combines two Options when both are Some."),
        ("Result", "and_then") => Some("Chains an Ok value through a Result-returning closure."),
        ("Result", "err") => Some("Converts the Err payload to Option."),
        ("Result", "ok") => Some("Converts the Ok payload to Option."),
        ("Result", "or_else") => Some("Calls a fallback closure only when Err."),
        ("Result", "unwrap_or") => Some("Returns the Ok payload or a fallback value."),
        ("Result", "unwrap_or_else") => Some("Calls a fallback closure only when Err."),
        _ => None,
    }
}

fn insert_core_method_entry(
    entries: &mut BTreeMap<(String, String), CoreMethodEntry>,
    entry: CoreMethodEntry,
) {
    entries
        .entry((entry.owner.clone(), entry.name.clone()))
        .or_insert(entry);
}

fn registered_core_method_path(path: &str) -> Option<(String, String)> {
    let (owner, name) = path.rsplit_once("::")?;
    if name.starts_with("__") || owner == "Type" {
        return None;
    }
    let owner = canonical_runtime_owner(owner)?;
    Some((owner, name.to_string()))
}

fn canonical_runtime_owner(owner: &str) -> Option<String> {
    let owner = owner.strip_prefix("collections::").unwrap_or(owner);
    let owner = match owner {
        "option" => "Option",
        "result" => "Result",
        "bytes::Buffer" => "Buffer",
        "bytes::Builder" => "Builder",
        other => other,
    };
    if matches!(
        owner,
        "i8" | "i16" | "i32" | "i64" | "isize" | "u8" | "u16" | "u32" | "u64" | "usize"
    ) {
        return Some(owner.to_string());
    }
    let last = owner.rsplit("::").next().unwrap_or(owner);
    if last.chars().next().is_some_and(char::is_uppercase) {
        Some(owner.to_string())
    } else {
        None
    }
}

fn runtime_assoc_name(name: &str) -> bool {
    matches!(
        name,
        "new"
            | "with_capacity"
            | "from"
            | "from_utf8"
            | "background"
            | "with_cancel"
            | "with_timeout"
            | "bind"
            | "connect"
            | "open"
            | "create"
            | "default"
            | "builder"
            | "object"
            | "Object"
            | "Array"
            | "String"
            | "Int"
            | "Float"
            | "Bool"
            | "Null"
    )
}

fn all_core_namespaces() -> Vec<String> {
    let mut owners = core_method_entries()
        .into_iter()
        .map(|method| method.owner)
        .collect::<Vec<_>>();
    owners.sort_unstable();
    owners.dedup();
    owners
}

fn matching_core_namespaces(query: &str) -> Vec<String> {
    all_core_namespaces()
        .into_iter()
        .filter(|owner| core_namespace_matches(owner, query))
        .collect()
}

fn matching_modules(query: &str) -> Vec<StdModule> {
    gossamer_std::registry::modules()
        .iter()
        .copied()
        .filter(|module| module_query_matches(module, query))
        .collect()
}

fn matching_items(query: &str) -> Vec<(StdModule, StdItem)> {
    let mut out = Vec::new();
    for module in gossamer_std::registry::modules() {
        for item in module.items {
            if item_query_matches(module, item, query) {
                out.push((*module, *item));
            }
        }
    }
    out
}

fn matching_core_methods(query: &str) -> Vec<CoreMethodEntry> {
    core_method_entries()
        .into_iter()
        .filter(|method| core_method_query_matches(method, query))
        .collect()
}

fn matching_builtin_macros(query: &str) -> Vec<&'static BuiltinMacro> {
    BUILTIN_MACROS
        .iter()
        .filter(|builtin| symbol_query_matches(builtin.name, query))
        .collect()
}

fn matching_prelude_builtins(query: &str) -> Vec<&'static PreludeBuiltinHelp> {
    PRELUDE_BUILTINS
        .iter()
        .filter(|builtin| symbol_query_matches(builtin.name, query))
        .collect()
}

fn module_query_matches(module: &StdModule, query: &str) -> bool {
    module_aliases(module.path)
        .iter()
        .any(|alias| symbol_query_matches(alias, query))
}

fn core_namespace_matches(owner: &str, query: &str) -> bool {
    let (query, substring) = split_symbol_query(query);
    if substring {
        owner
            .to_ascii_lowercase()
            .contains(&query.to_ascii_lowercase())
    } else {
        owner == query || owner.eq_ignore_ascii_case(query)
    }
}

fn item_query_matches(module: &StdModule, item: &StdItem, query: &str) -> bool {
    if symbol_query_matches(item.name, query) {
        return true;
    }
    module_aliases(module.path)
        .iter()
        .any(|alias| symbol_query_matches(&format!("{alias}::{}", item.name), query))
}

fn core_method_query_matches(method: &CoreMethodEntry, query: &str) -> bool {
    let (query, substring) = split_symbol_query(query);
    if substring {
        return method.name.contains(query)
            || format!("{}::{}", method.owner, method.name).contains(query)
            || core_lower_path(method).contains(query);
    }
    method.name == query
        || format!("{}::{}", method.owner, method.name) == query
        || core_lower_path(method) == query
        || query
            .rsplit_once("::")
            .is_some_and(|(owner, name)| name == method.name && owner == method.owner)
}

fn core_lower_path(method: &CoreMethodEntry) -> String {
    format!("{}::{}", method.owner.to_ascii_lowercase(), method.name)
}

fn info_search_query(arg: &str) -> String {
    let query = normalize_query(arg);
    if catalog_has_exact_match(query) {
        query.to_string()
    } else {
        format!("\0{query}")
    }
}

fn catalog_has_exact_match(query: &str) -> bool {
    !matching_builtin_macros(query).is_empty()
        || !matching_prelude_builtins(query).is_empty()
        || !matching_core_namespaces(query).is_empty()
        || !matching_core_methods(query).is_empty()
        || !matching_modules(query).is_empty()
        || !matching_items(query).is_empty()
}

fn symbol_query_matches(candidate: &str, query: &str) -> bool {
    let (query, substring) = split_symbol_query(query);
    if substring {
        candidate.contains(query)
    } else {
        candidate == query
    }
}

fn split_symbol_query(query: &str) -> (&str, bool) {
    query
        .strip_prefix('\0')
        .map_or((query, false), |query| (query, true))
}

fn module_matches_regex(pattern: &Regex, module: &StdModule) -> bool {
    pattern.is_match(module.path) || pattern.is_match(module.summary)
}

fn item_matches_regex(pattern: &Regex, module: &StdModule, item: &StdItem) -> bool {
    pattern.is_match(&format!("{}::{}", module.path, item.name))
        || pattern.is_match(item.name)
        || pattern.is_match(item.doc)
}

fn module_aliases(path: &'static str) -> Vec<&'static str> {
    let mut aliases = vec![path];
    if let Some(stripped) = path.strip_prefix("std::") {
        aliases.push(stripped);
    }
    if let Some(last) = path.rsplit("::").next()
        && !aliases.contains(&last)
    {
        aliases.push(last);
    }
    aliases
}

fn normalize_query(arg: &str) -> &str {
    arg.trim_matches('`').trim()
}

fn item_kind_label(kind: StdItemKind) -> &'static str {
    match kind {
        StdItemKind::Function => "fn",
        StdItemKind::Type => "type",
        StdItemKind::Trait => "trait",
        StdItemKind::Macro => "macro",
        StdItemKind::Const => "const",
    }
}

/// True for assignment, built-in mutating collection calls, or loop forms that
/// can mutate through a mutable reference.
/// Type checking remains authoritative for receiver mutability and whether
/// the method exists. This parser-only classification only controls replay.
fn input_mutates_binding(input: &str, user_mutating_methods: &HashSet<String>) -> bool {
    use gossamer_ast::{ExprKind, ItemKind, StmtKind};

    // End the input before the synthetic closing brace so a trailing line
    // comment cannot consume it.
    let source = format!("fn __irepl_classify() {{ {input}\n}}\n");
    let mut map = gossamer_lex::SourceMap::new();
    let file = map.add_file("irepl-classify".to_string(), source.clone());
    let (sf, diags) = gossamer_parse::parse_source_file(&source, file);
    if !diags.is_empty() {
        return false;
    }
    let Some(item) = sf.items.first() else {
        return false;
    };
    let ItemKind::Fn(decl) = &item.kind else {
        return false;
    };
    let Some(body) = &decl.body else {
        return false;
    };
    let ExprKind::Block(block) = &body.kind else {
        return false;
    };
    let target = block.tail.as_deref().or_else(|| match block.stmts.last() {
        Some(stmt) => match &stmt.kind {
            StmtKind::Expr { expr, .. } => Some(expr.as_ref()),
            _ => None,
        },
        None => None,
    });

    target.is_some_and(|expr| repl_expr_mutates_binding(expr, user_mutating_methods))
}

fn collect_repl_mut_self_method_names(declarations: &[String]) -> HashSet<String> {
    use gossamer_ast::{FnParam, ImplItem, ItemKind, Receiver};

    let source = declarations.join("\n");
    if source.trim().is_empty() {
        return HashSet::new();
    }
    let mut map = gossamer_lex::SourceMap::new();
    let file = map.add_file("irepl-mut-self-methods".to_string(), source.clone());
    let (sf, diags) = gossamer_parse::parse_source_file(&source, file);
    if !diags.is_empty() {
        return HashSet::new();
    }
    let mut names = HashSet::new();
    for item in sf.items {
        let ItemKind::Impl(decl) = item.kind else {
            continue;
        };
        for item in decl.items {
            let ImplItem::Fn(method) = item else {
                continue;
            };
            if matches!(
                method.params.first(),
                Some(FnParam::Receiver(Receiver::RefMut))
            ) {
                names.insert(method.name.name);
            }
        }
    }
    names
}

fn repl_stmt_mutates_binding(
    stmt: &gossamer_ast::Stmt,
    user_mutating_methods: &HashSet<String>,
) -> bool {
    use gossamer_ast::StmtKind;

    match &stmt.kind {
        StmtKind::Let { init, .. } => init
            .as_deref()
            .is_some_and(|expr| repl_expr_mutates_binding(expr, user_mutating_methods)),
        StmtKind::Expr { expr, .. } | StmtKind::Defer(expr) | StmtKind::Go(expr) => {
            repl_expr_mutates_binding(expr, user_mutating_methods)
        }
        StmtKind::Item(_) => false,
    }
}

fn repl_select_op_contains_ref_mut(op: &gossamer_ast::expr::SelectOp) -> bool {
    use gossamer_ast::expr::SelectOp;

    match op {
        SelectOp::Recv { channel, .. } => repl_expr_contains_ref_mut(channel),
        SelectOp::Send { channel, value } => {
            repl_expr_contains_ref_mut(channel) || repl_expr_contains_ref_mut(value)
        }
        SelectOp::Default => false,
    }
}

fn repl_stmt_contains_ref_mut(stmt: &gossamer_ast::Stmt) -> bool {
    use gossamer_ast::StmtKind;

    match &stmt.kind {
        StmtKind::Let { init, .. } => init.as_deref().is_some_and(repl_expr_contains_ref_mut),
        StmtKind::Expr { expr, .. } | StmtKind::Defer(expr) | StmtKind::Go(expr) => {
            repl_expr_contains_ref_mut(expr)
        }
        StmtKind::Item(_) => false,
    }
}

fn repl_expr_contains_ref_mut(expr: &gossamer_ast::Expr) -> bool {
    use gossamer_ast::ExprKind;
    use gossamer_ast::common::UnaryOp;

    match &expr.kind {
        ExprKind::Unary {
            op: UnaryOp::RefMut,
            ..
        } => true,
        ExprKind::Call { callee, args } => {
            repl_expr_contains_ref_mut(callee) || args.iter().any(repl_expr_contains_ref_mut)
        }
        ExprKind::MethodCall { receiver, args, .. } => {
            repl_expr_contains_ref_mut(receiver) || args.iter().any(repl_expr_contains_ref_mut)
        }
        ExprKind::FieldAccess { receiver, .. } => repl_expr_contains_ref_mut(receiver),
        ExprKind::Index { base, index } => {
            repl_expr_contains_ref_mut(base) || repl_expr_contains_ref_mut(index)
        }
        ExprKind::Unary { operand, .. } => repl_expr_contains_ref_mut(operand),
        ExprKind::Binary { lhs, rhs, .. } => {
            repl_expr_contains_ref_mut(lhs) || repl_expr_contains_ref_mut(rhs)
        }
        ExprKind::Assign { place, value, .. } => {
            repl_expr_contains_ref_mut(place) || repl_expr_contains_ref_mut(value)
        }
        ExprKind::Cast { value, .. } | ExprKind::Try(value) | ExprKind::Go(value) => {
            repl_expr_contains_ref_mut(value)
        }
        ExprKind::If {
            condition,
            then_branch,
            else_branch,
        } => {
            repl_expr_contains_ref_mut(condition)
                || repl_expr_contains_ref_mut(then_branch)
                || else_branch
                    .as_deref()
                    .is_some_and(repl_expr_contains_ref_mut)
        }
        ExprKind::Match { scrutinee, arms } => {
            repl_expr_contains_ref_mut(scrutinee)
                || arms.iter().any(|arm| {
                    arm.guard.as_ref().is_some_and(repl_expr_contains_ref_mut)
                        || repl_expr_contains_ref_mut(&arm.body)
                })
        }
        ExprKind::Loop { body, .. } => repl_expr_contains_ref_mut(body),
        ExprKind::While {
            condition, body, ..
        } => repl_expr_contains_ref_mut(condition) || repl_expr_contains_ref_mut(body),
        ExprKind::For { iter, body, .. } => {
            repl_expr_contains_ref_mut(iter) || repl_expr_contains_ref_mut(body)
        }
        ExprKind::Block(block) | ExprKind::Unsafe(block) => {
            block.stmts.iter().any(repl_stmt_contains_ref_mut)
                || block
                    .tail
                    .as_deref()
                    .is_some_and(repl_expr_contains_ref_mut)
        }
        ExprKind::Closure { body, .. } => repl_expr_contains_ref_mut(body),
        ExprKind::Return(value) => value.as_deref().is_some_and(repl_expr_contains_ref_mut),
        ExprKind::Break { value, .. } => value.as_deref().is_some_and(repl_expr_contains_ref_mut),
        ExprKind::Tuple(elems) => elems.iter().any(repl_expr_contains_ref_mut),
        ExprKind::Struct { fields, base, .. } => {
            fields
                .iter()
                .any(|field| field.value.as_ref().is_some_and(repl_expr_contains_ref_mut))
                || base.as_deref().is_some_and(repl_expr_contains_ref_mut)
        }
        ExprKind::Array(array) => repl_array_expr_contains_ref_mut(array),
        ExprKind::Range { start, end, .. } => {
            start.as_deref().is_some_and(repl_expr_contains_ref_mut)
                || end.as_deref().is_some_and(repl_expr_contains_ref_mut)
        }
        ExprKind::Select(arms) => arms.iter().any(|arm| {
            repl_select_op_contains_ref_mut(&arm.op) || repl_expr_contains_ref_mut(&arm.body)
        }),
        ExprKind::MacroCall(_)
        | ExprKind::Literal(_)
        | ExprKind::Path(_)
        | ExprKind::Continue { .. }
        | ExprKind::Error => false,
    }
}

fn repl_array_expr_contains_ref_mut(array: &gossamer_ast::expr::ArrayExpr) -> bool {
    match array {
        gossamer_ast::expr::ArrayExpr::List(elems) => elems.iter().any(repl_expr_contains_ref_mut),
        gossamer_ast::expr::ArrayExpr::Repeat { value, count } => {
            repl_expr_contains_ref_mut(value) || repl_expr_contains_ref_mut(count)
        }
    }
}

fn repl_expr_mutates_binding(
    expr: &gossamer_ast::Expr,
    user_mutating_methods: &HashSet<String>,
) -> bool {
    use gossamer_ast::ExprKind;

    match &expr.kind {
        ExprKind::Assign { .. } => true,
        ExprKind::MethodCall {
            receiver,
            name,
            args,
            ..
        } => {
            gossamer_types::is_mutating_method_name(&name.name)
                || user_mutating_methods.contains(&name.name)
                || repl_expr_mutates_binding(receiver, user_mutating_methods)
                || args
                    .iter()
                    .any(|arg| repl_expr_mutates_binding(arg, user_mutating_methods))
        }
        ExprKind::Call { callee, args } => {
            repl_callee_is_mutating_name(callee)
                || repl_expr_mutates_binding(callee, user_mutating_methods)
                || args
                    .iter()
                    .any(|arg| repl_expr_mutates_binding(arg, user_mutating_methods))
        }
        ExprKind::For { iter, body, .. } => {
            repl_expr_contains_ref_mut(iter)
                || repl_expr_mutates_binding(body, user_mutating_methods)
        }
        ExprKind::Block(block) | ExprKind::Unsafe(block) => {
            repl_block_mutates_binding(block, user_mutating_methods)
        }
        ExprKind::If {
            condition,
            then_branch,
            else_branch,
        } => repl_if_mutates_binding(
            condition,
            then_branch,
            else_branch.as_deref(),
            user_mutating_methods,
        ),
        ExprKind::Match { scrutinee, arms } => {
            repl_match_mutates_binding(scrutinee, arms, user_mutating_methods)
        }
        ExprKind::Loop { body, .. } => repl_expr_mutates_binding(body, user_mutating_methods),
        ExprKind::While {
            condition, body, ..
        } => repl_pair_mutates_binding(condition, body, user_mutating_methods),
        ExprKind::FieldAccess { receiver, .. } => {
            repl_expr_mutates_binding(receiver, user_mutating_methods)
        }
        ExprKind::Index { base, index } => {
            repl_expr_mutates_binding(base, user_mutating_methods)
                || repl_expr_mutates_binding(index, user_mutating_methods)
        }
        ExprKind::Unary { operand, .. } => {
            repl_expr_mutates_binding(operand, user_mutating_methods)
        }
        ExprKind::Binary { lhs, rhs, .. } => {
            repl_expr_mutates_binding(lhs, user_mutating_methods)
                || repl_expr_mutates_binding(rhs, user_mutating_methods)
        }
        ExprKind::Cast { value, .. } | ExprKind::Try(value) | ExprKind::Go(value) => {
            repl_expr_mutates_binding(value, user_mutating_methods)
        }
        ExprKind::Closure { body, .. } => repl_expr_mutates_binding(body, user_mutating_methods),
        ExprKind::Return(value) => value
            .as_deref()
            .is_some_and(|expr| repl_expr_mutates_binding(expr, user_mutating_methods)),
        ExprKind::Break { value, .. } => value
            .as_deref()
            .is_some_and(|expr| repl_expr_mutates_binding(expr, user_mutating_methods)),
        ExprKind::Tuple(elems) => elems
            .iter()
            .any(|expr| repl_expr_mutates_binding(expr, user_mutating_methods)),
        ExprKind::Struct { fields, base, .. } => {
            repl_struct_expr_mutates_binding(fields, base.as_deref(), user_mutating_methods)
        }
        ExprKind::Array(array) => repl_array_expr_mutates_binding(array, user_mutating_methods),
        ExprKind::Range { start, end, .. } => repl_optional_pair_mutates_binding(
            start.as_deref(),
            end.as_deref(),
            user_mutating_methods,
        ),
        ExprKind::Select(arms) => arms
            .iter()
            .any(|arm| repl_expr_mutates_binding(&arm.body, user_mutating_methods)),
        ExprKind::Literal(_)
        | ExprKind::Path(_)
        | ExprKind::Continue { .. }
        | ExprKind::MacroCall(_)
        | ExprKind::Error => false,
    }
}

fn repl_block_mutates_binding(
    block: &gossamer_ast::expr::Block,
    user_mutating_methods: &HashSet<String>,
) -> bool {
    block
        .stmts
        .iter()
        .any(|stmt| repl_stmt_mutates_binding(stmt, user_mutating_methods))
        || block
            .tail
            .as_deref()
            .is_some_and(|expr| repl_expr_mutates_binding(expr, user_mutating_methods))
}

fn repl_if_mutates_binding(
    condition: &gossamer_ast::Expr,
    then_branch: &gossamer_ast::Expr,
    else_branch: Option<&gossamer_ast::Expr>,
    user_mutating_methods: &HashSet<String>,
) -> bool {
    repl_pair_mutates_binding(condition, then_branch, user_mutating_methods)
        || else_branch.is_some_and(|expr| repl_expr_mutates_binding(expr, user_mutating_methods))
}

fn repl_match_mutates_binding(
    scrutinee: &gossamer_ast::Expr,
    arms: &[gossamer_ast::expr::MatchArm],
    user_mutating_methods: &HashSet<String>,
) -> bool {
    repl_expr_mutates_binding(scrutinee, user_mutating_methods)
        || arms.iter().any(|arm| {
            arm.guard
                .as_ref()
                .is_some_and(|expr| repl_expr_mutates_binding(expr, user_mutating_methods))
                || repl_expr_mutates_binding(&arm.body, user_mutating_methods)
        })
}

fn repl_pair_mutates_binding(
    left: &gossamer_ast::Expr,
    right: &gossamer_ast::Expr,
    user_mutating_methods: &HashSet<String>,
) -> bool {
    repl_expr_mutates_binding(left, user_mutating_methods)
        || repl_expr_mutates_binding(right, user_mutating_methods)
}

fn repl_optional_pair_mutates_binding(
    left: Option<&gossamer_ast::Expr>,
    right: Option<&gossamer_ast::Expr>,
    user_mutating_methods: &HashSet<String>,
) -> bool {
    left.is_some_and(|expr| repl_expr_mutates_binding(expr, user_mutating_methods))
        || right.is_some_and(|expr| repl_expr_mutates_binding(expr, user_mutating_methods))
}

fn repl_struct_expr_mutates_binding(
    fields: &[gossamer_ast::expr::StructExprField],
    base: Option<&gossamer_ast::Expr>,
    user_mutating_methods: &HashSet<String>,
) -> bool {
    fields.iter().any(|field| {
        field
            .value
            .as_ref()
            .is_some_and(|expr| repl_expr_mutates_binding(expr, user_mutating_methods))
    }) || base.is_some_and(|expr| repl_expr_mutates_binding(expr, user_mutating_methods))
}

fn repl_array_expr_mutates_binding(
    array: &gossamer_ast::expr::ArrayExpr,
    user_mutating_methods: &HashSet<String>,
) -> bool {
    match array {
        gossamer_ast::expr::ArrayExpr::List(elems) => elems
            .iter()
            .any(|expr| repl_expr_mutates_binding(expr, user_mutating_methods)),
        gossamer_ast::expr::ArrayExpr::Repeat { value, count } => {
            repl_expr_mutates_binding(value, user_mutating_methods)
                || repl_expr_mutates_binding(count, user_mutating_methods)
        }
    }
}

fn repl_callee_is_mutating_name(callee: &gossamer_ast::Expr) -> bool {
    let gossamer_ast::ExprKind::Path(path) = &callee.kind else {
        return false;
    };
    path.segments
        .last()
        .is_some_and(|segment| gossamer_types::is_mutating_method_name(&segment.name.name))
}

/// Validates that the accumulated declarations parse, resolve, and
/// compile onto the VM. The built `Vm` is discarded - the REPL keeps
/// declarations as source strings and full-recompiles each input - so
/// this is purely a probe: `Ok(())` means the declaration set is
/// loadable, `Err` rolls back the just-added declaration.
fn rebuild_session(declarations: &[String]) -> std::result::Result<(), String> {
    // Parse declarations before appending the synthetic probe function. A
    // missing item body must point at the user's end of input, not at the
    // generated `fn __irepl_probe` that follows it.
    let declarations_source = declarations.join("\n");
    let mut declarations_map = gossamer_lex::SourceMap::new();
    let declarations_file = declarations_map.add_file(
        "irepl-declarations".to_string(),
        declarations_source.clone(),
    );
    let (_, declaration_diags) =
        gossamer_parse::parse_source_file(&declarations_source, declarations_file);
    if !declaration_diags.is_empty() {
        return Err(format_parse_diags(
            &declaration_diags,
            &declarations_map,
            declarations_file,
        ));
    }

    let source = declarations.join("\n") + "\nfn __irepl_probe() { }\n";
    let source = gossamer_parse::autoderive::augment_source(&source);
    let mut map = gossamer_lex::SourceMap::new();
    let file = map.add_file("irepl".to_string(), source.clone());
    let (sf, parse_diags) = gossamer_parse::autoderive::parse_with_autoderive(&source, file);
    if !parse_diags.is_empty() {
        return Err(format_parse_diags(&parse_diags, &map, file));
    }
    let (res, resolve_diags) = gossamer_resolve::resolve_source_file(&sf);
    if !resolve_diags.is_empty() {
        return Err(format_resolve_diags(&sf, &resolve_diags));
    }
    let mut tcx = gossamer_types::TyCtxt::new();
    let (tbl, type_diags) = gossamer_types::typecheck_source_file(&sf, &res, &mut tcx);
    if !type_diags.is_empty() {
        return Err(format_semantic_diags("type", &type_diags));
    }
    let program = gossamer_hir::lower_source_file(&sf, &res, &tbl, &mut tcx);
    let mut vm = gossamer_interp::Vm::new();
    vm.load(&program, tcx, true).map_err(|e| format!("{e}"))?;
    Ok(())
}

fn build_and_call(
    source: &str,
    entry: &str,
) -> std::result::Result<gossamer_interp::Value, String> {
    build_and_call_with_type_inner(source, entry, false).map(|(value, _)| value)
}

fn build_and_call_with_type_for_inspection(
    source: &str,
    entry: &str,
) -> std::result::Result<(gossamer_interp::Value, ReplValueType), String> {
    build_and_call_with_type_inner(source, entry, true)
}

fn build_and_call_with_type_inner(
    source: &str,
    entry: &str,
    inspection: bool,
) -> std::result::Result<(gossamer_interp::Value, ReplValueType), String> {
    let source = gossamer_parse::autoderive::augment_source(source);
    let mut map = gossamer_lex::SourceMap::new();
    let file = map.add_file("irepl".to_string(), source.clone());
    let (sf, parse_diags) = gossamer_parse::autoderive::parse_with_autoderive(&source, file);
    if !parse_diags.is_empty() {
        return Err(format_parse_diags(&parse_diags, &map, file));
    }
    let (res, resolve_diags) = gossamer_resolve::resolve_source_file(&sf);
    if !resolve_diags.is_empty() {
        return Err(format_resolve_diags(&sf, &resolve_diags));
    }
    let mut tcx = gossamer_types::TyCtxt::new();
    let (tbl, type_diags) = if inspection {
        gossamer_types::typecheck_source_file_for_repl_inspection(&sf, &res, &mut tcx)
    } else {
        gossamer_types::typecheck_source_file(&sf, &res, &mut tcx)
    };
    // REPL expressions are installed as the tail of a generated function with
    // no written return annotation. The REPL deliberately returns that value
    // as REPL output, so its tail is neither discarded nor a user error.
    // Suppress only that exact generated-body diagnostic, never one from the
    // submitted expression's children or declarations.
    let tail_span = repl_generated_body_span(&sf);
    let user_type_diags: Vec<_> = type_diags
        .iter()
        .filter(|diag| !is_implicit_repl_tail_diag(diag, tail_span))
        .collect();
    if !user_type_diags.is_empty() {
        return Err(format_semantic_diags("type", &user_type_diags));
    }
    let tail_ty_id = repl_generated_tail_expr(&sf).and_then(|expr| tbl.get(expr.id));
    let tail_ty = tail_ty_id.map_or_else(ReplValueType::unknown, |ty| {
        ReplValueType::from_ty(&tcx, ty)
    });
    let mut program = gossamer_hir::lower_source_file(&sf, &res, &tbl, &mut tcx);
    // Generated REPL functions intentionally return their tail even though
    // the user did not write a return annotation. Keep the HIR signature in
    // sync with that inferred tail type so non-inlined calls return aggregate
    // values instead of applying the ordinary implicit-unit ABI.
    if let Some(tail_ty_id) = tail_ty_id {
        for item in &mut program.items {
            if let gossamer_hir::HirItemKind::Fn(function) = &mut item.kind
                && function.name.name == entry
            {
                function.ret = Some(tail_ty_id);
                break;
            }
        }
    }
    let mut vm = gossamer_interp::Vm::new();
    vm.load(&program, tcx, true).map_err(|e| format!("{e}"))?;
    let result =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| vm.call(entry, Vec::new())))
            .map_err(repl_panic_message)?;
    result
        .map(|value| (value, tail_ty))
        .map_err(|e| format!("{e}"))
}

fn repl_panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    let message = payload
        .downcast_ref::<&str>()
        .map(|message| (*message).to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic".to_string());
    format!("panic: {message}")
}

fn repl_generated_tail_expr(sf: &gossamer_ast::SourceFile) -> Option<&gossamer_ast::Expr> {
    use gossamer_ast::{ExprKind, ItemKind};

    sf.items.iter().find_map(|item| {
        let ItemKind::Fn(decl) = &item.kind else {
            return None;
        };
        if !decl.name.name.starts_with("__irepl_") {
            return None;
        }
        let body = decl.body.as_ref()?;
        let ExprKind::Block(block) = &body.kind else {
            return None;
        };
        block.tail.as_deref()
    })
}

fn repl_generated_body_span(sf: &gossamer_ast::SourceFile) -> Option<gossamer_lex::Span> {
    use gossamer_ast::ItemKind;

    sf.items.iter().find_map(|item| {
        let ItemKind::Fn(decl) = &item.kind else {
            return None;
        };
        if !decl.name.name.starts_with("__irepl_") {
            return None;
        }
        decl.body.as_ref().map(|body| body.span)
    })
}

fn is_implicit_repl_tail_diag(
    diag: &gossamer_types::TypeDiagnostic,
    tail_span: Option<gossamer_lex::Span>,
) -> bool {
    match (&diag.error, tail_span) {
        (gossamer_types::TypeError::TypeMismatch { expected, .. }, Some(span)) => {
            expected == "()" && diag.span == span
        }
        (gossamer_types::TypeError::DiscardedResult, Some(span)) => diag.span == span,
        _ => false,
    }
}

/// Renders hard resolver/type-checker failures before the REPL can lower a
/// program.  Keeping this gate here is essential: lowering after a rejected
/// call used to let missing or wrongly typed arguments reach permissive
/// runtime shims, which then silently substituted defaults.
fn format_semantic_diags<T: std::fmt::Display>(phase: &str, diags: &[T]) -> String {
    let noun = if diags.len() == 1 { "error" } else { "errors" };
    let mut out = format!("{} {phase} {noun}:\n", diags.len());
    for diag in diags {
        out.push_str("  ");
        out.push_str(&diag.to_string());
        out.push('\n');
    }
    out.pop();
    out
}

fn format_resolve_diags(
    sf: &gossamer_ast::SourceFile,
    diags: &[gossamer_resolve::ResolveDiagnostic],
) -> String {
    let in_scope = collect_source_file_names(sf);
    let structured = diags
        .iter()
        .map(|diag| diag.to_diagnostic(&in_scope))
        .collect::<Vec<_>>();
    format_structured_semantic_diags("resolution", &structured)
}

fn collect_source_file_names(sf: &gossamer_ast::SourceFile) -> Vec<&str> {
    use gossamer_ast::ItemKind;

    let mut out = Vec::new();
    for item in &sf.items {
        let name = match &item.kind {
            ItemKind::Fn(decl) => decl.name.name.as_str(),
            ItemKind::Struct(decl) => decl.name.name.as_str(),
            ItemKind::Enum(decl) => decl.name.name.as_str(),
            ItemKind::Trait(decl) => decl.name.name.as_str(),
            ItemKind::TypeAlias(decl) => decl.name.name.as_str(),
            ItemKind::Const(decl) => decl.name.name.as_str(),
            ItemKind::Static(decl) => decl.name.name.as_str(),
            ItemKind::Mod(decl) => decl.name.name.as_str(),
            ItemKind::Impl(_) | ItemKind::AttrItem(_) => continue,
        };
        out.push(name);
    }
    out
}

fn format_structured_semantic_diags(
    phase: &str,
    diags: &[gossamer_diagnostics::Diagnostic],
) -> String {
    let noun = if diags.len() == 1 { "error" } else { "errors" };
    let mut out = format!("{} {phase} {noun}:\n", diags.len());
    for diag in diags {
        out.push_str("  ");
        out.push_str(&diag.title);
        out.push('\n');
        for help in &diag.helps {
            out.push_str("  help: ");
            out.push_str(help);
            out.push('\n');
        }
    }
    out.pop();
    out
}

/// Renders a parse-diagnostic batch as one human-readable line per
/// error, prefixed by the count, so REPL users see *what* went wrong
/// instead of just "N parse error(s)". Each entry is annotated with
/// the one-based line / column derived from the source map.
fn format_parse_diags(
    diags: &[gossamer_parse::ParseDiagnostic],
    map: &gossamer_lex::SourceMap,
    file: gossamer_lex::FileId,
) -> String {
    let mut out = if diags.len() == 1 {
        String::from("1 parse error:\n")
    } else {
        format!("{} parse errors:\n", diags.len())
    };
    for diag in diags {
        let pos = map.line_col(file, diag.span.start);
        out.push_str(&format!("  {}:{}: {}\n", pos.line, pos.column, diag.error));
    }
    // Trim trailing newline so the surrounding `eprintln!` doesn't
    // double-space.
    if out.ends_with('\n') {
        out.pop();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repl_output_wraps_to_narrow_columns_and_keeps_indent() {
        let wrapped = wrap_repl_line(
            "    This description is intentionally long enough to wrap cleanly.",
            32,
        );
        assert!(wrapped.len() > 1);
        assert!(wrapped.iter().all(|line| line.chars().count() <= 32));
        assert!(wrapped.iter().skip(1).all(|line| line.starts_with("    ")));
    }

    #[test]
    fn repl_catalog_wrapping_uses_a_small_consistent_indent() {
        let mut output = String::new();
        push_catalog_entry(
            &mut output,
            "std::strings::replace",
            "fn",
            "Replaces every matching substring in the value.",
        );
        assert!(output.starts_with("std::strings::replace [fn]\n  Replaces"));
        let wrapped = wrap_repl_line("  Replaces every matching substring in the value.", 32);
        assert!(wrapped.len() > 1);
        assert!(wrapped.iter().all(|line| line.starts_with("  ")));
    }

    #[test]
    fn repl_metadata_never_exposes_untyped_runtime_registrations() {
        let incomplete = core_method_entries()
            .into_iter()
            .filter(|entry| entry.signature.contains("..."))
            .map(|entry| format!("{}::{}", entry.owner, entry.name))
            .collect::<Vec<_>>();
        assert!(
            incomplete.is_empty(),
            "REPL must not expose runtime registrations without concrete signatures: {incomplete:?}"
        );
    }

    #[test]
    fn sync_map_info_lists_complete_callable_signatures() {
        for owner in ["Map", "sync::Map"] {
            let entries = core_method_entries()
                .into_iter()
                .filter(|entry| entry.owner == owner)
                .collect::<Vec<_>>();
            assert_eq!(entries.len(), 7, "incomplete {owner} surface: {entries:?}");
            assert!(
                entries.iter().all(|entry| {
                    !entry.signature.trim().is_empty()
                        && entry.signature.starts_with("fn ")
                        && !entry.doc.starts_with("Built-in ")
                }),
                "{owner} contains placeholder metadata: {entries:?}"
            );
        }
        let rendered = render_catalog_query_matches("sync::Map", false);
        for expected in [
            "sync::Map::new() -> sync::Map [associated function]",
            "sync::Map::insert(self: &sync::Map, key: String, value: String) -> () [method]",
            "sync::Map::get(self: &sync::Map, key: String) -> Option<String> [method]",
            "sync::Map::remove(self: &sync::Map, key: String) -> () [method]",
            "sync::Map::len(self: &sync::Map) -> i64 [method]",
            "sync::Map::contains_key(self: &sync::Map, key: String) -> bool [method]",
            "sync::Map::keys(self: &sync::Map) -> Vec<String> [method]",
        ] {
            assert!(
                rendered.contains(expected),
                "missing `{expected}`:\n{rendered}"
            );
        }
    }

    #[test]
    fn audited_byte_handle_builtins_have_concrete_public_contracts() {
        let mut incomplete = core_method_entries()
            .into_iter()
            .filter(|entry| matches!(entry.owner.as_str(), "Buffer" | "Builder"))
            .filter(|entry| entry.signature.contains("..."))
            .map(|entry| format!("{}::{}", entry.owner, entry.name))
            .collect::<Vec<_>>();
        incomplete.sort();
        assert!(
            incomplete.is_empty(),
            "runtime type builtins without concrete public signatures: {incomplete:?}"
        );
    }

    #[test]
    fn string_methods_reuse_the_complete_stdlib_signature_catalog() {
        let mut incomplete = core_method_entries()
            .into_iter()
            .filter(|entry| entry.owner == "String")
            .filter(|entry| entry.signature.contains("..."))
            .map(|entry| entry.name)
            .collect::<Vec<_>>();
        incomplete.sort();
        assert!(
            incomplete.is_empty(),
            "String methods without concrete public signatures: {incomplete:?}"
        );
    }

    #[test]
    fn repl_metadata_does_not_leak_runtime_registration_text_for_core_types() {
        let checked = [
            "String", "Vec", "HashMap", "BTreeMap", "HashSet", "VecDeque", "Option", "Result",
        ];
        let mut leaked = Vec::new();
        for entry in core_method_entries() {
            if checked.contains(&entry.owner.as_str())
                && entry.doc.contains("Runtime builtin registered")
            {
                leaked.push(format!("{}::{}", entry.owner, entry.name));
            }
        }
        assert!(
            leaked.is_empty(),
            "core type methods should have user-facing REPL docs: {leaked:?}"
        );
    }

    #[test]
    fn repl_metadata_keeps_checked_core_collection_methods() {
        for query in [
            "String::parse",
            "Vec::push",
            "Vec::get",
            "Vec::capacity",
            "Vec::reserve",
            "Vec::truncate",
            "HashMap::insert",
            "BTreeMap::insert",
            "HashSet::union",
            "VecDeque::push_back",
            "Option::map",
            "Result::map_err",
        ] {
            assert!(
                !matching_core_methods(query).is_empty(),
                "missing REPL metadata for {query}"
            );
        }
    }

    #[test]
    fn every_std_defined_type_is_available_to_info() {
        let mut missing = Vec::new();
        for module in gossamer_std::registry::modules() {
            for item in module.items {
                if item.kind == StdItemKind::Type
                    && matching_items(&format!("{}::{}", module.path, item.name)).is_empty()
                {
                    missing.push(format!("{}::{}", module.path, item.name));
                }
            }
        }
        assert!(
            missing.is_empty(),
            "std types missing from %info: {missing:?}"
        );
    }

    #[test]
    fn every_detailed_catalog_description_is_followed_by_an_example() {
        for method in core_method_entries() {
            let mut rendered = String::new();
            push_core_method_match(&mut rendered, &method, true);
            assert!(
                rendered.contains(&format!("    {}\n    Builtin\n    Example: ", method.doc)),
                "missing example for {}::{}:\n{rendered}",
                method.owner,
                method.name
            );
        }

        for module in gossamer_std::registry::modules() {
            let mut rendered = String::new();
            push_module_match(&mut rendered, module, true);
            assert!(
                rendered.contains(&format!(
                    "    {}\n    Defined in: {}\n    Example: ",
                    module.summary, module.path
                )),
                "missing module example for {}:\n{rendered}",
                module.path
            );
            for item in module.items {
                let mut rendered = String::new();
                push_item_match(&mut rendered, module, item, true);
                assert!(
                    rendered.contains(&format!(
                        "    {}\n    Defined in: {}\n    Example: ",
                        item.doc, module.path
                    )),
                    "missing item example for {}::{}:\n{rendered}",
                    module.path,
                    item.name
                );
            }
        }

        for builtin in PRELUDE_BUILTINS {
            let mut rendered = String::new();
            push_catalog_match(
                &mut rendered,
                builtin.name,
                "builtin",
                builtin.signature,
                builtin.doc,
                None,
                true,
            );
            assert!(
                rendered.contains(&format!("    {}\n    Builtin\n    Example: ", builtin.doc)),
                "missing builtin example for {}:\n{rendered}",
                builtin.name
            );
        }
        for builtin in BUILTIN_MACROS {
            let mut rendered = String::new();
            push_catalog_match(
                &mut rendered,
                builtin.name,
                "macro",
                builtin.signature,
                builtin.doc,
                None,
                true,
            );
            assert!(
                rendered.contains(&format!("    {}\n    Builtin\n    Example: ", builtin.doc)),
                "missing macro example for {}:\n{rendered}",
                builtin.name
            );
        }

        let directory = render_stdlib_dir();
        assert_eq!(
            directory.matches("\n  Example: ").count(),
            directory.matches(" [module]\n").count(),
            "every directory description must have an example:\n{directory}"
        );
    }

    #[test]
    fn every_runtime_method_on_a_std_defined_type_is_available_to_explain() {
        let std_type_names = gossamer_std::registry::modules()
            .iter()
            .flat_map(|module| module.items)
            .filter(|item| item.kind == StdItemKind::Type)
            .map(|item| item.name)
            .collect::<std::collections::BTreeSet<_>>();
        let catalog = core_method_entries();
        let mut missing = Vec::new();
        for registered in gossamer_interp::registered_names() {
            let Some((owner, name)) = registered_core_method_path(registered) else {
                continue;
            };
            if owner == "Iterator" && !gossamer_types::is_iterator_method(&name) {
                continue;
            }
            let short_owner = owner.rsplit("::").next().unwrap_or(&owner);
            if std_type_names.contains(short_owner)
                && !catalog
                    .iter()
                    .any(|entry| entry.owner == owner && entry.name == name)
            {
                missing.push(format!("{owner}::{name}"));
            }
        }
        missing.sort();
        missing.dedup();
        assert!(
            missing.is_empty(),
            "runtime methods on std types missing from %explain: {missing:?}"
        );
    }

    #[test]
    fn option_has_one_complete_method_surface() {
        let expected = gossamer_std::registry::module("std::option")
            .expect("std::option module")
            .items
            .iter()
            .filter(|item| item.kind == StdItemKind::Function)
            .map(|item| item.name)
            .collect::<std::collections::BTreeSet<_>>();
        let entries = core_method_entries()
            .into_iter()
            .filter(|entry| entry.owner == "Option")
            .collect::<Vec<_>>();
        let actual = entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(actual, expected);
        assert_eq!(entries.len(), expected.len(), "duplicate Option metadata");
        assert!(entries.iter().all(|entry| !entry.signature.contains("...")));
    }

    #[test]
    fn iterator_info_and_explain_have_complete_methods() {
        assert!(!matching_core_namespaces("Iterator").is_empty());
        let entries = core_method_entries()
            .into_iter()
            .filter(|entry| entry.owner == "Iterator")
            .collect::<Vec<_>>();
        assert!(!entries.is_empty(), "Iterator has no %explain methods");
        assert!(entries.iter().all(|entry| !entry.signature.contains("...")));
        assert!(entries.iter().all(|entry| !entry.signature.is_empty()));
        assert!(
            entries
                .iter()
                .all(|entry| gossamer_types::is_iterator_method(&entry.name))
        );
        let names = entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        for unavailable in ["next", "for_each", "position", "max_by_key"] {
            assert!(!names.contains(unavailable));
        }
        let map = entries.iter().find(|entry| entry.name == "map").unwrap();
        assert!(map.signature.contains("self: Iterator<T>"));
        assert!(map.signature.ends_with("-> Iterator<U>"));
        let fold = entries.iter().find(|entry| entry.name == "fold").unwrap();
        assert!(fold.signature.contains("init: U, f: Fn(U, T) -> U"));
    }

    #[test]
    fn history_search_filters_saved_and_current_entries() {
        let history = vec![
            "let prior = 1".to_string(),
            "prior".to_string(),
            "let current = 2".to_string(),
        ];
        assert_eq!(
            render_repl_history(&history, "^let").unwrap(),
            ["let prior = 1", "let current = 2"]
        );
    }

    #[test]
    fn repl_metadata_covers_typechecked_vec_method_surface() {
        let checked_vec_methods = [
            "clone",
            "push",
            "pop",
            "insert",
            "remove",
            "clear",
            "extend",
            "extend_from_slice",
            "truncate",
            "reserve",
            "reserve_exact",
            "capacity",
            "len",
            "is_empty",
            "slice",
            "first",
            "last",
            "get",
            "rev",
            "collect",
            "to_vec",
            "dedup",
            "take",
            "skip",
            "step_by",
            "chain",
            "zip",
            "windows",
            "chunks",
            "pairwise",
            "flatten",
            "join",
            "contains",
            "index_of",
            "count_of",
            "sort",
            "sort_by",
            "sort_by_key",
            "reverse",
            "swap",
            "fill",
            "map",
            "filter",
            "fold",
            "for_each",
            "any",
            "all",
            "find",
            "position",
            "count",
            "enumerate",
            "sum",
            "min",
            "max",
            "min_by_key",
            "max_by_key",
        ];
        let mut missing = Vec::new();
        for name in checked_vec_methods {
            let query = format!("Vec::{name}");
            if matching_core_methods(&query).is_empty() {
                missing.push(query);
            }
        }
        assert!(
            missing.is_empty(),
            "missing REPL metadata for typechecked Vec methods: {missing:?}"
        );
    }

    #[test]
    fn sequence_catalogs_match_the_canonical_type_surfaces() {
        let catalog = core_method_entries();
        for owner in ["Array", "Slice"] {
            let methods = catalog
                .iter()
                .filter(|entry| entry.owner == owner && entry.kind == "method")
                .collect::<Vec<_>>();
            assert!(!methods.is_empty(), "{owner} catalog is empty");
            for method in methods {
                let expected = if owner == "Array" {
                    gossamer_types::is_array_sequence_method(&method.name)
                } else {
                    gossamer_types::is_slice_sequence_method(&method.name)
                };
                assert!(expected, "{owner} unexpectedly exposes {}", method.name);
                assert!(
                    !gossamer_types::is_vec_only_sequence_method(&method.name),
                    "{owner} exposes Vec-only method {}",
                    method.name
                );
            }
        }

        for name in ["push", "pop", "capacity", "reserve", "map", "fold", "sum"] {
            assert!(
                catalog
                    .iter()
                    .any(|entry| entry.owner == "Vec" && entry.name == name),
                "Vec is missing {name}"
            );
            assert!(
                !catalog.iter().any(|entry| {
                    matches!(entry.owner.as_str(), "Array" | "Slice") && entry.name == name
                }),
                "{name} leaked from Vec into Array or Slice"
            );
        }

        let array_clone = catalog
            .iter()
            .find(|entry| entry.owner == "Array" && entry.name == "clone")
            .expect("Array::clone metadata");
        assert!(array_clone.signature.ends_with("-> [T; N]"));
        assert!(
            !catalog
                .iter()
                .any(|entry| entry.owner == "Slice" && entry.name == "clone")
        );
    }
}
