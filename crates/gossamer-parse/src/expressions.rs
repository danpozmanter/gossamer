//! Expression parsing - Pratt-style precedence climbing driven by
//! SPEC §4.7 plus hand-written prefix and postfix handlers.

#![forbid(unsafe_code)]

use gossamer_ast::visitor::{
    VisitorMut, walk_expr_mut, walk_item_mut, walk_pattern_mut, walk_stmt_mut, walk_type_mut,
};
use gossamer_ast::{
    ArrayExpr, AssignOp, BinaryOp, Block, ClosureParam, Expr, ExprKind, FieldSelector, Ident, Item,
    Label, Literal, MacroCall, MacroDelim, MatchArm, Mutability, NodeIdGenerator, PathExpr,
    PathSegment, Pattern, PatternKind, RangeKind, Stmt, StmtKind, StructExprField, Type, UnaryOp,
};
use gossamer_lex::{Keyword, Punct, Span, TokenKind};

use crate::diagnostic::ParseError;
use crate::parser::Parser;
use crate::patterns::{
    byte_literal_value, byte_string_literal_value, char_literal_value, string_literal_value,
};

/// Precedence level strictly stronger than any binary operator.
const PREC_BELOW_ASSIGN: u8 = 17;

/// Range (`..` / `..=`) precedence: looser than every arithmetic and
/// logical operator (`i * i..n` is `(i * i)..n`), tighter than `|>`
/// (`0..n |> f` pipes the whole range).
const RANGE_PREC: u8 = 14;

/// Maximum precedence for a single clause inside an `if`/`while`
/// condition chain. Equal to `&&`'s precedence so a clause stops at
/// the next `&&` separator (and rejects an unparenthesised `||`,
/// which binds looser than `&&`).
const COND_CLAUSE_PREC: u8 = BinaryOp::And.precedence();

/// A single clause in an `if`/`while` condition chain.
enum CondClause {
    /// `let PAT = SCRUTINEE`; the binding is in scope for later
    /// clauses and the branch body.
    Let {
        /// Pattern that must match for the clause to succeed.
        pattern: Pattern,
        /// Value tested against the pattern.
        scrutinee: Expr,
    },
    /// A boolean sub-expression that must evaluate to `true`.
    Bool(Expr),
}

/// The parsed head of an `if`/`while`: either an ordinary boolean
/// expression or a chain of `&&`-joined clauses with at least one
/// `let` binding.
enum Condition {
    /// No `let` clause; a normal boolean expression.
    Plain(Expr),
    /// One or more `&&`-joined clauses, at least one of which binds.
    Chain(Vec<CondClause>),
}

