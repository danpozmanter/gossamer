//! Numeric literal tokenization helpers.

use crate::cursor::Cursor;
use crate::token::TokenKind;

/// Numeric base selected by the opening prefix of an integer literal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NumBase {
    Binary,
    Octal,
    Decimal,
    Hexadecimal,
}

impl NumBase {
    /// Returns `true` when `character` is a valid digit in this base,
    /// counting `_` as a visual separator in every base.
    const fn accepts(self, character: char) -> bool {
        match self {
            Self::Binary => matches!(character, '0' | '1' | '_'),
            Self::Octal => matches!(character, '0'..='7' | '_'),
            Self::Decimal => matches!(character, '0'..='9' | '_'),
            Self::Hexadecimal => matches!(character, '0'..='9' | 'a'..='f' | 'A'..='F' | '_'),
        }
    }
}

/// Lexes a numeric literal starting at the current cursor position.
///
/// Returns the token kind (`IntLit` or `FloatLit`) and advances the
/// cursor past the full literal including any type suffix.
pub(crate) fn lex_number(cursor: &mut Cursor<'_>) -> TokenKind {
    let base = scan_base_prefix(cursor);
    scan_integer_body(cursor, base);
    if base == NumBase::Decimal && looks_like_float_continuation(cursor) {
        scan_float_tail(cursor);
        scan_numeric_suffix(cursor);
        TokenKind::FloatLit
    } else {
        scan_numeric_suffix(cursor);
        TokenKind::IntLit
    }
}

/// Consumes a leading base prefix (`0b`, `0o`, `0x`) and returns the
/// corresponding base. Leaves the cursor in place if no prefix is present.
fn scan_base_prefix(cursor: &mut Cursor<'_>) -> NumBase {
    if cursor.peek() != '0' {
        return NumBase::Decimal;
    }
    match cursor.peek_nth(1) {
        'b' | 'B' => {
            cursor.bump();
            cursor.bump();
            NumBase::Binary
        }
        'o' | 'O' => {
            cursor.bump();
            cursor.bump();
            NumBase::Octal
        }
        'x' | 'X' => {
            cursor.bump();
            cursor.bump();
            NumBase::Hexadecimal
        }
        _ => NumBase::Decimal,
    }
}

/// Consumes every digit in `base` plus `_` separators.
fn scan_integer_body(cursor: &mut Cursor<'_>, base: NumBase) {
    cursor.bump_while(|character| base.accepts(character));
}

/// Returns `true` when the cursor sits at `.` that should start a float
/// fractional part (i.e. `.` followed by a decimal digit).
fn looks_like_float_continuation(cursor: &Cursor<'_>) -> bool {
    if cursor.peek() == '.' && cursor.peek_nth(1).is_ascii_digit() {
        return true;
    }
    matches!(cursor.peek(), 'e' | 'E')
}

/// Consumes the fractional part and optional exponent of a float literal.
fn scan_float_tail(cursor: &mut Cursor<'_>) {
    if cursor.peek() == '.' {
        cursor.bump();
        cursor.bump_while(|character| NumBase::Decimal.accepts(character));
    }
    if matches!(cursor.peek(), 'e' | 'E') {
        cursor.bump();
        if matches!(cursor.peek(), '+' | '-') {
            cursor.bump();
        }
        cursor.bump_while(|character| NumBase::Decimal.accepts(character));
    }
}

/// Consumes a trailing numeric type suffix such as `i32`, `u64`, `f64`.
///
/// The suffix is a run of `[A-Za-z0-9_]` starting with an ASCII letter
/// that follows the numeric body with no intervening whitespace. Semantic
/// validation of the suffix (e.g. rejecting `i37`) happens in a later
/// pass so the lexer need only identify the lexical extent.
fn scan_numeric_suffix(cursor: &mut Cursor<'_>) {
    let first = cursor.peek();
    if !first.is_ascii_alphabetic() && first != '_' {
        return;
    }
    cursor.bump_while(|character| character.is_ascii_alphanumeric() || character == '_');
}
