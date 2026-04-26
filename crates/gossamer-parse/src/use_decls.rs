//! Parses `use` declarations (SPEC §6.6).

#![forbid(unsafe_code)]

use gossamer_ast::{Ident, ModulePath, UseDecl, UseListEntry, UseTarget};
use gossamer_lex::{Keyword, Punct, TokenKind};

use crate::diagnostic::ParseError;
use crate::parser::Parser;

impl Parser<'_> {
    /// Parses a single `use` declaration after the `use` keyword has been seen.
    pub(crate) fn parse_use_decl(&mut self) -> UseDecl {
        let start_span = self.peek_span();
        self.bump();
        let target = if matches!(self.peek().kind, TokenKind::StringLit) {
            self.parse_project_use_target()
        } else {
            self.parse_module_use_target()
        };
        let alias = if self.eat_keyword(Keyword::As) {
            Some(self.parse_use_ident())
        } else {
            None
        };
        if self.at_punct(Punct::ColonColon)
            && matches!(self.peek_nth(1).kind, TokenKind::Punct(Punct::LBrace))
        {
            self.bump();
        }
        let list = if self.at_punct(Punct::LBrace) {
            Some(self.parse_use_list())
        } else {
            None
        };
        self.eat_punct(Punct::Semi);
        let end_span = self.last_span();
        let span = self.join(start_span, end_span);
        let id = self.alloc_id();
        UseDecl {
            id,
            span,
            target,
            alias,
            list,
        }
    }

    fn parse_project_use_target(&mut self) -> UseTarget {
        let lit_span = self.peek_span();
        self.bump();
        let raw = self.slice(lit_span);
        let project_id = raw
            .strip_prefix('"')
            .and_then(|text| text.strip_suffix('"'))
            .unwrap_or(raw)
            .to_string();
        let module = if self.eat_punct(Punct::ColonColon) {
            Some(self.parse_module_path())
        } else {
            None
        };
        UseTarget::Project {
            id: project_id,
            module,
        }
    }

    fn parse_module_use_target(&mut self) -> UseTarget {
        UseTarget::Module(self.parse_module_path())
    }

    fn parse_module_path(&mut self) -> ModulePath {
        let mut segments = Vec::new();
        segments.push(self.parse_use_ident());
        while self.at_punct(Punct::ColonColon) {
            let checkpoint = self.tokens.checkpoint();
            self.bump();
            if self.at_punct(Punct::LBrace) {
                self.tokens.rewind(checkpoint);
                break;
            }
            segments.push(self.parse_use_ident());
        }
        ModulePath { segments }
    }

    fn parse_use_ident(&mut self) -> Ident {
        let span = self.peek_span();
        match self.peek().kind {
            TokenKind::Ident => {
                self.bump();
                Ident::new(self.slice(span))
            }
            TokenKind::Keyword(Keyword::Crate) => {
                self.bump();
                Ident::new("crate")
            }
            TokenKind::Keyword(Keyword::Super) => {
                self.bump();
                Ident::new("super")
            }
            TokenKind::Keyword(Keyword::SelfLower) => {
                self.bump();
                Ident::new("self")
            }
            _ => {
                self.record(ParseError::MalformedUse, span);
                Ident::new("<error>")
            }
        }
    }

    fn parse_use_list(&mut self) -> Vec<UseListEntry> {
        self.bump();
        let mut entries = Vec::new();
        while !self.at_punct(Punct::RBrace) && !self.at_eof() {
            let name = self.parse_use_ident();
            let alias = if self.eat_keyword(Keyword::As) {
                Some(self.parse_use_ident())
            } else {
                None
            };
            entries.push(UseListEntry { name, alias });
            if !self.eat_punct(Punct::Comma) {
                break;
            }
        }
        self.expect_punct(Punct::RBrace, "to close `use` list");
        entries
    }
}
