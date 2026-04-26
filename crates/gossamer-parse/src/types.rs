//! Parsing for Gossamer type expressions (SPEC §3).

#![forbid(unsafe_code)]

use gossamer_ast::{
    FnTypeKind, GenericArg, Mutability, Type, TypeKind, TypePath, TypePathSegment,
};
use gossamer_lex::{Keyword, Punct, TokenKind};

use crate::diagnostic::ParseError;
use crate::parser::Parser;

impl Parser<'_> {
    /// Parses a single `Type` production.
    pub(crate) fn parse_type(&mut self) -> Type {
        let start_span = self.peek_span();
        let kind = self.parse_type_kind();
        let end_span = self.last_span();
        let span = self.join(start_span, end_span);
        let id = self.alloc_id();
        Type::new(id, span, kind)
    }

    fn parse_type_kind(&mut self) -> TypeKind {
        if self.eat_punct(Punct::LParen) {
            return self.parse_tuple_or_unit_type();
        }
        if self.eat_punct(Punct::LBracket) {
            return self.parse_array_or_slice_type();
        }
        if self.at_punct(Punct::Amp) {
            return self.parse_ref_type();
        }
        if self.at_punct(Punct::Bang) {
            self.bump();
            return TypeKind::Never;
        }
        if self.at_keyword(Keyword::Fn) {
            return self.parse_fn_type(FnTypeKind::Fn);
        }
        if matches!(self.peek().kind, TokenKind::Ident) {
            let text = self.slice(self.peek_span());
            if text == "Fn" {
                self.bump();
                return self.parse_fn_type_after_keyword(FnTypeKind::ClosureFn);
            }
            if text == "FnMut" {
                self.bump();
                return self.parse_fn_type_after_keyword(FnTypeKind::ClosureFnMut);
            }
            if text == "FnOnce" {
                self.bump();
                return self.parse_fn_type_after_keyword(FnTypeKind::ClosureFnOnce);
            }
            if text == "_" {
                self.bump();
                return TypeKind::Infer;
            }
        }
        TypeKind::Path(self.parse_type_path())
    }

    fn parse_tuple_or_unit_type(&mut self) -> TypeKind {
        if self.eat_punct(Punct::RParen) {
            return TypeKind::Unit;
        }
        let first = self.parse_type();
        if self.eat_punct(Punct::RParen) {
            return first.kind;
        }
        let mut elements = vec![first];
        while self.eat_punct(Punct::Comma) {
            if self.at_punct(Punct::RParen) {
                break;
            }
            elements.push(self.parse_type());
        }
        self.expect_punct(Punct::RParen, "to close tuple type");
        TypeKind::Tuple(elements)
    }

    fn parse_array_or_slice_type(&mut self) -> TypeKind {
        let element = self.parse_type();
        if self.eat_punct(Punct::Semi) {
            let length = self.parse_expr();
            self.expect_punct(Punct::RBracket, "to close array type");
            return TypeKind::Array {
                elem: Box::new(element),
                len: Box::new(length),
            };
        }
        self.expect_punct(Punct::RBracket, "to close slice type");
        TypeKind::Slice(Box::new(element))
    }

    fn parse_ref_type(&mut self) -> TypeKind {
        self.bump();
        let mutability = if self.eat_keyword(Keyword::Mut) {
            Mutability::Mutable
        } else {
            Mutability::Immutable
        };
        let inner = self.parse_type();
        TypeKind::Ref {
            mutability,
            inner: Box::new(inner),
        }
    }

    fn parse_fn_type(&mut self, kind: FnTypeKind) -> TypeKind {
        self.bump();
        self.parse_fn_type_after_keyword(kind)
    }

    fn parse_fn_type_after_keyword(&mut self, kind: FnTypeKind) -> TypeKind {
        self.expect_punct(Punct::LParen, "to start function parameter types");
        let mut params = Vec::new();
        while !self.at_punct(Punct::RParen) && !self.at_eof() {
            params.push(self.parse_type());
            if !self.eat_punct(Punct::Comma) {
                break;
            }
        }
        self.expect_punct(Punct::RParen, "to close function parameter types");
        let ret = if self.eat_punct(Punct::Arrow) {
            Some(Box::new(self.parse_type()))
        } else {
            None
        };
        TypeKind::Fn { kind, params, ret }
    }

    /// Parses a `TypePath` — a path in type position (no turbofish `::<>`).
    pub(crate) fn parse_type_path(&mut self) -> TypePath {
        let first = self.parse_type_path_segment();
        let mut segments = vec![first];
        while self.eat_punct(Punct::ColonColon) {
            segments.push(self.parse_type_path_segment());
        }
        TypePath { segments }
    }

    fn parse_type_path_segment(&mut self) -> TypePathSegment {
        let name = self.parse_path_ident_text();
        let generics = if self.at_punct(Punct::Lt) {
            self.parse_type_generic_args()
        } else {
            Vec::new()
        };
        TypePathSegment::with_generics(name, generics)
    }

    fn parse_path_ident_text(&mut self) -> String {
        let token = self.peek();
        match token.kind {
            TokenKind::Ident => {
                self.bump();
                self.slice(token.span).to_string()
            }
            TokenKind::Keyword(Keyword::SelfUpper) => {
                self.bump();
                "Self".to_string()
            }
            TokenKind::Keyword(Keyword::SelfLower) => {
                self.bump();
                "self".to_string()
            }
            TokenKind::Keyword(Keyword::Super) => {
                self.bump();
                "super".to_string()
            }
            TokenKind::Keyword(Keyword::Crate) => {
                self.bump();
                "crate".to_string()
            }
            _ => {
                self.record(
                    ParseError::Unexpected {
                        expected: "path segment identifier".to_string(),
                        found: self.peek_text(),
                    },
                    token.span,
                );
                String::new()
            }
        }
    }

    fn parse_type_generic_args(&mut self) -> Vec<GenericArg> {
        if !self.eat_punct(Punct::Lt) {
            return Vec::new();
        }
        let mut args = Vec::new();
        while !self.at_punct(Punct::Gt) && !self.at_eof() {
            args.push(self.parse_generic_arg());
            if !self.eat_punct(Punct::Comma) {
                break;
            }
        }
        self.expect_punct(Punct::Gt, "to close generic argument list");
        args
    }

    /// Parses one generic argument (type or const expression).
    pub(crate) fn parse_generic_arg(&mut self) -> GenericArg {
        if is_const_arg_start(self) {
            return GenericArg::Const(self.parse_expr());
        }
        GenericArg::Type(self.parse_type())
    }

}

/// Returns `true` when the upcoming generic argument is a const expression
/// (integer literal, bool keyword, or a parenthesised expression).
fn is_const_arg_start(parser: &Parser<'_>) -> bool {
    matches!(
        parser.peek().kind,
        TokenKind::IntLit
            | TokenKind::FloatLit
            | TokenKind::Keyword(Keyword::True | Keyword::False)
    ) || parser.at_punct(Punct::LBrace)
}
