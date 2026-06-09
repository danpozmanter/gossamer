//! Statement parsing inside block expressions.

#![forbid(unsafe_code)]

use gossamer_ast::{
    Expr, ExprKind, Ident, MatchArm, Mutability, PathExpr, PathSegment, Pattern, PatternKind, Stmt,
    StmtKind,
};
use gossamer_lex::{Keyword, Punct, Span};

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
        // `let PAT = init else { … }` — desugar to a `match` binding so the
        // refutable pattern's bindings escape into the enclosing scope
        // (Rust/Swift semantics), reusing the match lowering on every tier.
        // The else block must diverge; that is enforced by its `!`-typed
        // position in the wildcard arm during type-checking.
        match (self.at_keyword(Keyword::Else), init) {
            (true, Some(init)) => self.desugar_let_else(pattern, init),
            (_, init) => {
                self.eat_punct(Punct::Semi);
                StmtKind::Let { pattern, ty, init }
            }
        }
    }

    fn desugar_let_else(&mut self, pattern: Pattern, init: Box<Expr>) -> StmtKind {
        self.bump(); // consume `else`
        self.expect_punct(Punct::LBrace, "to open `let ... else` block");
        let start = self.last_span();
        let block = self.parse_block_body();
        let else_span = self.join(start, self.last_span());
        let else_expr = Expr::new(self.alloc_id(), else_span, ExprKind::Block(block));
        self.eat_punct(Punct::Semi);

        let mut binds: Vec<Ident> = Vec::new();
        collect_pattern_bindings(&pattern, &mut binds);
        let pat_span = pattern.span;

        let success_body = self.make_var_tuple_expr(&binds, pat_span);
        let outer_pat = self.make_binding_pattern(&binds, pat_span);
        let wildcard = Pattern::new(self.alloc_id(), else_span, PatternKind::Wildcard);
        let match_expr = Expr::new(
            self.alloc_id(),
            pat_span,
            ExprKind::Match {
                scrutinee: init,
                arms: vec![
                    MatchArm {
                        pattern,
                        guard: None,
                        body: success_body,
                    },
                    MatchArm {
                        pattern: wildcard,
                        guard: None,
                        body: else_expr,
                    },
                ],
            },
        );
        StmtKind::Let {
            pattern: outer_pat,
            ty: None,
            init: Some(Box::new(match_expr)),
        }
    }

    /// Builds the success-arm body: the pattern's bindings as a tuple (single
    /// value when there is one, `()` when there are none).
    fn make_var_tuple_expr(&mut self, binds: &[Ident], span: Span) -> Expr {
        let mut refs: Vec<Expr> = binds
            .iter()
            .map(|id| {
                let path = PathExpr {
                    segments: vec![PathSegment {
                        name: id.clone(),
                        generics: Vec::new(),
                    }],
                };
                Expr::new(self.alloc_id(), span, ExprKind::Path(path))
            })
            .collect();
        if refs.len() == 1 {
            refs.pop().expect("len checked")
        } else {
            Expr::new(self.alloc_id(), span, ExprKind::Tuple(refs))
        }
    }

    /// Builds the outer `let` pattern that receives the match result: the
    /// pattern's bindings as a tuple of irrefutable name bindings (single when
    /// there is one, `_` when there are none).
    fn make_binding_pattern(&mut self, binds: &[Ident], span: Span) -> Pattern {
        if binds.is_empty() {
            return Pattern::new(self.alloc_id(), span, PatternKind::Wildcard);
        }
        let mut pats: Vec<Pattern> = binds
            .iter()
            .map(|id| {
                Pattern::new(
                    self.alloc_id(),
                    span,
                    PatternKind::Ident {
                        mutability: Mutability::Immutable,
                        name: id.clone(),
                        subpattern: None,
                    },
                )
            })
            .collect();
        if pats.len() == 1 {
            pats.pop().expect("len checked")
        } else {
            Pattern::new(self.alloc_id(), span, PatternKind::Tuple(pats))
        }
    }
}

/// Collects the binding identifiers introduced by a pattern, in source order,
/// so `let ... else` can thread them out of the desugared `match`.
fn collect_pattern_bindings(pat: &Pattern, out: &mut Vec<Ident>) {
    match &pat.kind {
        PatternKind::Ident {
            name, subpattern, ..
        } => {
            out.push(name.clone());
            if let Some(sub) = subpattern {
                collect_pattern_bindings(sub, out);
            }
        }
        PatternKind::Tuple(ps) => {
            for p in ps {
                collect_pattern_bindings(p, out);
            }
        }
        PatternKind::TupleStruct { elems, .. } => {
            for p in elems {
                collect_pattern_bindings(p, out);
            }
        }
        PatternKind::Struct { fields, .. } => {
            for f in fields {
                match &f.pattern {
                    Some(p) => collect_pattern_bindings(p, out),
                    None => out.push(f.name.clone()),
                }
            }
        }
        // Every alternative of an or-pattern binds the same names; take the first.
        PatternKind::Or(ps) => {
            if let Some(first) = ps.first() {
                collect_pattern_bindings(first, out);
            }
        }
        PatternKind::Ref { inner, .. } => collect_pattern_bindings(inner, out),
        _ => {}
    }
}
