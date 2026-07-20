//! Per-document state held by the LSP server.
//! A session is cheap: for each open document we keep the source
//! text, the source-map file id, and the outputs of the last full
//! front-end pipeline run. Every `didOpen` / `didChange` rebuilds
//! them - the front end is fast enough that incremental reuse is
//! not yet worth its complexity.

#![forbid(unsafe_code)]

use gossamer_ast::SourceFile;
use gossamer_diagnostics::Diagnostic;
use gossamer_lex::{FileId, SourceMap, Span};
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
///
/// Memory-shape note: the source text is held only inside `map`
/// (the `SourceMap` already owns one copy) and surfaced through
/// `source()` instead of being duplicated as a separate `String`
/// field. Top-level item names/spans are surfaced through
/// `top_level_span` / `index.def_iter()` instead of being mirrored
/// in a parallel `Vec<(Ident, Span)>`. Both changes shave per-file
/// LSP residency on workspaces with many open documents.
#[allow(
    dead_code,
    reason = "fields are reads from the LSP request handlers populated lazily as capabilities expand"
)]
pub(crate) struct DocumentAnalysis {
    pub(crate) uri: String,
    pub(crate) file: FileId,
    pub(crate) map: SourceMap,
    pub(crate) sf: SourceFile,
    pub(crate) resolutions: Resolutions,
    pub(crate) types: TypeTable,
    pub(crate) tcx: TyCtxt,
    pub(crate) diagnostics: Vec<Diagnostic>,
    pub(crate) index: DefinitionIndex,
    /// Byte length of the text the editor actually holds. The stored
    /// source is the AUGMENTED program (user text + synthesized
    /// autoderive tail appended); anything the server sends back to
    /// the client - formatting edits above all - must stay within
    /// this prefix.
    pub(crate) user_len: u32,
}

/// Runs the full pipeline over `source` and returns the resulting
/// [`DocumentAnalysis`].
pub(crate) fn analyse(uri: &str, source: &str) -> DocumentAnalysis {
    // Mirror the driver pipeline: the parse-time autoderive step
    // synthesizes the serde free functions (`from_json::<T>` and
    // friends), `#[derive]` impls, and stdlib struct wrappers. The
    // synthesized text is APPENDED, so every user-code span is
    // unchanged; without this step the LSP reports `from_json` (and
    // every other synthesized name) as unresolved while `gos check`
    // accepts the file.
    let augmented = gossamer_parse::autoderive::augment_source(source);
    let user_len = u32::try_from(source.len()).unwrap_or(u32::MAX);
    let mut map = SourceMap::new();
    let file = map.add_file(uri.to_string(), augmented.clone());
    let (sf, parse_diags) = gossamer_parse::autoderive::parse_with_autoderive(&augmented, file);
    let (resolutions, resolve_diags) = resolve_source_file(&sf);
    let mut tcx = TyCtxt::new();
    let (types, type_diags) = typecheck_source_file(&sf, &resolutions, &mut tcx);

    let mut diagnostics: Vec<Diagnostic> = Vec::new();
    diagnostics.extend(
        parse_diags
            .iter()
            .map(gossamer_parse::ParseDiagnostic::to_diagnostic),
    );
    // Seed `did you mean ...?` candidates from the file's
    // top-level item names so the resolver attaches a Suggestion
    // that LSP code-actions can surface as a quickfix.
    let in_scope = collect_top_level_names(&sf);
    diagnostics.extend(resolve_diags.iter().map(|d| d.to_diagnostic(&in_scope)));
    diagnostics.extend(
        type_diags
            .iter()
            .map(gossamer_types::TypeDiagnostic::to_diagnostic),
    );
    // Editors should see the same default lint findings as `gos lint`.
    // Parse the user source without the synthesized autoderive tail so
    // lint spans and fixes can never point outside the editor buffer.
    let (lint_sf, lint_parse_diags) = gossamer_parse::parse_source_file(source, file);
    if lint_parse_diags.is_empty() {
        let mut registry = gossamer_lint::Registry::with_defaults();
        for item in &lint_sf.items {
            gossamer_lint::apply_attributes(&item.attrs, &mut registry);
        }
        let mut lint_diagnostics = gossamer_lint::run(&lint_sf, source, &registry);
        let lint_fixes = gossamer_lint::fixes(&lint_sf, &registry, source);
        attach_lint_fixes(&mut lint_diagnostics, lint_fixes);
        diagnostics.extend(lint_diagnostics);
    }
    // Diagnostics pointing into the synthesized tail would land past
    // the end of the buffer the editor displays; `gos check` surfaces
    // them against the augmented text, but an LSP client cannot.
    // `<=` keeps unexpected-EOF parse errors, which point exactly AT
    // the user text's end; the synthesized tail begins at least two
    // newlines later.
    diagnostics.retain(|d| {
        d.labels
            .iter()
            .find(|l| l.primary)
            .or_else(|| d.labels.first())
            .is_none_or(|l| l.location.span.start <= user_len)
    });

    let index = DefinitionIndex::build(&sf, &augmented, &resolutions);

    DocumentAnalysis {
        uri: uri.to_string(),
        file,
        map,
        sf,
        resolutions,
        types,
        tcx,
        diagnostics,
        index,
        user_len,
    }
}

