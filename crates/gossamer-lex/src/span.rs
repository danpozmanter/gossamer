//! Source byte-range types used by the lexer and later compiler passes.

/// Opaque identifier for a file registered in a `SourceMap`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct FileId(pub(crate) u32);

impl FileId {
    /// Returns the raw numeric index of this file identifier.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self.0
    }
}

/// Half-open byte range `[start, end)` within a single source file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Span {
    /// File this span points into.
    pub file: FileId,
    /// Inclusive starting byte offset.
    pub start: u32,
    /// Exclusive ending byte offset.
    pub end: u32,
}

impl Span {
    /// Returns a new span covering `[start, end)` in `file`.
    #[must_use]
    pub const fn new(file: FileId, start: u32, end: u32) -> Self {
        Self { file, start, end }
    }

    /// Returns the number of bytes covered by this span.
    #[must_use]
    pub const fn len(self) -> u32 {
        self.end - self.start
    }

    /// Returns `true` when the span covers zero bytes.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.start == self.end
    }

    /// Returns the smallest span covering both `self` and `other`.
    ///
    /// Both spans must belong to the same file.
    #[must_use]
    pub fn join(self, other: Self) -> Self {
        debug_assert_eq!(self.file, other.file, "join across distinct files");
        Self::new(
            self.file,
            self.start.min(other.start),
            self.end.max(other.end),
        )
    }
}

/// One-based line and column position within a source file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct LineCol {
    /// One-based line number.
    pub line: u32,
    /// One-based column number, measured in Unicode scalar values.
    pub column: u32,
}
