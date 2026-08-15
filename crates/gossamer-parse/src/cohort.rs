//! `cohort { }` - structured concurrency, parsed as a contextual
//! keyword and desugared here.
//!
//! The block owns every goroutine `spawn`ed while it runs and cannot be
//! left until each of them has finished. It desugars to
//!
//! ```text
//! {
//!     runtime::cohort_push(policy, timeout_ms, context)
//!     defer runtime::cohort_pop()
//!     <body statements>
//!     runtime::cohort_join()
//! }
//! ```
//!
//! so the join rides `defer`'s existing exit edges: block end, `return`,
//! `break`, `continue`, and `?` all run the pop, which is what makes the
//! guarantee hold without unwind tables. The tail `cohort_join()` is the
//! block's value, `Result<(), errors::Error>`.

#![forbid(unsafe_code)]

use gossamer_ast::{Expr, ExprKind, Literal, PathExpr, Stmt, StmtKind};
use gossamer_lex::{Punct, Span, TokenKind};

use crate::diagnostic::ParseError;
use crate::parser::Parser;

/// Completion policies, in the order the runtime numbers them.
const POLICIES: &[(&str, i64)] = &[("FailFast", 0), ("CollectAll", 1), ("Race", 2)];
/// Execution contexts, in the order the runtime numbers them.
const CONTEXTS: &[(&str, i64)] = &[("Default", 0), ("Isolated", 1)];

/// A cohort header's settings. The default is a bare `cohort { }`:
/// fail-fast, no deadline, children on the shared carriers.
#[derive(Clone, Copy, Default)]
struct CohortHeader {
    policy: i64,
    timeout_ms: i64,
    context: i64,
}