/// Attaches `gos lint --fix` edits to their nearest same-lint
/// diagnostic so the generic LSP suggestion path can expose them.
fn attach_lint_fixes(diagnostics: &mut [Diagnostic], fixes: Vec<gossamer_lint::Fix>) {
    use gossamer_diagnostics::{Location, Suggestion};

    for fix in fixes {
        let lint_note = format!("lint: {}", fix.lint_id);
        let best = diagnostics
            .iter()
            .enumerate()
            .filter(|(_, diag)| diag.notes.iter().any(|note| note == &lint_note))
            .min_by_key(|(_, diag)| {
                let start = diag
                    .labels
                    .iter()
                    .find(|label| label.primary)
                    .or_else(|| diag.labels.first())
                    .map_or(0, |label| label.location.span.start);
                start.abs_diff(fix.span.start)
            })
            .map(|(index, _)| index);
        let Some(index) = best else {
            continue;
        };
        let title = fix.lint_id.replace('_', " ");
        diagnostics[index].suggestions.push(Suggestion::replacement(
            Location::new(fix.span.file, fix.span),
            format!("Fix {title}"),
            fix.replacement,
        ));
    }
}

/// Best-effort enumeration of every top-level item name a source
/// file declares. Seeds the resolver's `did you mean ...?`
/// suggestion candidates so `GR0001` diagnostics carry a
/// machine-applicable replacement that LSP code-actions can
/// surface as a quickfix. Mirrors the same function in
/// `gossamer-cli/src/loaders.rs`; duplicated here so the LSP
/// crate stays decoupled from the CLI crate.
fn collect_top_level_names(sf: &gossamer_ast::SourceFile) -> Vec<&str> {
    let mut out = Vec::new();
    for item in &sf.items {
        match &item.kind {
            gossamer_ast::ItemKind::Fn(decl) => out.push(decl.name.name.as_str()),
            gossamer_ast::ItemKind::Struct(decl) => out.push(decl.name.name.as_str()),
            gossamer_ast::ItemKind::Enum(decl) => out.push(decl.name.name.as_str()),
            gossamer_ast::ItemKind::Trait(decl) => out.push(decl.name.name.as_str()),
            gossamer_ast::ItemKind::TypeAlias(decl) => out.push(decl.name.name.as_str()),
            gossamer_ast::ItemKind::Const(decl) => out.push(decl.name.name.as_str()),
            gossamer_ast::ItemKind::Static(decl) => out.push(decl.name.name.as_str()),
            gossamer_ast::ItemKind::Mod(decl) => out.push(decl.name.name.as_str()),
            gossamer_ast::ItemKind::Impl(_) | gossamer_ast::ItemKind::AttrItem(_) => {}
        }
    }
    out
}

