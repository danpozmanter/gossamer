//! Statement parsing inside block expressions.

#![forbid(unsafe_code)]

use gossamer_ast::{Stmt, StmtKind};
use gossamer_lex::{Keyword, Punct};

use crate::parser::Parser;
use crate::recovery::{is_item_start, is_stmt_start};

impl Parser<'_> {
    /// Parses a single statement.
    pub(crate) fn parse_stmt(&mut self) -> Stmt {
        let start_span = self.peek_span();
        let kind = self.parse_stmt_kind();
        let end_span = self.last_span();
        let span = self.join(start_span, end_span);
        let id = self.alloc_id();
        Stmt::new(id, span, kind)
    }

    fn parse_stmt_kind(&mut self) -> StmtKind {
        if self.at_keyword(Keyword::Let) {
            return self.parse_let_stmt();
        }
        if self.at_keyword(Keyword::Defer) {
            self.bump();
            let body = self.parse_expr();
            self.eat_punct(Punct::Semi);
            return StmtKind::Defer(Box::new(body));
        }
        if self.at_keyword(Keyword::Go) {
            self.bump();
            let value = self.parse_expr();
            self.eat_punct(Punct::Semi);
            return StmtKind::Go(Box::new(value));
        }
        if is_item_start(self) {
            let item = self.parse_item();
            return StmtKind::Item(Box::new(item));
        }
        let before = self.tokens.checkpoint();
        let expression = self.parse_expr();
        if self.tokens.checkpoint() == before && !is_stmt_start(self) {
            self.recover_in_block();
        }
        let has_semi = self.eat_punct(Punct::Semi);
        StmtKind::Expr {
            expr: Box::new(expression),
            has_semi,
        }
    }

    fn parse_let_stmt(&mut self) -> StmtKind {
        self.bump();
        let pattern = self.parse_pattern();
        let ty = if self.eat_punct(Punct::Colon) {
            Some(self.parse_type())
        } else {
            None
        };
        let init = if self.eat_punct(Punct::Eq) {
            Some(Box::new(self.parse_expr()))
        } else {
            None
        };
        self.eat_punct(Punct::Semi);
        StmtKind::Let { pattern, ty, init }
    }
}
