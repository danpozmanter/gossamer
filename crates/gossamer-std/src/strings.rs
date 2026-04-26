//! Runtime support for `std::strings`.

#![forbid(unsafe_code)]

/// Splits `text` on `delimiter`, returning every segment (including
/// trailing empties), mirroring Rust's `str::split`.
#[must_use]
pub fn split(text: &str, delimiter: &str) -> Vec<String> {
    text.split(delimiter).map(str::to_string).collect()
}

/// Splits `text` into at most `n` parts on `delimiter`.
#[must_use]
pub fn splitn(text: &str, n: usize, delimiter: &str) -> Vec<String> {
    text.splitn(n, delimiter).map(str::to_string).collect()
}

/// Splits on ASCII whitespace, dropping empty segments.
#[must_use]
pub fn split_whitespace(text: &str) -> Vec<String> {
    text.split_whitespace().map(str::to_string).collect()
}

/// Trims leading and trailing whitespace (Unicode-aware).
#[must_use]
pub fn trim(text: &str) -> String {
    text.trim().to_string()
}

/// Returns whether `text` contains `needle`.
#[must_use]
pub fn contains(text: &str, needle: &str) -> bool {
    text.contains(needle)
}

/// Byte offset of the first occurrence of `needle` in `text`, or
/// `None` if absent.
#[must_use]
pub fn find(text: &str, needle: &str) -> Option<usize> {
    text.find(needle)
}

/// Replaces every occurrence of `from` with `to`.
#[must_use]
pub fn replace(text: &str, from: &str, to: &str) -> String {
    text.replace(from, to)
}

/// Lowercases every character using Unicode scalar semantics.
#[must_use]
pub fn to_lowercase(text: &str) -> String {
    text.to_lowercase()
}

/// Uppercases every character using Unicode scalar semantics.
#[must_use]
pub fn to_uppercase(text: &str) -> String {
    text.to_uppercase()
}

/// Returns whether `text` starts with `prefix`.
#[must_use]
pub fn starts_with(text: &str, prefix: &str) -> bool {
    text.starts_with(prefix)
}

/// Returns whether `text` ends with `suffix`.
#[must_use]
pub fn ends_with(text: &str, suffix: &str) -> bool {
    text.ends_with(suffix)
}

/// Repeats `text` `count` times.
#[must_use]
pub fn repeat(text: &str, count: usize) -> String {
    text.repeat(count)
}

/// Returns an iterator-style `Vec<String>` of lines (no trailing
/// line-terminators).
#[must_use]
pub fn lines(text: &str) -> Vec<String> {
    text.lines().map(str::to_string).collect()
}