impl Parser<'_> {
    /// Parses a full expression, including assignment at statement position.
    pub(crate) fn parse_expr(&mut self) -> Expr {
        self.parse_expr_with_prec(PREC_BELOW_ASSIGN, true)
    }

    /// Parses an expression that is not allowed to bind assignment at
    /// its top level (e.g. argument positions).
    pub(crate) fn parse_expr_no_assign(&mut self) -> Expr {
        self.parse_expr_with_prec(PREC_BELOW_ASSIGN, false)
    }

    /// Precedence-climbing core used by `parse_expr`.
    fn parse_expr_with_prec(&mut self, max_prec: u8, allow_assign: bool) -> Expr {
        let entry_span = self.peek_span();
        if self.enter_recursion(entry_span).is_err() {
            let id = self.alloc_id();
            // Skip one token so the outer driver cannot loop on the same
            // input forever after the limit fires.
            if !self.at_eof() {
                self.bump();
            }
            return Expr::new(id, entry_span, ExprKind::Error);
        }
        let result = self.parse_expr_with_prec_inner(max_prec, allow_assign);
        self.leave_recursion();
        result
    }

    fn parse_expr_with_prec_inner(&mut self, max_prec: u8, allow_assign: bool) -> Expr {
        let lhs = self.parse_prefix();
        self.continue_binary(lhs, max_prec, allow_assign)
    }

    /// Resumes Pratt-style precedence climbing from an already-parsed
    /// left-hand side. Used both by the core driver and by condition
    /// parsing, where a `&&`-chain is parsed clause-by-clause and then
    /// rejoined into a normal expression when no `let` clause appears.
    fn continue_binary(&mut self, mut lhs: Expr, max_prec: u8, allow_assign: bool) -> Expr {
        loop {
            if allow_assign && self.peek_assign_op().is_some() {
                lhs = self.parse_assignment(lhs);
                break;
            }
            if let Some(op) = self.peek_binary_op() {
                let precedence = op.precedence();
                if precedence >= max_prec {
                    break;
                }
                if op == BinaryOp::BitOr && self.in_pattern_pipe() {
                    break;
                }
                if is_unary_startable(op) && self.newline_before_peek() {
                    break;
                }
                self.bump();
                if is_non_associative_compare(op)
                    && self.peek_matches_compare_after_parse(op, precedence)
                {
                    self.record(
                        ParseError::NonAssociativeCompare {
                            op: op.as_str().to_string(),
                        },
                        lhs.span,
                    );
                }
                let rhs = self.parse_expr_with_prec(precedence, false);
                if op == BinaryOp::PipeGt {
                    lhs = self.validate_pipe_rhs(lhs, rhs);
                    continue;
                }
                let span = self.join(lhs.span, rhs.span);
                let id = self.alloc_id();
                lhs = Expr::new(
                    id,
                    span,
                    ExprKind::Binary {
                        op,
                        lhs: Box::new(lhs),
                        rhs: Box::new(rhs),
                    },
                );
                continue;
            }
            if self.peek_range_op().is_some() {
                // Range binds looser than every arithmetic / logical
                // operator and tighter than `|>` (SPEC §4.7), so
                // `i * i..n` is `(i * i)..n` and `0..n |> f` pipes the
                // whole range. Inside a tighter operand parse the `..`
                // belongs to the enclosing level.
                if RANGE_PREC >= max_prec {
                    break;
                }
                let range = self.parse_range_infix(lhs);
                lhs = range;
                continue;
            }
            if self.at_keyword(Keyword::As) {
                self.bump();
                let ty = self.parse_type();
                let span = self.join(lhs.span, ty.span);
                let id = self.alloc_id();
                lhs = Expr::new(
                    id,
                    span,
                    ExprKind::Cast {
                        value: Box::new(lhs),
                        ty: Box::new(ty),
                    },
                );
                continue;
            }
            break;
        }
        lhs
    }

    fn peek_matches_compare_after_parse(&self, _op: BinaryOp, _precedence: u8) -> bool {
        false
    }

    fn peek_binary_op(&self) -> Option<BinaryOp> {
        use Punct::{
            Amp, AmpAmp, Caret, EqEq, Gt, GtEq, Lt, LtEq, Minus, NotEq, Percent, Pipe, PipeGt,
            PipePipe, Plus, ShiftL, ShiftR, Slash, Star,
        };
        let TokenKind::Punct(punct) = self.peek().kind else {
            return None;
        };
        Some(match punct {
            Star => BinaryOp::Mul,
            Slash => BinaryOp::Div,
            Percent => BinaryOp::Rem,
            Plus => BinaryOp::Add,
            Minus => BinaryOp::Sub,
            ShiftL => BinaryOp::Shl,
            ShiftR => BinaryOp::Shr,
            Amp => BinaryOp::BitAnd,
            Caret => BinaryOp::BitXor,
            Pipe => BinaryOp::BitOr,
            EqEq => BinaryOp::Eq,
            NotEq => BinaryOp::Ne,
            Lt => BinaryOp::Lt,
            LtEq => BinaryOp::Le,
            Gt => BinaryOp::Gt,
            GtEq => BinaryOp::Ge,
            AmpAmp => BinaryOp::And,
            PipePipe => BinaryOp::Or,
            PipeGt => BinaryOp::PipeGt,
            _ => return None,
        })
    }

    fn peek_range_op(&self) -> Option<RangeKind> {
        if self.at_punct(Punct::DotDotEq) {
            return Some(RangeKind::Inclusive);
        }
        if self.at_punct(Punct::DotDot) {
            return Some(RangeKind::Exclusive);
        }
        None
    }

    fn parse_range_infix(&mut self, lhs: Expr) -> Expr {
        let kind = if self.eat_punct(Punct::DotDotEq) {
            RangeKind::Inclusive
        } else {
            self.bump();
            RangeKind::Exclusive
        };
        let end = if is_expression_start(self) {
            Some(Box::new(self.parse_expr_with_prec(RANGE_PREC, false)))
        } else {
            None
        };
        let end_span = end.as_ref().map_or(self.last_span(), |expr| expr.span);
        let span = self.join(lhs.span, end_span);
        let id = self.alloc_id();
        Expr::new(
            id,
            span,
            ExprKind::Range {
                start: Some(Box::new(lhs)),
                end,
                kind,
            },
        )
    }

    fn peek_assign_op(&self) -> Option<AssignOp> {
        let TokenKind::Punct(punct) = self.peek().kind else {
            return None;
        };
        Some(match punct {
            Punct::Eq => AssignOp::Assign,
            Punct::PlusEq => AssignOp::AddAssign,
            Punct::MinusEq => AssignOp::SubAssign,
            Punct::StarEq => AssignOp::MulAssign,
            Punct::SlashEq => AssignOp::DivAssign,
            Punct::PercentEq => AssignOp::RemAssign,
            Punct::AmpEq => AssignOp::BitAndAssign,
            Punct::PipeEq => AssignOp::BitOrAssign,
            Punct::CaretEq => AssignOp::BitXorAssign,
            Punct::ShiftLEq => AssignOp::ShlAssign,
            Punct::ShiftREq => AssignOp::ShrAssign,
            _ => return None,
        })
    }

    fn parse_assignment(&mut self, place: Expr) -> Expr {
        let Some(op) = self.peek_assign_op() else {
            return place;
        };
        self.bump();
        let value = self.parse_expr_with_prec(PREC_BELOW_ASSIGN, false);
        let span = self.join(place.span, value.span);
        let id = self.alloc_id();
        Expr::new(
            id,
            span,
            ExprKind::Assign {
                op,
                place: Box::new(place),
                value: Box::new(value),
            },
        )
    }

    fn validate_pipe_rhs(&mut self, lhs: Expr, mut rhs: Expr) -> Expr {
        // `_`-headed RHS threads the piped value in as the receiver, so the
        // resolver only ever sees an ordinary method/field/index expression -
        // no `_` placeholder escapes into later passes.
        //   x |> _.trim          => x.trim()      (bare ident => nullary method)
        //   x |> _.replace(a, b) => x.replace(a, b)
        //   x |> _.0             => x.0           (tuple index)
        //   x |> _[i]            => x[i]
        //   x |> _               => x
        // A direct `_.ident` with no parens is a nullary method call, not a
        // field access (bare `s.trim` is a field access elsewhere and would
        // not resolve). Field access through a pipe keeps the closure idiom
        // (`x |> |v| v.field`).
        if let ExprKind::FieldAccess { receiver, field } = &rhs.kind {
            if let (ExprKind::Path(p), FieldSelector::Named(name)) = (&receiver.kind, field) {
                if is_pipe_placeholder(p) {
                    let span = self.join(lhs.span, rhs.span);
                    let id = self.alloc_id();
                    return Expr::new(
                        id,
                        span,
                        ExprKind::MethodCall {
                            receiver: Box::new(lhs),
                            name: name.clone(),
                            generics: Vec::new(),
                            args: Vec::new(),
                        },
                    );
                }
            }
        }
        if has_pipe_dotdot_placeholder(&rhs) {
            self.record(ParseError::PipeDotDotPlaceholder, rhs.span);
        }
        let lhs_span = lhs.span;
        let mut piped = Some(lhs);
        if substitute_pipe_placeholder(&mut rhs, &mut piped) {
            if contains_pipe_placeholder(&rhs) {
                self.record(ParseError::PipePlaceholderInvalid, rhs.span);
            }
            rhs.span = self.join(lhs_span, rhs.span);
            return rhs;
        }
        match substitute_pipe_argument_placeholder(&mut rhs, &mut piped) {
            PipeArgumentPlaceholder::None => {}
            PipeArgumentPlaceholder::Substituted => {
                if contains_pipe_placeholder(&rhs) {
                    self.record(ParseError::PipePlaceholderInvalid, rhs.span);
                }
                rhs.span = self.join(lhs_span, rhs.span);
                return rhs;
            }
            PipeArgumentPlaceholder::Invalid => {
                self.record(ParseError::PipePlaceholderInvalid, rhs.span);
            }
        }
        let lhs = piped.take().expect("piped value left unconsumed");
        let rhs_span = rhs.span;
        let valid = matches!(
            rhs.kind,
            ExprKind::Path(_)
                | ExprKind::Call { .. }
                | ExprKind::MethodCall { .. }
                | ExprKind::MacroCall(_)
                | ExprKind::Closure { .. }
        );
        if !valid {
            self.record(ParseError::PipeRhsInvalid, rhs_span);
        }
        let span = self.join(lhs.span, rhs.span);
        let id = self.alloc_id();
        Expr::new(
            id,
            span,
            ExprKind::Binary {
                op: BinaryOp::PipeGt,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
            },
        )
    }

    /// Parses a prefix (primary + unary) expression.
    fn parse_prefix(&mut self) -> Expr {
        let entry_span = self.peek_span();
        if self.enter_recursion(entry_span).is_err() {
            let id = self.alloc_id();
            if !self.at_eof() {
                self.bump();
            }
            return Expr::new(id, entry_span, ExprKind::Error);
        }
        let result = self.parse_prefix_inner();
        self.leave_recursion();
        result
    }

    fn parse_prefix_inner(&mut self) -> Expr {
        if self.peek_range_op().is_some() {
            return self.parse_open_range_prefix();
        }
        if let Some(prefix_op) = self.peek_unary_op() {
            let op_span = self.peek_span();
            self.bump();
            let mutability_consumed =
                prefix_op == UnaryOp::RefShared && self.eat_keyword(Keyword::Mut);
            let actual_op = if mutability_consumed {
                UnaryOp::RefMut
            } else {
                prefix_op
            };
            let operand = self.parse_prefix();
            let span = self.join(op_span, operand.span);
            let id = self.alloc_id();
            return Expr::new(
                id,
                span,
                ExprKind::Unary {
                    op: actual_op,
                    operand: Box::new(operand),
                },
            );
        }
        self.parse_postfix()
    }

    /// Parses a range with an omitted lower bound: `..end`, `..=end`, or
    /// `..`. The normal infix path handles an omitted upper bound.
    fn parse_open_range_prefix(&mut self) -> Expr {
        let start_span = self.peek_span();
        let kind = if self.eat_punct(Punct::DotDotEq) {
            RangeKind::Inclusive
        } else {
            self.bump();
            RangeKind::Exclusive
        };
        let end = if is_expression_start(self) {
            Some(Box::new(self.parse_expr_with_prec(RANGE_PREC, false)))
        } else {
            None
        };
        let end_span = end.as_ref().map_or(self.last_span(), |expr| expr.span);
        let id = self.alloc_id();
        Expr::new(
            id,
            self.join(start_span, end_span),
            ExprKind::Range {
                start: None,
                end,
                kind,
            },
        )
    }

    fn peek_unary_op(&self) -> Option<UnaryOp> {
        let TokenKind::Punct(punct) = self.peek().kind else {
            return None;
        };
        Some(match punct {
            Punct::Minus => UnaryOp::Neg,
            Punct::Bang => UnaryOp::Not,
            Punct::Amp => UnaryOp::RefShared,
            Punct::Star => UnaryOp::Deref,
            _ => return None,
        })
    }

    fn parse_postfix(&mut self) -> Expr {
        let mut primary = self.parse_primary();
        loop {
            if self.at_punct(Punct::Dot) {
                primary = self.parse_dot_suffix(primary);
                continue;
            }
            if self.at_punct(Punct::LParen) {
                // A `(` on the next source line never attaches to the
                // previous expression. Without this break, a function
                // body like `for {...}\n(a, 7.0)` parses as one
                // expression - calling the for-loop's `()` result with
                // `(a, 7.0)` as args - instead of two statements.
                // Mirrors the same statement-boundary rule already
                // applied to `&` / `*` / `-` in `parse_expr_with_prec`.
                if self.newline_before_peek() {
                    break;
                }
                primary = self.parse_call_suffix(primary);
                continue;
            }
            if self.at_punct(Punct::LBracket) {
                // Same as `(` above: a `[` on the next line opens a
                // fresh array literal, not an index into the previous
                // expression.
                if self.newline_before_peek() {
                    break;
                }
                primary = self.parse_index_suffix(primary);
                continue;
            }
            if self.at_punct(Punct::Question) {
                let q_span = self.peek_span();
                self.bump();
                let span = self.join(primary.span, q_span);
                let id = self.alloc_id();
                primary = Expr::new(id, span, ExprKind::Try(Box::new(primary)));
                continue;
            }
            break;
        }
        primary
    }

    fn parse_dot_suffix(&mut self, receiver: Expr) -> Expr {
        self.bump();
        let token = self.peek();
        let start_span = receiver.span;
        match token.kind {
            TokenKind::IntLit => {
                self.bump();
                let text = self.slice(token.span);
                let index = text.parse::<u32>().unwrap_or_else(|_| {
                    self.record(ParseError::InvalidTupleIndex, token.span);
                    0
                });
                let span = self.join(start_span, token.span);
                let id = self.alloc_id();
                Expr::new(
                    id,
                    span,
                    ExprKind::FieldAccess {
                        receiver: Box::new(receiver),
                        field: FieldSelector::Index(index),
                    },
                )
            }
            TokenKind::Ident => {
                self.bump();
                let name = Ident::new(self.slice(token.span));
                self.parse_method_or_field(receiver, name, token.span)
            }
            TokenKind::Keyword(Keyword::Await) => {
                self.bump();
                let name = Ident::new("await");
                self.parse_method_or_field(receiver, name, token.span)
            }
            _ => {
                self.record(
                    ParseError::Unexpected {
                        expected: "field or method name after `.`".to_string(),
                        found: self.peek_text(),
                    },
                    token.span,
                );
                receiver
            }
        }
    }

    fn parse_method_or_field(&mut self, receiver: Expr, name: Ident, name_span: Span) -> Expr {
        let generics = if self.at_punct(Punct::ColonColon)
            && self.peek_nth(1).kind == TokenKind::Punct(Punct::Lt)
        {
            self.bump();
            let checkpoint = self.tokens.checkpoint();
            self.bump();
            let args = self.parse_generic_args_in_turbofish();
            if args.is_empty() {
                self.tokens.rewind(checkpoint);
                Vec::new()
            } else {
                args
            }
        } else {
            Vec::new()
        };
        if self.at_punct(Punct::LParen) {
            self.bump();
            let args = self.parse_call_args();
            let end_span = self.last_span();
            let span = self.join(receiver.span, end_span);
            let id = self.alloc_id();
            return Expr::new(
                id,
                span,
                ExprKind::MethodCall {
                    receiver: Box::new(receiver),
                    name,
                    generics,
                    args,
                },
            );
        }
        let span = self.join(receiver.span, name_span);
        let id = self.alloc_id();
        Expr::new(
            id,
            span,
            ExprKind::FieldAccess {
                receiver: Box::new(receiver),
                field: FieldSelector::Named(name),
            },
        )
    }

    fn parse_generic_args_in_turbofish(&mut self) -> Vec<gossamer_ast::GenericArg> {
        let mut args = Vec::new();
        while !self.at_close_angle() && !self.at_eof() {
            args.push(self.parse_generic_arg());
            if !self.eat_punct(Punct::Comma) {
                break;
            }
        }
        self.expect_close_angle("to close turbofish generics");
        args
    }

    fn parse_call_suffix(&mut self, callee: Expr) -> Expr {
        self.bump();
        let args = self.parse_call_args();
        let end_span = self.last_span();
        let span = self.join(callee.span, end_span);
        // 0.7.0: `errors::newf(fmt, args…)` is a format-shaped sibling
        // of `errors::new`. Rewrite at parse time to
        // `errors::new(format!(fmt, args…))` so the same parse-time
        // template expansion that powers `format!` produces a single
        // `__concat`-based String - keeps all three tiers identical
        // (the VM gets the same lowered call shape compiled-mode
        // sees, no separate variadic runtime helper required).
        if is_errors_newf_path(&callee) {
            return self.rewrite_errors_newf(span, args);
        }
        let id = self.alloc_id();
        Expr::new(
            id,
            span,
            ExprKind::Call {
                callee: Box::new(callee),
                args,
            },
        )
    }

    fn rewrite_errors_newf(&mut self, span: gossamer_lex::Span, args: Vec<Expr>) -> Expr {
        let concat = self.expand_format_macro("format", args);
        let concat_id = self.alloc_id();
        let concat_expr = Expr::new(concat_id, span, concat);
        // Build a real two-segment `errors::new` path so the
        // resolver picks up the module-qualified import instead
        // of trying to look up a single-segment "errors::new"
        // identifier (which doesn't exist).
        let new_path = PathExpr::from_names(["errors", "new"]);
        let callee_id = self.alloc_id();
        let callee = Expr::new(callee_id, span, ExprKind::Path(new_path));
        let call_id = self.alloc_id();
        Expr::new(
            call_id,
            span,
            ExprKind::Call {
                callee: Box::new(callee),
                args: vec![concat_expr],
            },
        )
    }

    fn parse_index_suffix(&mut self, base: Expr) -> Expr {
        self.bump();
        let index = self.with_struct_literals_allowed(Self::parse_expr_no_assign);
        self.expect_punct(Punct::RBracket, "to close index expression");
        let end_span = self.last_span();
        let span = self.join(base.span, end_span);
        let id = self.alloc_id();
        Expr::new(
            id,
            span,
            ExprKind::Index {
                base: Box::new(base),
                index: Box::new(index),
            },
        )
    }

    pub(crate) fn parse_call_args(&mut self) -> Vec<Expr> {
        self.with_struct_literals_allowed(|p| {
            let mut args = Vec::new();
            while !p.at_punct(Punct::RParen) && !p.at_eof() {
                if p.at_punct(Punct::DotDotDot) {
                    p.bump();
                    continue;
                }
                args.push(p.parse_expr_no_assign());
                if !p.eat_punct(Punct::Comma) {
                    break;
                }
            }
            p.expect_punct(Punct::RParen, "to close argument list");
            args
        })
    }

    fn parse_primary(&mut self) -> Expr {
        let start_span = self.peek_span();
        let kind = self.parse_primary_kind();
        let end_span = self.last_span();
        let span = self.join(start_span, end_span);
        let id = self.alloc_id();
        Expr::new(id, span, kind)
    }

    fn parse_primary_kind(&mut self) -> ExprKind {
        if self.eat_punct(Punct::LParen) {
            return self.parse_paren_or_tuple();
        }
        if self.eat_punct(Punct::LBracket) {
            return self.parse_array_expr();
        }
        if self.eat_punct(Punct::LBrace) {
            return ExprKind::Block(self.parse_block_body());
        }
        if let Some(literal) = self.try_parse_literal() {
            return ExprKind::Literal(literal);
        }
        if self.at_keyword(Keyword::If) {
            return self.parse_if_expr();
        }
        if self.at_keyword(Keyword::Match) {
            return self.parse_match_expr();
        }
        if self.at_keyword(Keyword::Loop) {
            return self.parse_loop_expr(None);
        }
        if self.at_keyword(Keyword::While) {
            return self.parse_while_expr(None);
        }
        if self.at_keyword(Keyword::For) {
            return self.parse_for_expr(None);
        }
        if self.at_keyword(Keyword::Unsafe) {
            self.bump();
            self.expect_punct(Punct::LBrace, "to open `unsafe` block");
            return ExprKind::Unsafe(self.parse_block_body());
        }
        if self.at_keyword(Keyword::Comptime) {
            self.bump();
            self.expect_punct(Punct::LBrace, "to open `comptime` block");
            let mut block = self.parse_block_body();
            block.is_comptime = true;
            return ExprKind::Block(block);
        }
        if self.at_keyword(Keyword::Return) {
            self.bump();
            if !is_expression_start(self) || at_block_end(self) {
                return ExprKind::Return(None);
            }
            let value = self.parse_expr_no_assign();
            return ExprKind::Return(Some(Box::new(value)));
        }
        if self.at_keyword(Keyword::Break) {
            return self.parse_break_expr();
        }
        if self.at_keyword(Keyword::Continue) {
            return self.parse_continue_expr();
        }
        if self.at_keyword(Keyword::Go) {
            self.bump();
            let value = self.parse_expr_no_assign();
            return ExprKind::Go(Box::new(value));
        }
        if self.at_keyword(Keyword::Select) {
            return self.parse_select_expr();
        }
        if self.at_punct(Punct::Pipe) || self.at_punct(Punct::PipePipe) {
            return self.parse_closure_expr();
        }
        if self.at_keyword(Keyword::Fn) {
            return self.parse_fn_closure_expr();
        }
        if self.at_label_start() {
            return self.parse_labelled_loop();
        }
        if self.is_path_expr_start() {
            return self.parse_path_expr_or_struct();
        }
        self.record(
            ParseError::Unexpected {
                expected: "expression".to_string(),
                found: self.peek_text(),
            },
            self.peek_span(),
        );
        self.bump();
        ExprKind::Error
    }

    fn parse_paren_or_tuple(&mut self) -> ExprKind {
        self.with_struct_literals_allowed(|p| {
            if p.eat_punct(Punct::RParen) {
                return ExprKind::Literal(Literal::Unit);
            }
            let first = p.parse_expr();
            if p.eat_punct(Punct::RParen) {
                return first.kind;
            }
            let mut elements = vec![first];
            while p.eat_punct(Punct::Comma) {
                if p.at_punct(Punct::RParen) {
                    break;
                }
                elements.push(p.parse_expr());
            }
            p.expect_punct(Punct::RParen, "to close tuple expression");
            ExprKind::Tuple(elements)
        })
    }

    fn parse_array_expr(&mut self) -> ExprKind {
        self.with_struct_literals_allowed(Self::parse_array_expr_inner)
    }

    fn parse_array_expr_inner(&mut self) -> ExprKind {
        if self.eat_punct(Punct::RBracket) {
            return ExprKind::Array(ArrayExpr::List(Vec::new()));
        }
        let first = self.parse_expr_no_assign();
        if self.eat_punct(Punct::Semi) {
            let count = self.parse_expr_no_assign();
            self.expect_punct(Punct::RBracket, "to close array expression");
            return ExprKind::Array(ArrayExpr::Repeat {
                value: Box::new(first),
                count: Box::new(count),
            });
        }
        let mut elements = vec![first];
        while self.eat_punct(Punct::Comma) {
            if self.at_punct(Punct::RBracket) {
                break;
            }
            elements.push(self.parse_expr_no_assign());
        }
        self.expect_punct(Punct::RBracket, "to close array expression");
        ExprKind::Array(ArrayExpr::List(elements))
    }

    fn parse_if_expr(&mut self) -> ExprKind {
        self.bump();
        self.enter_no_struct();
        let condition = self.parse_condition();
        self.leave_no_struct();
        self.expect_punct(Punct::LBrace, "to open `if` branch");
        let then_block_span_start = self.last_span();
        let then_block = self.parse_block_body();
        let then_span = self.join(then_block_span_start, self.last_span());
        let then_expr = Expr::new(self.alloc_id(), then_span, ExprKind::Block(then_block));
        let else_branch = if self.eat_keyword(Keyword::Else) {
            if self.at_keyword(Keyword::If) {
                let start = self.peek_span();
                let kind = self.parse_if_expr();
                let end = self.last_span();
                let span = self.join(start, end);
                let id = self.alloc_id();
                Some(Box::new(Expr::new(id, span, kind)))
            } else {
                self.expect_punct(Punct::LBrace, "to open `else` branch");
                let start = self.last_span();
                let block = self.parse_block_body();
                let span = self.join(start, self.last_span());
                let id = self.alloc_id();
                Some(Box::new(Expr::new(id, span, ExprKind::Block(block))))
            }
        } else {
            None
        };
        match condition {
            Condition::Plain(condition) => ExprKind::If {
                condition: Box::new(condition),
                then_branch: Box::new(then_expr),
                else_branch,
            },
            Condition::Chain(clauses) => self.desugar_if_chain(clauses, then_expr, else_branch),
        }
    }

    /// Parses an `if`/`while` condition, recognising `let`-chains.
    ///
    /// Each clause is parsed at [`COND_CLAUSE_PREC`] so it stops at the
    /// next `&&`. When no clause binds a `let`, the `&&` chain is folded
    /// back into a single boolean expression and full-precedence parsing
    /// resumes - so plain `&&`/`||` conditions are unchanged.
    fn parse_condition(&mut self) -> Condition {
        let mut clauses = vec![self.parse_cond_clause()];
        while self.at_punct(Punct::AmpAmp) {
            self.bump();
            clauses.push(self.parse_cond_clause());
        }
        let has_let = clauses
            .iter()
            .any(|clause| matches!(clause, CondClause::Let { .. }));
        if has_let {
            if self.peek_binary_op().is_some() {
                self.record(
                    ParseError::Unexpected {
                        expected: "`&&`; `let` in a condition can only be chained with `&&`"
                            .to_string(),
                        found: self.peek_text(),
                    },
                    self.peek_span(),
                );
                // Consume the trailing operand so the branch-opening `{`
                // is still found and the parse does not cascade.
                let stub = Expr::new(self.alloc_id(), self.peek_span(), ExprKind::Error);
                let _ = self.continue_binary(stub, PREC_BELOW_ASSIGN, false);
            }
            return Condition::Chain(clauses);
        }
        let mut iter = clauses.into_iter();
        let Some(CondClause::Bool(mut expr)) = iter.next() else {
            unreachable!("a condition without a `let` clause is all boolean");
        };
        for clause in iter {
            let CondClause::Bool(rhs) = clause else {
                unreachable!("a condition without a `let` clause is all boolean");
            };
            let span = self.join(expr.span, rhs.span);
            expr = Expr::new(
                self.alloc_id(),
                span,
                ExprKind::Binary {
                    op: BinaryOp::And,
                    lhs: Box::new(expr),
                    rhs: Box::new(rhs),
                },
            );
        }
        Condition::Plain(self.continue_binary(expr, PREC_BELOW_ASSIGN, false))
    }

    /// Parses one clause of a condition chain: either `let PAT = EXPR`
    /// or a boolean expression, stopping at the next `&&`.
    fn parse_cond_clause(&mut self) -> CondClause {
        if self.at_keyword(Keyword::Let) {
            self.bump();
            let pattern = self.parse_pattern();
            self.expect_punct(Punct::Eq, "after `let` pattern in condition");
            let scrutinee = self.parse_expr_with_prec(COND_CLAUSE_PREC, false);
            CondClause::Let { pattern, scrutinee }
        } else {
            CondClause::Bool(self.parse_expr_with_prec(COND_CLAUSE_PREC, false))
        }
    }

    /// Lowers an `if` let-chain to nested `match`/`if` so every tier
    /// sees only constructs it already handles. The chain succeeds only
    /// when every `let` matches and every boolean is `true`; any failure
    /// runs the `else` branch (an empty block when none was written).
    fn desugar_if_chain(
        &mut self,
        clauses: Vec<CondClause>,
        then_expr: Expr,
        else_branch: Option<Box<Expr>>,
    ) -> ExprKind {
        let else_template = else_branch.map(|boxed| *boxed);
        let mut acc = then_expr;
        for clause in clauses.into_iter().rev() {
            acc = match clause {
                CondClause::Bool(cond) => {
                    let fail = self.make_chain_else(else_template.as_ref());
                    self.make_if_clause(cond, acc, fail)
                }
                CondClause::Let { pattern, scrutinee } if is_irrefutable_binding(&pattern) => {
                    self.make_block_let(pattern, scrutinee, acc)
                }
                CondClause::Let { pattern, scrutinee } => {
                    let fail = self.make_chain_else(else_template.as_ref());
                    self.make_match_clause(pattern, scrutinee, acc, fail)
                }
            };
        }
        acc.kind
    }

    /// `if cond { acc } else { fail }`.
    fn make_if_clause(&mut self, cond: Expr, acc: Expr, fail: Expr) -> Expr {
        let span = self.join(cond.span, acc.span);
        let then_block = self.wrap_in_block(acc);
        let else_block = self.wrap_in_block(fail);
        Expr::new(
            self.alloc_id(),
            span,
            ExprKind::If {
                condition: Box::new(cond),
                then_branch: Box::new(then_block),
                else_branch: Some(Box::new(else_block)),
            },
        )
    }

    /// `match scrutinee { pattern => acc, _ => fail }` for a refutable
    /// `let` clause.
    fn make_match_clause(
        &mut self,
        pattern: Pattern,
        scrutinee: Expr,
        acc: Expr,
        fail: Expr,
    ) -> Expr {
        let span = self.join(scrutinee.span, acc.span);
        let wildcard = Pattern::new(self.alloc_id(), fail.span, PatternKind::Wildcard);
        Expr::new(
            self.alloc_id(),
            span,
            ExprKind::Match {
                scrutinee: Box::new(scrutinee),
                arms: vec![
                    MatchArm {
                        pattern,
                        guard: None,
                        body: acc,
                    },
                    MatchArm {
                        pattern: wildcard,
                        guard: None,
                        body: fail,
                    },
                ],
            },
        )
    }

    /// `{ let pattern = scrutinee; acc }` for an irrefutable `let`
    /// clause. The binding always succeeds, so the chain has no failure
    /// edge here and the bound value types through ordinary `let`
    /// inference.
    fn make_block_let(&mut self, pattern: Pattern, scrutinee: Expr, acc: Expr) -> Expr {
        let span = self.join(scrutinee.span, acc.span);
        let stmt_span = self.join(pattern.span, scrutinee.span);
        let let_stmt = Stmt::new(
            self.alloc_id(),
            stmt_span,
            StmtKind::Let {
                pattern,
                ty: None,
                init: Some(Box::new(scrutinee)),
            },
        );
        let block = Block {
            stmts: vec![let_stmt],
            tail: Some(Box::new(acc)),
            synthetic: true,
            is_arena: false,
            is_comptime: false,
        };
        Expr::new(self.alloc_id(), span, ExprKind::Block(block))
    }

    /// Builds a fresh `else`-branch expression for one failure site of a
    /// chain. A written `else` is deep-cloned with fresh node ids so each
    /// site is an independent subtree; a missing `else` yields unit.
    fn make_chain_else(&mut self, template: Option<&Expr>) -> Expr {
        if let Some(expr) = template {
            self.clone_with_fresh_ids(expr)
        } else {
            let span = self.last_span();
            Expr::new(self.alloc_id(), span, ExprKind::Block(Block::empty()))
        }
    }

    /// Builds an unlabelled `break` for a `while` chain's failure edge.
    /// It targets the synthetic loop that replaces the `while`, so a
    /// failed clause exits the loop.
    fn make_loop_break(&mut self, span: Span) -> Expr {
        Expr::new(
            self.alloc_id(),
            span,
            ExprKind::Break {
                label: None,
                value: None,
            },
        )
    }

    /// Wraps an expression in a synthetic block unless it already is one,
    /// so `if`-branch slots always hold a block.
    fn wrap_in_block(&mut self, expr: Expr) -> Expr {
        if matches!(expr.kind, ExprKind::Block(_)) {
            return expr;
        }
        let span = expr.span;
        let block = Block {
            stmts: Vec::new(),
            tail: Some(Box::new(expr)),
            synthetic: true,
            is_arena: false,
            is_comptime: false,
        };
        Expr::new(self.alloc_id(), span, ExprKind::Block(block))
    }

    /// Deep-clones an expression subtree, assigning every node a fresh
    /// id so the copy is independent for resolution and type-checking.
    fn clone_with_fresh_ids(&mut self, expr: &Expr) -> Expr {
        let mut cloned = expr.clone();
        let mut reassign = ReassignIds { ids: &mut self.ids };
        reassign.visit_expr(&mut cloned);
        cloned
    }

    fn parse_match_expr(&mut self) -> ExprKind {
        self.bump();
        self.enter_no_struct();
        let scrutinee = self.parse_expr_no_assign();
        self.leave_no_struct();
        self.expect_punct(Punct::LBrace, "to open `match` body");
        let mut arms = Vec::new();
        while !self.at_punct(Punct::RBrace) && !self.at_eof() {
            let pattern = self.parse_pattern();
            let guard = if self.eat_keyword(Keyword::If) {
                Some(self.parse_expr_no_assign())
            } else {
                None
            };
            self.expect_punct(Punct::FatArrow, "after match pattern");
            let body = self.parse_expr();
            let body_is_block = matches!(body.kind, ExprKind::Block(_));
            arms.push(gossamer_ast::MatchArm {
                pattern,
                guard,
                body,
            });
            let ate_comma = self.eat_punct(Punct::Comma);
            if !ate_comma && !body_is_block {
                break;
            }
        }
        self.expect_punct(Punct::RBrace, "to close `match` body");
        ExprKind::Match {
            scrutinee: Box::new(scrutinee),
            arms,
        }
    }

    fn parse_loop_expr(&mut self, label: Option<Label>) -> ExprKind {
        self.bump();
        self.expect_punct(Punct::LBrace, "to open loop body");
        let body = self.parse_block_body();
        let span = self.last_span();
        let id = self.alloc_id();
        let body_expr = Expr::new(id, span, ExprKind::Block(body));
        ExprKind::Loop {
            label,
            body: Box::new(body_expr),
        }
    }

    fn parse_while_expr(&mut self, label: Option<Label>) -> ExprKind {
        self.bump();
        self.enter_no_struct();
        let condition = self.parse_condition();
        self.leave_no_struct();
        self.expect_punct(Punct::LBrace, "to open `while` body");
        let body_start = self.last_span();
        let body_block = self.parse_block_body();
        let body_span = self.join(body_start, self.last_span());
        let body_expr = Expr::new(self.alloc_id(), body_span, ExprKind::Block(body_block));
        match condition {
            Condition::Plain(condition) => ExprKind::While {
                label,
                condition: Box::new(condition),
                body: Box::new(body_expr),
            },
            Condition::Chain(clauses) => self.desugar_while_chain(label, clauses, body_expr),
        }
    }

    /// Lowers a `while` let-chain to `loop { match/if ... }`. On success
    /// the body runs and the loop iterates; any failed clause `break`s
    /// out of the loop. The body's own `break`/`continue` target this
    /// loop, matching ordinary `while` semantics.
    fn desugar_while_chain(
        &mut self,
        label: Option<Label>,
        clauses: Vec<CondClause>,
        body_expr: Expr,
    ) -> ExprKind {
        let body_span = body_expr.span;
        let mut acc = body_expr;
        for clause in clauses.into_iter().rev() {
            acc = match clause {
                CondClause::Bool(cond) => {
                    let fail = self.make_loop_break(body_span);
                    self.make_if_clause(cond, acc, fail)
                }
                CondClause::Let { pattern, scrutinee } if is_irrefutable_binding(&pattern) => {
                    self.make_block_let(pattern, scrutinee, acc)
                }
                CondClause::Let { pattern, scrutinee } => {
                    let fail = self.make_loop_break(body_span);
                    self.make_match_clause(pattern, scrutinee, acc, fail)
                }
            };
        }
        let loop_body_block = Block {
            stmts: Vec::new(),
            tail: Some(Box::new(acc)),
            synthetic: true,
            is_arena: false,
            is_comptime: false,
        };
        let loop_body = Expr::new(self.alloc_id(), body_span, ExprKind::Block(loop_body_block));
        ExprKind::Loop {
            label,
            body: Box::new(loop_body),
        }
    }

    fn parse_for_expr(&mut self, label: Option<Label>) -> ExprKind {
        self.bump();
        let pattern = self.parse_pattern();
        self.expect_keyword(Keyword::In, "after `for` pattern");
        self.enter_no_struct();
        let iter = self.parse_expr_no_assign();
        self.leave_no_struct();
        self.expect_punct(Punct::LBrace, "to open `for` body");
        let body = self.parse_block_body();
        let span = self.last_span();
        let id = self.alloc_id();
        let body_expr = Expr::new(id, span, ExprKind::Block(body));
        ExprKind::For {
            label,
            pattern,
            iter: Box::new(iter),
            body: Box::new(body_expr),
        }
    }

    fn parse_break_expr(&mut self) -> ExprKind {
        self.bump();
        let label = self.try_parse_label();
        let value = if is_expression_start(self) && !at_block_end(self) {
            Some(Box::new(self.parse_expr_no_assign()))
        } else {
            None
        };
        ExprKind::Break { label, value }
    }

    fn parse_continue_expr(&mut self) -> ExprKind {
        self.bump();
        let label = self.try_parse_label();
        ExprKind::Continue { label }
    }

    fn try_parse_label(&mut self) -> Option<Label> {
        if !self.at_label_start() {
            return None;
        }
        let token = self.bump();
        Some(Label::new(label_name(self.slice(token.span))))
    }

    fn at_label_start(&self) -> bool {
        matches!(self.peek().kind, TokenKind::Label)
    }

    fn parse_labelled_loop(&mut self) -> ExprKind {
        let token = self.bump();
        let label = Some(Label::new(label_name(self.slice(token.span))));
        self.expect_punct(Punct::Colon, "after loop label");
        if self.at_keyword(Keyword::Loop) {
            return self.parse_loop_expr(label);
        }
        if self.at_keyword(Keyword::While) {
            return self.parse_while_expr(label);
        }
        if self.at_keyword(Keyword::For) {
            return self.parse_for_expr(label);
        }
        self.record(
            ParseError::Unexpected {
                expected: "`loop`, `while`, or `for` after label".to_string(),
                found: self.peek_text(),
            },
            self.peek_span(),
        );
        ExprKind::Error
    }

    fn parse_closure_expr(&mut self) -> ExprKind {
        let params = if self.eat_punct(Punct::PipePipe) {
            Vec::new()
        } else {
            self.bump();
            let mut list = Vec::new();
            while !self.at_punct(Punct::Pipe) && !self.at_eof() {
                let pattern = self.parse_pattern_no_or();
                let ty = if self.eat_punct(Punct::Colon) {
                    Some(self.parse_type())
                } else {
                    None
                };
                list.push(ClosureParam { pattern, ty });
                if !self.eat_punct(Punct::Comma) {
                    break;
                }
            }
            self.expect_punct(Punct::Pipe, "to close closure parameters");
            list
        };
        let ret = if self.eat_punct(Punct::Arrow) {
            Some(self.parse_type())
        } else {
            None
        };
        let body = self.parse_expr();
        ExprKind::Closure {
            params,
            ret,
            body: Box::new(body),
        }
    }

    fn parse_fn_closure_expr(&mut self) -> ExprKind {
        self.bump();
        self.expect_punct(Punct::LParen, "to open `fn` closure parameters");
        let mut params = Vec::new();
        while !self.at_punct(Punct::RParen) && !self.at_eof() {
            let pattern = self.parse_pattern_no_or();
            let ty = if self.eat_punct(Punct::Colon) {
                Some(self.parse_type())
            } else {
                None
            };
            params.push(ClosureParam { pattern, ty });
            if !self.eat_punct(Punct::Comma) {
                break;
            }
        }
        self.expect_punct(Punct::RParen, "to close `fn` closure parameters");
        let ret = if self.eat_punct(Punct::Arrow) {
            Some(self.parse_type())
        } else {
            None
        };
        self.expect_punct(Punct::LBrace, "to open `fn` closure body");
        let block = self.parse_block_body();
        let span = self.last_span();
        let id = self.alloc_id();
        let body = Expr::new(id, span, ExprKind::Block(block));
        ExprKind::Closure {
            params,
            ret,
            body: Box::new(body),
        }
    }

    fn parse_select_expr(&mut self) -> ExprKind {
        self.bump();
        self.expect_punct(Punct::LBrace, "to open `select` body");
        let mut arms = Vec::new();
        while !self.at_punct(Punct::RBrace) && !self.at_eof() {
            let checkpoint = self.tokens.checkpoint();
            if self.eat_keyword(Keyword::Else) || self.at_ident_text("default") {
                if self.at_ident_text("default") {
                    self.bump();
                }
                self.expect_punct(Punct::FatArrow, "after `default`");
                let body = self.parse_expr();
                arms.push(gossamer_ast::SelectArm {
                    op: gossamer_ast::SelectOp::Default,
                    body,
                });
            } else {
                let pattern = self.parse_pattern();
                if self.eat_punct(Punct::Eq) {
                    let raw = self.parse_expr_no_assign();
                    let channel = strip_recv_call(raw);
                    self.expect_punct(Punct::FatArrow, "after select recv");
                    let body = self.parse_expr();
                    arms.push(gossamer_ast::SelectArm {
                        op: gossamer_ast::SelectOp::Recv { pattern, channel },
                        body,
                    });
                } else {
                    // Not `pattern = chan.recv()`: try a send arm
                    // `chan.send(value) => body`.
                    self.tokens.rewind(checkpoint);
                    let raw = self.parse_expr_no_assign();
                    if let Some((channel, value)) = strip_send_call(raw) {
                        self.expect_punct(Punct::FatArrow, "after select send");
                        let body = self.parse_expr();
                        arms.push(gossamer_ast::SelectArm {
                            op: gossamer_ast::SelectOp::Send { channel, value },
                            body,
                        });
                    } else if self.eat_punct(Punct::FatArrow) {
                        // Unrecognised arm head; consume its body to keep
                        // forward progress instead of desyncing the parser.
                        let _ = self.parse_expr();
                    } else {
                        self.bump();
                    }
                }
            }
            if !self.eat_punct(Punct::Comma) {
                break;
            }
        }
        self.expect_punct(Punct::RBrace, "to close `select` body");
        ExprKind::Select(arms)
    }

    fn at_ident_text(&self, text: &str) -> bool {
        matches!(self.peek().kind, TokenKind::Ident) && self.slice(self.peek_span()) == text
    }

    fn is_path_expr_start(&self) -> bool {
        matches!(
            self.peek().kind,
            TokenKind::Ident
                | TokenKind::Keyword(
                    Keyword::SelfUpper | Keyword::SelfLower | Keyword::Super | Keyword::Crate
                )
        )
    }

    fn parse_path_expr_or_struct(&mut self) -> ExprKind {
        let path = self.parse_path_expr();
        if self.at_punct(Punct::Bang) {
            return self.parse_macro_tail(path);
        }
        if self.at_punct(Punct::LBrace)
            && !self.struct_literal_forbidden()
            && self.can_begin_struct_literal()
        {
            return self.parse_struct_literal_tail(path);
        }
        ExprKind::Path(path)
    }

    fn can_begin_struct_literal(&self) -> bool {
        let mut depth = 1usize;
        let mut offset = 1usize;
        loop {
            let token = self.peek_nth(offset);
            match token.kind {
                TokenKind::Eof => return false,
                TokenKind::Punct(Punct::LBrace) => depth += 1,
                TokenKind::Punct(Punct::RBrace) => {
                    depth -= 1;
                    if depth == 0 {
                        return true;
                    }
                }
                TokenKind::Ident if depth == 1 => {
                    let after = self.peek_nth(offset + 1);
                    if matches!(
                        after.kind,
                        TokenKind::Punct(Punct::Colon | Punct::Comma | Punct::RBrace)
                    ) {
                        return true;
                    }
                    return false;
                }
                TokenKind::Punct(Punct::DotDot) if depth == 1 => return true,
                _ => {}
            }
            offset += 1;
            if offset > 64 {
                return false;
            }
        }
    }

    fn parse_struct_literal_tail(&mut self, path: PathExpr) -> ExprKind {
        self.bump();
        self.with_struct_literals_allowed(|p| p.parse_struct_literal_fields(path))
    }

    fn parse_struct_literal_fields(&mut self, path: PathExpr) -> ExprKind {
        let mut fields = Vec::new();
        let mut base = None;
        while !self.at_punct(Punct::RBrace) && !self.at_eof() {
            // `..base` functional update may appear anywhere in the field
            // list (`{ ..base, x: 1 }` or `{ x: 1, ..base }`); explicit
            // fields override the base's value for the same name. Only one
            // spread is allowed.
            if self.eat_punct(Punct::DotDot) {
                let spread_span = self.peek_span();
                let expr = self.parse_expr_no_assign();
                if base.is_some() {
                    self.record(
                        ParseError::Unexpected {
                            expected: "a single `..base` spread in a struct literal".to_string(),
                            found: "a second `..` spread".to_string(),
                        },
                        spread_span,
                    );
                } else {
                    base = Some(Box::new(expr));
                }
                if !self.eat_punct(Punct::Comma) {
                    break;
                }
                continue;
            }
            let name_span = self.peek_span();
            if !matches!(self.peek().kind, TokenKind::Ident) {
                self.record(
                    ParseError::Unexpected {
                        expected: "struct field name".to_string(),
                        found: self.peek_text(),
                    },
                    name_span,
                );
                break;
            }
            let name = Ident::new(self.slice(name_span));
            self.bump();
            let value = if self.eat_punct(Punct::Colon) {
                Some(self.parse_expr_no_assign())
            } else {
                None
            };
            fields.push(StructExprField { name, value });
            if !self.eat_punct(Punct::Comma) {
                break;
            }
        }
        self.expect_punct(Punct::RBrace, "to close struct literal");
        ExprKind::Struct {
            path,
            fields,
            base,
            syntax: gossamer_ast::expr::StructExprSyntax::Braced,
        }
    }

    /// Gossamer exposes a deliberately narrow macro surface: only
    /// `format!` / `println!` / `print!` / `eprintln!` / `eprint!` /
    /// `panic!`. Each is **expanded at parse time** to a plain call
    /// on the matching variadic builtin - there is no runtime macro
    /// engine, no custom macros, no procedural macros. The
    /// expansion shape is a single `Call` whose args are the
    /// alternating literal / interpolated segments, so the whole
    /// format builds in one pass inside the builtin rather than
    /// chained `+` allocations.
    ///
    /// Unrecognised `name!(...)` invocations land as a parse
    /// diagnostic steering users to the plain-function form.
    fn parse_macro_tail(&mut self, path: PathExpr) -> ExprKind {
        let bang_span = self.peek_span();
        self.bump();
        let macro_name = path
            .segments
            .last()
            .map_or("?", |s| s.name.name.as_str())
            .to_string();
        let delim = if self.at_punct(Punct::LParen) {
            MacroDelim::Paren
        } else if self.at_punct(Punct::LBracket) {
            MacroDelim::Bracket
        } else if self.at_punct(Punct::LBrace) {
            MacroDelim::Brace
        } else {
            MacroDelim::Paren
        };

        let recognised = matches!(
            macro_name.as_str(),
            "format" | "println" | "print" | "eprintln" | "eprint" | "panic"
        ) && delim == MacroDelim::Paren;

        if recognised {
            self.expect_punct(Punct::LParen, "to open macro invocation");
            let args = self.parse_call_args();
            return self.expand_format_macro(&macro_name, args);
        }

        // Compile-time validation macros. `regex!("…")` / `sql!("…")`
        // expand to a call to a synthesized `comptime fn` validator
        // (injected by autoderive), so a malformed pattern / statement
        // fails the build rather than reaching runtime. The validator
        // returns the original string on success, folded in place by the
        // comptime pass.
        if matches!(macro_name.as_str(), "regex" | "sql") && delim == MacroDelim::Paren {
            self.expect_punct(Punct::LParen, "to open macro invocation");
            let args = self.parse_call_args();
            let validator = format!("__gos_{macro_name}_validate");
            return self.alloc_function_call(&validator, args);
        }

        // Code-emitting comptime. `codegen!(EXPR)` evaluates `EXPR` (a
        // `comptime fn` call over reflected fields) during compilation and
        // splices its `String` result back as raw source - the same
        // zero-cost stratum autoderive uses, but driven by user code. The
        // call lowers to the synthesized identity `comptime fn`
        // `__gos_codegen`, which the comptime pass recognizes and renders
        // unquoted (as source) rather than as a string literal.
        if macro_name == "codegen" && delim == MacroDelim::Paren {
            self.expect_punct(Punct::LParen, "to open macro invocation");
            let args = self.parse_call_args();
            return self.alloc_function_call("__gos_codegen", args);
        }

        // Control-flow / inspection desugars (`matches!`, `todo!`,
        // `unimplemented!`, `unreachable!`, `dbg!`) expand to plain
        // constructs every tier already handles. See `expand_builtin_macro`.
        if delim == MacroDelim::Paren
            && matches!(
                macro_name.as_str(),
                "matches" | "todo" | "unimplemented" | "unreachable" | "dbg"
            )
        {
            return self.expand_builtin_macro(&macro_name);
        }

        // Gossamer has no `vec!`: collection literals use the array
        // form `[...]` / `[v; n]`, which coerces to `Vec<T>` at every
        // expected-type site (SPEC §3.3). Steer the common Rust habit
        // there rather than to the misleading "drop the `!`" form.
        let expected = if macro_name == "vec" {
            "an array literal `[...]` - Gossamer has no `vec!`; `[...]` coerces to `Vec<T>`"
                .to_string()
        } else {
            format!("`{macro_name}(...)` - Gossamer has no user-defined macros, drop the `!`")
        };
        self.record(
            ParseError::Unexpected {
                expected,
                found: "!".to_string(),
            },
            bang_span,
        );
        let (open, close) = delim.pair();
        let open_punct = match open {
            "(" => Punct::LParen,
            "[" => Punct::LBracket,
            _ => Punct::LBrace,
        };
        let close_punct = match close {
            ")" => Punct::RParen,
            "]" => Punct::RBracket,
            _ => Punct::RBrace,
        };
        if self.eat_punct(open_punct) {
            let _tokens = self.collect_delimited_tokens(open_punct, close_punct);
            self.eat_punct(close_punct);
        }
        ExprKind::MacroCall(MacroCall {
            path,
            delim,
            tokens: String::new(),
        })
    }

    /// Expands the parser-level desugar macros into ordinary AST. None of
    /// these introduce a new node kind, so they lower uniformly on every
    /// tier: `matches!(expr, pat)` -> a two-arm boolean `match`; `todo!` /
    /// `unimplemented!` / `unreachable!` -> `panic!` with a fixed (or
    /// supplied) message; `dbg!(expr)` -> see `expand_dbg_macro`. The caller
    /// has matched the name and the `(` delimiter; the paren is unconsumed.
    fn expand_builtin_macro(&mut self, macro_name: &str) -> ExprKind {
        if macro_name == "matches" {
            self.expect_punct(Punct::LParen, "to open macro invocation");
            let scrutinee = self.with_struct_literals_allowed(Self::parse_expr_no_assign);
            self.expect_punct(Punct::Comma, "after `matches!` scrutinee");
            let pattern = self.parse_pattern();
            self.expect_punct(Punct::RParen, "to close macro invocation");
            let yes = self.alloc_literal_expr(Literal::Bool(true));
            let no = self.alloc_literal_expr(Literal::Bool(false));
            return self.make_match_clause(pattern, scrutinee, yes, no).kind;
        }
        if macro_name == "dbg" {
            return self.expand_dbg_macro();
        }
        self.expect_punct(Punct::LParen, "to open macro invocation");
        let args = self.parse_call_args();
        let args = if args.is_empty() {
            let message = match macro_name {
                "todo" => "not yet implemented",
                "unimplemented" => "not implemented",
                _ => "internal error: entered unreachable code",
            };
            vec![self.alloc_literal_expr(Literal::String(message.to_string()))]
        } else {
            args
        };
        self.expand_format_macro("panic", args)
    }

    /// Expands `dbg!(expr)` to `{ let __dbg = expr; eprintln!("{:?}", __dbg);
    /// __dbg }`, so the value is printed for inspection yet flows on
    /// unchanged. Any non-one arity degrades to a bare `eprintln!("")`.
    fn expand_dbg_macro(&mut self) -> ExprKind {
        self.expect_punct(Punct::LParen, "to open macro invocation");
        let mut args = self.parse_call_args();
        if args.len() != 1 {
            let empty = self.alloc_literal_expr(Literal::String(String::new()));
            return self.expand_format_macro("eprintln", vec![empty]);
        }
        let value = args.pop().expect("dbg! has exactly one argument here");
        let value_span = value.span;
        let binding = Pattern::new(
            self.alloc_id(),
            value_span,
            PatternKind::Ident {
                mutability: Mutability::Immutable,
                name: Ident::new("__dbg"),
                subpattern: None,
            },
        );
        let let_stmt = Stmt::new(
            self.alloc_id(),
            value_span,
            StmtKind::Let {
                pattern: binding,
                ty: None,
                init: Some(Box::new(value)),
            },
        );
        let fmt = self.alloc_literal_expr(Literal::String("{:?}".to_string()));
        let printed = self.alloc_path_expr("__dbg");
        let eprintln_kind = self.expand_format_macro("eprintln", vec![fmt, printed]);
        let eprintln_expr = Expr::new(self.alloc_id(), value_span, eprintln_kind);
        let print_stmt = Stmt::new(
            self.alloc_id(),
            value_span,
            StmtKind::Expr {
                expr: Box::new(eprintln_expr),
                has_semi: true,
            },
        );
        let tail = self.alloc_path_expr("__dbg");
        let block = Block {
            stmts: vec![let_stmt, print_stmt],
            tail: Some(Box::new(tail)),
            synthetic: true,
            is_arena: false,
            is_comptime: false,
        };
        ExprKind::Block(block)
    }

    /// Compile-time expansion for the six recognised format-shaped
    /// macros. Splits the leading format-string literal into
    /// alternating `Literal` / `Named` / `Positional` segments and
    /// emits one call to the internal zero-separator concat
    /// builtin. For `format!` the concat *is* the result; for
    /// `println!` / `print!` / `eprintln!` / `eprint!` / `panic!`
    /// the concat is passed as the single argument to the outer
    /// function - so the whole format builds in one allocation
    /// inside `__concat` rather than chained `+` calls.
    ///
    /// If the first argument is not a string literal, falls back to
    /// a plain call (drop the `!`, keep the args as-is).
    fn expand_format_macro(&mut self, macro_name: &str, args: Vec<Expr>) -> ExprKind {
        let (first, rest) = match args.split_first() {
            Some((first, rest)) => (first.clone(), rest.to_vec()),
            None => {
                return self.alloc_function_call(macro_name, Vec::new());
            }
        };
        let Some(template) = literal_string(&first) else {
            let mut all = vec![first];
            all.extend(rest);
            return self.alloc_function_call(macro_name, all);
        };
        let segments = parse_format_template(&template);
        let mut concat_args: Vec<Expr> = Vec::new();
        let mut positional_iter = rest.into_iter();
        for segment in segments {
            match segment {
                FormatSegment::Invalid(text) => {
                    self.record(
                        ParseError::MalformedFormatPlaceholder { text: text.clone() },
                        first.span,
                    );
                    concat_args
                        .push(self.alloc_literal_expr(Literal::String(format!("{{{text}}}"))));
                }
                FormatSegment::Literal(text) => {
                    if text.is_empty() {
                        continue;
                    }
                    concat_args.push(self.alloc_literal_expr(Literal::String(text)));
                }
                FormatSegment::Named(name) => {
                    let expr = self.alloc_named_capture_expr(&name);
                    concat_args.push(expr);
                }
                FormatSegment::Positional => {
                    if let Some(expr) = positional_iter.next() {
                        concat_args.push(expr);
                    }
                }
                FormatSegment::PositionalPrec(prec) => {
                    if let Some(expr) = positional_iter.next() {
                        let prec_lit = self.alloc_literal_expr(Literal::Int(prec.to_string()));
                        concat_args.push(
                            self.alloc_function_call_expr("__fmt_prec", vec![expr, prec_lit]),
                        );
                    }
                }
                FormatSegment::NamedPrec(name, prec) => {
                    let arg = self.alloc_named_capture_expr(&name);
                    let prec_lit = self.alloc_literal_expr(Literal::Int(prec.to_string()));
                    concat_args
                        .push(self.alloc_function_call_expr("__fmt_prec", vec![arg, prec_lit]));
                }
                FormatSegment::PositionalSpec(spec) => {
                    if let Some(expr) = positional_iter.next() {
                        let e = self.build_format_spec_expr(expr, &spec);
                        concat_args.push(e);
                    }
                }
                FormatSegment::NamedSpec(name, spec) => {
                    let arg = self.alloc_named_capture_expr(&name);
                    let e = self.build_format_spec_expr(arg, &spec);
                    concat_args.push(e);
                }
            }
        }
        for extra in positional_iter {
            concat_args.push(extra);
        }
        let concat_call = self.alloc_function_call_expr("__concat", concat_args);
        if macro_name == "format" {
            return concat_call.kind;
        }
        self.alloc_function_call(macro_name, vec![concat_call])
    }

    fn alloc_function_call_expr(&mut self, name: &str, args: Vec<Expr>) -> Expr {
        let id = self.alloc_id();
        let span = self.last_span();
        Expr::new(id, span, self.alloc_function_call(name, args))
    }

    /// Expands a `{:spec}` argument into a composition of already-wired
    /// stdlib calls: render the value (`__concat` for Display,
    /// `strconv::format_i64_radix` for `{:x}`/`{:b}`/`{:o}`, `__fmt_prec` for
    /// `{:.N}`), then pad to width via `strings::pad_left` / `pad_right` /
    /// `center`. No tier-specific lowering is needed.
    fn build_format_spec_expr(&mut self, value: Expr, spec: &FormatSpec) -> Expr {
        let rendered = if let Some(base) = spec.radix {
            let base_lit = self.alloc_literal_expr(Literal::Int(base.to_string()));
            let r = self.alloc_function_call_expr("__fmt_radix", vec![value, base_lit]);
            if spec.upper {
                self.alloc_function_call_expr("__fmt_upper", vec![r])
            } else {
                r
            }
        } else if let Some(prec) = spec.precision {
            let prec_lit = self.alloc_literal_expr(Literal::Int(prec.to_string()));
            self.alloc_function_call_expr("__fmt_prec", vec![value, prec_lit])
        } else {
            self.alloc_function_call_expr("__concat", vec![value])
        };
        if spec.width == 0 {
            return rendered;
        }
        let width_lit = self.alloc_literal_expr(Literal::Int(spec.width.to_string()));
        let fill_lit = self.alloc_literal_expr(Literal::Int((spec.fill as u32).to_string()));
        let align_code = match spec.align {
            Align::Left => 1,
            Align::Center => 2,
            Align::Right | Align::Default => 0,
        };
        let align_lit = self.alloc_literal_expr(Literal::Int(align_code.to_string()));
        self.alloc_function_call_expr("__fmt_pad", vec![rendered, width_lit, fill_lit, align_lit])
    }

    fn alloc_function_call(&mut self, name: &str, args: Vec<Expr>) -> ExprKind {
        let callee = self.alloc_path_expr(name);
        ExprKind::Call {
            callee: Box::new(callee),
            args,
        }
    }

    fn alloc_literal_expr(&mut self, lit: Literal) -> Expr {
        let id = self.alloc_id();
        let span = self.last_span();
        Expr::new(id, span, ExprKind::Literal(lit))
    }

    fn alloc_path_expr(&mut self, name: &str) -> Expr {
        let id = self.alloc_id();
        let span = self.last_span();
        Expr::new(id, span, ExprKind::Path(PathExpr::single(name.to_string())))
    }

    /// Expression for a named format capture: a bare `{ident}` is a
    /// path, a dotted `{a.balance}` / `{t.0}` folds field / tuple-index
    /// accesses over the leading binding.
    fn alloc_named_capture_expr(&mut self, name: &str) -> Expr {
        let mut parts = name.split('.');
        let head = parts.next().unwrap_or(name);
        let mut expr = self.alloc_path_expr(head);
        for part in parts {
            let selector = match part.parse::<u32>() {
                Ok(index) => FieldSelector::Index(index),
                Err(_) => FieldSelector::Named(Ident::new(part.to_string())),
            };
            let id = self.alloc_id();
            let span = self.last_span();
            expr = Expr::new(
                id,
                span,
                ExprKind::FieldAccess {
                    receiver: Box::new(expr),
                    field: selector,
                },
            );
        }
        expr
    }

    fn collect_delimited_tokens(&mut self, open: Punct, close: Punct) -> String {
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

    /// Parses a `PathExpr`: one or more `::`-separated segments, each of
    /// which may carry a turbofish `::<...>` list of generic arguments.
    pub(crate) fn parse_path_expr(&mut self) -> PathExpr {
        let first = self.parse_path_expr_segment();
        let mut segments = vec![first];
        while self.at_punct(Punct::ColonColon) {
            let checkpoint = self.tokens.checkpoint();
            self.bump();
            if !self.is_path_expr_start() && !self.at_punct(Punct::Lt) {
                self.tokens.rewind(checkpoint);
                break;
            }
            if self.at_punct(Punct::Lt) {
                let mut tail = segments.pop().expect("at least one path segment");
                self.bump();
                tail.generics = self.parse_generic_args_in_turbofish();
                segments.push(tail);
                continue;
            }
            segments.push(self.parse_path_expr_segment());
        }
        PathExpr { segments }
    }

    fn parse_path_expr_segment(&mut self) -> PathSegment {
        let token = self.peek();
        let name = match token.kind {
            TokenKind::Ident
            | TokenKind::Keyword(
                Keyword::SelfUpper | Keyword::SelfLower | Keyword::Super | Keyword::Crate,
            ) => {
                self.bump();
                keyword_or_ident_text(self.slice(token.span))
            }
            _ => {
                self.record(
                    ParseError::Unexpected {
                        expected: "path segment".to_string(),
                        found: self.peek_text(),
                    },
                    token.span,
                );
                String::new()
            }
        };
        PathSegment::new(name)
    }

    /// Parses a block body (`{ ... }`) after the opening brace has been consumed.
    pub(crate) fn parse_block_body(&mut self) -> Block {
        self.with_struct_literals_allowed(Self::parse_block_body_inner)
    }

    fn parse_block_body_inner(&mut self) -> Block {
        let mut stmts = Vec::new();
        let mut tail: Option<Box<Expr>> = None;
        while !self.at_punct(Punct::RBrace) && !self.at_eof() {
            let before = self.tokens.checkpoint();
            let stmt = self.parse_stmt();
            if self.tokens.checkpoint() == before {
                self.bump();
                continue;
            }
            if let gossamer_ast::StmtKind::Expr { expr, has_semi } = &stmt.kind {
                if !has_semi && self.at_punct(Punct::RBrace) {
                    tail = Some(expr.clone());
                    break;
                }
            }
            stmts.push(stmt);
        }
        self.expect_punct(Punct::RBrace, "to close block");
        Block {
            stmts,
            tail,
            synthetic: false,
            is_arena: false,
            is_comptime: false,
        }
    }

    fn try_parse_literal(&mut self) -> Option<Literal> {
        let token = self.peek();
        match token.kind {
            TokenKind::IntLit => {
                self.bump();
                Some(Literal::Int(self.slice(token.span).to_string()))
            }
            TokenKind::FloatLit => {
                self.bump();
                Some(Literal::Float(self.slice(token.span).to_string()))
            }
            TokenKind::StringLit => {
                self.bump();
                Some(Literal::String(string_literal_value(
                    self.slice(token.span),
                )))
            }
            TokenKind::RawStringLit { hashes } => {
                self.bump();
                let body = self.slice(token.span);
                Some(Literal::RawString {
                    hashes,
                    value: extract_raw_string_body(body, hashes),
                })
            }
            TokenKind::CharLit => {
                self.bump();
                Some(Literal::Char(char_literal_value(self.slice(token.span))))
            }
            TokenKind::ByteLit => {
                self.bump();
                Some(Literal::Byte(byte_literal_value(self.slice(token.span))))
            }
            TokenKind::ByteStringLit => {
                self.bump();
                Some(Literal::ByteString(byte_string_literal_value(
                    self.slice(token.span),
                )))
            }
            TokenKind::RawByteStringLit { hashes } => {
                self.bump();
                let body = self.slice(token.span);
                Some(Literal::RawByteString {
                    hashes,
                    value: extract_raw_string_body(body, hashes).into_bytes(),
                })
            }
            TokenKind::Keyword(Keyword::True) => {
                self.bump();
                Some(Literal::Bool(true))
            }
            TokenKind::Keyword(Keyword::False) => {
                self.bump();
                Some(Literal::Bool(false))
            }
            _ => None,
        }
    }
}

