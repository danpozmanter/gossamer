//! String, character, raw string, and byte literal tokenization helpers.

use crate::cursor::Cursor;
use crate::diagnostic::LexError;
use crate::span::{FileId, Span};
use crate::token::TokenKind;

/// Outcome of lexing one of the quoted-literal forms.
pub(crate) struct QuotedOutcome {
    /// Token kind to emit for the literal.
    pub(crate) kind: TokenKind,
    /// Any diagnostics raised while scanning the literal.
    pub(crate) diagnostics: Vec<LexError>,
}

impl QuotedOutcome {
    /// Returns an outcome with no diagnostics attached.
    const fn ok(kind: TokenKind) -> Self {
        Self {
            kind,
            diagnostics: Vec::new(),
        }
    }
}

/// Lexes a double-quoted string literal beginning at the current cursor
/// position (which points at the opening `"`).
pub(crate) fn lex_string(
    cursor: &mut Cursor<'_>,
    file: FileId,
    literal_start: u32,
) -> QuotedOutcome {
    debug_assert_eq!(cursor.peek(), '"');
    cursor.bump();
    let mut diagnostics = Vec::new();
    loop {
        match cursor.peek() {
            '\0' if cursor.is_eof() => {
                diagnostics.push(LexError::UnterminatedString {
                    span: span_to_here(file, literal_start, cursor),
                });
                return QuotedOutcome {
                    kind: TokenKind::StringLit,
                    diagnostics,
                };
            }
            '"' => {
                cursor.bump();
                return QuotedOutcome {
                    kind: TokenKind::StringLit,
                    diagnostics,
                };
            }
            '\\' => consume_escape(cursor, file, &mut diagnostics),
            _ => {
                cursor.bump();
            }
        }
    }
}

/// Lexes a raw string literal beginning at `r` or `br` (after any `b`
/// prefix has already been consumed by `lex_ident_or_prefix`).
///
/// Expects the cursor to be positioned at the `r` of `r"..."` /
/// `r#"..."#`.
pub(crate) fn lex_raw_string(
    cursor: &mut Cursor<'_>,
    file: FileId,
    literal_start: u32,
    byte_flavor: bool,
) -> QuotedOutcome {
    debug_assert_eq!(cursor.peek(), 'r');
    cursor.bump();
    let hashes = consume_opening_hashes(cursor);
    if !cursor.bump_if(|character| character == '"') {
        let span = span_to_here(file, literal_start, cursor);
        return QuotedOutcome {
            kind: if byte_flavor {
                TokenKind::RawByteStringLit { hashes }
            } else {
                TokenKind::RawStringLit { hashes }
            },
            diagnostics: vec![LexError::UnterminatedRawString { span }],
        };
    }
    let terminated = consume_raw_body(cursor, hashes);
    let kind = if byte_flavor {
        TokenKind::RawByteStringLit { hashes }
    } else {
        TokenKind::RawStringLit { hashes }
    };
    if terminated {
        QuotedOutcome::ok(kind)
    } else {
        QuotedOutcome {
            kind,
            diagnostics: vec![LexError::UnterminatedRawString {
                span: span_to_here(file, literal_start, cursor),
            }],
        }
    }
}

/// Lexes a character literal (`'x'`) starting at the current cursor.
pub(crate) fn lex_char(cursor: &mut Cursor<'_>, file: FileId, literal_start: u32) -> QuotedOutcome {
    debug_assert_eq!(cursor.peek(), '\'');
    cursor.bump();
    let mut diagnostics = Vec::new();
    let mut char_count = 0usize;
    loop {
        match cursor.peek() {
            '\0' if cursor.is_eof() => {
                diagnostics.push(LexError::UnterminatedChar {
                    span: span_to_here(file, literal_start, cursor),
                });
                return QuotedOutcome {
                    kind: TokenKind::CharLit,
                    diagnostics,
                };
            }
            '\n' => {
                diagnostics.push(LexError::UnterminatedChar {
                    span: span_to_here(file, literal_start, cursor),
                });
                return QuotedOutcome {
                    kind: TokenKind::CharLit,
                    diagnostics,
                };
            }
            '\'' => {
                cursor.bump();
                if char_count != 1 {
                    diagnostics.push(LexError::BadCharLiteralLength {
                        span: span_to_here(file, literal_start, cursor),
                    });
                }
                return QuotedOutcome {
                    kind: TokenKind::CharLit,
                    diagnostics,
                };
            }
            '\\' => {
                consume_escape(cursor, file, &mut diagnostics);
                char_count += 1;
            }
            _ => {
                cursor.bump();
                char_count += 1;
            }
        }
    }
}