impl DocumentAnalysis {
    /// Returns the document's source text. The text lives inside the
    /// embedded `SourceMap`, so this is a borrow into existing
    /// storage - no extra `String` clone per document.
    #[must_use]
    pub(crate) fn source(&self) -> &str {
        self.map.source(self.file)
    }

    /// Returns exactly the text the editor holds: the stored source
    /// minus the synthesized autoderive tail. Everything sent back to
    /// the client (formatting edits above all) must be computed
    /// against this prefix, or edits reference positions past the
    /// client's buffer.
    #[must_use]
    pub(crate) fn user_source(&self) -> &str {
        &self.map.source(self.file)[..self.user_len as usize]
    }

    /// Looks up the source span of the top-level item declaring
    /// `name`. Replaces the previous parallel `top_level: Vec<(Ident,
    /// Span)>` cache by reading from the existing
    /// [`DefinitionIndex`].
    #[must_use]
    pub(crate) fn top_level_span(&self, name: &str) -> Option<Span> {
        // The index records each definition's `name_span` (the
        // identifier itself) but not the whole-item span. For
        // go-to-def of top-level names the identifier span is the
        // editor-friendly target - the previous cache returned the
        // whole item span, but no caller actually needed that
        // wider range; navigation handlers only consume the
        // identifier position to centre the editor view.
        for (_, info) in self.index.def_iter() {
            if info.name == name {
                return Some(info.name_span);
            }
        }
        None
    }

    /// Translates a 0-based LSP position (whose character is a UTF-16
    /// code-unit offset) into a UTF-8 byte offset.
    #[must_use]
    pub(crate) fn position_to_offset(&self, line: u32, column: u32) -> Option<u32> {
        let source = self.user_source();
        let mut line_start = 0usize;
        for _ in 0..line {
            let newline = source[line_start..].find('\n')?;
            line_start += newline + 1;
        }
        let remainder = &source[line_start..];
        let mut line_text = remainder
            .split_once('\n')
            .map_or(remainder, |(text, _)| text);
        if let Some(without_cr) = line_text.strip_suffix('\r') {
            line_text = without_cr;
        }

        let mut utf16_column = 0u32;
        for (byte, ch) in line_text.char_indices() {
            if utf16_column == column {
                return u32::try_from(line_start + byte).ok();
            }
            utf16_column += ch.len_utf16() as u32;
            if utf16_column > column {
                return None;
            }
        }
        (utf16_column == column)
            .then(|| u32::try_from(line_start + line_text.len()).ok())
            .flatten()
    }

    /// Translates a UTF-8 byte offset back into an LSP 0-based
    /// position using UTF-16 code units for the character field.
    #[must_use]
    pub(crate) fn offset_to_position(&self, offset: u32) -> (u32, u32) {
        let source = self.user_source();
        let mut cap = std::cmp::min(offset as usize, source.len());
        while cap > 0 && !source.is_char_boundary(cap) {
            cap -= 1;
        }
        let prefix = &source[..cap];
        let line = prefix.bytes().filter(|byte| *byte == b'\n').count() as u32;
        let line_start = prefix.rfind('\n').map_or(0, |index| index + 1);
        let column = prefix[line_start..].encode_utf16().count() as u32;
        (line, column)
    }

    /// Returns the identifier covering `offset`, if any. Used by
    /// hover and go-to-def to map a cursor onto a symbol.
    #[must_use]
    pub(crate) fn word_at(&self, offset: u32) -> Option<&str> {
        let source = self.user_source();
        let offset = offset as usize;
        if offset > source.len() || !source.is_char_boundary(offset) {
            return None;
        }
        let is_word = |ch: char| ch == '_' || unicode_ident::is_xid_continue(ch);
        let mut start = offset;
        for (index, ch) in source[..offset].char_indices().rev() {
            if !is_word(ch) {
                break;
            }
            start = index;
        }
        let mut end = offset;
        for (relative, ch) in source[offset..].char_indices() {
            if !is_word(ch) {
                break;
            }
            end = offset + relative + ch.len_utf8();
        }
        if start == end {
            return None;
        }
        Some(&source[start..end])
    }

