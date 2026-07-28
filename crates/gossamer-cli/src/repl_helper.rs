//! Rustyline `Helper` that paints Gossamer source with ANSI colour
//! escapes as the user types in the REPL.
//! Lexing is delegated to `gossamer_lex::tokenize`; the helper does
//! not build an AST, so partially-typed input (unterminated strings,
//! dangling punctuation) still paints correctly up to the last
//! lexable boundary.

#![forbid(unsafe_code)]

use std::borrow::Cow;

use gossamer_lex::{Punct, SourceMap, TokenKind, tokenize};
use rustyline::completion::Completer;
use rustyline::highlight::{CmdKind, Highlighter};
use rustyline::hint::Hinter;
use rustyline::validate::{ValidationContext, ValidationResult, Validator};
use rustyline::{Cmd, ConditionalEventHandler, Event, EventContext, Helper, RepeatCount};

/// ANSI colour escapes used by the REPL. Chosen to read well on both
/// light and dark terminals; dim for comments keeps them present but
/// low-contrast.
const RESET: &str = "\x1b[0m";
const CYAN_BOLD: &str = "\x1b[1;36m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const MAGENTA: &str = "\x1b[35m";
const DIM: &str = "\x1b[2m";

/// REPL helper that highlights Gossamer source as the user types.
#[derive(Default)]
pub(crate) struct GosReplHelper;

impl GosReplHelper {
    /// Constructs a fresh helper with no per-session state.
    #[must_use]
    pub(crate) fn new() -> Self {
        Self
    }
}

impl Helper for GosReplHelper {}

/// Adds language-aware indentation when Enter extends incomplete input.
pub(crate) struct ReplEnterHandler;

impl ConditionalEventHandler for ReplEnterHandler {
    fn handle(
        &self,
        _evt: &Event,
        _n: RepeatCount,
        _positive: bool,
        ctx: &EventContext<'_>,
    ) -> Option<Cmd> {
        let input = ctx.line();
        if ctx.pos() != input.len() || incomplete_reason(input).is_none() {
            return None;
        }
        Some(Cmd::Insert(1, format!("\n{}", continuation_indent(input))))
    }
}

fn continuation_indent(input: &str) -> String {
    const INDENT: &str = "    ";

    let current_line = input.rsplit('\n').next().unwrap_or(input);
    let leading = &current_line[..current_line
        .find(|ch: char| !ch.is_whitespace())
        .unwrap_or(current_line.len())];

    let mut map = SourceMap::new();
    let file = map.add_file("repl-indent.gos", input.to_string());
    let (tokens, _) = tokenize(input, file);
    let opens_block = tokens
        .iter()
        .rev()
        .find(|token| {
            !matches!(
                token.kind,
                TokenKind::Eof
                    | TokenKind::Whitespace
                    | TokenKind::LineComment
                    | TokenKind::BlockComment
            )
        })
        .is_some_and(|token| {
            matches!(
                token.kind,
                TokenKind::Punct(Punct::LBrace | Punct::LParen | Punct::LBracket)
            )
        });

    if opens_block {
        format!("{leading}{INDENT}")
    } else {
        leading.to_string()
    }
}

/// Every Gossamer keyword, completed when the cursor word is unqualified.
/// Mirrors `gossamer_lex::Keyword`; the enum exposes no all-variants
/// iterator, so the set is listed here and kept in step with the lexer.
const KEYWORDS: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "defer", "else", "enum",
    "extern", "false", "fn", "for", "go", "if", "impl", "in", "let", "loop", "match", "mod", "mut",
    "package", "pub", "return", "select", "self", "static", "struct", "super", "trait", "true",
    "type", "unsafe", "use", "where", "while", "yield",
];

impl Completer for GosReplHelper {
    type Candidate = String;

    /// Completes the identifier-or-path word ending at the cursor against
    /// the keyword set and the standard-library surface (`std::registry`):
    /// module paths and their `module::item` members. Returns the byte
    /// offset where the replacement begins plus the prefix-matching
    /// candidates, sorted and de-duplicated.
    fn complete(
        &self,
        line: &str,
        pos: usize,
        _ctx: &rustyline::Context<'_>,
    ) -> rustyline::Result<(usize, Vec<String>)> {
        Ok(complete_at(line, pos))
    }
}