/// Reassigns a fresh node id to every node in a cloned subtree.
///
/// Desugaring a let-chain `else` branch duplicates the written block at
/// each failure site; each copy must own a distinct set of node ids so
/// resolution and type-checking treat the copies independently.
struct ReassignIds<'a> {
    ids: &'a mut NodeIdGenerator,
}

impl VisitorMut for ReassignIds<'_> {
    fn visit_expr(&mut self, expr: &mut Expr) {
        expr.id = self.ids.next();
        walk_expr_mut(self, expr);
    }

    fn visit_pattern(&mut self, pattern: &mut Pattern) {
        pattern.id = self.ids.next();
        walk_pattern_mut(self, pattern);
    }

    fn visit_stmt(&mut self, stmt: &mut Stmt) {
        stmt.id = self.ids.next();
        walk_stmt_mut(self, stmt);
    }

    fn visit_type(&mut self, ty: &mut Type) {
        ty.id = self.ids.next();
        walk_type_mut(self, ty);
    }

    fn visit_item(&mut self, item: &mut Item) {
        item.id = self.ids.next();
        walk_item_mut(self, item);
    }
}

/// Returns `true` for patterns that always match, so a `let` clause
/// using them binds unconditionally and lowers to a plain binding with
/// no failure edge.
fn is_irrefutable_binding(pattern: &Pattern) -> bool {
    match &pattern.kind {
        PatternKind::Wildcard => true,
        PatternKind::Ident { subpattern, .. } => subpattern
            .as_ref()
            .is_none_or(|sub| is_irrefutable_binding(sub)),
        PatternKind::Tuple(elems) => elems.iter().all(is_irrefutable_binding),
        PatternKind::Ref { inner, .. } => is_irrefutable_binding(inner),
        _ => false,
    }
}

