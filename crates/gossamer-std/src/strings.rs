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

/// Joins every element of `parts` with `sep` between them.
#[must_use]
pub fn join(parts: &[String], sep: &str) -> String {
    parts.join(sep)
}

/// Trims leading whitespace only.
#[must_use]
pub fn trim_start(text: &str) -> String {
    text.trim_start().to_string()
}

/// Trims trailing whitespace only.
#[must_use]
pub fn trim_end(text: &str) -> String {
    text.trim_end().to_string()
}

/// Strips `prefix` from the start of `text` if present.
#[must_use]
pub fn strip_prefix(text: &str, prefix: &str) -> Option<String> {
    text.strip_prefix(prefix).map(str::to_string)
}

/// Strips `suffix` from the end of `text` if present.
#[must_use]
pub fn strip_suffix(text: &str, suffix: &str) -> Option<String> {
    text.strip_suffix(suffix).map(str::to_string)
}

/// Pads `text` on the left with `pad_char` until it reaches `width`.
#[must_use]
pub fn pad_left(text: &str, width: usize, pad_char: char) -> String {
    let count = text.chars().count();
    if count >= width {
        return text.to_string();
    }
    let mut out = String::new();
    for _ in 0..(width - count) {
        out.push(pad_char);
    }
    out.push_str(text);
    out
}

/// Pads `text` on the right with `pad_char` until it reaches `width`.
#[must_use]
pub fn pad_right(text: &str, width: usize, pad_char: char) -> String {
    let count = text.chars().count();
    if count >= width {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len() + width - count);
    out.push_str(text);
    for _ in 0..(width - count) {
        out.push(pad_char);
    }
    out
}

/// Returns the byte offset of the last occurrence of `needle` in
/// `text`, or `None` if absent.
#[must_use]
pub fn rfind(text: &str, needle: &str) -> Option<usize> {
    text.rfind(needle)
}

/// Replaces at most `n` occurrences of `from` with `to`.
#[must_use]
pub fn replacen(text: &str, from: &str, to: &str, n: usize) -> String {
    text.replacen(from, to, n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_inserts_separator_between_parts() {
        let parts = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        assert_eq!(join(&parts, ","), "a,b,c");
        assert_eq!(join(&parts, ""), "abc");
        let empty: Vec<String> = Vec::new();
        assert_eq!(join(&empty, ","), "");
    }

    #[test]
    fn pad_helpers_round_to_width() {
        assert_eq!(pad_left("42", 5, '0'), "00042");
        assert_eq!(pad_right("hi", 4, '_'), "hi__");
        assert_eq!(pad_left("toolong", 3, ' '), "toolong");
    }

    #[test]
    fn strip_helpers_only_match_at_anchor() {
        assert_eq!(strip_prefix("abcdef", "abc").as_deref(), Some("def"));
        assert_eq!(strip_prefix("abcdef", "xyz"), None);
        assert_eq!(strip_suffix("file.txt", ".txt").as_deref(), Some("file"));
    }

    #[test]
    fn replacen_caps_replacement_count() {
        assert_eq!(replacen("aaa", "a", "b", 2), "bba");
        assert_eq!(replacen("aaa", "a", "b", 0), "aaa");
    }
}