/// Computes completion candidates for the word ending at `pos`. Split out
/// from the trait method so it is testable without a rustyline `Context`.
fn complete_at(line: &str, pos: usize) -> (usize, Vec<String>) {
    let start = word_start(line, pos);
    let word = &line[start..pos];
    if word.is_empty() {
        return (start, Vec::new());
    }
    let mut out: Vec<String> = Vec::new();
    if !word.contains(':') {
        out.extend(
            KEYWORDS
                .iter()
                .filter(|kw| kw.starts_with(word))
                .map(|kw| (*kw).to_string()),
        );
    }
    for module in gossamer_std::registry::modules() {
        for prefix in module_prefixes(module.path) {
            if prefix.starts_with(word) {
                out.push(prefix.to_string());
            }
            for item in module.items {
                let qualified = format!("{prefix}::{}", item.name);
                if qualified.starts_with(word) {
                    out.push(qualified);
                }
            }
        }
    }
    out.sort();
    out.dedup();
    (start, out)
}

/// The module path forms a user might type to reach a module: the canonical
/// path (`std::strings`), the `std::`-stripped form used after `use std::…`
/// (`strings`, `encoding::json`), and the bare last segment (`json`).
fn module_prefixes(path: &'static str) -> Vec<&'static str> {
    let mut forms = vec![path];
    if let Some(stripped) = path.strip_prefix("std::") {
        if !forms.contains(&stripped) {
            forms.push(stripped);
        }
    }
    if let Some(last) = path.rsplit("::").next() {
        if !forms.contains(&last) {
            forms.push(last);
        }
    }
    forms
}

/// Byte offset of the start of the identifier-or-path word ending at `pos`.
/// Walks back over identifier bytes and `:` path separators so a
/// partially-typed `strings::sp` completes as a single unit. Only ASCII
/// identifier bytes are consumed, so `start` lands on a char boundary.
fn word_start(line: &str, pos: usize) -> usize {
    let bytes = line.as_bytes();
    let mut start = pos;
    while start > 0 {
        let c = bytes[start - 1];
        if c.is_ascii_alphanumeric() || c == b'_' || c == b':' {
            start -= 1;
        } else {
            break;
        }
    }
    start
}

impl Hinter for GosReplHelper {
    type Hint = String;
}

impl Validator for GosReplHelper {
    fn validate(&self, ctx: &mut ValidationContext<'_>) -> rustyline::Result<ValidationResult> {
        let input = ctx.input();
        if input.is_empty() {
            return Ok(ValidationResult::Valid(None));
        }
        if incomplete_reason(input).is_some() {
            return Ok(ValidationResult::Incomplete);
        }
        Ok(ValidationResult::Valid(None))
    }
}

/// Returns `Some(_)` when `input` is syntactically incomplete - an
/// unclosed brace/paren/bracket, or a trailing unterminated block
/// comment. In that case the REPL keeps reading subsequent lines as
/// a continuation of the same expression.
fn incomplete_reason(input: &str) -> Option<&'static str> {
    // Meta-command arguments may themselves be incomplete Gossamer syntax,
    // especially regexes such as `[` or `foo(`. Submit them immediately so
    // the command can either use the pattern or report its regex error.
    if input.trim_start().starts_with('%') {
        return None;
    }
    let mut map = SourceMap::new();
    let file = map.add_file("repl.gos", input.to_string());
    let (tokens, lex_errors) = tokenize(input, file);
    for err in &lex_errors {
        let message = format!("{err:?}");
        if message.contains("Unterminated") {
            return Some("unterminated literal or comment");
        }
    }
    let mut depth_brace: i32 = 0;
    let mut depth_paren: i32 = 0;
    let mut depth_bracket: i32 = 0;
    for token in tokens {
        match token.kind {
            TokenKind::Punct(Punct::LBrace) => depth_brace += 1,
            TokenKind::Punct(Punct::RBrace) => depth_brace -= 1,
            TokenKind::Punct(Punct::LParen) => depth_paren += 1,
            TokenKind::Punct(Punct::RParen) => depth_paren -= 1,
            TokenKind::Punct(Punct::LBracket) => depth_bracket += 1,
            TokenKind::Punct(Punct::RBracket) => depth_bracket -= 1,
            _ => {}
        }
    }
    if depth_brace > 0 {
        return Some("unbalanced `{`");
    }
    if depth_paren > 0 {
        return Some("unbalanced `(`");
    }
    if depth_bracket > 0 {
        return Some("unbalanced `[`");
    }
    None
}

