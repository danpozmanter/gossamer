//! Character cursor used by the lexer to walk UTF-8 source.

/// Sentinel character returned by `Cursor::peek` and friends when past
/// the end of the input.
pub(crate) const EOF_CHAR: char = '\0';

/// Character-level cursor over a `&str` with lookahead and byte-offset
/// tracking. Lexer helpers use this to consume source without juggling
/// indices by hand.
pub(crate) struct Cursor<'src> {
    source: &'src str,
    offset: usize,
}

impl<'src> Cursor<'src> {
    /// Returns a new cursor positioned at the start of `source`.
    pub(crate) const fn new(source: &'src str) -> Self {
        Self { source, offset: 0 }
    }

    /// Returns the current byte offset into the source.
    pub(crate) const fn offset(&self) -> usize {
        self.offset
    }

    /// Returns the underlying source string.
    pub(crate) const fn source(&self) -> &'src str {
        self.source
    }

    /// Returns `true` when no more input is available.
    pub(crate) fn is_eof(&self) -> bool {
        self.offset >= self.source.len()
    }

    /// Returns the remainder of the input from the current offset.
    pub(crate) fn rest(&self) -> &'src str {
        &self.source[self.offset..]
    }

    /// Returns the next character without consuming it.
    pub(crate) fn peek(&self) -> char {
        self.rest().chars().next().unwrap_or(EOF_CHAR)
    }

    /// Returns the character `n` positions ahead of the cursor.
    pub(crate) fn peek_nth(&self, n: usize) -> char {
        self.rest().chars().nth(n).unwrap_or(EOF_CHAR)
    }

    /// Consumes and returns the next character, or `None` at end of input.
    pub(crate) fn bump(&mut self) -> Option<char> {
        let next = self.rest().chars().next()?;
        self.offset += next.len_utf8();
        Some(next)
    }

    /// Consumes the next character when `predicate` accepts it.
    pub(crate) fn bump_if(&mut self, predicate: impl FnOnce(char) -> bool) -> bool {
        if predicate(self.peek()) {
            self.bump();
            true
        } else {
            false
        }
    }

    /// Consumes characters while `predicate` accepts them.
    pub(crate) fn bump_while(&mut self, mut predicate: impl FnMut(char) -> bool) {
        while !self.is_eof() && predicate(self.peek()) {
            self.bump();
        }
    }
}
