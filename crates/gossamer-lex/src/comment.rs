//! Comment tokenization helpers.

use crate::cursor::{Cursor, EOF_CHAR};
use crate::token::TokenKind;

/// Outcome of attempting to lex a comment starting at `/`.
pub(crate) enum CommentOutcome {
    /// The `/` was not followed by another `/` or `*`; not a comment.
    NotAComment,
    /// A comment was lexed successfully.
    Lexed(TokenKind),
    /// A `/* ... */` block comment ran off the end of the file.
    Unterminated,
}

/// Attempts to lex a `//` line comment or `/* ... */` block comment
/// starting at the current cursor position (which points at the
/// opening `/`). Advances the cursor past the whole comment on success.
///
/// Gossamer has one line-comment form and one block-comment form. There
/// is no separate `///` or `//!` doc syntax; tooling derives doc text
/// from comments positioned immediately above an item.
pub(crate) fn lex_comment(cursor: &mut Cursor<'_>) -> CommentOutcome {
    debug_assert_eq!(cursor.peek(), '/');
    match cursor.peek_nth(1) {
        '/' => {
            cursor.bump();
            cursor.bump();
            cursor.bump_while(|character| character != '\n');
            CommentOutcome::Lexed(TokenKind::LineComment)
        }
        '*' => lex_block_comment(cursor),
        _ => CommentOutcome::NotAComment,
    }
}

/// Lexes a `/* ... */` block comment, consuming up to and including the
/// closing `*/`. Block comments do **not** nest (SPEC §"Comments"):
/// the first `*/` terminates the comment regardless of any `/*`
/// sequences inside it. Returns `Unterminated` if the input ends
/// before the closing `*/`.
fn lex_block_comment(cursor: &mut Cursor<'_>) -> CommentOutcome {
    cursor.bump();
    cursor.bump();
    while !cursor.is_eof() {
        let current = cursor.bump().unwrap_or(EOF_CHAR);
        if current == '*' && cursor.peek() == '/' {
            cursor.bump();
            return CommentOutcome::Lexed(TokenKind::BlockComment);
        }
    }
    CommentOutcome::Unterminated
}