    /// Path-aware cursor context. Walks left from `offset` over the
    /// source bytes and decomposes the construct under the cursor into
    /// `(qualifier, suffix)` plus a couple of position flags. This is
    /// the input every modern completion path consumes.
    #[must_use]
    pub(crate) fn cursor_context(&self, offset: u32) -> CursorContext<'_> {
        let source = self.user_source();
        let bytes = source.as_bytes();
        let mut end = (offset as usize).min(bytes.len());
        while end > 0 && !source.is_char_boundary(end) {
            end -= 1;
        }
        let is_word = |ch: char| ch == '_' || unicode_ident::is_xid_continue(ch);
        // Walk left across the suffix word (the partial identifier the
        // cursor is currently typing).
        let mut start = end;
        while let Some((index, ch)) = source[..start].char_indices().next_back() {
            if !is_word(ch) {
                break;
            }
            start = index;
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
            while let Some((index, ch)) = source[..scan].char_indices().next_back() {
                if !is_word(ch) {
                    break;
                }
                scan = index;
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
            // Make sure it's not a `..` (range op) - if so leave it alone.
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
        let source = self.user_source();
        let is_word = |ch: char| ch == '_' || unicode_ident::is_xid_continue(ch);
        let mut out = Vec::new();
        for (cursor, _) in source.match_indices(name) {
            let end = cursor + name.len();
            let before_ok = source[..cursor]
                .chars()
                .next_back()
                .is_none_or(|ch| !is_word(ch));
            let after_ok = source[end..].chars().next().is_none_or(|ch| !is_word(ch));
            if before_ok && after_ok {
                out.push(Span::new(self.file, cursor as u32, end as u32));
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

    #[test]
    fn lsp_positions_use_utf16_code_units() {
        let doc = analyse("file:///unicode.gos", "fn main() { \"😀\"; café() }\n");
        let cafe_offset = doc.user_source().find("café").unwrap() as u32;
        let (line, column) = doc.offset_to_position(cafe_offset);
        assert_eq!(line, 0);
        assert_eq!(column, 18, "emoji must occupy two UTF-16 code units");
        assert_eq!(doc.position_to_offset(line, column), Some(cafe_offset));
        assert_eq!(
            doc.position_to_offset(0, 14),
            None,
            "a position inside the emoji surrogate pair is invalid"
        );
    }

    #[test]
    fn unicode_words_and_completion_prefixes_remain_whole() {
        let doc = analyse(
            "file:///unicode.gos",
            "fn café() {}\nfn main() { café() }\n",
        );
        let call = doc.user_source().rfind("café").unwrap() as u32;
        assert_eq!(doc.word_at(call + 3), Some("café"));
        assert_eq!(
            doc.cursor_context(call + "café".len() as u32).suffix,
            "café"
        );
        assert_eq!(doc.find_references("café").len(), 2);
    }

    #[test]
    fn top_level_statements_produce_no_spurious_diagnostics() {
        // An entry file with bare top-level statements is implicitly `fn main`;
        // the LSP must analyse it cleanly, not report it as malformed items.
        let doc = analyse(
            "file:///t.gos",
            "println!(\"hi\")\nlet x = 1\nprintln!(\"{}\", x)\n",
        );
        assert!(
            doc.diagnostics.is_empty(),
            "unexpected diagnostics: {:?}",
            doc.diagnostics
        );
    }

    #[test]
    fn top_level_question_operator_is_accepted() {
        let src = "use std::errors\n\
                   fn f() -> Result<i64, errors::Error> { Ok(1) }\n\
                   let n = f()?\n\
                   println!(\"{}\", n)\n";
        let doc = analyse("file:///t.gos", src);
        assert!(
            doc.diagnostics.is_empty(),
            "unexpected diagnostics: {:?}",
            doc.diagnostics
        );
    }

    #[test]
    fn mixing_top_level_statements_with_explicit_main_is_reported() {
        let doc = analyse("file:///t.gos", "println!(\"hi\")\nfn main() { }\n");
        assert!(
            !doc.diagnostics.is_empty(),
            "expected a conflict diagnostic for mixed entry forms"
        );
    }
}