impl Highlighter for GosReplHelper {
    fn highlight<'l>(&self, line: &'l str, _pos: usize) -> Cow<'l, str> {
        if line.is_empty() {
            return Cow::Borrowed(line);
        }
        let mut map = SourceMap::new();
        let file = map.add_file("repl.gos", line.to_string());
        let (tokens, _) = tokenize(line, file);
        let mut out = String::with_capacity(line.len() + tokens.len() * 6);
        let mut cursor = 0usize;
        for token in tokens {
            let start = token.span.start as usize;
            let end = token.span.end as usize;
            if start > cursor {
                out.push_str(&line[cursor..start]);
            }
            if end <= start || end > line.len() {
                cursor = start.max(cursor);
                continue;
            }
            let text = &line[start..end];
            match token.kind {
                TokenKind::Eof => {}
                TokenKind::Keyword(_) => {
                    out.push_str(CYAN_BOLD);
                    out.push_str(text);
                    out.push_str(RESET);
                }
                TokenKind::StringLit
                | TokenKind::RawStringLit { .. }
                | TokenKind::ByteStringLit
                | TokenKind::RawByteStringLit { .. }
                | TokenKind::CharLit
                | TokenKind::ByteLit => {
                    out.push_str(GREEN);
                    out.push_str(text);
                    out.push_str(RESET);
                }
                TokenKind::IntLit | TokenKind::FloatLit => {
                    out.push_str(YELLOW);
                    out.push_str(text);
                    out.push_str(RESET);
                }
                TokenKind::LineComment | TokenKind::BlockComment => {
                    out.push_str(DIM);
                    out.push_str(text);
                    out.push_str(RESET);
                }
                TokenKind::Ident => {
                    if text.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
                        out.push_str(MAGENTA);
                        out.push_str(text);
                        out.push_str(RESET);
                    } else {
                        out.push_str(text);
                    }
                }
                TokenKind::Label => {
                    out.push_str(CYAN_BOLD);
                    out.push_str(text);
                    out.push_str(RESET);
                }
                TokenKind::Punct(_) | TokenKind::Whitespace | TokenKind::Invalid => {
                    out.push_str(text);
                }
            }
            cursor = end.max(cursor);
        }
        if cursor < line.len() {
            out.push_str(&line[cursor..]);
        }
        Cow::Owned(out)
    }

    fn highlight_char(&self, _line: &str, _pos: usize, _kind: CmdKind) -> bool {
        true
    }
}

#[cfg(test)]
mod repl_helper_tests {
    use super::{complete_at, continuation_indent, incomplete_reason};

    #[test]
    fn keyword_prefix_completes() {
        let (start, cands) = complete_at("le", 2);
        assert_eq!(start, 0);
        assert!(cands.iter().any(|c| c == "let"));
    }

    #[test]
    fn empty_word_yields_nothing() {
        let (_, cands) = complete_at("let x = ", 8);
        assert!(cands.is_empty());
    }

    #[test]
    fn qualified_path_completes_member() {
        let (start, cands) = complete_at("println!(strings::jo", 20);
        assert_eq!(start, 9);
        assert!(
            cands.iter().any(|c| c == "strings::join"),
            "expected strings::join in {cands:?}"
        );
        // A qualified word must not pull in keyword candidates.
        assert!(cands.iter().all(|c| c.contains("::")));
    }

    #[test]
    fn completion_offset_is_word_start_not_line_start() {
        let (start, _) = complete_at("    fo", 6);
        assert_eq!(start, 4);
    }

    #[test]
    fn meta_command_regex_is_never_treated_as_incomplete_source() {
        assert_eq!(incomplete_reason("%find ["), None);
        assert_eq!(incomplete_reason("%bindings foo("), None);
    }

    #[test]
    fn continuation_indent_steps_in_after_an_opening_delimiter() {
        assert_eq!(continuation_indent("fn main() {"), "    ");
        assert_eq!(continuation_indent("    let values = ["), "        ");
    }

    #[test]
    fn continuation_indent_preserves_the_current_level() {
        assert_eq!(continuation_indent("fn main() {\n    let x = 1"), "    ");
        assert_eq!(
            continuation_indent("fn main() { // setup"),
            "    ",
            "a trailing comment should not hide the opening brace"
        );
    }
}
