//! Top-level item parsing: functions, structs, enums, traits, impls,
//! type aliases, constants, statics, and modules.

#![forbid(unsafe_code)]

use gossamer_ast::{
    Attribute, Attrs, ConstDecl, EnumDecl, EnumVariant, Expr, ExprKind, FnDecl, FnParam, Ident,
    ImplDecl, ImplItem, Item, ItemKind, ModBody, ModDecl, Mutability, Receiver, StaticDecl,
    StructBody, StructDecl, StructField, TraitBound, TraitDecl, TraitItem, TupleField,
    TypeAliasDecl, TypePath, TypePathSegment, Visibility,
};
use gossamer_lex::{Keyword, Punct, TokenKind};

use crate::diagnostic::ParseError;
use crate::parser::Parser;

impl Parser<'_> {
    /// Parses a single top-level item.
    pub(crate) fn parse_item(&mut self) -> Item {
        let start_span = self.peek_span();
        let attrs = self.parse_attrs();
        let visibility = if self.eat_keyword(Keyword::Pub) {
            Visibility::Public
        } else {
            Visibility::Inherited
        };
        let kind = self.parse_item_kind();
        let end_span = self.last_span();
        let span = self.join(start_span, end_span);
        let id = self.alloc_id();
        Item::new(id, span, attrs, visibility, kind)
    }

    fn parse_item_kind(&mut self) -> ItemKind {
        if self.at_keyword(Keyword::Fn) || self.at_keyword(Keyword::Unsafe) {
            return ItemKind::Fn(self.parse_fn_decl());
        }
        if self.at_keyword(Keyword::Struct) {
            return ItemKind::Struct(self.parse_struct_decl());
        }
        if self.at_keyword(Keyword::Enum) {
            return ItemKind::Enum(self.parse_enum_decl());
        }
        if self.at_keyword(Keyword::Trait) {
            return ItemKind::Trait(self.parse_trait_decl());
        }
        if self.at_keyword(Keyword::Impl) {
            return ItemKind::Impl(self.parse_impl_decl());
        }
        if self.at_keyword(Keyword::Type) {
            return ItemKind::TypeAlias(self.parse_type_alias_decl());
        }
        if self.at_keyword(Keyword::Const) {
            return ItemKind::Const(self.parse_const_decl());
        }
        if self.at_keyword(Keyword::Static) {
            return ItemKind::Static(self.parse_static_decl());
        }
        if self.at_keyword(Keyword::Mod) {
            return ItemKind::Mod(self.parse_mod_decl());
        }
        self.record(
            ParseError::Unexpected {
                expected: "item keyword".to_string(),
                found: self.peek_text(),
            },
            self.peek_span(),
        );
        self.recover_to_item_start();
        ItemKind::Mod(ModDecl {
            name: Ident::new("<error>"),
            body: ModBody::External,
        })
    }

    /// Parses the outer attribute list preceding an item.
    pub(crate) fn parse_attrs(&mut self) -> Attrs {
        let mut outer = Vec::new();
        while self.at_punct(Punct::Hash) {
            if let Some(attribute) = self.parse_attribute() {
                outer.push(attribute);
            } else {
                break;
            }
        }
        Attrs {
            outer,
            inner: Vec::new(),
        }
    }

    fn parse_attribute(&mut self) -> Option<Attribute> {
        if !self.eat_punct(Punct::Hash) {
            return None;
        }
        let _inner = self.eat_punct(Punct::Bang);
        if !self.eat_punct(Punct::LBracket) {
            self.record(ParseError::MalformedAttribute, self.peek_span());
            return None;
        }
        let path = self.parse_path_expr();
        let tokens = if self.at_punct(Punct::LParen) {
            self.bump();
            let body = self.collect_delimited_tokens_public(Punct::LParen, Punct::RParen);
            self.expect_punct(Punct::RParen, "to close attribute arguments");
            Some(body)
        } else if self.at_punct(Punct::Eq) {
            self.bump();
            let rest = self.collect_until_rbracket();
            Some(format!("= {rest}"))
        } else {
            None
        };
        self.expect_punct(Punct::RBracket, "to close attribute");
        Some(Attribute { path, tokens })
    }

    fn collect_delimited_tokens_public(&mut self, open: Punct, close: Punct) -> String {
        self.collect_delimited_tokens_in_attr(open, close)
    }

    fn collect_delimited_tokens_in_attr(&mut self, open: Punct, close: Punct) -> String {
        let mut depth = 1u32;
        let mut output = String::new();
        while !self.at_eof() {
            let token = self.peek();
            match token.kind {
                TokenKind::Punct(found) if found == open => {
                    depth += 1;
                    output.push_str(self.slice(token.span));
                    output.push(' ');
                    self.bump();
                }
                TokenKind::Punct(found) if found == close => {
                    depth -= 1;
                    if depth == 0 {
                        return output.trim_end().to_string();
                    }
                    output.push_str(self.slice(token.span));
                    output.push(' ');
                    self.bump();
                }
                _ => {
                    output.push_str(self.slice(token.span));
                    output.push(' ');
                    self.bump();
                }
            }
        }
        output.trim_end().to_string()
    }

    fn collect_until_rbracket(&mut self) -> String {
        let mut output = String::new();
        while !self.at_eof() && !self.at_punct(Punct::RBracket) {
            let token = self.peek();
            output.push_str(self.slice(token.span));
            output.push(' ');
            self.bump();
        }
        output.trim_end().to_string()
    }

    fn parse_fn_decl(&mut self) -> FnDecl {
        let is_unsafe = self.eat_keyword(Keyword::Unsafe);
        self.expect_keyword(Keyword::Fn, "to start function declaration");
        let name = self.parse_ident_required("function name");
        let generics = self.parse_generics();
        self.expect_punct(Punct::LParen, "to open function parameter list");
        let params = self.parse_fn_params();
        self.expect_punct(Punct::RParen, "to close function parameter list");
        let ret = if self.eat_punct(Punct::Arrow) {
            Some(self.parse_type())
        } else {
            None
        };
        let where_clause = self.parse_where_clause();
        let body = if self.at_punct(Punct::LBrace) {
            self.bump();
            let block = self.parse_block_body();
            let span = self.last_span();
            let id = self.alloc_id();
            Some(Box::new(Expr::new(id, span, ExprKind::Block(block))))
        } else {
            self.eat_punct(Punct::Semi);
            None
        };
        FnDecl {
            is_unsafe,
            name,
            generics,
            params,
            ret,
            where_clause,
            body,
        }
    }

    fn parse_fn_params(&mut self) -> Vec<FnParam> {
        let mut params = Vec::new();
        if self.at_receiver_start() {
            if let Some(receiver) = self.parse_receiver() {
                params.push(FnParam::Receiver(receiver));
                if !self.eat_punct(Punct::Comma) {
                    return params;
                }
            }
        }
        while !self.at_punct(Punct::RParen) && !self.at_eof() {
            let pattern = self.parse_pattern_no_or();
            self.expect_punct(Punct::Colon, "after parameter pattern");
            let ty = self.parse_type();
            params.push(FnParam::Typed { pattern, ty });
            if !self.eat_punct(Punct::Comma) {
                break;
            }
        }
        params
    }

    fn at_receiver_start(&self) -> bool {
        if self.at_keyword(Keyword::SelfLower) {
            return true;
        }
        if self.at_punct(Punct::Amp) {
            let after = self.peek_nth(1);
            if matches!(after.kind, TokenKind::Keyword(Keyword::SelfLower)) {
                return true;
            }
            if matches!(after.kind, TokenKind::Keyword(Keyword::Mut))
                && matches!(
                    self.peek_nth(2).kind,
                    TokenKind::Keyword(Keyword::SelfLower)
                )
            {
                return true;
            }
        }
        false
    }

    fn parse_receiver(&mut self) -> Option<Receiver> {
        if self.eat_keyword(Keyword::SelfLower) {
            return Some(Receiver::Owned);
        }
        if self.eat_punct(Punct::Amp) {
            let mutability = self.eat_keyword(Keyword::Mut);
            self.expect_keyword(Keyword::SelfLower, "after `&`/`&mut`");
            return Some(if mutability {
                Receiver::RefMut
            } else {
                Receiver::RefShared
            });
        }
        None
    }

    fn parse_struct_decl(&mut self) -> StructDecl {
        self.bump();
        let name = self.parse_ident_required("struct name");
        let generics = self.parse_generics();
        if self.at_keyword(Keyword::Where) {
            let where_clause = self.parse_where_clause();
            let body = self.parse_struct_body_terminated();
            return StructDecl {
                name,
                generics,
                where_clause,
                body,
            };
        }
        let body = self.parse_struct_body();
        let where_clause = if matches!(&body, StructBody::Tuple(_) | StructBody::Unit) {
            let clause = self.parse_where_clause();
            self.eat_punct(Punct::Semi);
            clause
        } else {
            self.parse_where_clause()
        };
        StructDecl {
            name,
            generics,
            where_clause,
            body,
        }
    }

    fn parse_struct_body_terminated(&mut self) -> StructBody {
        let body = self.parse_struct_body();
        if matches!(&body, StructBody::Tuple(_) | StructBody::Unit) {
            self.eat_punct(Punct::Semi);
        }
        body
    }

    fn parse_struct_body(&mut self) -> StructBody {
        if self.eat_punct(Punct::LBrace) {
            let mut fields = Vec::new();
            while !self.at_punct(Punct::RBrace) && !self.at_eof() {
                let attrs = self.parse_attrs();
                let visibility = if self.eat_keyword(Keyword::Pub) {
                    Visibility::Public
                } else {
                    Visibility::Inherited
                };
                let name = self.parse_ident_required("field name");
                self.expect_punct(Punct::Colon, "after field name");
                let ty = self.parse_type();
                fields.push(StructField {
                    attrs,
                    visibility,
                    name,
                    ty,
                });
                if !self.eat_punct(Punct::Comma) {
                    break;
                }
            }
            self.expect_punct(Punct::RBrace, "to close struct body");
            return StructBody::Named(fields);
        }
        if self.eat_punct(Punct::LParen) {
            let mut fields = Vec::new();
            while !self.at_punct(Punct::RParen) && !self.at_eof() {
                let attrs = self.parse_attrs();
                let visibility = if self.eat_keyword(Keyword::Pub) {
                    Visibility::Public
                } else {
                    Visibility::Inherited
                };
                let ty = self.parse_type();
                fields.push(TupleField {
                    attrs,
                    visibility,
                    ty,
                });
                if !self.eat_punct(Punct::Comma) {
                    break;
                }
            }
            self.expect_punct(Punct::RParen, "to close tuple struct body");
            return StructBody::Tuple(fields);
        }
        StructBody::Unit
    }

    fn parse_enum_decl(&mut self) -> EnumDecl {
        self.bump();
        let name = self.parse_ident_required("enum name");
        let generics = self.parse_generics();
        let where_clause = self.parse_where_clause();
        self.expect_punct(Punct::LBrace, "to open enum body");
        let mut variants = Vec::new();
        while !self.at_punct(Punct::RBrace) && !self.at_eof() {
            let attrs = self.parse_attrs();
            let variant_name = self.parse_ident_required("variant name");
            let body = if self.eat_punct(Punct::LBrace) {
                let mut fields = Vec::new();
                while !self.at_punct(Punct::RBrace) && !self.at_eof() {
                    let field_attrs = self.parse_attrs();
                    let visibility = if self.eat_keyword(Keyword::Pub) {
                        Visibility::Public
                    } else {
                        Visibility::Inherited
                    };
                    let field_name = self.parse_ident_required("field name");
                    self.expect_punct(Punct::Colon, "after field name");
                    let ty = self.parse_type();
                    fields.push(StructField {
                        attrs: field_attrs,
                        visibility,
                        name: field_name,
                        ty,
                    });
                    if !self.eat_punct(Punct::Comma) {
                        break;
                    }
                }
                self.expect_punct(Punct::RBrace, "to close variant body");
                StructBody::Named(fields)
            } else if self.eat_punct(Punct::LParen) {
                let mut fields = Vec::new();
                while !self.at_punct(Punct::RParen) && !self.at_eof() {
                    let field_attrs = self.parse_attrs();
                    let visibility = if self.eat_keyword(Keyword::Pub) {
                        Visibility::Public
                    } else {
                        Visibility::Inherited
                    };
                    let ty = self.parse_type();
                    fields.push(TupleField {
                        attrs: field_attrs,
                        visibility,
                        ty,
                    });
                    if !self.eat_punct(Punct::Comma) {
                        break;
                    }
                }
                self.expect_punct(Punct::RParen, "to close variant body");
                StructBody::Tuple(fields)
            } else {
                StructBody::Unit
            };
            let discriminant = if self.eat_punct(Punct::Eq) {
                Some(self.parse_expr_no_assign())
            } else {
                None
            };
            variants.push(EnumVariant {
                attrs,
                name: variant_name,
                body,
                discriminant,
            });
            if !self.eat_punct(Punct::Comma) {
                break;
            }
        }
        self.expect_punct(Punct::RBrace, "to close enum body");
        EnumDecl {
            name,
            generics,
            where_clause,
            variants,
        }
    }

    fn parse_trait_decl(&mut self) -> TraitDecl {
        self.bump();
        let name = self.parse_ident_required("trait name");
        let generics = self.parse_generics();
        let supertraits = if self.eat_punct(Punct::Colon) {
            self.parse_trait_bound_list()
        } else {
            Vec::new()
        };
        let where_clause = self.parse_where_clause();
        self.expect_punct(Punct::LBrace, "to open trait body");
        let mut items = Vec::new();
        while !self.at_punct(Punct::RBrace) && !self.at_eof() {
            let attrs = self.parse_attrs();
            if self.eat_keyword(Keyword::Type) {
                let name = self.parse_ident_required("associated type name");
                let bounds = if self.eat_punct(Punct::Colon) {
                    self.parse_trait_bound_list()
                } else {
                    Vec::new()
                };
                let default = if self.eat_punct(Punct::Eq) {
                    Some(self.parse_type())
                } else {
                    None
                };
                self.eat_punct(Punct::Semi);
                items.push(TraitItem::Type {
                    attrs,
                    name,
                    bounds,
                    default,
                });
                continue;
            }
            if self.eat_keyword(Keyword::Const) {
                let name = self.parse_ident_required("associated constant name");
                self.expect_punct(Punct::Colon, "after associated constant name");
                let ty = self.parse_type();
                let default = if self.eat_punct(Punct::Eq) {
                    Some(self.parse_expr())
                } else {
                    None
                };
                self.eat_punct(Punct::Semi);
                items.push(TraitItem::Const {
                    attrs,
                    name,
                    ty,
                    default,
                });
                continue;
            }
            drop(attrs);
            items.push(TraitItem::Fn(self.parse_fn_decl()));
        }
        self.expect_punct(Punct::RBrace, "to close trait body");
        TraitDecl {
            name,
            generics,
            supertraits,
            where_clause,
            items,
        }
    }

    fn parse_impl_decl(&mut self) -> ImplDecl {
        self.bump();
        let generics = self.parse_generics();
        let first_type = self.parse_type();
        let (trait_ref, self_ty) = if self.eat_keyword(Keyword::For) {
            let self_ty = self.parse_type();
            let bound = match first_type.kind {
                gossamer_ast::TypeKind::Path(path) => TraitBound { path },
                _ => TraitBound {
                    path: TypePath {
                        segments: vec![TypePathSegment::new("<error>")],
                    },
                },
            };
            (Some(bound), self_ty)
        } else {
            (None, first_type)
        };
        let where_clause = self.parse_where_clause();
        self.expect_punct(Punct::LBrace, "to open impl body");
        let mut items = Vec::new();
        while !self.at_punct(Punct::RBrace) && !self.at_eof() {
            items.push(self.parse_impl_item());
        }
        self.expect_punct(Punct::RBrace, "to close impl body");
        ImplDecl {
            generics,
            trait_ref,
            self_ty,
            where_clause,
            items,
        }
    }

    fn parse_impl_item(&mut self) -> ImplItem {
        let attrs = self.parse_attrs();
        let _visibility = self.eat_keyword(Keyword::Pub);
        if self.eat_keyword(Keyword::Type) {
            let name = self.parse_ident_required("associated type name");
            self.expect_punct(Punct::Eq, "in associated type");
            let ty = self.parse_type();
            self.eat_punct(Punct::Semi);
            return ImplItem::Type { attrs, name, ty };
        }
        if self.eat_keyword(Keyword::Const) {
            let name = self.parse_ident_required("associated constant name");
            self.expect_punct(Punct::Colon, "after associated constant name");
            let ty = self.parse_type();
            self.expect_punct(Punct::Eq, "in associated constant");
            let value = self.parse_expr();
            self.eat_punct(Punct::Semi);
            return ImplItem::Const {
                attrs,
                name,
                ty,
                value,
            };
        }
        ImplItem::Fn(self.parse_fn_decl())
    }

    fn parse_type_alias_decl(&mut self) -> TypeAliasDecl {
        self.bump();
        let name = self.parse_ident_required("type alias name");
        let generics = self.parse_generics();
        self.expect_punct(Punct::Eq, "in type alias");
        let ty = self.parse_type();
        self.eat_punct(Punct::Semi);
        TypeAliasDecl { name, generics, ty }
    }

    fn parse_const_decl(&mut self) -> ConstDecl {
        self.bump();
        let name = self.parse_ident_required("constant name");
        self.expect_punct(Punct::Colon, "after constant name");
        let ty = self.parse_type();
        self.expect_punct(Punct::Eq, "in constant declaration");
        let value = self.parse_expr();
        self.eat_punct(Punct::Semi);
        ConstDecl { name, ty, value }
    }

    fn parse_static_decl(&mut self) -> StaticDecl {
        self.bump();
        let mutability = if self.eat_keyword(Keyword::Mut) {
            Mutability::Mutable
        } else {
            Mutability::Immutable
        };
        let name = self.parse_ident_required("static name");
        self.expect_punct(Punct::Colon, "after static name");
        let ty = self.parse_type();
        self.expect_punct(Punct::Eq, "in static declaration");
        let value = self.parse_expr();
        self.eat_punct(Punct::Semi);
        StaticDecl {
            mutability,
            name,
            ty,
            value,
        }
    }

    fn parse_mod_decl(&mut self) -> ModDecl {
        self.bump();
        let name = self.parse_ident_required("module name");
        if self.eat_punct(Punct::Semi) {
            return ModDecl {
                name,
                body: ModBody::External,
            };
        }
        self.expect_punct(Punct::LBrace, "to open inline module");
        let mut items = Vec::new();
        while !self.at_punct(Punct::RBrace) && !self.at_eof() {
            let before = self.checkpoint_public();
            if self.at_keyword(Keyword::Use) {
                let _ = self.parse_use_decl();
                continue;
            }
            items.push(self.parse_item());
            if self.checkpoint_public() == before {
                self.bump();
            }
        }
        self.expect_punct(Punct::RBrace, "to close inline module");
        ModDecl {
            name,
            body: ModBody::Inline(items),
        }
    }

    fn parse_ident_required(&mut self, context: &str) -> Ident {
        let span = self.peek_span();
        if matches!(self.peek().kind, TokenKind::Ident) {
            self.bump();
            return Ident::new(self.slice(span));
        }
        self.record(
            ParseError::Unexpected {
                expected: context.to_string(),
                found: self.peek_text(),
            },
            span,
        );
        Ident::new("<error>")
    }
}
