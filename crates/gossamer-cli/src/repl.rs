//! Interactive REPL.
//!
//! Kept in its own module so `main.rs` stays under the 2000-line
//! hard limit defined in `GUIDELINES.md`.

use std::collections::BTreeMap;
use std::io::Write as _;

use anyhow::{Result, anyhow};
use gossamer_parse::builtin_macros::{BUILTIN_MACROS, BuiltinMacro};
use gossamer_std::registry::{StdItem, StdItemKind, StdModule};
use regex::Regex;

use crate::paths::repl_history_path;

const REPL_HELP_TEXT: &str = "\
REPL commands

  %help (%h) [name|/regex/]
    Show command help or documentation for a symbol.
  %ls (%l) [module|type|/regex/]
    List standard-library modules, members, or core methods.
  %find (%f) <regex>
    Search public symbol names.
  %bindings (%b) [regex]
    Show persistent `let` bindings.
  %declarations (%d) [regex]
    Show persistent declarations.
  %history
    Show inputs from this session.
  %reset (%r)
    Clear persistent bindings and declarations.
  %quit (%q)
    Exit the REPL.

Expressions print their value. Declarations and `let` bindings persist.";

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
        return crate::style::heading(line);
    }
    if trimmed.starts_with('%') {
        return crate::style::accent(line);
    }
    crate::style::detail(line)
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

