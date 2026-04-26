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
        TokenKind::Punct(Punct::Hash) => true,
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

/// Returns `true` when the current token begins a fresh statement.
pub(crate) fn is_stmt_start(parser: &Parser<'_>) -> bool {
    let token = parser.peek();
    match token.kind {
        TokenKind::Keyword(keyword) => matches!(
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
        ),
        _ => false,
    }
}
