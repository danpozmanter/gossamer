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

/// Analysis result for a single document.
///
/// Fields beyond `uri`, `source`, `file`, `diagnostics`, and
/// `top_level` are retained for future hover/navigation work
/// (type-aware hover, workspace-symbol) that will query them.
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
    diagnostics.extend(parse_diags.iter().map(gossamer_parse::ParseDiagnostic::to_diagnostic));
    diagnostics.extend(resolve_diags.iter().map(|d| d.to_diagnostic(&[])));
    diagnostics.extend(type_diags.iter().map(gossamer_types::TypeDiagnostic::to_diagnostic));

    let top_level = collect_top_level(&sf);

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

    /// Returns every byte-range occurrence of `name` in the
    /// document, matched as a whole word. Used by
    /// `textDocument/references` and `textDocument/rename`.
    ///
    /// The match is syntactic, not semantic — a re-bound local
    /// with the same spelling as a top-level item is reported
    /// alongside the "real" references. That's a reasonable
    /// first-slice behaviour; semantic filtering lands when the
    /// resolver exposes a use-to-def map.
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
