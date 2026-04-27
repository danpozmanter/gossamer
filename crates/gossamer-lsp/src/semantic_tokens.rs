//! Semantic-tokens encoder.
//!
//! Walks the AST and emits one classification per identifier so the
//! editor can colour `fn`-call sites differently from `struct` paths,
//! locals, fields, and so on. The wire encoding is the LSP-mandated
//! delta-line / delta-start / length / type / modifier-bitmask quintuple.
//!
//! The token types vector advertised in `initialize` lives in [`TOKEN_TYPES`],
//! and each [`TokenKind`] variant indexes into it.

#![forbid(unsafe_code)]

use gossamer_ast::{
    ArrayExpr, Block, ClosureParam, Expr, ExprKind, FieldSelector, FnDecl, FnParam, ImplDecl,
    ImplItem, Item, ItemKind, MatchArm, ModBody, Pattern, PatternKind, SelectArm, SelectOp, Stmt,
    StmtKind, StructBody, StructDecl, StructExprField, TraitItem, Type, TypeKind, TypePath,
};
use gossamer_lex::Span;

use crate::session::DocumentAnalysis;

/// LSP semantic token types advertised in capabilities. The order
/// matters: clients map indices into this list, so adding new kinds
/// must always be at the end.
pub(crate) const TOKEN_TYPES: &[&str] = &[
    "namespace",
    "type",
    "struct",
    "enum",
    "interface",
    "typeParameter",
    "function",
    "method",
    "property",
    "variable",
    "parameter",
    "enumMember",
    "keyword",
    "string",
    "number",
];

/// LSP semantic token modifiers advertised in capabilities. Same
/// ordering rule applies as for [`TOKEN_TYPES`].
pub(crate) const TOKEN_MODIFIERS: &[&str] = &["declaration", "definition", "readonly", "static"];

#[derive(Debug, Clone, Copy)]
enum TokenKind {
    Namespace = 0,
    Type = 1,
    Struct = 2,
    Enum = 3,
    Interface = 4,
    TypeParameter = 5,
    Function = 6,
    Method = 7,
    Property = 8,
    Variable = 9,
    Parameter = 10,
    EnumMember = 11,
}

const MOD_DECLARATION: u32 = 1 << 0;

#[derive(Debug, Clone, Copy)]
struct RawToken {
    span: Span,
    kind: TokenKind,
    modifiers: u32,
}

/// Builds the LSP `data` array for `textDocument/semanticTokens/full`.
pub(crate) fn full_tokens(doc: &DocumentAnalysis) -> Vec<u32> {
    let mut tokens: Vec<RawToken> = Vec::new();
    for item in &doc.sf.items {
        visit_item(item, &mut tokens);
    }
    tokens.sort_by_key(|t| t.span.start);
    encode(doc, &tokens)
}

fn encode(doc: &DocumentAnalysis, tokens: &[RawToken]) -> Vec<u32> {
    let mut out = Vec::with_capacity(tokens.len() * 5);
    let mut prev_line: u32 = 0;
    let mut prev_start: u32 = 0;
    for token in tokens {
        let (line, start) = doc.offset_to_position(token.span.start);
        let length = token.span.end.saturating_sub(token.span.start);
        if length == 0 {
            continue;
        }
        let delta_line = line.saturating_sub(prev_line);
        let delta_start = if delta_line == 0 {
            start.saturating_sub(prev_start)
        } else {
            start
        };
        out.push(delta_line);
        out.push(delta_start);
        out.push(length);
        out.push(token.kind as u32);
        out.push(token.modifiers);
        prev_line = line;
        prev_start = start;
    }
    out
}