impl Parser<'_> {
    /// `true` at the head of a `cohort` block.
    ///
    /// `cohort` is contextual, never reserved, so a binding named
    /// `cohort` keeps working. Two shapes are claimed: `cohort {`, and
    /// `cohort (` whose balanced parenthesis group is followed by `{` -
    /// the second needs the scan because `cohort(x)` on its own is an
    /// ordinary call to a user function of that name.
    pub(crate) fn at_cohort_block(&mut self) -> bool {
        let cur = self.peek();
        if !matches!(cur.kind, TokenKind::Ident) || self.slice(cur.span) != "cohort" {
            return false;
        }
        match self.peek_nth(1).kind {
            // A condition or scrutinee forbids an unparenthesised `{`
            // exactly because it is ambiguous there, so a bare `cohort`
            // in that position stays an identifier.
            TokenKind::Punct(Punct::LBrace) => !self.struct_literal_forbidden(),
            TokenKind::Punct(Punct::LParen) => self.paren_group_precedes_brace(),
            _ => false,
        }
    }

    /// Scans the balanced parenthesis group that starts at the token
    /// after the cursor and reports whether a `{` follows it.
    fn paren_group_precedes_brace(&mut self) -> bool {
        let mut depth = 0i32;
        let mut index = 1usize;
        loop {
            match self.peek_nth(index).kind {
                TokenKind::Punct(Punct::LParen) => depth += 1,
                TokenKind::Punct(Punct::RParen) => {
                    depth -= 1;
                    if depth == 0 {
                        return matches!(
                            self.peek_nth(index + 1).kind,
                            TokenKind::Punct(Punct::LBrace)
                        );
                    }
                }
                TokenKind::Eof => return false,
                _ => {}
            }
            index += 1;
            // A header is a handful of tokens; anything longer is not one,
            // and the scan must not walk the rest of the file.
            if index > 64 {
                return false;
            }
        }
    }

    /// Parses `cohort [(header)] { ... }` and returns its desugaring.
    pub(crate) fn parse_cohort_expr(&mut self) -> ExprKind {
        let start = self.peek_span();
        self.bump(); // `cohort`
        let header = if self.at_punct(Punct::LParen) {
            self.parse_cohort_header()
        } else {
            CohortHeader::default()
        };
        if !self.expect_punct(Punct::LBrace, "to open the `cohort` block") {
            return ExprKind::Error;
        }
        let mut block = self.with_struct_literals_allowed(Self::parse_block_body);
        let mut stmts = Vec::with_capacity(block.stmts.len() + 3);
        stmts.push(self.runtime_call_stmt(
            "cohort_push",
            &[header.policy, header.timeout_ms, header.context],
            start,
        ));
        stmts.push(Stmt::new(
            self.alloc_id(),
            start,
            StmtKind::Defer(Box::new(self.runtime_call("cohort_pop", &[], start))),
        ));
        stmts.append(&mut block.stmts);
        // The body's own tail value has nowhere to go: the block's value
        // is the cohort's outcome. Demote it to a statement, as `arena`
        // does with the value that would outlive its region.
        if let Some(tail) = block.tail.take() {
            let span = tail.span;
            stmts.push(Stmt::new(
                self.alloc_id(),
                span,
                StmtKind::Expr {
                    expr: tail,
                    has_semi: true,
                },
            ));
        }
        block.stmts = stmts;
        block.tail = Some(Box::new(self.runtime_call("cohort_join", &[], start)));
        block.kind = gossamer_ast::BlockKind::Cohort;
        ExprKind::Block(block)
    }

    /// Parses the named arguments of a cohort header. Every one is
    /// optional and each is a compile-time constant, so the desugared
    /// call carries plain integers and the runtime needs no header type.
    fn parse_cohort_header(&mut self) -> CohortHeader {
        let mut header = CohortHeader::default();
        self.bump(); // `(`
        while !self.at_punct(Punct::RParen) && !self.at_eof() {
            let name_span = self.peek_span();
            let name = self.slice(name_span).to_string();
            if !matches!(self.peek().kind, TokenKind::Ident) {
                self.record(
                    ParseError::unexpected("a cohort setting name", self.peek_text()),
                    name_span,
                );
                break;
            }
            self.bump();
            if !self.expect_punct(Punct::Colon, "after a cohort setting name") {
                break;
            }
            match name.as_str() {
                "policy" => header.policy = self.parse_cohort_enum_arg("Policy", POLICIES),
                "context" => header.context = self.parse_cohort_enum_arg("Context", CONTEXTS),
                "timeout" => header.timeout_ms = self.parse_cohort_int_arg(),
                _ => {
                    self.record(
                        ParseError::unexpected(
                            "one of `policy`, `timeout`, or `context`",
                            format!("`{name}`"),
                        ),
                        name_span,
                    );
                    self.skip_cohort_header_value();
                }
            }
            if !self.eat_punct(Punct::Comma) && !self.newline_before_peek() {
                break;
            }
        }
        self.expect_punct(Punct::RParen, "to close the `cohort` header");
        header
    }

    /// Parses `Policy::FailFast` / `Context::Isolated` and folds it to
    /// the runtime's number for that variant.
    fn parse_cohort_enum_arg(&mut self, enum_name: &str, variants: &[(&str, i64)]) -> i64 {
        let span = self.peek_span();
        let mut text = String::new();
        while matches!(self.peek().kind, TokenKind::Ident) || self.at_punct(Punct::ColonColon) {
            text.push_str(self.slice(self.peek_span()));
            self.bump();
        }
        let variant = text.rsplit("::").next().unwrap_or_default().to_string();
        if let Some((_, value)) = variants.iter().find(|(name, _)| *name == variant) {
            return *value;
        }
        let expected = variants
            .iter()
            .map(|(name, _)| format!("`{enum_name}::{name}`"))
            .collect::<Vec<_>>()
            .join(", ");
        self.record(
            ParseError::unexpected(
                &expected,
                if text.is_empty() {
                    "nothing".to_string()
                } else {
                    text
                },
            ),
            span,
        );
        0
    }

    /// Parses a cohort header's millisecond count.
    fn parse_cohort_int_arg(&mut self) -> i64 {
        let span = self.peek_span();
        let text = self.slice(span).replace('_', "");
        if matches!(self.peek().kind, TokenKind::IntLit)
            && let Ok(value) = text.parse::<i64>()
            && value >= 0
        {
            self.bump();
            return value;
        }
        self.record(
            ParseError::unexpected("a non-negative millisecond literal", self.peek_text()),
            span,
        );
        self.skip_cohort_header_value();
        0
    }

    /// Steps over a header value the parser could not read, so the rest
    /// of the header still reports its own problems.
    fn skip_cohort_header_value(&mut self) {
        while !self.at_punct(Punct::Comma)
            && !self.at_punct(Punct::RParen)
            && !self.at_eof()
            && !self.newline_before_peek()
        {
            self.bump();
        }
    }

    /// `runtime::<name>(args...)` as an expression.
    fn runtime_call(&mut self, name: &str, args: &[i64], span: Span) -> Expr {
        let path = Expr::new(
            self.alloc_id(),
            span,
            ExprKind::Path(PathExpr::from_names(["runtime", name])),
        );
        let args = args
            .iter()
            .map(|value| {
                Expr::new(
                    self.alloc_id(),
                    span,
                    ExprKind::Literal(Literal::Int(value.to_string())),
                )
            })
            .collect();
        Expr::new(
            self.alloc_id(),
            span,
            ExprKind::Call {
                callee: Box::new(path),
                args,
            },
        )
    }

    /// `runtime::<name>(args...)` as a statement.
    fn runtime_call_stmt(&mut self, name: &str, args: &[i64], span: Span) -> Stmt {
        let call = self.runtime_call(name, args, span);
        Stmt::new(
            self.alloc_id(),
            span,
            StmtKind::Expr {
                expr: Box::new(call),
                has_semi: true,
            },
        )
    }
}
