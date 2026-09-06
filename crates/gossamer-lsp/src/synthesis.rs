//! Mapping of diagnostics that land outside the editor's buffer back
//! onto the user construct that produced them.
//!
//! The analysed text is the package's compilation unit followed by the
//! parse-time autoderive tail (`__gos_serde_*` free functions,
//! `#[derive]` impls, stdlib struct wrappers). The editor's buffer is one
//! window inside the unit - the whole of it for a package entry, the
//! inlined body of one `mod` for any other file. A diagnostic anchored in
//! the autoderive tail still describes a defect in the user's own code,
//! so it is re-anchored on the type whose synthesis produced it; one
//! anchored in another file of the unit belongs to that file's own
//! document and is dropped here.

use gossamer_ast::{ItemKind, SourceFile};
use gossamer_diagnostics::{Diagnostic, Label, Location};
use gossamer_lex::{FileId, Span};

/// A user-declared type the autoderive tail can be synthesized for.
struct UserType<'a> {
    name: &'a str,
    name_span: Span,
}

/// Re-anchors every diagnostic whose primary span lies in the
/// synthesized autoderive tail onto the declaration of the type the
/// synthesis was generated for, and removes the ones that describe
/// another file of the unit or have no user-visible cause.
///
/// `doc_start .. doc_start + user_len` is the window the editor's buffer
/// occupies in the assembled unit and `bundle_len` the unit's own length;
/// the autoderive tail begins at `bundle_len`.
pub(crate) fn reanchor_out_of_buffer(
    diagnostics: &mut Vec<Diagnostic>,
    sf: &SourceFile,
    augmented: &str,
    file: FileId,
    doc_start: u32,
    user_len: u32,
    bundle_len: u32,
) {
    let doc_end = doc_start.saturating_add(user_len);
    let user_types = collect_user_types(sf, augmented, doc_start, doc_end);
    diagnostics.retain_mut(|diag| {
        let Some(start) = primary_start(diag) else {
            return true;
        };
        // `<=` keeps unexpected-EOF parse errors, which point exactly at
        // the buffer's end; whatever follows begins at least two newlines
        // later.
        if start >= doc_start && start <= doc_end {
            return true;
        }
        // A diagnostic in another module of the unit belongs to that
        // module's own file, which this document does not describe.
        if start < bundle_len {
            return false;
        }
        match anchor_for(augmented, diag, start, &user_types) {
            Some(anchor) => {
                reanchor(diag, file, anchor.name_span, anchor.name);
                true
            }
            None => false,
        }
    });
}

/// Moves every span `diag` carries `by` bytes further into the text it is
/// reported against.
pub(crate) fn shift_spans(diag: &mut Diagnostic, by: u32) {
    if by == 0 {
        return;
    }
    for label in &mut diag.labels {
        label.location.span.start = label.location.span.start.saturating_add(by);
        label.location.span.end = label.location.span.end.saturating_add(by);
    }
    for suggestion in &mut diag.suggestions {
        suggestion.location.span.start = suggestion.location.span.start.saturating_add(by);
        suggestion.location.span.end = suggestion.location.span.end.saturating_add(by);
    }
}

/// Byte offset of the diagnostic's primary span, or the first label's
/// when no label is marked primary.
fn primary_start(diag: &Diagnostic) -> Option<u32> {
    diag.labels
        .iter()
        .find(|label| label.primary)
        .or_else(|| diag.labels.first())
        .map(|label| label.location.span.start)
}

/// Every struct and enum the editor's buffer declares, paired with the
/// span of its name. The buffer is a window into the unit, so the walk
/// descends through the `mod` items the package layout wraps it in.
fn collect_user_types<'a>(
    sf: &'a SourceFile,
    augmented: &str,
    doc_start: u32,
    doc_end: u32,
) -> Vec<UserType<'a>> {
    let mut out = Vec::new();
    collect_user_types_in(&sf.items, augmented, doc_start, doc_end, &mut out);
    out
}

fn collect_user_types_in<'a>(
    items: &'a [gossamer_ast::Item],
    augmented: &str,
    doc_start: u32,
    doc_end: u32,
    out: &mut Vec<UserType<'a>>,
) {
    for item in items {
        if item.span.end < doc_start || item.span.start > doc_end {
            continue;
        }
        let name = match &item.kind {
            ItemKind::Struct(decl) => decl.name.name.as_str(),
            ItemKind::Enum(decl) => decl.name.name.as_str(),
            ItemKind::Mod(decl) => {
                if let gossamer_ast::ModBody::Inline(items) = &decl.body {
                    collect_user_types_in(items, augmented, doc_start, doc_end, out);
                }
                continue;
            }
            _ => continue,
        };
        out.push(UserType {
            name,
            name_span: name_span(augmented, item.span, name),
        });
    }
}

/// Span of `name` inside the declaration covering `item`, falling back
/// to the item's own span. An editor underlines the identifier rather
/// than a multi-line declaration body.
fn name_span(augmented: &str, item: Span, name: &str) -> Span {
    let start = item.start as usize;
    let end = (item.end as usize).min(augmented.len());
    if start >= end {
        return item;
    }
    augmented[start..end]
        .find(name)
        .and_then(|at| {
            let from = u32::try_from(start + at).ok()?;
            let to = from.checked_add(u32::try_from(name.len()).ok()?)?;
            Some(Span::new(item.file, from, to))
        })
        .unwrap_or(item)
}