fn is_non_associative_compare(op: BinaryOp) -> bool {
    matches!(
        op,
        BinaryOp::Eq | BinaryOp::Ne | BinaryOp::Lt | BinaryOp::Le | BinaryOp::Gt | BinaryOp::Ge
    )
}

fn keyword_or_ident_text(text: &str) -> String {
    text.to_string()
}

/// Whether a binary operator's punctuation also serves as a unary
/// prefix in Gossamer. These are the only ops for which a leading
/// newline must be treated as a statement boundary, so that
/// `let x = expr\n&y` parses as two statements (`let x = expr;`
/// followed by `&y`) rather than the binary `expr & y`. The other
/// unary prefix `!` has no binary form so does not need this guard.
fn is_unary_startable(op: BinaryOp) -> bool {
    matches!(op, BinaryOp::Sub | BinaryOp::BitAnd | BinaryOp::Mul)
}

fn extract_raw_string_body(source: &str, hashes: u8) -> String {
    let prefix_len = 1 + usize::from(hashes) + 1;
    let suffix_len = 1 + usize::from(hashes);
    if source.len() < prefix_len + suffix_len {
        return String::new();
    }
    // Unterminated raw strings can leave the trailing suffix
    // offset inside a multi-byte codepoint when the lexer
    // synthesised the token's span to end of input. Walk the end
    // back to the closest char boundary so the slice is well-typed
    // - Rust's panic on bad boundaries would otherwise tear the
    // parser down on adversarial input.
    let mut end = source.len() - suffix_len;
    while end > prefix_len && !source.is_char_boundary(end) {
        end -= 1;
    }
    if prefix_len >= end {
        return String::new();
    }
    source[prefix_len..end].to_string()
}

