//! Error-recovery helpers used to resynchronise after parse errors.

#![forbid(unsafe_code)]

use gossamer_lex::{Keyword, Punct, TokenKind};

use crate::parser::Parser;

impl Parser<'_> {
    /// Advances tokens until reaching an item-starter keyword or EOF.
    pub(crate) fn recover_to_item_start(&mut self) {
        while !self.at_eof() {
            if is_item_start(self) {
                return;
            }
            self.bump();
        }
    }

    /// Advances to the `close` delimiter matching an `open` the parser has
    /// already consumed, consuming it, and returns whether it was found.
    /// Resynchronising on the delimiter keeps one malformed element inside
    /// a bracketed list from being re-read as a sequence of statements.
    /// A `{` at the top nesting level ends the search: it belongs to the
    /// construct that follows the list, not to the list itself.
    pub(crate) fn recover_to_close(&mut self, open: Punct, close: Punct) -> bool {
        let mut depth = 1u32;
        while !self.at_eof() {
            if self.at_punct(open) {
                depth += 1;
            } else if self.at_punct(close) {
                depth -= 1;
                if depth == 0 {
                    self.bump();
                    return true;
                }
            } else if depth == 1 && close != Punct::RBrace && self.at_punct(Punct::LBrace) {
                return false;
            }
            self.bump();
        }
        false
    }

    /// Advances to the `>` closing an already-opened generic list,
    /// consuming it, and returns whether it was found. The body of a
    /// following block or statement ends the search so an unterminated
    /// list cannot swallow the rest of the file.
    pub(crate) fn recover_to_close_angle(&mut self) -> bool {
        let mut depth = 1u32;
        while !self.at_eof() {
            if self.at_punct(Punct::LBrace) || self.at_punct(Punct::Semi) {
                return false;
            }
            if self.at_close_angle() {
                depth -= 1;
                self.tokens.eat_close_angle();
                if depth == 0 {
                    return true;
                }
                continue;
            }
            if self.at_punct(Punct::Lt) {
                depth += 1;
            }
            self.bump();
        }
        false
    }

    /// Advances tokens until reaching a statement-starter, `;`, or `}`.
    pub(crate) fn recover_in_block(&mut self) {
        while !self.at_eof() {
            if self.at_punct(Punct::Semi) {
                self.bump();
                return;
            }
            if self.at_punct(Punct::RBrace) {
                return;
            }
            if is_stmt_start(self) {
                return;
            }
            self.bump();
        }
    }
}

/// Returns `true` when the current token begins a top-level item.
pub(crate) fn is_item_start(parser: &Parser<'_>) -> bool {
    let token = parser.peek();
    match token.kind {
        TokenKind::Punct(Punct::Hash) => hash_prefixed_item_start(parser),
        // `comptime` is ambiguous: `comptime fn` starts an item, but
        // `comptime { ... }` is an expression. Only treat it as an item
        // start when a function keyword follows.
        TokenKind::Keyword(Keyword::Comptime) => matches!(
            parser.peek_nth(1).kind,
            TokenKind::Keyword(Keyword::Fn | Keyword::Unsafe)
        ),
        TokenKind::Keyword(keyword) => matches!(
            keyword,
            Keyword::Pub
                | Keyword::Fn
                | Keyword::Struct
                | Keyword::Enum
                | Keyword::Trait
                | Keyword::Impl
                | Keyword::Type
                | Keyword::Const
                | Keyword::Static
                | Keyword::Mod
                | Keyword::Use
                | Keyword::Unsafe
                | Keyword::Extern
        ),
        _ => false,
    }
}

fn hash_prefixed_item_start(parser: &Parser<'_>) -> bool {
    // `#{..}` is a `Set` literal. Only `#[` can open an attribute, so this
    // shape is always an expression and never introduces an item.
    if matches!(parser.peek_nth(1).kind, TokenKind::Punct(Punct::LBrace)) {
        return false;
    }
    let Some(offset) = skip_outer_attrs(parser, 0) else {
        return true;
    };
    matches!(
        parser.peek_nth(offset).kind,
        TokenKind::Keyword(Keyword::Pub)
    ) && keyword_item_start_after_attrs(parser.peek_nth(offset + 1).kind)
        || keyword_item_start_after_attrs(parser.peek_nth(offset).kind)
}

fn skip_outer_attrs(parser: &Parser<'_>, mut offset: usize) -> Option<usize> {
    while matches!(parser.peek_nth(offset).kind, TokenKind::Punct(Punct::Hash)) {
        if !matches!(
            parser.peek_nth(offset + 1).kind,
            TokenKind::Punct(Punct::LBracket)
        ) {
            return None;
        }
        offset += 2;
        let mut depth = 1usize;
        while depth > 0 {
            match parser.peek_nth(offset).kind {
                TokenKind::Eof => return None,
                TokenKind::Punct(Punct::LBracket) => depth += 1,
                TokenKind::Punct(Punct::RBracket) => depth -= 1,
                _ => {}
            }
            offset += 1;
        }
    }
    Some(offset)
}

fn keyword_item_start_after_attrs(kind: TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::Keyword(
            Keyword::Comptime
                | Keyword::Fn
                | Keyword::Struct
                | Keyword::Enum
                | Keyword::Trait
                | Keyword::Impl
                | Keyword::Type
                | Keyword::Const
                | Keyword::Static
                | Keyword::Mod
                | Keyword::Use
                | Keyword::Unsafe
                | Keyword::Extern
        )
    )
}

/// Returns `true` when the current token begins a fresh statement.
pub(crate) fn is_stmt_start(parser: &Parser<'_>) -> bool {
    match parser.peek().kind {
        TokenKind::Keyword(keyword) => is_stmt_start_keyword(keyword),
        _ => false,
    }
}

/// Returns `true` for a keyword that can only begin a statement.
pub(crate) fn is_stmt_start_keyword(keyword: Keyword) -> bool {
    matches!(
        keyword,
        Keyword::Let
            | Keyword::Return
            | Keyword::Break
            | Keyword::Continue
            | Keyword::If
            | Keyword::While
            | Keyword::For
            | Keyword::Loop
            | Keyword::Match
            | Keyword::Fn
            | Keyword::Struct
            | Keyword::Enum
            | Keyword::Trait
            | Keyword::Impl
            | Keyword::Use
            | Keyword::Type
            | Keyword::Const
            | Keyword::Static
            | Keyword::Mod
            | Keyword::Go
            | Keyword::Defer
            | Keyword::Pub
    )
}
