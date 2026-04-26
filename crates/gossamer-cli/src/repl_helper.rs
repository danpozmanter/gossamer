//! Rustyline `Helper` that paints Gossamer source with ANSI colour
//! escapes as the user types in the REPL.
//! Lexing is delegated to `gossamer_lex::tokenize`; the helper does
//! not build an AST, so partially-typed input (unterminated strings,
//! dangling punctuation) still paints correctly up to the last
//! lexable boundary.

#![forbid(unsafe_code)]

use std::borrow::Cow;

use gossamer_lex::{Punct, SourceMap, TokenKind, tokenize};
use rustyline::Helper;
use rustyline::completion::Completer;
use rustyline::hint::Hinter;
use rustyline::highlight::Highlighter;
use rustyline::validate::{ValidationContext, ValidationResult, Validator};

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

impl Completer for GosReplHelper {
    type Candidate = String;
}

impl Hinter for GosReplHelper {
    type Hint = String;
}

impl Validator for GosReplHelper {
    fn validate(
        &self,
        ctx: &mut ValidationContext<'_>,
    ) -> rustyline::Result<ValidationResult> {
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

/// Returns `Some(_)` when `input` is syntactically incomplete — an
/// unclosed brace/paren/bracket, or a trailing unterminated block
/// comment. In that case the REPL keeps reading subsequent lines as
/// a continuation of the same expression.
fn incomplete_reason(input: &str) -> Option<&'static str> {
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
                    if text
                        .chars()
                        .next()
                        .is_some_and(|c| c.is_ascii_uppercase())
                    {
                        out.push_str(MAGENTA);
                        out.push_str(text);
                        out.push_str(RESET);
                    } else {
                        out.push_str(text);
                    }
                }
                TokenKind::Punct(_)
                | TokenKind::Whitespace
                | TokenKind::Invalid => out.push_str(text),
            }
            cursor = end.max(cursor);
        }
        if cursor < line.len() {
            out.push_str(&line[cursor..]);
        }
        Cow::Owned(out)
    }

    fn highlight_char(&self, _line: &str, _pos: usize, _forced: bool) -> bool {
        true
    }
}