fn visit_item(item: &Item, out: &mut Vec<RawToken>) {
    match &item.kind {
        ItemKind::Fn(decl) => {
            push(
                out,
                ident_span(item.span, &decl.name.name),
                TokenKind::Function,
                MOD_DECLARATION,
            );
            visit_fn(decl, out);
        }
        ItemKind::Struct(decl) => visit_struct(item, decl, out),
        ItemKind::Enum(decl) => {
            push(
                out,
                ident_span(item.span, &decl.name.name),
                TokenKind::Enum,
                MOD_DECLARATION,
            );
            for variant in &decl.variants {
                if let StructBody::Named(fields) = &variant.body {
                    for f in fields {
                        visit_type(&f.ty, out);
                    }
                }
            }
        }
        ItemKind::Trait(decl) => {
            push(
                out,
                ident_span(item.span, &decl.name.name),
                TokenKind::Interface,
                MOD_DECLARATION,
            );
            for trait_item in &decl.items {
                if let TraitItem::Fn(fn_decl) = trait_item {
                    push(
                        out,
                        ident_span(item.span, &fn_decl.name.name),
                        TokenKind::Method,
                        MOD_DECLARATION,
                    );
                    visit_fn(fn_decl, out);
                }
            }
        }
        ItemKind::Impl(decl) => visit_impl(decl, out),
        ItemKind::TypeAlias(decl) => {
            push(
                out,
                ident_span(item.span, &decl.name.name),
                TokenKind::Type,
                MOD_DECLARATION,
            );
        }
        ItemKind::Const(decl) => {
            push(
                out,
                ident_span(item.span, &decl.name.name),
                TokenKind::Variable,
                MOD_DECLARATION,
            );
            visit_type(&decl.ty, out);
            visit_expr(&decl.value, out);
        }
        ItemKind::Static(decl) => {
            push(
                out,
                ident_span(item.span, &decl.name.name),
                TokenKind::Variable,
                MOD_DECLARATION,
            );
            visit_type(&decl.ty, out);
            visit_expr(&decl.value, out);
        }
        ItemKind::Mod(decl) => {
            push(
                out,
                ident_span(item.span, &decl.name.name),
                TokenKind::Namespace,
                MOD_DECLARATION,
            );
            if let ModBody::Inline(items) = &decl.body {
                for nested in items {
                    visit_item(nested, out);
                }
            }
        }
        ItemKind::AttrItem(_) => {}
    }
}

fn visit_struct(item: &Item, decl: &StructDecl, out: &mut Vec<RawToken>) {
    push(
        out,
        ident_span(item.span, &decl.name.name),
        TokenKind::Struct,
        MOD_DECLARATION,
    );
    if let StructBody::Named(fields) = &decl.body {
        for field in fields {
            push(
                out,
                ident_span(item.span, &field.name.name),
                TokenKind::Property,
                MOD_DECLARATION,
            );
            visit_type(&field.ty, out);
        }
    }
}

fn visit_impl(decl: &ImplDecl, out: &mut Vec<RawToken>) {
    visit_type(&decl.self_ty, out);
    for impl_item in &decl.items {
        match impl_item {
            ImplItem::Fn(fn_decl) => visit_fn(fn_decl, out),
            ImplItem::Const { ty, value, .. } => {
                visit_type(ty, out);
                visit_expr(value, out);
            }
            ImplItem::Type { ty, .. } => visit_type(ty, out),
        }
    }
}

fn visit_fn(decl: &FnDecl, out: &mut Vec<RawToken>) {
    for param in &decl.params {
        if let FnParam::Typed { pattern, ty } = param {
            visit_pattern(pattern, TokenKind::Parameter, out);
            visit_type(ty, out);
        }
    }
    if let Some(ret) = &decl.ret {
        visit_type(ret, out);
    }
    if let Some(body) = &decl.body {
        visit_expr(body, out);
    }
}

fn visit_type(ty: &Type, out: &mut Vec<RawToken>) {
    match &ty.kind {
        TypeKind::Path(path) => visit_type_path(path, ty.span, out),
        TypeKind::Ref { inner, .. } | TypeKind::Slice(inner) => visit_type(inner, out),
        TypeKind::Array { elem, .. } => visit_type(elem, out),
        TypeKind::Tuple(parts) => {
            for part in parts {
                visit_type(part, out);
            }
        }
        TypeKind::Fn { params, ret, .. } => {
            for param in params {
                visit_type(param, out);
            }
            if let Some(ret) = ret {
                visit_type(ret, out);
            }
        }
        TypeKind::Infer | TypeKind::Never | TypeKind::Unit => {}
    }
}

fn visit_type_path(path: &TypePath, fallback: Span, out: &mut Vec<RawToken>) {
    if let Some(seg) = path.segments.first() {
        push(
            out,
            Span::new(
                fallback.file,
                fallback.start,
                fallback.start + seg.name.name.len() as u32,
            ),
            classify_type_name(&seg.name.name),
            0,
        );
    }
}

fn classify_type_name(name: &str) -> TokenKind {
    // Convention: `PascalCase` is a type, `snake_case` is a value /
    // module. The LSP can't tell from a path alone whether the head
    // segment refers to a struct, enum, or trait without the
    // resolver's verdict; treating the leading-uppercase / -lowercase
    // split as the heuristic gets the common cases right and matches
    // gopls / rust-analyzer's defaults.
    if name.chars().next().is_some_and(char::is_uppercase) {
        TokenKind::Type
    } else {
        TokenKind::Namespace
    }
}