/// Returns `true` when the next token could begin an expression.
pub(crate) fn is_expression_start(parser: &Parser<'_>) -> bool {
    let token = parser.peek();
    match token.kind {
        TokenKind::Ident
        | TokenKind::IntLit
        | TokenKind::FloatLit
        | TokenKind::StringLit
        | TokenKind::RawStringLit { .. }
        | TokenKind::CharLit
        | TokenKind::ByteLit
        | TokenKind::ByteStringLit
        | TokenKind::RawByteStringLit { .. }
        | TokenKind::Label => true,
        TokenKind::Keyword(keyword) => matches!(
            keyword,
            Keyword::True
                | Keyword::False
                | Keyword::If
                | Keyword::Match
                | Keyword::Loop
                | Keyword::While
                | Keyword::For
                | Keyword::Unsafe
                | Keyword::Comptime
                | Keyword::Return
                | Keyword::Break
                | Keyword::Continue
                | Keyword::Go
                | Keyword::Select
                | Keyword::Fn
                | Keyword::SelfLower
                | Keyword::SelfUpper
                | Keyword::Super
                | Keyword::Crate
                | Keyword::Mut
        ),
        TokenKind::Punct(punct) => matches!(
            punct,
            Punct::LParen
                | Punct::LBracket
                | Punct::LBrace
                | Punct::Minus
                | Punct::Bang
                | Punct::Amp
                | Punct::Star
                | Punct::Pipe
                | Punct::PipePipe
                | Punct::DotDot
                | Punct::DotDotEq
        ),
        _ => false,
    }
}

