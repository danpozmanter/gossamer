//! Registry of source files and byte-offset to line/column resolution.

use crate::span::{FileId, LineCol, Span};

/// A single file registered with a `SourceMap`.
#[derive(Debug)]
struct SourceFile {
    name: String,
    source: String,
    line_starts: Vec<u32>,
}

impl SourceFile {
    /// Builds a new source file record, indexing line start offsets.
    fn new(name: String, source: String) -> Self {
        let mut line_starts = vec![0u32];
        for (index, byte) in source.bytes().enumerate() {
            if byte == b'\n' {
                let next_line_start = u32::try_from(index + 1).expect("source file exceeds 4 GiB");
                line_starts.push(next_line_start);
            }
        }
        Self {
            name,
            source,
            line_starts,
        }
    }

    /// Returns the one-based line and column for `offset` in this file.
    fn line_col(&self, offset: u32) -> LineCol {
        let line_index = match self.line_starts.binary_search(&offset) {
            Ok(exact) => exact,
            Err(after) => after.saturating_sub(1),
        };
        let line_start = self.line_starts[line_index];
        let column_bytes = &self.source.as_bytes()[line_start as usize..offset as usize];
        let column_chars = std::str::from_utf8(column_bytes)
            .map_or(column_bytes.len(), |s| s.chars().count());
        LineCol {
            line: u32::try_from(line_index + 1).unwrap_or(u32::MAX),
            column: u32::try_from(column_chars + 1).unwrap_or(u32::MAX),
        }
    }
}

/// Registry of source files. Gives every file a stable `FileId` and
/// resolves `Span`s back to line/column positions.
#[derive(Debug, Default)]
pub struct SourceMap {
    files: Vec<SourceFile>,
}

impl SourceMap {
    /// Returns an empty source map.
    #[must_use]
    pub const fn new() -> Self {
        Self { files: Vec::new() }
    }

    /// Registers a new file and returns its `FileId`.
    pub fn add_file(&mut self, name: impl Into<String>, source: impl Into<String>) -> FileId {
        let file_id = FileId(u32::try_from(self.files.len()).expect("too many source files"));
        self.files.push(SourceFile::new(name.into(), source.into()));
        file_id
    }

    /// Returns the display name registered for `file`.
    #[must_use]
    pub fn file_name(&self, file: FileId) -> &str {
        &self.files[file.0 as usize].name
    }

    /// Returns the full source text of `file`.
    #[must_use]
    pub fn source(&self, file: FileId) -> &str {
        &self.files[file.0 as usize].source
    }

    /// Returns the source slice covered by `span`.
    #[must_use]
    pub fn slice(&self, span: Span) -> &str {
        let source = self.source(span.file);
        &source[span.start as usize..span.end as usize]
    }

    /// Returns the one-based line and column of `offset` in `file`.
    #[must_use]
    pub fn line_col(&self, file: FileId, offset: u32) -> LineCol {
        self.files[file.0 as usize].line_col(offset)
    }
}