fn visit_pattern(pattern: &Pattern, kind: TokenKind, out: &mut Vec<RawToken>) {
    match &pattern.kind {
        PatternKind::Ident { name, .. } => {
            push(
                out,
                ident_span(pattern.span, &name.name),
                kind,
                MOD_DECLARATION,
            );
        }
        PatternKind::Tuple(parts) | PatternKind::TupleStruct { elems: parts, .. } => {
            for part in parts {
                visit_pattern(part, kind, out);
            }
        }
        PatternKind::Struct { fields, .. } => {
            for field in fields {
                push(
                    out,
                    ident_span(pattern.span, &field.name.name),
                    TokenKind::Property,
                    0,
                );
                if let Some(sub) = &field.pattern {
                    visit_pattern(sub, kind, out);
                }
            }
        }
        PatternKind::Or(alts) => {
            for alt in alts {
                visit_pattern(alt, kind, out);
            }
        }
        PatternKind::Ref { inner, .. } => visit_pattern(inner, kind, out),
        PatternKind::Wildcard
        | PatternKind::Literal(_)
        | PatternKind::Path(_)
        | PatternKind::Range { .. }
        | PatternKind::Rest => {}
    }
}

#[allow(clippy::too_many_lines)]
fn visit_expr(expr: &Expr, out: &mut Vec<RawToken>) {
    match &expr.kind {
        ExprKind::Block(block) | ExprKind::Unsafe(block) => visit_block(block, out),
        ExprKind::If {
            condition,
            then_branch,
            else_branch,
        } => {
            visit_expr(condition, out);
            visit_expr(then_branch, out);
            if let Some(else_branch) = else_branch {
                visit_expr(else_branch, out);
            }
        }
        ExprKind::Match { scrutinee, arms } => {
            visit_expr(scrutinee, out);
            for arm in arms {
                visit_match_arm(arm, out);
            }
        }
        ExprKind::Loop { body, .. } | ExprKind::While { body, .. } => visit_expr(body, out),
        ExprKind::For {
            pattern,
            iter,
            body,
            ..
        } => {
            visit_pattern(pattern, TokenKind::Variable, out);
            visit_expr(iter, out);
            visit_expr(body, out);
        }
        ExprKind::Closure { params, body, .. } => visit_closure(params, body, out),
        ExprKind::Path(path) => {
            if let Some(seg) = path.segments.first() {
                push(
                    out,
                    ident_span(expr.span, &seg.name.name),
                    TokenKind::Variable,
                    0,
                );
            }
        }
        ExprKind::Call { callee, args } => {
            // Reclassify the callee as `function` when it's a bare path.
            if let ExprKind::Path(path) = &callee.kind {
                if let Some(seg) = path.segments.first() {
                    push(
                        out,
                        ident_span(callee.span, &seg.name.name),
                        TokenKind::Function,
                        0,
                    );
                }
            } else {
                visit_expr(callee, out);
            }
            for arg in args {
                visit_expr(arg, out);
            }
        }
        ExprKind::MethodCall {
            receiver,
            name,
            args,
            ..
        } => {
            visit_expr(receiver, out);
            push(out, ident_span(expr.span, &name.name), TokenKind::Method, 0);
            for arg in args {
                visit_expr(arg, out);
            }
        }
        ExprKind::FieldAccess { receiver, field } => {
            visit_expr(receiver, out);
            if let FieldSelector::Named(name) = field {
                push(
                    out,
                    ident_span(expr.span, &name.name),
                    TokenKind::Property,
                    0,
                );
            }
        }
        ExprKind::Index { base, index } => {
            visit_expr(base, out);
            visit_expr(index, out);
        }
        ExprKind::Binary { lhs, rhs, .. }
        | ExprKind::Assign {
            place: lhs,
            value: rhs,
            ..
        } => {
            visit_expr(lhs, out);
            visit_expr(rhs, out);
        }
        ExprKind::Unary { operand, .. } => visit_expr(operand, out),
        ExprKind::Cast { value, ty } => {
            visit_expr(value, out);
            visit_type(ty, out);
        }
        ExprKind::Try(value) => visit_expr(value, out),
        ExprKind::Tuple(parts) => {
            for part in parts {
                visit_expr(part, out);
            }
        }
        ExprKind::Struct { path, fields, base } => {
            if let Some(seg) = path.segments.first() {
                push(
                    out,
                    ident_span(expr.span, &seg.name.name),
                    TokenKind::Struct,
                    0,
                );
            }
            for field in fields {
                visit_struct_field(field, out);
            }
            if let Some(base) = base {
                visit_expr(base, out);
            }
        }
        ExprKind::Array(arr) => match arr {
            ArrayExpr::List(elems) => {
                for elem in elems {
                    visit_expr(elem, out);
                }
            }
            ArrayExpr::Repeat { value, count } => {
                visit_expr(value, out);
                visit_expr(count, out);
            }
        },
        ExprKind::Range { start, end, .. } => {
            if let Some(start) = start {
                visit_expr(start, out);
            }
            if let Some(end) = end {
                visit_expr(end, out);
            }
        }
        ExprKind::Select(arms) => {
            for arm in arms {
                visit_select_arm(arm, out);
            }
        }
        ExprKind::Go(inner) => visit_expr(inner, out),
        ExprKind::Return(inner) => {
            if let Some(inner) = inner {
                visit_expr(inner, out);
            }
        }
        ExprKind::Break { value, .. } => {
            if let Some(value) = value {
                visit_expr(value, out);
            }
        }
        ExprKind::Literal(_) | ExprKind::Continue { .. } | ExprKind::MacroCall(_) => {}
    }
}

