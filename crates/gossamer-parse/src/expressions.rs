//! Expression parsing — Pratt-style precedence climbing driven by
//! SPEC §4.7 plus hand-written prefix and postfix handlers.

#![forbid(unsafe_code)]

use gossamer_ast::{
    ArrayExpr, AssignOp, BinaryOp, Block, ClosureParam, Expr, ExprKind, FieldSelector, Ident,
    Label, Literal, MacroCall, MacroDelim, PathExpr, PathSegment, RangeKind, StructExprField,
    UnaryOp,
};
use gossamer_lex::{Keyword, Punct, Span, TokenKind};

use crate::diagnostic::ParseError;
use crate::parser::Parser;
use crate::patterns::{byte_literal_value, char_literal_value, string_literal_value};

/// Precedence level strictly stronger than any binary operator.
const PREC_BELOW_ASSIGN: u8 = 17;

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
        let mut lhs = self.parse_prefix();
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
            Some(Box::new(self.parse_expr_with_prec(15, false)))
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

    fn validate_pipe_rhs(&mut self, lhs: Expr, rhs: Expr) -> Expr {
        let rhs_span = rhs.span;
        let valid = matches!(
            rhs.kind,
            ExprKind::Path(_)
                | ExprKind::Call { .. }
                | ExprKind::MethodCall { .. }
                | ExprKind::FieldAccess { .. }
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
                primary = self.parse_call_suffix(primary);
                continue;
            }
            if self.at_punct(Punct::LBracket) {
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
        while !self.at_punct(Punct::Gt) && !self.at_eof() {
            args.push(self.parse_generic_arg());
            if !self.eat_punct(Punct::Comma) {
                break;
            }
        }
        self.expect_punct(Punct::Gt, "to close turbofish generics");
        args
    }

    fn parse_call_suffix(&mut self, callee: Expr) -> Expr {
        self.bump();
        let args = self.parse_call_args();
        let end_span = self.last_span();
        let span = self.join(callee.span, end_span);
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

    fn parse_index_suffix(&mut self, base: Expr) -> Expr {
        self.bump();
        let index = self.parse_expr_no_assign();
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
        let mut args = Vec::new();
        while !self.at_punct(Punct::RParen) && !self.at_eof() {
            if self.at_punct(Punct::DotDot) || self.at_punct(Punct::DotDotDot) {
                self.bump();
                continue;
            }
            args.push(self.parse_expr_no_assign());
            if !self.eat_punct(Punct::Comma) {
                break;
            }
        }
        self.expect_punct(Punct::RParen, "to close argument list");
        args
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
        ExprKind::Literal(Literal::Unit)
    }

    fn parse_paren_or_tuple(&mut self) -> ExprKind {
        if self.eat_punct(Punct::RParen) {
            return ExprKind::Literal(Literal::Unit);
        }
        let first = self.parse_expr();
        if self.eat_punct(Punct::RParen) {
            return first.kind;
        }
        let mut elements = vec![first];
        while self.eat_punct(Punct::Comma) {
            if self.at_punct(Punct::RParen) {
                break;
            }
            elements.push(self.parse_expr());
        }
        self.expect_punct(Punct::RParen, "to close tuple expression");
        ExprKind::Tuple(elements)
    }

    fn parse_array_expr(&mut self) -> ExprKind {
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
        let condition = self.parse_expr_no_assign();
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
        ExprKind::If {
            condition: Box::new(condition),
            then_branch: Box::new(then_expr),
            else_branch,
        }
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
        let condition = self.parse_expr_no_assign();
        self.leave_no_struct();
        self.expect_punct(Punct::LBrace, "to open `while` body");
        let body = self.parse_block_body();
        let span = self.last_span();
        let id = self.alloc_id();
        let body_expr = Expr::new(id, span, ExprKind::Block(body));
        ExprKind::While {
            label,
            condition: Box::new(condition),
            body: Box::new(body_expr),
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
        self.bump();
        if let Some(name_span) = self.eat_ident() {
            return Some(Label::new(self.slice(name_span)));
        }
        self.record(ParseError::MalformedLabel, self.peek_span());
        None
    }

    fn at_label_start(&self) -> bool {
        false
    }

    fn parse_labelled_loop(&mut self) -> ExprKind {
        ExprKind::Literal(Literal::Unit)
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
                    self.tokens.rewind(checkpoint);
                    self.bump();
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
        let mut fields = Vec::new();
        let mut base = None;
        while !self.at_punct(Punct::RBrace) && !self.at_eof() {
            if self.eat_punct(Punct::DotDot) {
                base = Some(Box::new(self.parse_expr_no_assign()));
                break;
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
        ExprKind::Struct { path, fields, base }
    }

    /// Gossamer exposes a deliberately narrow macro surface: only
    /// `format!` / `println!` / `print!` / `eprintln!` / `eprint!` /
    /// `panic!`. Each is **expanded at parse time** to a plain call
    /// on the matching variadic builtin — there is no runtime macro
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

        self.record(
            ParseError::Unexpected {
                expected: format!(
                    "`{macro_name}(...)` — Gossamer has no user-defined macros, drop the `!`"
                ),
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

    /// Compile-time expansion for the six recognised format-shaped
    /// macros. Splits the leading format-string literal into
    /// alternating `Literal` / `Named` / `Positional` segments and
    /// emits one call to the internal zero-separator concat
    /// builtin. For `format!` the concat *is* the result; for
    /// `println!` / `print!` / `eprintln!` / `eprint!` / `panic!`
    /// the concat is passed as the single argument to the outer
    /// function — so the whole format builds in one allocation
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
                FormatSegment::Literal(text) => {
                    if text.is_empty() {
                        continue;
                    }
                    concat_args.push(self.alloc_literal_expr(Literal::String(text)));
                }
                FormatSegment::Named(name) => {
                    concat_args.push(self.alloc_path_expr(&name));
                }
                FormatSegment::Positional => {
                    if let Some(expr) = positional_iter.next() {
                        concat_args.push(expr);
                    }
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
        Block { stmts, tail }
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
                Some(Literal::ByteString(
                    self.slice(token.span).as_bytes().to_vec(),
                ))
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

fn is_non_associative_compare(op: BinaryOp) -> bool {
    matches!(
        op,
        BinaryOp::Eq | BinaryOp::Ne | BinaryOp::Lt | BinaryOp::Le | BinaryOp::Gt | BinaryOp::Ge
    )
}

fn keyword_or_ident_text(text: &str) -> String {
    text.to_string()
}

fn extract_raw_string_body(source: &str, hashes: u8) -> String {
    let prefix_len = 1 + usize::from(hashes) + 1;
    let suffix_len = 1 + usize::from(hashes);
    if source.len() < prefix_len + suffix_len {
        return String::new();
    }
    source[prefix_len..source.len() - suffix_len].to_string()
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
        | TokenKind::RawByteStringLit { .. } => true,
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
#[derive(Debug, Clone, PartialEq, Eq)]
enum FormatSegment {
    /// Plain text written into the output verbatim.
    Literal(String),
    /// `{ident}` — expands to a path expression that resolves
    /// `ident` from the enclosing scope.
    Named(String),
    /// `{}` — consumed in order from the macro's trailing args.
    Positional,
}

/// Splits a template into `FormatSegment`s. `{{` / `}}` escape
/// literal braces; malformed specs (`{x:?}`, `{x:0>5}`) fall
/// through as literal text so the resulting expression still
/// compiles.
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
                } else if is_identifier(inner) {
                    segments.push(FormatSegment::Named(inner.to_string()));
                } else {
                    segments.push(FormatSegment::Literal(format!("{{{inner}}}")));
                }
                i = close + 1;
            }
            _ => {
                literal.push(bytes[i] as char);
                i += 1;
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

fn is_identifier(text: &str) -> bool {
    let mut chars = text.chars();
    match chars.next() {
        Some(c) if c == '_' || c.is_ascii_alphabetic() => {}
        _ => return false,
    }
    chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}

fn literal_string(expr: &Expr) -> Option<String> {
    if let ExprKind::Literal(Literal::String(s)) = &expr.kind {
        Some(s.clone())
    } else {
        None
    }
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
