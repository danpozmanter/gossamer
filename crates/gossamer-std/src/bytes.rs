//! Runtime support for `std::bytes` — ergonomic byte-slice + buffer
//! helpers used by protocol and parser code.

#![forbid(unsafe_code)]

/// Growable byte buffer with amortised O(1) `push`, plus helpers for
/// building protocol payloads incrementally.
#[derive(Debug, Default, Clone)]
pub struct Buffer {
    inner: Vec<u8>,
}

impl Buffer {
    /// Empty buffer.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Buffer with pre-allocated capacity `n`.
    #[must_use]
    pub fn with_capacity(n: usize) -> Self {
        Self {
            inner: Vec::with_capacity(n),
        }
    }

    /// Appends `bytes` to the end of the buffer.
    pub fn write(&mut self, bytes: &[u8]) {
        self.inner.extend_from_slice(bytes);
    }

    /// Appends `text`'s UTF-8 bytes.
    pub fn write_str(&mut self, text: &str) {
        self.inner.extend_from_slice(text.as_bytes());
    }

    /// Appends one byte.
    pub fn push(&mut self, byte: u8) {
        self.inner.push(byte);
    }

    /// Current length.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Whether the buffer is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Borrowed view of the full contents.
    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        &self.inner
    }

    /// Consumes the buffer and returns the backing vector.
    #[must_use]
    pub fn into_vec(self) -> Vec<u8> {
        self.inner
    }

    /// Resets the buffer to empty without releasing capacity.
    pub fn clear(&mut self) {
        self.inner.clear();
    }
}

/// String-oriented builder that is cheaper than repeated `+=` on an
/// immutable `String`. Construct with [`Builder::new`] and call
/// [`Builder::build`] at the end.
#[derive(Debug, Default, Clone)]
pub struct Builder {
    buf: String,
}

impl Builder {
    /// Empty builder.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Builder with pre-allocated capacity.
    #[must_use]
    pub fn with_capacity(n: usize) -> Self {
        Self {
            buf: String::with_capacity(n),
        }
    }

    /// Appends `text`.
    pub fn write(&mut self, text: &str) {
        self.buf.push_str(text);
    }

    /// Appends a single character.
    pub fn write_char(&mut self, c: char) {
        self.buf.push(c);
    }

    /// Returns the built string, consuming the builder.
    #[must_use]
    pub fn build(self) -> String {
        self.buf
    }

    /// Borrowed view.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.buf
    }
}

/// Returns the first index at which `needle` appears in `haystack`,
/// or `None` when it doesn't.
#[must_use]
pub fn index_of(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    if needle.len() > haystack.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Returns `true` when `haystack` contains `needle`.
#[must_use]
pub fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    index_of(haystack, needle).is_some()
}

/// Returns `true` when `haystack` starts with `prefix`.
#[must_use]
pub fn has_prefix(haystack: &[u8], prefix: &[u8]) -> bool {
    haystack.len() >= prefix.len() && &haystack[..prefix.len()] == prefix
}

/// Returns `true` when `haystack` ends with `suffix`.
#[must_use]
pub fn has_suffix(haystack: &[u8], suffix: &[u8]) -> bool {
    haystack.len() >= suffix.len() && &haystack[haystack.len() - suffix.len()..] == suffix
}

/// Splits `haystack` on every `separator` occurrence, returning owned
/// byte-vector chunks.
#[must_use]
pub fn split(haystack: &[u8], separator: &[u8]) -> Vec<Vec<u8>> {
    if separator.is_empty() {
        return vec![haystack.to_vec()];
    }
    let mut out = Vec::new();
    let mut cursor = 0;
    while cursor <= haystack.len() {
        if let Some(off) = index_of(&haystack[cursor..], separator) {
            out.push(haystack[cursor..cursor + off].to_vec());
            cursor += off + separator.len();
        } else {
            out.push(haystack[cursor..].to_vec());
            break;
        }
    }
    out
}

/// Joins `parts` with `separator` between each pair.
#[must_use]
pub fn join(parts: &[&[u8]], separator: &[u8]) -> Vec<u8> {
    if parts.is_empty() {
        return Vec::new();
    }
    let total: usize = parts.iter().map(|p| p.len()).sum::<usize>()
        + separator.len() * parts.len().saturating_sub(1);
    let mut out = Vec::with_capacity(total);
    for (i, part) in parts.iter().enumerate() {
        if i > 0 {
            out.extend_from_slice(separator);
        }
        out.extend_from_slice(part);
    }
    out
}

/// Replaces every occurrence of `from` in `haystack` with `to`.
#[must_use]
pub fn replace(haystack: &[u8], from: &[u8], to: &[u8]) -> Vec<u8> {
    if from.is_empty() {
        return haystack.to_vec();
    }
    let parts = split(haystack, from);
    let views: Vec<&[u8]> = parts.iter().map(Vec::as_slice).collect();
    join(&views, to)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buffer_accumulates_bytes() {
        let mut buf = Buffer::with_capacity(16);
        buf.write(b"hello");
        buf.push(b' ');
        buf.write_str("world");
        assert_eq!(buf.as_slice(), b"hello world");
        assert_eq!(buf.len(), 11);
    }

    #[test]
    fn builder_produces_string() {
        let mut b = Builder::new();
        b.write("hi");
        b.write_char(' ');
        b.write("there");
        assert_eq!(b.build(), "hi there");
    }

    #[test]
    fn index_of_finds_needle() {
        assert_eq!(index_of(b"hello world", b"world"), Some(6));
        assert_eq!(index_of(b"hello", b"xyz"), None);
        assert_eq!(index_of(b"hello", b""), Some(0));
    }

    #[test]
    fn split_and_join_round_trip() {
        let parts = split(b"a,b,,c", b",");
        let joined = join(&parts.iter().map(Vec::as_slice).collect::<Vec<_>>(), b",");
        assert_eq!(joined, b"a,b,,c");
    }

    #[test]
    fn replace_rewrites_all_occurrences() {
        assert_eq!(replace(b"aaabaaa", b"aa", b"X"), b"XabXa");
        assert_eq!(replace(b"", b"a", b"b"), b"");
    }

    #[test]
    fn prefix_and_suffix_helpers_match_expected() {
        assert!(has_prefix(b"hello", b"he"));
        assert!(has_suffix(b"hello", b"lo"));
        assert!(!has_prefix(b"a", b"abc"));
    }
}