fn visit_block(block: &Block, out: &mut Vec<RawToken>) {
    for stmt in &block.stmts {
        visit_stmt(stmt, out);
    }
    if let Some(tail) = &block.tail {
        visit_expr(tail, out);
    }
}

fn visit_stmt(stmt: &Stmt, out: &mut Vec<RawToken>) {
    match &stmt.kind {
        StmtKind::Let { pattern, ty, init } => {
            visit_pattern(pattern, TokenKind::Variable, out);
            if let Some(ty) = ty {
                visit_type(ty, out);
            }
            if let Some(init) = init {
                visit_expr(init, out);
            }
        }
        StmtKind::Expr { expr, .. } | StmtKind::Defer(expr) | StmtKind::Go(expr) => {
            visit_expr(expr, out);
        }
        StmtKind::Item(item) => visit_item(item, out),
    }
}

fn visit_match_arm(arm: &MatchArm, out: &mut Vec<RawToken>) {
    visit_pattern(&arm.pattern, TokenKind::Variable, out);
    if let Some(guard) = &arm.guard {
        visit_expr(guard, out);
    }
    visit_expr(&arm.body, out);
}

fn visit_select_arm(arm: &SelectArm, out: &mut Vec<RawToken>) {
    match &arm.op {
        SelectOp::Recv { pattern, channel } => {
            visit_pattern(pattern, TokenKind::Variable, out);
            visit_expr(channel, out);
        }
        SelectOp::Send { channel, value } => {
            visit_expr(channel, out);
            visit_expr(value, out);
        }
        SelectOp::Default => {}
    }
    visit_expr(&arm.body, out);
}

fn visit_closure(params: &[ClosureParam], body: &Expr, out: &mut Vec<RawToken>) {
    for param in params {
        visit_pattern(&param.pattern, TokenKind::Parameter, out);
        if let Some(ty) = &param.ty {
            visit_type(ty, out);
        }
    }
    visit_expr(body, out);
}

fn visit_struct_field(field: &StructExprField, out: &mut Vec<RawToken>) {
    if let Some(value) = &field.value {
        push(
            out,
            ident_in(value.span, &field.name.name),
            TokenKind::Property,
            0,
        );
        visit_expr(value, out);
    }
}

fn ident_span(item_span: Span, name: &str) -> Span {
    Span::new(
        item_span.file,
        item_span.start,
        item_span.start + name.len() as u32,
    )
}

fn ident_in(value_span: Span, name: &str) -> Span {
    Span::new(
        value_span.file,
        value_span.start,
        value_span.start + name.len() as u32,
    )
}

fn push(out: &mut Vec<RawToken>, span: Span, kind: TokenKind, modifiers: u32) {
    if span.end <= span.start {
        return;
    }
    out.push(RawToken {
        span,
        kind,
        modifiers,
    });
}

#[allow(dead_code)]
const _ALL_KINDS_USED: [TokenKind; 12] = [
    TokenKind::Namespace,
    TokenKind::Type,
    TokenKind::Struct,
    TokenKind::Enum,
    TokenKind::Interface,
    TokenKind::TypeParameter,
    TokenKind::Function,
    TokenKind::Method,
    TokenKind::Property,
    TokenKind::Variable,
    TokenKind::Parameter,
    TokenKind::EnumMember,
];