/// Returns `true` when the parser is looking at a block terminator.
pub(crate) fn at_block_end(parser: &Parser<'_>) -> bool {
    parser.at_punct(Punct::RBrace)
        || parser.at_punct(Punct::Semi)
        || parser.at_punct(Punct::Comma)
        || parser.at_punct(Punct::RParen)
        || parser.at_punct(Punct::RBracket)
        || parser.at_eof()
}

/// One parsed segment of a format-string template.
/// True if `path` is the bare `_` pipe placeholder: a single segment
/// named `_` with no turbofish generics.
fn is_pipe_placeholder(path: &PathExpr) -> bool {
    path.segments.len() == 1
        && path.segments[0].name.name == "_"
        && path.segments[0].generics.is_empty()
}

/// Replaces a `_` placeholder at the head of `expr`'s receiver/base chain
/// with the piped value, consuming `value`. Walks through `MethodCall`,
/// `FieldAccess`, and `Index` receivers down to the leading `_`. Returns
/// true (and leaves `value` taken) when a placeholder was substituted;
/// returns false and leaves `value` intact when the RHS has no `_` head.
fn substitute_pipe_placeholder(expr: &mut Expr, value: &mut Option<Expr>) -> bool {
    match &mut expr.kind {
        ExprKind::MethodCall { receiver, .. } | ExprKind::FieldAccess { receiver, .. } => {
            substitute_pipe_placeholder(receiver, value)
        }
        ExprKind::Index { base, .. } => substitute_pipe_placeholder(base, value),
        ExprKind::Path(path) if is_pipe_placeholder(path) => {
            if let Some(piped) = value.take() {
                *expr = piped;
            }
            true
        }
        _ => false,
    }
}

