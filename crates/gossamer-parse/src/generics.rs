//! Generic parameter lists, trait bounds, and `where` clauses.

#![forbid(unsafe_code)]

use gossamer_ast::{GenericParam, Generics, Ident, TraitBound, WhereClause, WherePredicate};
use gossamer_lex::{Keyword, Punct, TokenKind};

use crate::diagnostic::ParseError;
use crate::parser::Parser;

impl Parser<'_> {
    /// Parses an optional `<...>` generic parameter list.
    pub(crate) fn parse_generics(&mut self) -> Generics {
        if !self.at_punct(Punct::Lt) {
            return Generics::default();
        }
        self.bump();
        let mut params = Vec::new();
        while !self.at_punct(Punct::Gt) && !self.at_eof() {
            params.push(self.parse_generic_param());
            if !self.eat_punct(Punct::Comma) {
                break;
            }
        }
        self.expect_punct(Punct::Gt, "to close generic parameters");
        Generics { params }
    }

    fn parse_generic_param(&mut self) -> GenericParam {
        if self.eat_keyword(Keyword::Const) {
            let name_span = self.peek_span();
            let name_text = if matches!(self.peek().kind, TokenKind::Ident) {
                self.bump();
                self.slice(name_span).to_string()
            } else {
                self.record(
                    ParseError::Unexpected {
                        expected: "const generic name".to_string(),
                        found: self.peek_text(),
                    },
                    name_span,
                );
                String::new()
            };
            self.expect_punct(Punct::Colon, "after const generic name");
            let ty = self.parse_type();
            let default = if self.eat_punct(Punct::Eq) {
                Some(self.parse_expr())
            } else {
                None
            };
            return GenericParam::Const {
                name: Ident::new(name_text),
                ty,
                default,
            };
        }
        let token = self.peek();
        if matches!(token.kind, TokenKind::Ident) && self.slice(token.span).starts_with('\'') {
            self.bump();
            return GenericParam::Lifetime {
                name: self.slice(token.span).trim_start_matches('\'').to_string(),
            };
        }
        let name_span = self.peek_span();
        let name_text = if matches!(self.peek().kind, TokenKind::Ident) {
            self.bump();
            self.slice(name_span).to_string()
        } else {
            self.record(
                ParseError::Unexpected {
                    expected: "type parameter name".to_string(),
                    found: self.peek_text(),
                },
                name_span,
            );
            String::new()
        };
        let mut bounds = Vec::new();
        if self.eat_punct(Punct::Colon) {
            bounds = self.parse_trait_bound_list();
        }
        let default = if self.eat_punct(Punct::Eq) {
            Some(self.parse_type())
        } else {
            None
        };
        GenericParam::Type {
            name: Ident::new(name_text),
            bounds,
            default,
        }
    }

    /// Parses a `+`-separated list of trait bounds.
    pub(crate) fn parse_trait_bound_list(&mut self) -> Vec<TraitBound> {
        let mut bounds = vec![self.parse_trait_bound()];
        while self.eat_punct(Punct::Plus) {
            bounds.push(self.parse_trait_bound());
        }
        bounds
    }

    fn parse_trait_bound(&mut self) -> TraitBound {
        let path = self.parse_type_path();
        TraitBound { path }
    }

    /// Parses an optional `where` clause.
    pub(crate) fn parse_where_clause(&mut self) -> WhereClause {
        if !self.eat_keyword(Keyword::Where) {
            return WhereClause::default();
        }
        let mut predicates = Vec::new();
        loop {
            if is_clause_terminator(self) {
                break;
            }
            let bounded = self.parse_type();
            self.expect_punct(Punct::Colon, "in where predicate");
            let bounds = self.parse_trait_bound_list();
            predicates.push(WherePredicate { bounded, bounds });
            if !self.eat_punct(Punct::Comma) {
                break;
            }
        }
        WhereClause { predicates }
    }
}

/// Returns `true` when the current token terminates a `where` clause.
fn is_clause_terminator(parser: &Parser<'_>) -> bool {
    parser.at_punct(Punct::LBrace)
        || parser.at_punct(Punct::Semi)
        || parser.at_punct(Punct::Eq)
        || parser.at_eof()
}
