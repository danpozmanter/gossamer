//! Per-document state held by the LSP server.
//! A session is cheap: for each open document we keep the source
//! text, the source-map file id, and the outputs of the last full
//! front-end pipeline run. Every `didOpen` / `didChange` rebuilds
//! them — the front end is fast enough that incremental reuse is
//! not yet worth its complexity.

#![forbid(unsafe_code)]

use gossamer_ast::{Ident, ItemKind, SourceFile};
use gossamer_diagnostics::Diagnostic;
use gossamer_lex::{FileId, SourceMap, Span};
use gossamer_parse::parse_source_file;
use gossamer_resolve::{Resolutions, resolve_source_file};
use gossamer_types::{TyCtxt, TypeTable, typecheck_source_file};

use crate::navigation::DefinitionIndex;

/// Path-aware cursor context produced by [`DocumentAnalysis::cursor_context`].
/// Decomposes the source slice immediately to the left of the cursor into
/// the partial identifier the user is typing (`suffix`) plus the path
/// segments that preceded it (`qualifier`).
#[derive(Debug, Clone, Default)]
pub(crate) struct CursorContext<'a> {
    /// Identifier prefix immediately to the left of the cursor.
    /// Empty when the cursor sits in whitespace or on punctuation.
    pub suffix: &'a str,
    /// `::`-joined identifier segments preceding `suffix`. Empty when
    /// the cursor is on a bare prefix.
    pub qualifier: Vec<&'a str>,
    /// `true` when the cursor follows a `.` (receiver-method position).
    pub is_method_position: bool,
    /// `true` when the cursor is inside a `use ...` statement.
    pub is_use_context: bool,
}

impl<'a> CursorContext<'a> {
    /// Returns `qualifier` as a borrowed slice. Convenience for callers
    /// who want to forward the segments to a `&[&str]` API.
    #[must_use]
    pub(crate) fn qualifier_segments(&self) -> Vec<&'a str> {
        self.qualifier.clone()
    }
}

/// Analysis result for a single document.
#[allow(dead_code)]
pub(crate) struct DocumentAnalysis {
    pub(crate) uri: String,
    pub(crate) source: String,
    pub(crate) file: FileId,
    pub(crate) map: SourceMap,
    pub(crate) sf: SourceFile,
    pub(crate) resolutions: Resolutions,
    pub(crate) types: TypeTable,
    pub(crate) tcx: TyCtxt,
    pub(crate) diagnostics: Vec<Diagnostic>,
    pub(crate) top_level: Vec<(Ident, Span)>,
    pub(crate) index: DefinitionIndex,
}

/// Runs the full pipeline over `source` and returns the resulting
/// [`DocumentAnalysis`].
pub(crate) fn analyse(uri: &str, source: &str) -> DocumentAnalysis {
    let mut map = SourceMap::new();
    let file = map.add_file(uri.to_string(), source.to_string());
    let (sf, parse_diags) = parse_source_file(source, file);
    let (resolutions, resolve_diags) = resolve_source_file(&sf);
    let mut tcx = TyCtxt::new();
    let (types, type_diags) = typecheck_source_file(&sf, &resolutions, &mut tcx);

    let mut diagnostics: Vec<Diagnostic> = Vec::new();
    diagnostics.extend(
        parse_diags
            .iter()
            .map(gossamer_parse::ParseDiagnostic::to_diagnostic),
    );
    diagnostics.extend(resolve_diags.iter().map(|d| d.to_diagnostic(&[])));
    diagnostics.extend(
        type_diags
            .iter()
            .map(gossamer_types::TypeDiagnostic::to_diagnostic),
    );

    let top_level = collect_top_level(&sf);
    let index = DefinitionIndex::build(&sf, source, &resolutions);

    DocumentAnalysis {
        uri: uri.to_string(),
        source: source.to_string(),
        file,
        map,
        sf,
        resolutions,
        types,
        tcx,
        diagnostics,
        top_level,
        index,
    }
}

fn collect_top_level(sf: &SourceFile) -> Vec<(Ident, Span)> {
    let mut out = Vec::new();
    for item in &sf.items {
        match &item.kind {
            ItemKind::Fn(decl) => out.push((decl.name.clone(), item.span)),
            ItemKind::Struct(decl) => out.push((decl.name.clone(), item.span)),
            ItemKind::Enum(decl) => out.push((decl.name.clone(), item.span)),
            ItemKind::Trait(decl) => out.push((decl.name.clone(), item.span)),
            ItemKind::TypeAlias(decl) => out.push((decl.name.clone(), item.span)),
            ItemKind::Const(decl) => out.push((decl.name.clone(), item.span)),
            ItemKind::Static(decl) => out.push((decl.name.clone(), item.span)),
            ItemKind::Mod(decl) => out.push((decl.name.clone(), item.span)),
            ItemKind::Impl(_) | ItemKind::AttrItem(_) => {}
        }
    }
    out
}