#[derive(Clone)]
struct CoreMethodEntry {
    owner: String,
    name: String,
    kind: &'static str,
    signature: String,
    doc: String,
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
// `"123".parse()` are not hidden from `%help`, `%ls`, and `%find`.
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
        signature: "fn from(value) -> String",
        doc: "Converts a displayable value into a string.",
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
        signature: "fn sort_by_key<T, K>(self: Vec<T>, f: fn(T) -> K) -> Vec<T>",
        doc: "Returns values sorted by a derived key.",
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
        name: "insert",
        kind: "method",
        signature: "fn insert<K, V>(self: &mut HashMap<K, V>, key: K, value: V) -> ()",
        doc: "Inserts or replaces a key-value pair.",
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
        signature: "fn remove<K, V>(self: &mut HashMap<K, V>, key: K) -> ()",
        doc: "Removes a key in place.",
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
        signature: "fn insert<K, V>(self: &mut BTreeMap<K, V>, key: K, value: V) -> ()",
        doc: "Inserts or replaces a key-value pair.",
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
        owner: "Option",
        name: "map",
        kind: "method",
        signature: "fn map<T, U>(self: Option<T>, f: fn(T) -> U) -> Option<U>",
        doc: "Maps Some through a closure and leaves None unchanged.",
    },
    CoreMethodHelp {
        owner: "Option",
        name: "and_then",
        kind: "method",
        signature: "fn and_then<T, U>(self: Option<T>, f: fn(T) -> Option<U>) -> Option<U>",
        doc: "Chains an Option-returning closure.",
    },
    CoreMethodHelp {
        owner: "Option",
        name: "is_some",
        kind: "method",
        signature: "fn is_some<T>(self: Option<T>) -> bool",
        doc: "Returns true for Some.",
    },
    CoreMethodHelp {
        owner: "Option",
        name: "is_none",
        kind: "method",
        signature: "fn is_none<T>(self: Option<T>) -> bool",
        doc: "Returns true for None.",
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
        "gos repl - type an expression or declaration\n\
         up/down cycles history · Enter continues until braces close · Ctrl-D or %quit exits"
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

    let tty = std::io::IsTerminal::is_terminal(&std::io::stdout());
    if tty {
        crate::style::force_enable();
    }
    // Greeting on a TTY only - keeps non-interactive consumers
    // (`echo expr | gos`) clean.
    if tty {
        println!(
            "\x1b[1mgos {ver}\x1b[0m  type expressions, or \x1b[36m%help\x1b[0m for meta commands",
            ver = env!("CARGO_PKG_VERSION"),
        );
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
                eprintln!("{}: {err}", crate::style::error("repl"));
                return Ok(());
            }
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
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
                "history" => {
                    for entry in &transcript {
                        println!("{}", crate::style::accent(entry));
                    }
                    continue;
                }
                "bindings" | "b" => {
                    if bindings.is_empty() {
                        println!("    no `let` bindings yet");
                    } else {
                        let pattern = if arg.is_empty() {
                            None
                        } else {
                            match compile_search_regex("bindings", arg) {
                                Ok(pattern) => Some(pattern),
                                Err(message) => {
                                    eprintln!("{message}");
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
                            println!("    no bindings match `{arg}`");
                            continue;
                        }
                        for line in matches {
                            println!("{}", crate::style::heading(line));
                        }
                    }
                    continue;
                }
                "declarations" | "decls" | "d" => {
                    if declarations.is_empty() {
                        println!("    no declarations yet");
                    } else {
                        let pattern = if arg.is_empty() {
                            None
                        } else {
                            match compile_search_regex("declarations", arg) {
                                Ok(pattern) => Some(pattern),
                                Err(message) => {
                                    eprintln!("{message}");
                                    continue;
                                }
                            }
                        };
                        let matches = declarations
                            .iter()
                            .filter(|line| pattern.as_ref().is_none_or(|re| re.is_match(line)))
                            .collect::<Vec<_>>();
                        if matches.is_empty() {
                            println!("    no declarations match `{arg}`");
                            continue;
                        }
                        for line in matches {
                            println!("{}", crate::style::heading(line));
                        }
                    }
                    continue;
                }
                "reset" | "r" => {
                    declarations.clear();
                    lets.clear();
                    bindings.clear();
                    println!("{}", crate::style::accent("session cleared"));
                    continue;
                }
                "help" | "h" => {
                    match repl_help(arg) {
                        Ok(text) => print_repl_output(&text),
                        Err(msg) => eprintln!("{msg}"),
                    }
                    continue;
                }
                "ls" | "l" => {
                    match repl_ls(arg) {
                        Ok(text) => print_repl_output(&text),
                        Err(msg) => eprintln!("{msg}"),
                    }
                    continue;
                }
                "find" | "f" => {
                    match repl_find(arg) {
                        Ok(text) => print_repl_output(&text),
                        Err(msg) => eprintln!("{msg}"),
                    }
                    continue;
                }
                _ => {
                    eprintln!("unknown meta-command: %{rest}");
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
                    eprintln!("    {msg}");
                }
            }
            input_no += 1;
            continue;
        }

        if trimmed.starts_with("let ") {
            let candidate = trimmed.to_string();
            let new_binding = match repl_binding_from_let_source(&candidate) {
                Ok(binding) => binding,
                Err(msg) => {
                    eprintln!("    {msg}");
                    input_no += 1;
                    continue;
                }
            };
            lets.push(candidate.clone());
            let probe_body = format!("{}\n    ()\n", lets.join("\n    "));
            let probe = format!(
                "{}\nfn __irepl_{n}() {{\n    {body}}}\n",
                declarations.join("\n"),
                n = input_no,
                body = probe_body,
            );
            match build_and_call(&probe, &format!("__irepl_{input_no}")) {
                Ok(_) => {
                    update_repl_bindings(&mut bindings, new_binding);
                    if verbose {
                        println!("    binding added ({} total)", bindings.len());
                    }
                }
                Err(msg) => {
                    lets.pop();
                    eprintln!("    {msg}");
                }
            }
            input_no += 1;
            continue;
        }

        // Assignments and collection mutation calls must be replayed with the
        // preceding bindings so their effects survive into later inputs.
        if input_mutates_binding(trimmed) {
            lets.push(trimmed.to_string());
            let prefix = lets[..lets.len() - 1].join("\n    ");
            let probe_body = if prefix.is_empty() {
                trimmed.to_string()
            } else {
                format!("{prefix}\n    {trimmed}")
            };
            let probe = format!(
                "{}\nfn __irepl_{n}() {{\n    {body}}}\n",
                declarations.join("\n"),
                n = input_no,
                body = probe_body,
            );
            match build_and_call(&probe, &format!("__irepl_{input_no}")) {
                Ok(value) => {
                    if !matches!(value, gossamer_interp::Value::Unit) {
                        print_repl_result(&value);
                    }
                }
                Err(msg) => {
                    lets.pop();
                    eprintln!("{}: {msg}", crate::style::error("error"));
                }
            }
            input_no += 1;
            continue;
        }

        let let_body = if lets.is_empty() {
            String::new()
        } else {
            format!("{}\n    ", lets.join("\n    "))
        };
        let program_source = format!(
            "{}\nfn __irepl_{n}() {{ {lets}{expr} }}\n",
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
                eprintln!("{}: {msg}", crate::style::error("error"));
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

fn print_repl_result(value: &gossamer_interp::Value) {
    println!("{}", render_repl_value(value));
    std::io::stdout()
        .flush()
        .expect("flush REPL expression result");
}

struct ReplBinding {
    vars: Vec<ReplBindingVar>,
}

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

fn render_repl_bindings(
    declarations: &[String],
    lets: &[String],
    bindings: &[ReplBinding],
) -> Vec<String> {
    let let_body = if lets.is_empty() {
        String::new()
    } else {
        format!("{}\n    ", lets.join("\n    "))
    };
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
            let (value, ty) = match build_and_call_with_type(&source, &entry) {
                Ok((value, ty)) => (render_repl_value(&value), ty),
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

fn repl_binding_from_let_source(input: &str) -> std::result::Result<ReplBinding, String> {
    use gossamer_ast::{ExprKind, ItemKind, StmtKind};

    let source = format!("fn __irepl_binding_names() {{ {input} }}\n");
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
    if block.stmts.is_empty() || block.tail.is_some() {
        return Err(repl_let_shape_error());
    }
    let mut vars = Vec::new();
    for stmt in &block.stmts {
        let StmtKind::Let { pattern, init, .. } = &stmt.kind else {
            return Err(repl_let_shape_error());
        };
        if init.is_none() {
            return Err(repl_let_initializer_error());
        }
        collect_repl_pattern_bindings(pattern, &mut vars);
    }
    Ok(ReplBinding { vars })
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

fn repl_help(arg: &str) -> std::result::Result<String, String> {
    if arg.is_empty() {
        return Ok(REPL_HELP_TEXT.to_string());
    }
    if let Some(pattern) = regex_argument(arg)? {
        return Ok(render_help_matches(&pattern));
    }

    let query = normalize_query(arg);
    let mut out = String::new();
    for builtin in matching_builtin_macros(query) {
        push_builtin_macro_help(&mut out, builtin);
    }
    for builtin in matching_prelude_builtins(query) {
        push_prelude_builtin_help(&mut out, builtin);
    }
    for method in matching_core_methods(query) {
        push_core_method_help(&mut out, &method);
    }
    for module in matching_modules(query) {
        push_module_help(&mut out, &module);
    }
    for (module, item) in matching_items(query) {
        push_item_help(&mut out, &module, &item);
    }
    for feature in matching_features(query) {
        push_feature_help(&mut out, feature);
    }

    if out.is_empty() {
        Ok(format!("no help found for `{arg}`"))
    } else {
        Ok(out.trim_end().to_string())
    }
}

fn repl_ls(arg: &str) -> std::result::Result<String, String> {
    if arg.is_empty() {
        return Ok(render_module_dir(gossamer_std::registry::modules()));
    }
    if let Some(pattern) = regex_argument(arg)? {
        return Ok(render_dir_matches(&pattern));
    }

    let query = normalize_query(arg);
    let core_namespaces = matching_core_namespaces(query);
    if !core_namespaces.is_empty() {
        return Ok(render_core_method_dir(&core_namespaces));
    }

    let modules = matching_modules(query);
    if !modules.is_empty() {
        return Ok(render_module_dir(&modules));
    }

    if let Some(method) = matching_core_methods(query).into_iter().next() {
        return Err(format!(
            "`{}::{}` is a {}; %ls accepts module or core type names only (use %help for an item)",
            method.owner, method.name, method.kind
        ));
    }

    if let Some((module, item)) = matching_items(query).into_iter().next() {
        return Err(format!(
            "`{}::{}` is a {}; %ls accepts module names only (use %help for an item)",
            module.path,
            item.name,
            item_kind_label(item.kind)
        ));
    }
    Ok(format!("no stdlib module found for `{arg}`"))
}

/// Searches public symbol paths with a regular expression. Plain text remains
/// a substring search, while regex operators can broaden or constrain the
/// match.
fn repl_find(arg: &str) -> std::result::Result<String, String> {
    let query = normalize_query(arg);
    if query.is_empty() {
        return Err("usage: %find <name-regex>".to_string());
    }
    let pattern = compile_search_regex("find", query)?;

    let mut matches = Vec::new();
    for module in gossamer_std::registry::modules() {
        let candidate = FindCandidate {
            path: module.path.to_string(),
            kind: "module",
            doc: module.summary.to_string(),
        };
        if pattern.is_match(&candidate.path) {
            matches.push(candidate);
        }
        for item in module.items {
            let candidate = FindCandidate {
                path: format!("{}::{}", module.path, item.name),
                kind: item_kind_label(item.kind),
                doc: item.doc.to_string(),
            };
            if pattern.is_match(&candidate.path) {
                matches.push(candidate);
            }
        }
    }
    for builtin in BUILTIN_MACROS {
        let candidate = FindCandidate {
            path: builtin.name.to_string(),
            kind: "macro",
            doc: builtin.doc.to_string(),
        };
        if pattern.is_match(&candidate.path) {
            matches.push(candidate);
        }
    }
    for builtin in PRELUDE_BUILTINS {
        let candidate = FindCandidate {
            path: builtin.name.to_string(),
            kind: "builtin",
            doc: builtin.doc.to_string(),
        };
        if pattern.is_match(&candidate.path) {
            matches.push(candidate);
        }
    }
    for method in core_method_entries() {
        let candidate = FindCandidate {
            path: format!("{}::{}", method.owner, method.name),
            kind: method.kind,
            doc: method.doc.clone(),
        };
        if pattern.is_match(&candidate.path) || pattern.is_match(&core_lower_path(&method)) {
            matches.push(candidate);
        }
    }

    matches.sort_by(|left, right| left.path.cmp(&right.path));
    matches.dedup_by(|left, right| left.path == right.path && left.kind == right.kind);

    if matches.is_empty() {
        return Ok(format!("no public symbols found for `{query}`"));
    }
    let mut out = String::new();
    for candidate in matches.into_iter().take(50) {
        push_catalog_entry(&mut out, &candidate.path, candidate.kind, &candidate.doc);
    }
    Ok(out.trim_end().to_string())
}

struct FindCandidate<'a> {
    path: String,
    kind: &'a str,
    doc: String,
}

fn compile_search_regex(command: &str, query: &str) -> std::result::Result<Regex, String> {
    Regex::new(query).map_err(|error| format!("invalid {command} regex `{query}`: {error}"))
}

fn regex_argument(arg: &str) -> std::result::Result<Option<Regex>, String> {
    if !(arg.starts_with('/') && arg.ends_with('/') && arg.len() >= 2) {
        return Ok(None);
    }
    Regex::new(&arg[1..arg.len() - 1])
        .map(Some)
        .map_err(|e| format!("invalid regex `{arg}`: {e}"))
}

fn render_help_matches(pattern: &Regex) -> String {
    let mut out = String::new();
    for builtin in BUILTIN_MACROS {
        if pattern.is_match(builtin.name)
            || pattern.is_match(builtin.signature)
            || pattern.is_match(builtin.doc)
        {
            push_builtin_macro_help(&mut out, builtin);
        }
    }
    for builtin in PRELUDE_BUILTINS {
        if pattern.is_match(builtin.name)
            || pattern.is_match(builtin.signature)
            || pattern.is_match(builtin.doc)
        {
            push_prelude_builtin_help(&mut out, builtin);
        }
    }
    for method in core_method_entries() {
        let path = format!("{}::{}", method.owner, method.name);
        if pattern.is_match(&path)
            || pattern.is_match(&core_lower_path(&method))
            || pattern.is_match(&method.name)
            || pattern.is_match(&method.signature)
            || pattern.is_match(&method.doc)
        {
            push_core_method_help(&mut out, &method);
        }
    }
    for module in gossamer_std::registry::modules() {
        if module_matches_regex(pattern, module) {
            push_module_help(&mut out, module);
        }
        for item in module.items {
            if item_matches_regex(pattern, module, item) {
                push_item_help(&mut out, module, item);
            }
        }
    }
    for feature in gossamer_std::manifest::feature_status::all_entries() {
        if !is_stdlib_module_path(feature.path)
            && (pattern.is_match(feature.path) || pattern.is_match(feature.doc))
        {
            push_feature_help(&mut out, feature);
        }
    }
    if out.is_empty() {
        "no help matches".to_string()
    } else {
        out.trim_end().to_string()
    }
}

fn render_dir_matches(pattern: &Regex) -> String {
    let mut lines = Vec::new();
    for owner in all_core_namespaces() {
        if pattern.is_match(&owner) || pattern.is_match(&owner.to_ascii_lowercase()) {
            let mut line = String::new();
            push_core_namespace_dir_line(&mut line, &owner);
            lines.push(line);
        }
    }
    for module in gossamer_std::registry::modules() {
        if module_matches_regex(pattern, module) {
            let mut line = String::new();
            push_module_dir_line(&mut line, module);
            lines.push(line);
        }
    }
    if lines.is_empty() {
        "no stdlib modules match".to_string()
    } else {
        lines.sort_unstable();
        lines.concat().trim_end().to_string()
    }
}

fn render_core_method_dir(owners: &[String]) -> String {
    let mut entries = Vec::new();
    for owner in owners {
        let mut entry = String::new();
        push_core_namespace_dir_line(&mut entry, owner);
        entries.push(entry);
        for method in core_method_entries()
            .into_iter()
            .filter(|method| method.owner == **owner)
        {
            let mut entry = String::new();
            push_core_method_dir(&mut entry, &method);
            entries.push(entry);
        }
    }
    entries.sort_unstable();
    entries.concat().trim_end().to_string()
}

fn render_module_dir(modules: &[StdModule]) -> String {
    let mut entries = Vec::new();
    for module in modules {
        let mut entry = String::new();
        push_module_dir_line(&mut entry, module);
        entries.push(entry);
        if modules.len() == 1 {
            // A directory command names a module, so render its complete
            // namespace tree: the module's own members plus every registered
            // descendant module and its members.  A plain `%ls` deliberately
            // stays shallow; recursively expanding the entire standard
            // library there would make the useful module overview unusable.
            push_module_items(&mut entries, module);
            let prefix = format!("{}::", module.path);
            for child in gossamer_std::registry::modules()
                .iter()
                .filter(|child| child.path.starts_with(&prefix))
            {
                let mut entry = String::new();
                push_module_dir_line(&mut entry, child);
                entries.push(entry);
                push_module_items(&mut entries, child);
            }
        }
    }
    entries.sort_unstable();
    entries.concat().trim_end().to_string()
}

fn push_module_items(entries: &mut Vec<String>, module: &StdModule) {
    for item in module.items {
        let mut entry = String::new();
        push_item_dir(&mut entry, module, item);
        entries.push(entry);
    }
}

fn push_module_help(out: &mut String, module: &StdModule) {
    out.push_str(&format!("{}\n", module.path));
    out.push_str(&format!("  {}\n", module.summary));
    out.push_str(&format!("  items: {}\n\n", module.items.len()));
}

fn push_item_help(out: &mut String, module: &StdModule, item: &StdItem) {
    out.push_str(&format!(
        "{}::{} [{}]\n",
        module.path,
        item.name,
        item_kind_label(item.kind)
    ));
    if let Some(signature) = gossamer_types::stdlib_function_signature(module.path, item.name) {
        out.push_str(&format!("  {signature}\n"));
    }
    out.push_str(&format!("  {}\n\n", item.doc));
}

fn push_core_method_help(out: &mut String, method: &CoreMethodEntry) {
    out.push_str(&format!(
        "{}::{} [{}]\n",
        method.owner, method.name, method.kind
    ));
    out.push_str(&format!("  {}\n", method.signature));
    out.push_str(&format!("  {}\n\n", method.doc));
}

fn push_builtin_macro_help(out: &mut String, builtin: &BuiltinMacro) {
    out.push_str(&format!("{} [macro]\n", builtin.name));
    out.push_str(&format!("  {}\n", builtin.signature));
    out.push_str(&format!("  {}\n\n", builtin.doc));
}

fn push_prelude_builtin_help(out: &mut String, builtin: &PreludeBuiltinHelp) {
    out.push_str(&format!("{} [builtin]\n", builtin.name));
    out.push_str(&format!("  {}\n", builtin.signature));
    out.push_str(&format!("  {}\n\n", builtin.doc));
}

fn push_feature_help(out: &mut String, feature: gossamer_std::manifest::FeatureStatus) {
    out.push_str(&format!("{} ({})\n", feature.path, feature.status.tag()));
    out.push_str(&format!("  {}\n\n", feature.doc));
}

fn push_module_dir_line(out: &mut String, module: &StdModule) {
    push_catalog_entry(out, module.path, "module", module.summary);
}

fn push_core_namespace_dir_line(out: &mut String, owner: &str) {
    push_catalog_entry(
        out,
        owner,
        "type",
        "Built-in receiver and associated methods.",
    );
}

fn push_item_dir(out: &mut String, module: &StdModule, item: &StdItem) {
    push_catalog_entry(
        out,
        &format!("{}::{}", module.path, item.name),
        item_kind_label(item.kind),
        item.doc,
    );
}

fn push_core_method_dir(out: &mut String, method: &CoreMethodEntry) {
    push_catalog_entry(
        out,
        &format!("{}::{}", method.owner, method.name),
        method.kind,
        &method.doc,
    );
}

fn push_catalog_entry(out: &mut String, path: &str, kind: &str, description: &str) {
    out.push_str(&format!("{path} [{kind}]\n  {description}\n"));
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
    for registered in gossamer_interp::registered_names() {
        if let Some((owner, name)) = registered_core_method_path(registered) {
            let kind = if runtime_assoc_name(&name) {
                "assoc"
            } else {
                "method"
            };
            let signature = if kind == "assoc" {
                format!("fn {name}(...) -> ...")
            } else {
                format!("fn {name}(self, ...) -> ...")
            };
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
    entries.into_values().collect()
}

#[allow(
    clippy::too_many_lines,
    reason = "flat metadata table keeps REPL core-method docs auditable"
)]
fn runtime_core_method_doc(owner: &str, name: &str) -> Option<&'static str> {
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

fn matching_features(query: &str) -> Vec<gossamer_std::manifest::FeatureStatus> {
    gossamer_std::manifest::feature_status::all_entries()
        .into_iter()
        .filter(|entry| !is_stdlib_module_path(entry.path))
        .filter(|entry| feature_query_matches(entry.path, query))
        .collect()
}

fn matching_builtin_macros(query: &str) -> Vec<&'static BuiltinMacro> {
    BUILTIN_MACROS
        .iter()
        .filter(|builtin| builtin.name == query)
        .collect()
}

fn matching_prelude_builtins(query: &str) -> Vec<&'static PreludeBuiltinHelp> {
    PRELUDE_BUILTINS
        .iter()
        .filter(|builtin| builtin.name == query)
        .collect()
}

fn is_stdlib_module_path(path: &str) -> bool {
    gossamer_std::registry::module(path).is_some()
}

fn module_query_matches(module: &StdModule, query: &str) -> bool {
    module_aliases(module.path).contains(&query)
}

fn core_namespace_matches(owner: &str, query: &str) -> bool {
    owner == query || owner.eq_ignore_ascii_case(query)
}

fn item_query_matches(module: &StdModule, item: &StdItem, query: &str) -> bool {
    if item.name == query {
        return true;
    }
    module_aliases(module.path)
        .iter()
        .any(|alias| format!("{alias}::{}", item.name) == query)
}

fn core_method_query_matches(method: &CoreMethodEntry, query: &str) -> bool {
    method.name == query
        || format!("{}::{}", method.owner, method.name) == query
        || core_lower_path(method) == query
}

fn core_lower_path(method: &CoreMethodEntry) -> String {
    format!("{}::{}", method.owner.to_ascii_lowercase(), method.name)
}

fn feature_query_matches(path: &str, query: &str) -> bool {
    if path == query {
        return true;
    }
    let stripped = path
        .strip_prefix("lang::")
        .or_else(|| path.strip_prefix("std::"))
        .unwrap_or(path);
    stripped == query || path.rsplit("::").next().is_some_and(|last| last == query)
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
fn input_mutates_binding(input: &str) -> bool {
    use gossamer_ast::{ExprKind, ItemKind, StmtKind};

    let source = format!("fn __irepl_classify() {{ {input} }}\n");
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

    target.is_some_and(repl_expr_mutates_binding)
}

fn repl_stmt_mutates_binding(stmt: &gossamer_ast::Stmt) -> bool {
    use gossamer_ast::StmtKind;

    match &stmt.kind {
        StmtKind::Let { init, .. } => init.as_deref().is_some_and(repl_expr_mutates_binding),
        StmtKind::Expr { expr, .. } | StmtKind::Defer(expr) | StmtKind::Go(expr) => {
            repl_expr_mutates_binding(expr)
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

fn repl_expr_mutates_binding(expr: &gossamer_ast::Expr) -> bool {
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
                || repl_expr_mutates_binding(receiver)
                || args.iter().any(repl_expr_mutates_binding)
        }
        ExprKind::Call { callee, args } => {
            repl_callee_is_mutating_name(callee)
                || repl_expr_mutates_binding(callee)
                || args.iter().any(repl_expr_mutates_binding)
        }
        ExprKind::For { iter, body, .. } => {
            repl_expr_contains_ref_mut(iter) || repl_expr_mutates_binding(body)
        }
        ExprKind::Block(block) | ExprKind::Unsafe(block) => {
            block.stmts.iter().any(repl_stmt_mutates_binding)
                || block.tail.as_deref().is_some_and(repl_expr_mutates_binding)
        }
        ExprKind::If {
            condition,
            then_branch,
            else_branch,
        } => {
            repl_expr_mutates_binding(condition)
                || repl_expr_mutates_binding(then_branch)
                || else_branch
                    .as_deref()
                    .is_some_and(repl_expr_mutates_binding)
        }
        ExprKind::Match { scrutinee, arms } => {
            repl_expr_mutates_binding(scrutinee)
                || arms.iter().any(|arm| {
                    arm.guard.as_ref().is_some_and(repl_expr_mutates_binding)
                        || repl_expr_mutates_binding(&arm.body)
                })
        }
        ExprKind::Loop { body, .. } => repl_expr_mutates_binding(body),
        ExprKind::While {
            condition, body, ..
        } => repl_expr_mutates_binding(condition) || repl_expr_mutates_binding(body),
        ExprKind::FieldAccess { receiver, .. } => repl_expr_mutates_binding(receiver),
        ExprKind::Index { base, index } => {
            repl_expr_mutates_binding(base) || repl_expr_mutates_binding(index)
        }
        ExprKind::Unary { operand, .. } => repl_expr_mutates_binding(operand),
        ExprKind::Binary { lhs, rhs, .. } => {
            repl_expr_mutates_binding(lhs) || repl_expr_mutates_binding(rhs)
        }
        ExprKind::Cast { value, .. } | ExprKind::Try(value) | ExprKind::Go(value) => {
            repl_expr_mutates_binding(value)
        }
        ExprKind::Closure { body, .. } => repl_expr_mutates_binding(body),
        ExprKind::Return(value) => value.as_deref().is_some_and(repl_expr_mutates_binding),
        ExprKind::Break { value, .. } => value.as_deref().is_some_and(repl_expr_mutates_binding),
        ExprKind::Tuple(elems) => elems.iter().any(repl_expr_mutates_binding),
        ExprKind::Struct { fields, base, .. } => {
            fields
                .iter()
                .any(|field| field.value.as_ref().is_some_and(repl_expr_mutates_binding))
                || base.as_deref().is_some_and(repl_expr_mutates_binding)
        }
        ExprKind::Array(array) => repl_array_expr_mutates_binding(array),
        ExprKind::Range { start, end, .. } => {
            start.as_deref().is_some_and(repl_expr_mutates_binding)
                || end.as_deref().is_some_and(repl_expr_mutates_binding)
        }
        ExprKind::Select(arms) => arms.iter().any(|arm| repl_expr_mutates_binding(&arm.body)),
        ExprKind::Literal(_)
        | ExprKind::Path(_)
        | ExprKind::Continue { .. }
        | ExprKind::MacroCall(_)
        | ExprKind::Error => false,
    }
}

fn repl_array_expr_mutates_binding(array: &gossamer_ast::expr::ArrayExpr) -> bool {
    match array {
        gossamer_ast::expr::ArrayExpr::List(elems) => elems.iter().any(repl_expr_mutates_binding),
        gossamer_ast::expr::ArrayExpr::Repeat { value, count } => {
            repl_expr_mutates_binding(value) || repl_expr_mutates_binding(count)
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
        return Err(format_semantic_diags("resolution", &resolve_diags));
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
    build_and_call_with_type(source, entry).map(|(value, _)| value)
}

fn build_and_call_with_type(
    source: &str,
    entry: &str,
) -> std::result::Result<(gossamer_interp::Value, String), String> {
    let source = gossamer_parse::autoderive::augment_source(source);
    let mut map = gossamer_lex::SourceMap::new();
    let file = map.add_file("irepl".to_string(), source.clone());
    let (sf, parse_diags) = gossamer_parse::autoderive::parse_with_autoderive(&source, file);
    if !parse_diags.is_empty() {
        return Err(format_parse_diags(&parse_diags, &map, file));
    }
    let (res, resolve_diags) = gossamer_resolve::resolve_source_file(&sf);
    if !resolve_diags.is_empty() {
        return Err(format_semantic_diags("resolution", &resolve_diags));
    }
    let mut tcx = gossamer_types::TyCtxt::new();
    let (tbl, type_diags) = gossamer_types::typecheck_source_file(&sf, &res, &mut tcx);
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
    let tail_ty = tail_ty_id.map_or_else(
        || "<unknown>".to_string(),
        |ty| gossamer_types::render_public_ty(&tcx, ty),
    );
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
    fn repl_metadata_covers_registered_runtime_type_builtins() {
        let mut missing = Vec::new();
        for name in gossamer_interp::registered_names() {
            let Some((owner, method)) = registered_core_method_path(name) else {
                continue;
            };
            let query = format!("{owner}::{method}");
            if matching_core_methods(&query).is_empty() {
                missing.push(query);
            }
        }
        missing.sort();
        missing.dedup();
        assert!(
            missing.is_empty(),
            "missing REPL metadata for registered runtime type builtins: {missing:?}"
        );
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
}