/// Result of searching the immediate arguments of a piped call for `_`.
/// The placeholder is intentionally limited to direct arguments: allowing it
/// to escape into arbitrary nested expressions would make the pipe target
/// unclear and leave the resolver to report an unrelated unresolved name.
enum PipeArgumentPlaceholder {
    None,
    Substituted,
    Invalid,
}

/// Replaces the one direct `_` argument in a call or method call with the
/// piped value. A trailing `_` is allowed as an explicit spelling of the
/// ordinary data-last pipe rule, while a non-trailing one selects that exact
/// argument position.
fn substitute_pipe_argument_placeholder(
    expr: &mut Expr,
    value: &mut Option<Expr>,
) -> PipeArgumentPlaceholder {
    let (ExprKind::Call { args, .. } | ExprKind::MethodCall { args, .. }) = &mut expr.kind else {
        return PipeArgumentPlaceholder::None;
    };
    let placeholders: Vec<usize> = args
        .iter()
        .enumerate()
        .filter_map(|(index, arg)| match &arg.kind {
            ExprKind::Path(path) if is_pipe_placeholder(path) => Some(index),
            _ => None,
        })
        .collect();
    match placeholders.as_slice() {
        [] => PipeArgumentPlaceholder::None,
        &[index] => {
            let piped = value
                .take()
                .expect("piped value must exist while substituting a placeholder");
            args[index] = piped;
            PipeArgumentPlaceholder::Substituted
        }
        _ => PipeArgumentPlaceholder::Invalid,
    }
}

/// Whether an unsubstituted `_` remains anywhere in the immediate pipe step.
/// This rejects both repeated placeholders and nested forms such as
/// `x |> outer(inner(_))`; only the top-level call argument list may select
/// the value being piped.
fn contains_pipe_placeholder(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Path(path) => is_pipe_placeholder(path),
        ExprKind::Call { callee, args } => {
            contains_pipe_placeholder(callee) || args.iter().any(contains_pipe_placeholder)
        }
        ExprKind::MethodCall { receiver, args, .. } => {
            contains_pipe_placeholder(receiver) || args.iter().any(contains_pipe_placeholder)
        }
        ExprKind::FieldAccess { receiver, .. } => contains_pipe_placeholder(receiver),
        ExprKind::Index { base, index } => {
            contains_pipe_placeholder(base) || contains_pipe_placeholder(index)
        }
        ExprKind::Unary { operand, .. }
        | ExprKind::Cast { value: operand, .. }
        | ExprKind::Try(operand)
        | ExprKind::Go(operand) => contains_pipe_placeholder(operand),
        ExprKind::Binary { lhs, rhs, .. }
        | ExprKind::Assign {
            place: lhs,
            value: rhs,
            ..
        } => contains_pipe_placeholder(lhs) || contains_pipe_placeholder(rhs),
        ExprKind::Tuple(items) => items.iter().any(contains_pipe_placeholder),
        ExprKind::Array(ArrayExpr::List(items)) => items.iter().any(contains_pipe_placeholder),
        ExprKind::Array(ArrayExpr::Repeat { value, count }) => {
            contains_pipe_placeholder(value) || contains_pipe_placeholder(count)
        }
        ExprKind::Range { start, end, .. } => {
            start.as_deref().is_some_and(contains_pipe_placeholder)
                || end.as_deref().is_some_and(contains_pipe_placeholder)
        }
        ExprKind::Return(value) => value.as_deref().is_some_and(contains_pipe_placeholder),
        ExprKind::Break { value, .. } => value.as_deref().is_some_and(contains_pipe_placeholder),
        _ => false,
    }
}

