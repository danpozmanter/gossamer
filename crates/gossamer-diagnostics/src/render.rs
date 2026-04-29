//! Rendering for [`Diagnostic`] into a rustc/elm-style text frame.
//! Output goes through [`render`] by default. Tests and machine
//! consumers can use [`render_plain`] for a colour-free form that is
//! stable across runs.

use std::fmt::Write;

use gossamer_lex::{FileId, SourceMap};

use crate::{Diagnostic, Label};

/// Style options for [`render`]. Kept small on purpose — callers that
/// want colour should opt in explicitly.
#[derive(Debug, Clone, Copy, Default)]
pub struct RenderOptions {
    /// Emit ANSI colour escapes.
    pub colour: bool,
}

/// Renders `diag` as an `error[GP0001]: …` frame using the supplied
/// source map for resolving file names and line/column info.
#[must_use]
pub fn render(diag: &Diagnostic, map: &SourceMap, options: RenderOptions) -> String {
    let mut out = String::new();
    let severity_colour = if options.colour {
        colour_for(diag.severity)
    } else {
        ""
    };
    let bold_colour = if options.colour { BOLD } else { "" };
    let dim_colour = if options.colour { DIM } else { "" };
    let reset = if options.colour { RESET } else { "" };
    let _ = writeln!(
        out,
        "{severity_colour}{}[{}]{reset}{bold_colour}: {}{reset}",
        diag.severity, diag.code, diag.title,
    );
    for label in &diag.labels {
        render_label(&mut out, label, map, &diag.title, options.colour);
    }
    for note in &diag.notes {
        let _ = writeln!(out, "  {dim_colour}= note:{reset} {note}");
    }
    for help in &diag.helps {
        let _ = writeln!(out, "  {dim_colour}= help:{reset} {help}");
    }
    for suggestion in &diag.suggestions {
        let _ = writeln!(
            out,
            "  {dim_colour}= suggestion:{reset} {} → `{}`",
            suggestion.message, suggestion.replacement
        );
    }
    out
}

/// Colour-free one-line form for tests and JSON consumers.
#[must_use]
pub fn render_plain(diag: &Diagnostic) -> String {
    let mut out = format!("{}[{}]: {}", diag.severity, diag.code, diag.title);
    if let Some(primary) = diag.primary_label()
        && let Some(msg) = &primary.message
    {
        out.push_str(" — ");
        out.push_str(msg);
    }
    out
}

fn render_label(
    out: &mut String,
    label: &Label,
    map: &SourceMap,
    header_title: &str,
    colour: bool,
) {
    let (path, line, column) = resolve(map, label.location.file, label.location.span);
    let prefix = if label.primary { "-->" } else { "::>" };
    let cyan = if colour { CYAN } else { "" };
    let red = if colour { RED } else { "" };
    let dim = if colour { DIM } else { "" };
    let reset = if colour { RESET } else { "" };
    let _ = writeln!(out, "  {cyan}{prefix}{reset} {path}:{line}:{column}");
    if let Some(source_line) = source_line_of(map, label.location.file, line) {
        let gutter = format!("{line:>4}");
        let _ = writeln!(out, "  {dim}{gutter} |{reset} {source_line}");
        let padding = " ".repeat(column.saturating_sub(1) as usize);
        let span_len = label
            .location
            .span
            .end
            .saturating_sub(label.location.span.start)
            .max(1);
        let caret = "^".repeat(span_len as usize);
        let caret_colour = if label.primary { red } else { cyan };
        let _ = writeln!(
            out,
            "       {dim}|{reset} {padding}{caret_colour}{caret}{reset}",
        );
    }
    if let Some(msg) = &label.message {
        let is_redundant = label.primary && msg == header_title;
        if !is_redundant {
            let tag = if label.primary { "error" } else { "note" };
            let tag_colour = if label.primary { red } else { cyan };
            let _ = writeln!(out, "     {tag_colour}{tag}{reset}: {msg}");
        }
    }
}

fn source_line_of(map: &SourceMap, file: FileId, line: u32) -> Option<String> {
    if line == 0 {
        return None;
    }
    let source = map.source(file);
    source
        .lines()
        .nth(line.saturating_sub(1) as usize)
        .map(std::string::ToString::to_string)
}

fn resolve(map: &SourceMap, file: FileId, span: gossamer_lex::Span) -> (String, u32, u32) {
    let name = map.file_name(file).to_string();
    let line_col = map.line_col(file, span.start);
    (name, line_col.line, line_col.column)
}

const RESET: &str = "\x1b[0m";
const RED: &str = "\x1b[31m";
const CYAN: &str = "\x1b[36m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";

const fn colour_for(severity: crate::Severity) -> &'static str {
    match severity {
        crate::Severity::Error => "\x1b[31;1m",
        crate::Severity::Warning => "\x1b[33;1m",
        crate::Severity::Note => "\x1b[36m",
        crate::Severity::Help => "\x1b[32m",
    }
}