/// Lexes a `b'...'` byte literal.
pub(crate) fn lex_byte(cursor: &mut Cursor<'_>, file: FileId, literal_start: u32) -> QuotedOutcome {
    debug_assert_eq!(cursor.peek(), '\'');
    let inner = lex_char(cursor, file, literal_start);
    QuotedOutcome {
        kind: TokenKind::ByteLit,
        diagnostics: inner.diagnostics,
    }
}

/// Lexes a `b"..."` byte string literal.
pub(crate) fn lex_byte_string(
    cursor: &mut Cursor<'_>,
    file: FileId,
    literal_start: u32,
) -> QuotedOutcome {
    debug_assert_eq!(cursor.peek(), '"');
    let inner = lex_string(cursor, file, literal_start);
    QuotedOutcome {
        kind: TokenKind::ByteStringLit,
        diagnostics: inner.diagnostics,
    }
}

/// Consumes opening `#` characters preceding the `"` of a raw string
/// and returns their count, saturating at `u8::MAX`.
fn consume_opening_hashes(cursor: &mut Cursor<'_>) -> u8 {
    let mut hashes: u16 = 0;
    while cursor.peek() == '#' && hashes < u16::from(u8::MAX) {
        cursor.bump();
        hashes += 1;
    }
    u8::try_from(hashes).unwrap_or(u8::MAX)
}

/// Consumes the body of a raw string up to (and including) a closing
/// `"` followed by the expected number of `#` characters.
fn consume_raw_body(cursor: &mut Cursor<'_>, hashes: u8) -> bool {
    while !cursor.is_eof() {
        let character = cursor.peek();
        if character == '"' && lookahead_matches_closing(cursor, hashes) {
            cursor.bump();
            for _ in 0..hashes {
                cursor.bump();
            }
            return true;
        }
        cursor.bump();
    }
    false
}

/// Returns `true` when the cursor sits at `"` followed by `hashes` more
/// `#` characters — the valid raw-string closing sequence.
fn lookahead_matches_closing(cursor: &Cursor<'_>, hashes: u8) -> bool {
    debug_assert_eq!(cursor.peek(), '"');
    let rest = &cursor.rest()[1..];
    rest.bytes().take(hashes as usize).all(|byte| byte == b'#') && rest.len() >= hashes as usize
}

/// Consumes an escape sequence beginning at the current `\`.
///
/// Reports diagnostics for malformed escapes but always advances past
/// the escape so lexing can continue.
fn consume_escape(cursor: &mut Cursor<'_>, file: FileId, diagnostics: &mut Vec<LexError>) {
    let escape_start = cursor.offset();
    debug_assert_eq!(cursor.peek(), '\\');
    cursor.bump();
    match cursor.peek() {
        'n' | 't' | 'r' | '\\' | '\'' | '"' | '0' => {
            cursor.bump();
        }
        'x' => {
            cursor.bump();
            let ok = consume_hex_digits(cursor, 2);
            if !ok {
                diagnostics.push(LexError::BadEscape {
                    span: span_from_offset(file, escape_start, cursor),
                });
            }
        }
        'u' => {
            cursor.bump();
            if !consume_unicode_escape(cursor) {
                diagnostics.push(LexError::BadUnicodeEscape {
                    span: span_from_offset(file, escape_start, cursor),
                });
            }
        }
        _ => {
            cursor.bump();
            diagnostics.push(LexError::BadEscape {
                span: span_from_offset(file, escape_start, cursor),
            });
        }
    }
}

/// Consumes exactly `count` hex digits. Returns `false` if fewer than
/// `count` are available.
fn consume_hex_digits(cursor: &mut Cursor<'_>, count: usize) -> bool {
    for _ in 0..count {
        if !cursor.peek().is_ascii_hexdigit() {
            return false;
        }
        cursor.bump();
    }
    true
}

/// Consumes a `{XXXX}` unicode escape body. Returns `false` on malformed input.
fn consume_unicode_escape(cursor: &mut Cursor<'_>) -> bool {
    if !cursor.bump_if(|character| character == '{') {
        return false;
    }
    let mut digit_count = 0usize;
    while cursor.peek().is_ascii_hexdigit() && digit_count < 6 {
        cursor.bump();
        digit_count += 1;
    }
    if digit_count == 0 {
        return false;
    }
    cursor.bump_if(|character| character == '}')
}

/// Builds a span from `literal_start` to the cursor's current offset.
fn span_to_here(file: FileId, literal_start: u32, cursor: &Cursor<'_>) -> Span {
    let end = u32::try_from(cursor.offset()).unwrap_or(u32::MAX);
    Span::new(file, literal_start, end)
}

/// Builds a span from `start` to the cursor's current offset.
fn span_from_offset(file: FileId, start: usize, cursor: &Cursor<'_>) -> Span {
    let start_u32 = u32::try_from(start).unwrap_or(u32::MAX);
    let end_u32 = u32::try_from(cursor.offset()).unwrap_or(u32::MAX);
    Span::new(file, start_u32, end_u32)
}