impl DocumentAnalysis {
    /// Translates a 0-based (line, column) LSP position into a byte
    /// offset, or `None` when the position is past EOF.
    #[must_use]
    pub(crate) fn position_to_offset(&self, line: u32, column: u32) -> Option<u32> {
        let mut current_line = 0u32;
        let mut offset = 0u32;
        let bytes = self.source.as_bytes();
        while offset < bytes.len() as u32 {
            if current_line == line {
                return Some(offset + column);
            }
            if bytes[offset as usize] == b'\n' {
                current_line += 1;
            }
            offset += 1;
        }
        if current_line == line {
            return Some(offset + column);
        }
        None
    }

    /// Translates a byte offset back into an LSP 0-based
    /// (line, column) position.
    #[must_use]
    pub(crate) fn offset_to_position(&self, offset: u32) -> (u32, u32) {
        let mut line = 0u32;
        let mut column = 0u32;
        let bytes = self.source.as_bytes();
        let cap = std::cmp::min(offset as usize, bytes.len());
        for &b in &bytes[..cap] {
            if b == b'\n' {
                line += 1;
                column = 0;
            } else {
                column += 1;
            }
        }
        (line, column)
    }

    /// Returns the identifier covering `offset`, if any. Used by
    /// hover and go-to-def to map a cursor onto a symbol.
    #[must_use]
    pub(crate) fn word_at(&self, offset: u32) -> Option<&str> {
        let bytes = self.source.as_bytes();
        let offset = offset as usize;
        if offset > bytes.len() {
            return None;
        }
        let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
        let mut start = offset;
        while start > 0 && is_word(bytes[start - 1]) {
            start -= 1;
        }
        let mut end = offset;
        while end < bytes.len() && is_word(bytes[end]) {
            end += 1;
        }
        if start == end {
            return None;
        }
        std::str::from_utf8(&bytes[start..end]).ok()
    }

    /// Returns the span of the top-level item declaring `name`, if
    /// any.
    #[must_use]
    pub(crate) fn top_level_span(&self, name: &str) -> Option<Span> {
        self.top_level
            .iter()
            .find(|(ident, _)| ident.name == name)
            .map(|(_, span)| *span)
    }

    /// Path-aware cursor context. Walks left from `offset` over the
    /// source bytes and decomposes the construct under the cursor into
    /// `(qualifier, suffix)` plus a couple of position flags. This is
    /// the input every modern completion path consumes.
    #[must_use]
    pub(crate) fn cursor_context(&self, offset: u32) -> CursorContext<'_> {
        let bytes = self.source.as_bytes();
        let mut end = (offset as usize).min(bytes.len());
        let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
        // Walk left across the suffix word (the partial identifier the
        // cursor is currently typing).
        let mut start = end;
        while start > 0 && is_word(bytes[start - 1]) {
            start -= 1;
        }
        let suffix_start = start;
        let suffix_end = end;
        // Pre-suffix marker for detecting `.` (method position) or `::`
        // (path qualifier).
        let mut qualifier: Vec<&str> = Vec::new();
        let mut is_method_position = false;
        let mut scan = start;
        // Detect `::` immediately preceding the suffix.
        while scan >= 2 && bytes[scan - 1] == b':' && bytes[scan - 2] == b':' {
            scan -= 2;
            // Walk left over a word.
            let seg_end = scan;
            while scan > 0 && is_word(bytes[scan - 1]) {
                scan -= 1;
            }
            let seg_start = scan;
            if seg_start == seg_end {
                break;
            }
            if let Ok(seg) = std::str::from_utf8(&bytes[seg_start..seg_end]) {
                qualifier.push(seg);
            } else {
                break;
            }
        }
        qualifier.reverse();
        // Method position: a single `.` immediately before the suffix
        // (or the qualifier head if any).
        let dot_pos = if qualifier.is_empty() { start } else { scan };
        if dot_pos > 0 && bytes[dot_pos - 1] == b'.' {
            // Make sure it's not a `..` (range op) — if so leave it alone.
            if !(dot_pos >= 2 && bytes[dot_pos - 2] == b'.') {
                is_method_position = true;
            }
        }
        // Use-statement detection: scan backwards across the line
        // (skipping word/`::` chars + whitespace) and look for a leading
        // `use` keyword at the start of the current statement.
        let is_use_context = is_inside_use_statement(bytes, suffix_start);
        end = suffix_end;
        let suffix = std::str::from_utf8(&bytes[suffix_start..end]).unwrap_or("");
        CursorContext {
            suffix,
            qualifier,
            is_method_position,
            is_use_context,
        }
    }

    /// Returns every byte-range occurrence of `name` in the document,
    /// matched as a whole word. This is the legacy text-based
    /// fallback used when no resolution is available; semantic
    /// callers should prefer `find_semantic_references`.
    #[must_use]
    pub(crate) fn find_references(&self, name: &str) -> Vec<Span> {
        if name.is_empty() {
            return Vec::new();
        }
        let bytes = self.source.as_bytes();
        let needle = name.as_bytes();
        let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
        let mut out = Vec::new();
        let mut cursor = 0;
        while cursor + needle.len() <= bytes.len() {
            if &bytes[cursor..cursor + needle.len()] != needle {
                cursor += 1;
                continue;
            }
            let before_ok = cursor == 0 || !is_word(bytes[cursor - 1]);
            let after_ok =
                cursor + needle.len() == bytes.len() || !is_word(bytes[cursor + needle.len()]);
            if before_ok && after_ok {
                let end = (cursor + needle.len()) as u32;
                out.push(Span::new(self.file, cursor as u32, end));
                cursor += needle.len();
            } else {
                cursor += 1;
            }
        }
        out
    }
}

