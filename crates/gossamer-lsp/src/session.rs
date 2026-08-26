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
use gossamer_types::{
    ExhaustivenessError, TyCtxt, TypeTable, check_arena_escapes, check_exhaustiveness,
    normalize_caller_side_spellings, typecheck_source_file,
};

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

/// Assembles the project compilation unit for a `file://` document,
/// using the editor's buffer for the open file and the on-disk text for
/// everything else. Returns `source` unchanged for a document with no
/// filesystem path or one that is not inside a project.
#[cfg(not(target_arch = "wasm32"))]
fn bundle_project_unit(uri: &str, source: &str) -> String {
    let Some(path) = uri_to_path(uri) else {
        return source.to_string();
    };
    gossamer_pkg::bundle::bundle_entry_source(&path, source.to_string())
}

/// The wasm build has no filesystem to read sibling modules from, so a
/// document is its own compilation unit there.
#[cfg(target_arch = "wasm32")]
fn bundle_project_unit(_uri: &str, source: &str) -> String {
    source.to_string()
}

/// Decodes a `file://` URI into a filesystem path, undoing the percent
/// escapes an editor applies to spaces and other reserved characters.
#[cfg(not(target_arch = "wasm32"))]
fn uri_to_path(uri: &str) -> Option<std::path::PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    // `file:///path` (empty authority) is the only form editors send for
    // a local file; anything with a host is not a path we can read.
    let rest = rest.strip_prefix('/').map(|r| format!("/{r}"))?;
    let bytes = rest.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?;
            if let Ok(byte) = u8::from_str_radix(hex, 16) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    Some(std::path::PathBuf::from(String::from_utf8(out).ok()?))
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
    // Mirror the driver's project bundling too: an entry file's sibling
    // and subdirectory modules, plus its path dependencies, form one
    // compilation unit. Without this a cross-module reference that
    // `gos check` / `gos run` resolve reads as an unresolved name in the
    // editor. The bundle appends, so every span in the open buffer is
    // unchanged.
    // The source map strips a leading byte-order mark so spans and
    // diagnostic columns share one basis with the rest of the toolchain.
    // Measure the editor's text after the same strip, or every length
    // derived here describes three bytes the stored source does not have.
    let source = source.strip_prefix('\u{feff}').unwrap_or(source);
    let bundled = bundle_project_unit(uri, source);
    let augmented = gossamer_parse::autoderive::augment_source(&bundled);
    let user_len = u32::try_from(source.len()).unwrap_or(u32::MAX);
    let bundle_len = u32::try_from(bundled.len()).unwrap_or(u32::MAX);
    let mut map = SourceMap::new();
    let file = map.add_file(uri.to_string(), augmented.clone());
    let (mut sf, parse_diags) = gossamer_parse::autoderive::parse_with_autoderive(&augmented, file);
    let (resolutions, resolve_diags) = resolve_source_file(&sf);
    // A named argument, a parameter default, and a std function named in
    // value position are caller-side spellings the checker never sees.
    // Every front end runs this rewrite, or a call that omits a defaulted
    // parameter reaches the checker with fewer arguments than the function
    // declares and reads as an arity error the command line accepts.
    let named_arg_diags = normalize_caller_side_spellings(&mut sf, &resolutions);
    let mut tcx = TyCtxt::new();
    let (types, type_diags) = typecheck_source_file(&sf, &resolutions, &mut tcx);

    let mut diagnostics: Vec<Diagnostic> = Vec::new();
    diagnostics.extend(
        parse_diags
            .iter()
            .map(gossamer_parse::ParseDiagnostic::to_diagnostic),
    );
    // A program that does not parse is not the program the later passes
    // see: `autoderive::augment_source` declines to synthesize from a
    // recovered tree, so the derived `fmt` / `to_string` / serde surface a
    // clean parse would carry is absent, and every pass below would report
    // its absence against a line the user wrote correctly. The parse
    // diagnostics are the actionable report, exactly as on the command
    // line; the passes still run so navigation keeps a type table.
    let parse_failed = !parse_diags.is_empty();
    // Seed `did you mean ...?` candidates from the file's
    // top-level item names so the resolver attaches a Suggestion
    // that LSP code-actions can surface as a quickfix.
    let in_scope = collect_top_level_names(&sf);
    if !parse_failed {
        diagnostics.extend(named_arg_diags.iter().map(|d| d.to_diagnostic(&in_scope)));
        diagnostics.extend(resolve_diags.iter().map(|d| d.to_diagnostic(&in_scope)));
        diagnostics.extend(
            type_diags
                .iter()
                .map(gossamer_types::TypeDiagnostic::to_diagnostic),
        );
        // The editor must run every phase the command-line gate runs, or a
        // file reads clean here and fails `gos check`. Exhaustiveness
        // (GM0001) and arena escape (GM0003) are fatal there, so they are
        // reported here under the same policy.
        for diag in check_exhaustiveness(&sf, &resolutions, &types, &tcx) {
            if matches!(diag.error, ExhaustivenessError::NonExhaustive { .. }) {
                diagnostics.push(diag.to_diagnostic());
            }
        }
        for diag in check_arena_escapes(&sf, &resolutions, &types, &tcx) {
            diagnostics.push(diag.to_diagnostic());
        }
    }
    // The comptime fold lowers the program, so it runs only once every
    // earlier phase has accepted it - exactly the order `gos check` uses.
    if diagnostics.is_empty()
        && let Some(diag) = crate::comptime::fold_diagnostic(
            uri,
            &augmented,
            &sf,
            &resolutions,
            &types,
            &mut tcx,
            file,
        )
    {
        diagnostics.push(diag);
    }
    // Editors should see the same default lint findings as `gos lint`.
    // Parse the user source without the synthesized autoderive tail so
    // lint spans and fixes can never point outside the editor buffer.
    let (lint_sf, lint_parse_diags) = gossamer_parse::parse_source_file(source, file);
    if lint_parse_diags.is_empty() {
        let mut registry = gossamer_lint::Registry::with_defaults();
        gossamer_lint::apply_attributes(&lint_sf.attrs, &mut registry);
        let mut lint_diagnostics = gossamer_lint::run(&lint_sf, source, &registry);
        let lint_fixes = gossamer_lint::fixes(&lint_sf, &registry, source);
        attach_lint_fixes(&mut lint_diagnostics, lint_fixes);
        diagnostics.extend(lint_diagnostics);
    }
    // A diagnostic anchored in the synthesized autoderive tail still
    // describes a defect in the user's own declarations, so it is moved
    // onto the construct that caused the synthesis rather than dropped:
    // dropping it lets the editor call a file clean that `gos check`
    // rejects.
    crate::synthesis::reanchor_out_of_buffer(
        &mut diagnostics,
        &sf,
        &augmented,
        file,
        user_len,
        bundle_len,
    );

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
            "println(\"hi\")\nlet x = 1\nprintln(\"{}\", x)\n",
        );
        assert!(
            doc.diagnostics.is_empty(),
            "unexpected diagnostics: {:?}",
            doc.diagnostics
        );
    }

    #[test]
    fn top_level_stdlib_paths_mark_grouped_imports_used() {
        let doc = analyse(
            "file:///imports.gos",
            "use std::{env, fs}\n\
             let root = env::args().first()\n\
             let exists = fs::exists(\".\")\n\
             println(\"{} {:?}\", exists, root)\n",
        );
        assert!(
            doc.diagnostics
                .iter()
                .all(|diag| diag.code.as_str() != "GL0002"),
            "used grouped imports were reported unused: {:?}",
            doc.diagnostics
        );
    }

    #[test]
    fn top_level_question_operator_is_accepted() {
        let src = "use std::errors\n\
                   fn f() -> Result<i64, errors::Error> { Ok(1) }\n\
                   let n = f()?\n\
                   println(\"{}\", n)\n";
        let doc = analyse("file:///t.gos", src);
        assert!(
            doc.diagnostics.is_empty(),
            "unexpected diagnostics: {:?}",
            doc.diagnostics
        );
    }

    #[test]
    fn bare_mutable_argument_reports_explicit_reference_diagnostic() {
        let doc = analyse(
            "file:///mut-arg.gos",
            "fn change(value: &mut i64) { *value = 0 }\n\
             fn main() { let mut value = 1\n change(value) }\n",
        );
        assert!(
            doc.diagnostics
                .iter()
                .any(|diag| diag.code.as_str() == "GT0046"),
            "expected explicit mutable-reference diagnostic: {:?}",
            doc.diagnostics
        );
    }

    #[test]
    fn a_parse_error_suppresses_the_later_phases_report() {
        // Synthesis declines to run on a recovered tree, so the struct's
        // derived `fmt` is absent and the checker would report the call on
        // line 2 as an unknown method. The command line reports the parse
        // error alone; the editor must agree.
        let doc = analyse(
            "file:///parse-gate.gos",
            "struct Point(i64)\n             println(\"{}\", Point(1).fmt())\n             println({}, Point(1))\n",
        );
        let codes: Vec<&str> = doc
            .diagnostics
            .iter()
            .map(|diag| diag.code.as_str())
            .collect();
        assert_eq!(
            codes,
            vec!["GP0024"],
            "only the parse error is actionable: {:?}",
            doc.diagnostics
        );
    }

    #[test]
    fn defaulted_and_named_arguments_type_check() {
        let doc = analyse(
            "file:///named-args.gos",
            "fn volume(width: i64, height: i64 = 2) -> i64 { width * height }\n             println(\"{}\", volume(2))\n             println(\"{}\", volume(width: 2, height: 3))\n",
        );
        assert!(
            doc.diagnostics.is_empty(),
            "unexpected diagnostics: {:?}",
            doc.diagnostics
        );
    }

    #[test]
    fn mixing_top_level_statements_with_explicit_main_is_reported() {
        let doc = analyse("file:///t.gos", "println(\"hi\")\nfn main() { }\n");
        assert!(
            !doc.diagnostics.is_empty(),
            "expected a conflict diagnostic for mixed entry forms"
        );
    }
}