/// Finds the user type whose synthesized support code encloses `start`.
///
/// Synthesized items always begin at column zero, so the nearest
/// preceding top-level header line names the construct: a mangled
/// `__gos_serde_<op>_<T>` function, or an `impl Trait for T` block. The
/// diagnostic's own title is consulted as well, because a diagnostic
/// about a missing sibling helper names that helper rather than the
/// enclosing item.
fn anchor_for<'a>(
    augmented: &str,
    diag: &Diagnostic,
    start: u32,
    user_types: &'a [UserType<'a>],
) -> Option<&'a UserType<'a>> {
    let header = enclosing_header(augmented, start as usize).unwrap_or("");
    longest_named_type(&diag.title, user_types).or_else(|| longest_named_type(header, user_types))
}

/// The nearest top-level item header at or above `offset`: the last
/// line starting at column zero with an item keyword.
fn enclosing_header(augmented: &str, offset: usize) -> Option<&str> {
    let cap = offset.min(augmented.len());
    augmented[..cap].lines().rfind(|line| {
        let head = line.trim_end();
        head.starts_with("fn ")
            || head.starts_with("impl ")
            || head.starts_with("pub fn ")
            || head.starts_with("struct ")
            || head.starts_with("enum ")
    })
}

/// The longest user type name `text` names. Longest wins so a mention
/// of `Outer` is not credited to a type named `Out`.
fn longest_named_type<'a>(text: &str, user_types: &'a [UserType<'a>]) -> Option<&'a UserType<'a>> {
    let identifiers: Vec<&str> = identifiers_of(text);
    user_types
        .iter()
        .filter(|ty| identifiers.iter().any(|ident| names_type(ident, ty.name)))
        .max_by_key(|ty| ty.name.len())
}

/// True when `ident` is the type itself or a compiler-generated helper
/// for it. Generated helpers append the type to a mangled prefix, so
/// the type name is not a standalone identifier there.
fn names_type(ident: &str, type_name: &str) -> bool {
    if type_name.is_empty() {
        return false;
    }
    ident == type_name || (ident.starts_with("__gos_") && ident.ends_with(&format!("_{type_name}")))
}

/// Splits `text` into its maximal identifier runs.
fn identifiers_of(text: &str) -> Vec<&str> {
    let is_word = |ch: char| ch == '_' || unicode_ident::is_xid_continue(ch);
    text.split(|ch: char| !is_word(ch))
        .filter(|run| !run.is_empty())
        .collect()
}

/// Points `diag` at `anchor`, keeping its message and adding a note
/// naming the type whose generated support code raised it.
fn reanchor(diag: &mut Diagnostic, file: FileId, anchor: Span, type_name: &str) {
    let message = diag
        .labels
        .iter()
        .find(|label| label.primary)
        .or_else(|| diag.labels.first())
        .and_then(|label| label.message.clone());
    diag.labels = vec![Label {
        location: Location::new(file, anchor),
        primary: true,
        message,
    }];
    // A fix-it computed against generated text would edit bytes the
    // editor's buffer does not contain.
    diag.suggestions.clear();
    diag.notes.push(format!(
        "raised by the code the compiler generates for `{type_name}`"
    ));
}

#[cfg(test)]
mod synthesis_tests {
    use super::*;
    use gossamer_diagnostics::Code;

    fn analysed(source: &str) -> crate::session::DocumentAnalysis {
        crate::session::analyse("file:///synth.gos", source)
    }

    #[test]
    fn a_type_is_recognised_in_headers_and_generated_helper_names() {
        assert!(names_type("__gos_serde_to_json_Outer", "Outer"));
        assert!(!names_type("__gos_serde_to_json_Outer", "Out"));
        assert!(names_type("Outer", "Outer"));
        assert!(!names_type("Outermost", "Outer"));
        assert_eq!(
            identifiers_of("fn __gos_serde_to_json_Outer(value: Outer)"),
            vec!["fn", "__gos_serde_to_json_Outer", "value", "Outer"]
        );
    }

    #[test]
    fn nested_struct_with_unsupported_field_reports_against_the_declaration() {
        let source = "use std::encoding::json\n\
                      struct Inner { m: Map<i64, i64> }\n\
                      struct Outer { i: Inner }\n\
                      fn main() {\n\
                      \x20   let o = Outer { i: Inner { m: {} } }\n\
                      \x20   println(\"{}\", json::to_json::<Outer>(o))\n\
                      }\n";
        let doc = analysed(source);
        assert!(
            !doc.diagnostics.is_empty(),
            "a file `gos check` rejects must not analyse clean"
        );
        let user_len = doc.user_len;
        for diag in &doc.diagnostics {
            let start = primary_start(diag).unwrap_or(0);
            assert!(
                start <= user_len,
                "diagnostic {} points outside the editor buffer: {diag:?}",
                diag.code
            );
        }
    }

    #[test]
    fn a_tail_diagnostic_with_no_user_type_is_dropped() {
        let source = "fn main() { }\n";
        let mut map = gossamer_lex::SourceMap::new();
        let file = map.add_file("t.gos", source);
        let sf = gossamer_parse::parse_source_file(source, file).0;
        let mut diagnostics = vec![
            Diagnostic::error(Code("GR0001"), "cannot find `__gos_hidden`").with_primary(
                Location::new(file, Span::new(file, 10_000, 10_010)),
                "not found",
            ),
        ];
        let len = source.len() as u32;
        reanchor_out_of_buffer(&mut diagnostics, &sf, source, file, 0, len, len);
        assert!(diagnostics.is_empty());
    }
}