/// True when the byte at `pos` in `bytes` sits inside a `use ...`
/// statement. Walks left across the current statement (stopping at the
/// nearest `;`, `{`, or `}`) and checks whether the first non-whitespace
/// run is the keyword `use`.
fn is_inside_use_statement(bytes: &[u8], pos: usize) -> bool {
    let cap = pos.min(bytes.len());
    let mut idx = cap;
    while idx > 0 {
        match bytes[idx - 1] {
            b';' | b'{' | b'}' => break,
            _ => idx -= 1,
        }
    }
    while idx < cap && bytes[idx].is_ascii_whitespace() {
        idx += 1;
    }
    let needle = b"use";
    if idx + needle.len() > cap {
        return false;
    }
    if &bytes[idx..idx + needle.len()] != needle {
        return false;
    }
    let after = idx + needle.len();
    after < bytes.len()
        && (bytes[after].is_ascii_whitespace() || bytes[after] == b':' || bytes[after] == b'{')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx_at(source: &str) -> CursorContext<'_> {
        let cursor = source.find('|').expect("expected | cursor marker");
        // We can't construct a DocumentAnalysis without running the
        // parser, so call cursor_context against a synthetic doc that
        // shares only the source/file. Build via `analyse` for fidelity.
        // Static lifetime-erased buffer keeps the borrow valid.
        let cleaned: String = source[..cursor].to_string() + &source[cursor + 1..];
        let doc = Box::leak(Box::new(analyse("file:///t.gos", &cleaned)));
        let offset = u32::try_from(cursor).expect("cursor offset");
        doc.cursor_context(offset)
    }

    #[test]
    fn cursor_context_extracts_qualifier() {
        let ctx = ctx_at("fn main() { os::path::p| }\n");
        assert_eq!(ctx.suffix, "p");
        assert_eq!(ctx.qualifier, vec!["os", "path"]);
        assert!(!ctx.is_method_position);
        assert!(!ctx.is_use_context);
    }

    #[test]
    fn cursor_context_handles_method_position() {
        let ctx = ctx_at("fn main() { let v = vec![1]; v.p| }\n");
        assert_eq!(ctx.suffix, "p");
        assert!(ctx.qualifier.is_empty());
        assert!(ctx.is_method_position);
    }

    #[test]
    fn cursor_context_detects_use_statement() {
        let ctx = ctx_at("use std::os::|\n");
        assert_eq!(ctx.suffix, "");
        assert_eq!(ctx.qualifier, vec!["std", "os"]);
        assert!(ctx.is_use_context);
    }

    #[test]
    fn cursor_context_bare_prefix_returns_no_qualifier() {
        let ctx = ctx_at("fn main() { gr| }\n");
        assert_eq!(ctx.suffix, "gr");
        assert!(ctx.qualifier.is_empty());
        assert!(!ctx.is_method_position);
        assert!(!ctx.is_use_context);
    }
}