/// `..` is range syntax. A bare open range supplied directly to a pipe call is
/// not a valid pipe placeholder, and format-macro expansion must not silently
/// combine it with the implicit trailing pipe argument.
fn has_pipe_dotdot_placeholder(expr: &Expr) -> bool {
    let (ExprKind::Call { args, .. } | ExprKind::MethodCall { args, .. }) = &expr.kind else {
        return false;
    };
    args.iter().any(|arg| {
        matches!(
            &arg.kind,
            ExprKind::Range {
                start: None,
                end: None,
                kind: RangeKind::Exclusive,
            }
        )
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Align {
    Default,
    Left,
    Center,
    Right,
}

/// A parsed format spec covering width / alignment / fill / zero-pad /
/// precision / radix - the subset of Rust's `{:spec}` grammar Gossamer
/// expands by composing `__concat`, `strconv::format_i64_radix`, and the
/// `strings` padding helpers (all already wired on every tier).
#[derive(Debug, Clone, PartialEq, Eq)]
struct FormatSpec {
    fill: char,
    align: Align,
    width: usize,
    precision: Option<usize>,
    /// Integer radix (`x`/`X` => 16, `b` => 2, `o` => 8); `None` is decimal.
    radix: Option<u32>,
    /// `{:X}` - uppercase the radix digits.
    upper: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum FormatSegment {
    /// Plain text written into the output verbatim.
    Literal(String),
    /// `{ident}` - expands to a path expression that resolves
    /// `ident` from the enclosing scope.
    Named(String),
    /// `{}` - consumed in order from the macro's trailing args.
    Positional,
    /// `{:.N}` - consumed in order, formatted with N fractional
    /// digits (replaces the hand-rolled `fmt9` helper used by
    /// the benchmark ports).
    PositionalPrec(usize),
    /// `{ident:.N}` - same as `Positional` but with precision.
    NamedPrec(String, usize),
    /// `{:spec}` - a positional argument with a width/align/fill/radix spec.
    PositionalSpec(FormatSpec),
    /// `{ident:spec}` - same as `PositionalSpec` but a named binding.
    NamedSpec(String, FormatSpec),
    /// `{age + 1}` - a placeholder whose name part is an expression, not a
    /// binding. Carries the inner text for the diagnostic; the caller
    /// records a `MalformedFormatPlaceholder` error.
    Invalid(String),
}

/// Whether a placeholder's name part (the text before any `:`) looks like an
/// expression rather than a binding name or numeric index. Used to reject
/// `{age + 1}`-style placeholders that the macros silently passed through as
/// literal text. Empty name parts (`{:spec}`) and bare identifiers/numbers are
/// not flagged.
fn format_name_looks_like_expr(inner: &str) -> bool {
    let name = inner.split(':').next().unwrap_or("").trim();
    !name.is_empty()
        && !is_capture_name(name)
        && !name.chars().all(|c| c.is_alphanumeric() || c == '_')
}

/// Splits a template into `FormatSegment`s. `{{` / `}}` escape
/// literal braces; malformed specs (`{x:?}`, `{x:0>5}`) fall
/// through as literal text so the resulting expression still
/// compiles.
/// Returns the byte width of a UTF-8 code point given its leading byte.
/// Used by `parse_format_template` to step past multi-byte chars
/// without splitting them into 1-byte (Latin-1-style) cells. Falls
/// back to 1 for malformed leaders so the loop still makes
/// progress.
fn utf8_char_len(leader: u8) -> usize {
    if leader < 0x80 {
        1
    } else if leader & 0xE0 == 0xC0 {
        2
    } else if leader & 0xF0 == 0xE0 {
        3
    } else if leader & 0xF8 == 0xF0 {
        4
    } else {
        1
    }
}

fn parse_format_template(template: &str) -> Vec<FormatSegment> {
    let bytes = template.as_bytes();
    let mut segments: Vec<FormatSegment> = Vec::new();
    let mut literal = String::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'{' if i + 1 < bytes.len() && bytes[i + 1] == b'{' => {
                literal.push('{');
                i += 2;
            }
            b'}' if i + 1 < bytes.len() && bytes[i + 1] == b'}' => {
                literal.push('}');
                i += 2;
            }
            b'{' => {
                let close = if let Some(off) = template[i + 1..].find('}') {
                    i + 1 + off
                } else {
                    literal.push('{');
                    i += 1;
                    continue;
                };
                if !literal.is_empty() {
                    segments.push(FormatSegment::Literal(std::mem::take(&mut literal)));
                }
                let inner = template[i + 1..close].trim();
                if inner.is_empty() {
                    segments.push(FormatSegment::Positional);
                } else if is_capture_name(inner) {
                    segments.push(FormatSegment::Named(inner.to_string()));
                } else if format_name_looks_like_expr(inner) {
                    segments.push(FormatSegment::Invalid(inner.to_string()));
                } else if let Some(seg) = parse_precision_spec(inner) {
                    segments.push(seg);
                } else if inner == ":?" {
                    // `{:?}` - Debug spec. Display already produces a
                    // bracketed array / tuple / struct rendering, so
                    // alias to a positional argument rather than
                    // emitting the literal text the spec used to fall
                    // through to.
                    segments.push(FormatSegment::Positional);
                } else if let Some(name) = inner.strip_suffix(":?") {
                    let name = name.trim();
                    if is_capture_name(name) {
                        segments.push(FormatSegment::Named(name.to_string()));
                    } else {
                        segments.push(FormatSegment::Literal(format!("{{{inner}}}")));
                    }
                } else if let Some(seg) = parse_format_spec(inner) {
                    segments.push(seg);
                } else {
                    segments.push(FormatSegment::Literal(format!("{{{inner}}}")));
                }
                i = close + 1;
            }
            byte if byte < 0x80 => {
                literal.push(byte as char);
                i += 1;
            }
            byte => {
                // Non-ASCII UTF-8 sequence: copy the whole code
                // point's bytes verbatim. The previous
                // `literal.push(bytes[i] as char)` cast a single
                // byte to char (giving U+0080..U+00FF) and re-
                // encoded it as 2-byte UTF-8, which double-encoded
                // every multi-byte char (e.g. an em-dash `-` came
                // out as `â\x80\x94` after the runtime treated
                // each character as Latin-1 again).
                let len = utf8_char_len(byte);
                let end = (i + len).min(bytes.len());
                literal.push_str(&template[i..end]);
                i = end;
            }
        }
    }
    if !literal.is_empty() {
        segments.push(FormatSegment::Literal(literal));
    }
    segments
}

/// Unwraps a trailing `.recv()` method call so that `select` arms can
/// store only the channel expression.
///
/// Source syntax writes the recv explicitly (`x = chan.recv() => …`),
/// but the pretty-printer re-synthesises the `.recv()` on output; if
/// the parser stored the call it would stack up one extra `.recv()`
/// per format round-trip.
fn strip_recv_call(expr: Expr) -> Expr {
    if let ExprKind::MethodCall {
        receiver,
        name,
        generics,
        args,
    } = &expr.kind
    {
        if name.name == "recv" && generics.is_empty() && args.is_empty() {
            return (**receiver).clone();
        }
    }
    expr
}

/// Splits a `chan.send(value)` method call into its `(channel, value)` parts
/// for a `select` send arm. Returns `None` when the expression is not a
/// single-argument `.send(...)` call.
fn strip_send_call(expr: Expr) -> Option<(Expr, Expr)> {
    if let ExprKind::MethodCall {
        receiver,
        name,
        generics,
        args,
    } = &expr.kind
    {
        if name.name == "send" && generics.is_empty() && args.len() == 1 {
            return Some(((**receiver).clone(), args[0].clone()));
        }
    }
    None
}

/// Parses `:.N` or `name:.N` precision specs out of a `{...}` body.
/// Returns `None` for anything that doesn't match - the caller falls
/// back to emitting the brace block as a literal so unknown specs
/// don't break compilation.
fn parse_precision_spec(inner: &str) -> Option<FormatSegment> {
    let (head, prec_str) = inner.split_once(":.")?;
    let prec: usize = prec_str.parse().ok()?;
    let head = head.trim();
    if head.is_empty() {
        Some(FormatSegment::PositionalPrec(prec))
    } else if is_capture_name(head) {
        Some(FormatSegment::NamedPrec(head.to_string(), prec))
    } else {
        None
    }
}

/// Parses a full `{[name]:[[fill]align][0][width][.prec][type]}` spec into a
/// `PositionalSpec` / `NamedSpec`. Returns `None` (so the caller falls back to
/// emitting the brace block literally) for anything it doesn't understand, so
/// an unrecognized spec never breaks compilation. `type` is one of
/// `x`/`X`/`b`/`o`/`d`. The grammar mirrors Rust's `std::fmt` subset.
fn parse_format_spec(inner: &str) -> Option<FormatSegment> {
    let (head, spec) = inner.split_once(':')?;
    let head = head.trim();
    let chars: Vec<char> = spec.chars().collect();
    let mut pos = 0;

    let is_align = |c: char| matches!(c, '<' | '>' | '^');
    let to_align = |c: char| match c {
        '<' => Align::Left,
        '^' => Align::Center,
        _ => Align::Right,
    };

    let mut fill = ' ';
    let mut align = Align::Default;
    // `[fill]align`: a fill char only counts when an align char follows it.
    if chars.len() >= 2 && is_align(chars[1]) {
        fill = chars[0];
        align = to_align(chars[1]);
        pos = 2;
    } else if !chars.is_empty() && is_align(chars[0]) {
        align = to_align(chars[0]);
        pos = 1;
    }

    // `0` zero-pad flag: fill with '0', right-align by default.
    if chars.get(pos) == Some(&'0') {
        fill = '0';
        if align == Align::Default {
            align = Align::Right;
        }
        pos += 1;
    }

    // width
    let mut width = 0usize;
    let mut saw_width = false;
    while let Some(c) = chars.get(pos) {
        if let Some(d) = c.to_digit(10) {
            width = width.checked_mul(10)?.checked_add(d as usize)?;
            saw_width = true;
            pos += 1;
        } else {
            break;
        }
    }

    // `.precision`
    let mut precision = None;
    if chars.get(pos) == Some(&'.') {
        pos += 1;
        let mut p = 0usize;
        let mut saw = false;
        while let Some(c) = chars.get(pos) {
            if let Some(d) = c.to_digit(10) {
                p = p.checked_mul(10)?.checked_add(d as usize)?;
                saw = true;
                pos += 1;
            } else {
                break;
            }
        }
        if !saw {
            return None;
        }
        precision = Some(p);
    }

    // type
    let mut radix = None;
    let mut upper = false;
    if let Some(&c) = chars.get(pos) {
        match c {
            'x' => radix = Some(16),
            'X' => {
                radix = Some(16);
                upper = true;
            }
            'b' => radix = Some(2),
            'o' => radix = Some(8),
            'd' => radix = None,
            _ => return None,
        }
        pos += 1;
    }

    // Reject trailing junk and specs that carry no formatting at all
    // (a bare `{x:}` should not shadow the plain-name path).
    if pos != chars.len() {
        return None;
    }
    if align == Align::Default && !saw_width && precision.is_none() && radix.is_none() {
        return None;
    }

    let spec = FormatSpec {
        fill,
        align,
        width,
        precision,
        radix,
        upper,
    };
    if head.is_empty() {
        Some(FormatSegment::PositionalSpec(spec))
    } else if is_capture_name(head) {
        Some(FormatSegment::NamedSpec(head.to_string(), spec))
    } else {
        None
    }
}

/// Strips the leading apostrophe from a `'name` label token's source text.
fn label_name(source: &str) -> &str {
    source.strip_prefix('\'').unwrap_or(source)
}

fn is_identifier(text: &str) -> bool {
    let mut chars = text.chars();
    match chars.next() {
        Some(c) if c == '_' || c.is_ascii_alphabetic() => {}
        _ => return false,
    }
    chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}

/// A dotted capture path (`a.balance`, `t.0.name`): an identifier head
/// followed by identifier or tuple-index segments. Bare identifiers are
/// NOT field paths - they stay on the plain `Named` classification.
fn is_field_path(text: &str) -> bool {
    let mut parts = text.split('.');
    let Some(head) = parts.next() else {
        return false;
    };
    if !is_identifier(head) {
        return false;
    }
    let mut any = false;
    for part in parts {
        if !(is_identifier(part) || (!part.is_empty() && part.bytes().all(|b| b.is_ascii_digit())))
        {
            return false;
        }
        any = true;
    }
    any
}

/// A bare identifier or a dotted capture path - the shapes a named
/// format capture accepts.
fn is_capture_name(text: &str) -> bool {
    is_identifier(text) || is_field_path(text)
}

fn literal_string(expr: &Expr) -> Option<String> {
    if let ExprKind::Literal(Literal::String(s)) = &expr.kind {
        Some(s.clone())
    } else {
        None
    }
}

/// True when `expr` is a Path expression that resolves to
/// `errors::newf` (either as a fully qualified two-segment path
/// or a single-segment `newf` import alias). Used by
/// [`Parser::parse_call_suffix`] to redirect a `errors::newf(...)`
/// call into the format-template expansion path so the same
/// `__concat`-shaped string assembly works on every tier.
fn is_errors_newf_path(expr: &Expr) -> bool {
    let ExprKind::Path(path) = &expr.kind else {
        return false;
    };
    let segs: Vec<&str> = path.segments.iter().map(|s| s.name.name.as_str()).collect();
    matches!(segs.as_slice(), ["errors", "newf"] | ["newf"])
}

#[cfg(test)]
mod tests {
    use super::Parser;
    use gossamer_ast::{BinaryOp, ExprKind};
    use gossamer_lex::SourceMap;

    fn parse_expr_for_test(source: &str) -> gossamer_ast::Expr {
        let mut source_map = SourceMap::new();
        let file = source_map.add_file("test.gos", source.to_string());
        let mut parser = Parser::new(source, file);
        parser.parse_expr()
    }

    #[test]
    fn precedence_climber_groups_plus_times() {
        let expression = parse_expr_for_test("1 + 2 * 3");
        let ExprKind::Binary { op, rhs, .. } = expression.kind else {
            panic!("expected binary expression");
        };
        assert_eq!(op, BinaryOp::Add);
        let ExprKind::Binary { op: inner_op, .. } = rhs.kind else {
            panic!("expected inner binary expression");
        };
        assert_eq!(inner_op, BinaryOp::Mul);
    }
}
