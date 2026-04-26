//! AST-walk helpers and the per-lint implementations.
//!
//! Each `lint_*` function is a pure pass `(&SourceFile) -> Vec<Finding>`
//! invoked from [`run_lint`]. The walker helpers are private to the
//! crate so the lint pass and `fix.rs` can traverse every expression
//! in an item body without tripping on exotic constructs.

use std::collections::{BTreeMap, BTreeSet};

use gossamer_ast::{
    AssignOp, BinaryOp, Block, Expr, ExprKind, ImplItem, ItemKind, Literal, Mutability, Pattern,
    PatternKind, PathExpr, SourceFile, Stmt, StmtKind, UnaryOp, UseDecl, UseListEntry, UseTarget,
};
use gossamer_lex::Span;

use crate::Finding;

/// Dispatches on the lint id and returns the findings from that
/// specific pass. Used by [`crate::run`].
pub(crate) fn run_lint(id: &str, sf: &SourceFile) -> Vec<Finding> {
    match id {
        "unused_variable" => lint_unused_variable(sf),
        "unused_import" => lint_unused_import(sf),
        "unused_mut_variable" => lint_unused_mut_variable(sf),
        "needless_return" => lint_needless_return(sf),
        "needless_bool" => lint_needless_bool(sf),
        "comparison_to_bool_literal" => lint_bool_cmp(sf),
        "single_match" => lint_single_match(sf),
        "shadowed_binding" => lint_shadowed_binding(sf),
        "unchecked_result" => lint_unchecked_result(sf),
        "empty_block" => lint_empty_block(sf),
        "panic_in_main" => lint_panic_in_main(sf),
        "redundant_clone" => lint_redundant_clone(sf),
        "double_negation" => lint_double_negation(sf),
        "self_assignment" => lint_self_assignment(sf),
        "todo_macro" => lint_todo_macro(sf),
        "bool_literal_in_condition" => lint_bool_literal_in_condition(sf),
        "let_and_return" => lint_let_and_return(sf),
        "collapsible_if" => lint_collapsible_if(sf),
        "if_same_then_else" => lint_if_same_then_else(sf),
        "redundant_field_init" => lint_redundant_field_init(sf),
        "needless_else_after_return" => lint_needless_else_after_return(sf),
        "self_compare" => lint_self_compare(sf),
        "identity_op" => lint_identity_op(sf),
        "unit_let" => lint_unit_let(sf),
        "float_eq_zero" => lint_float_eq_zero(sf),
        "empty_else" => lint_empty_else(sf),
        "match_bool" => lint_match_bool(sf),
        "needless_parens" => lint_needless_parens(sf),
        "manual_not_equal" => lint_manual_not_equal(sf),
        "nested_ternary_if" => lint_nested_ternary_if(sf),
        "absurd_range" => lint_absurd_range(sf),
        "string_literal_concat" => lint_string_literal_concat(sf),
        "chained_negation_literals" => lint_chained_negation_literals(sf),
        "if_not_else" => lint_if_not_else(sf),
        "empty_string_concat" => lint_empty_string_concat(sf),
        "println_newline_only" => lint_println_newline_only(sf),
        "match_same_arms" => lint_match_same_arms(sf),
        "manual_swap" => lint_manual_swap(sf),
        "consecutive_assignment" => lint_consecutive_assignment(sf),
        "large_unreadable_literal" => lint_large_unreadable_literal(sf),
        "redundant_closure" => lint_redundant_closure(sf),
        "empty_if_body" => lint_empty_if_body(sf),
        "bool_to_int_match" => lint_bool_to_int_match(sf),
        "fn_returns_unit_explicit" => lint_fn_returns_unit_explicit(sf),
        "let_with_unit_type" => lint_let_with_unit_type(sf),
        "useless_default_only_match" => lint_useless_default_only_match(sf),
        "unnecessary_parens_in_condition" => lint_unnecessary_parens_in_condition(sf),
        "pattern_matching_unit" => lint_pattern_matching_unit(sf),
        "panic_without_message" => lint_panic_without_message(sf),
        "empty_loop" => lint_empty_loop(sf),
        _ => Vec::new(),
    }
}


fn each_fn_body<'a>(sf: &'a SourceFile, mut visitor: impl FnMut(&'a Expr)) {
    for item in &sf.items {
        match &item.kind {
            ItemKind::Fn(decl) => {
                if let Some(body) = &decl.body {
                    visitor(body);
                }
            }
            ItemKind::Impl(decl) => {
                for item in &decl.items {
                    if let ImplItem::Fn(method) = item {
                        if let Some(body) = &method.body {
                            visitor(body);
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

pub(crate) fn walk_expr(expr: &Expr, visitor: &mut dyn FnMut(&Expr)) {
    visitor(expr);
    match &expr.kind {
        ExprKind::Block(block) => walk_block(block, visitor),
        ExprKind::Unsafe(block) => walk_block(block, visitor),
        ExprKind::Call { callee, args } => {
            walk_expr(callee, visitor);
            for a in args {
                walk_expr(a, visitor);
            }
        }
        ExprKind::MethodCall { receiver, args, .. } => {
            walk_expr(receiver, visitor);
            for a in args {
                walk_expr(a, visitor);
            }
        }
        ExprKind::FieldAccess { receiver, .. } => walk_expr(receiver, visitor),
        ExprKind::Index { base, index } => {
            walk_expr(base, visitor);
            walk_expr(index, visitor);
        }
        ExprKind::Unary { operand, .. } => walk_expr(operand, visitor),
        ExprKind::Binary { lhs, rhs, .. } => {
            walk_expr(lhs, visitor);
            walk_expr(rhs, visitor);
        }
        ExprKind::Assign { place, value, .. } => {
            walk_expr(place, visitor);
            walk_expr(value, visitor);
        }
        ExprKind::Cast { value, .. } => walk_expr(value, visitor),
        ExprKind::If {
            condition,
            then_branch,
            else_branch,
        } => {
            walk_expr(condition, visitor);
            walk_expr(then_branch, visitor);
            if let Some(e) = else_branch {
                walk_expr(e, visitor);
            }
        }
        ExprKind::Match { scrutinee, arms } => {
            walk_expr(scrutinee, visitor);
            for arm in arms {
                if let Some(guard) = arm.guard.as_ref() {
                    walk_expr(guard, visitor);
                }
                walk_expr(&arm.body, visitor);
            }
        }
        ExprKind::Loop { body, .. } => walk_expr(body, visitor),
        ExprKind::While { condition, body, .. } => {
            walk_expr(condition, visitor);
            walk_expr(body, visitor);
        }
        ExprKind::For { iter, body, .. } => {
            walk_expr(iter, visitor);
            walk_expr(body, visitor);
        }
        ExprKind::Closure { body, .. } => walk_expr(body, visitor),
        ExprKind::Return(Some(inner)) | ExprKind::Try(inner) | ExprKind::Go(inner) => {
            walk_expr(inner, visitor);
        }
        ExprKind::Break { value: Some(v), .. } => walk_expr(v, visitor),
        ExprKind::Tuple(elems) => {
            for e in elems {
                walk_expr(e, visitor);
            }
        }
        ExprKind::Array(_) => {}
        ExprKind::Struct { fields, base, .. } => {
            for field in fields {
                if let Some(value) = &field.value {
                    walk_expr(value, visitor);
                }
            }
            if let Some(b) = base {
                walk_expr(b, visitor);
            }
        }
        ExprKind::Range { start, end, .. } => {
            if let Some(s) = start {
                walk_expr(s, visitor);
            }
            if let Some(e) = end {
                walk_expr(e, visitor);
            }
        }
        ExprKind::Select(_) => {}
        ExprKind::Path(_)
        | ExprKind::Literal(_)
        | ExprKind::Return(None)
        | ExprKind::Break { value: None, .. }
        | ExprKind::Continue { .. }
        | ExprKind::MacroCall(_) => {}
    }
}

pub(crate) fn walk_block(block: &Block, visitor: &mut dyn FnMut(&Expr)) {
    for stmt in &block.stmts {
        walk_stmt_exprs(stmt, visitor);
    }
    if let Some(tail) = &block.tail {
        walk_expr(tail, visitor);
    }
}

fn walk_stmt_exprs(stmt: &Stmt, visitor: &mut dyn FnMut(&Expr)) {
    match &stmt.kind {
        StmtKind::Let { init: Some(init), .. } => walk_expr(init, visitor),
        StmtKind::Expr { expr, .. } | StmtKind::Defer(expr) | StmtKind::Go(expr) => {
            walk_expr(expr, visitor);
        }
        StmtKind::Item(_) | StmtKind::Let { .. } => {}
    }
}

fn as_block(expr: &Expr) -> Option<&Block> {
    if let ExprKind::Block(block) = &expr.kind {
        Some(block)
    } else {
        None
    }
}

fn ident_name(pattern: &Pattern) -> Option<(&str, Span, Mutability)> {
    if let PatternKind::Ident {
        name, mutability, ..
    } = &pattern.kind
    {
        return Some((name.name.as_str(), pattern.span, *mutability));
    }
    None
}

fn last_path_seg(path: &PathExpr) -> Option<&str> {
    path.segments.last().map(|s| s.name.name.as_str())
}

// ---------------------------------------------------------------------
// Identifier-use tracker
// ---------------------------------------------------------------------

#[derive(Default)]
struct Uses {
    used: BTreeSet<String>,
}

impl Uses {
    fn collect(&mut self, sf: &SourceFile) {
        each_fn_body(sf, |body| {
            walk_expr(body, &mut |expr| {
                if let ExprKind::Path(path) = &expr.kind {
                    if let Some(seg) = path.segments.first() {
                        self.used.insert(seg.name.name.clone());
                    }
                }
            });
        });
    }

    fn contains(&self, name: &str) -> bool {
        self.used.contains(name)
    }
}

// ---------------------------------------------------------------------
// Lints
// ---------------------------------------------------------------------

fn lint_unused_variable(sf: &SourceFile) -> Vec<Finding> {
    let mut out = Vec::new();
    each_fn_body(sf, |body| {
        let Some(block) = as_block(body) else {
            return;
        };
        let mut bindings: Vec<(String, Span)> = Vec::new();
        for stmt in &block.stmts {
            if let StmtKind::Let { pattern, .. } = &stmt.kind {
                if let Some((name, span, _)) = ident_name(pattern) {
                    if !name.starts_with('_') {
                        bindings.push((name.to_string(), span));
                    }
                }
            }
        }
        let mut used: BTreeSet<String> = BTreeSet::new();
        walk_block(block, &mut |expr| {
            if let ExprKind::Path(path) = &expr.kind {
                if let Some(seg) = path.segments.first() {
                    used.insert(seg.name.name.clone());
                }
            }
        });
        for (name, span) in bindings {
            if !used.contains(&name) {
                out.push((
                    span,
                    format!("unused variable `{name}`"),
                    Some(format!("prefix with `_` to silence: `_{name}`")),
                ));
            }
        }
    });
    out
}

fn lint_unused_mut_variable(sf: &SourceFile) -> Vec<Finding> {
    let mut out = Vec::new();
    each_fn_body(sf, |body| {
        let Some(block) = as_block(body) else {
            return;
        };
        for stmt in &block.stmts {
            if let StmtKind::Let { pattern, .. } = &stmt.kind {
                if let Some((name, span, mutability)) = ident_name(pattern) {
                    if matches!(mutability, Mutability::Mutable)
                        && !name.starts_with('_')
                        && !block_reassigns(block, name)
                    {
                        out.push((
                            span,
                            format!("`{name}` is declared `mut` but never reassigned"),
                            Some("remove the `mut` keyword".to_string()),
                        ));
                    }
                }
            }
        }
    });
    out
}

fn block_reassigns(block: &Block, target: &str) -> bool {
    let mut found = false;
    walk_block(block, &mut |expr| {
        if found {
            return;
        }
        if let ExprKind::Assign { place, .. } = &expr.kind {
            if let ExprKind::Path(path) = &place.kind {
                if path
                    .segments
                    .first()
                    .is_some_and(|s| s.name.name == target)
                {
                    found = true;
                }
            }
        }
    });
    found
}

fn lint_unused_import(sf: &SourceFile) -> Vec<Finding> {
    let mut uses = Uses::default();
    uses.collect(sf);
    let mut out = Vec::new();
    for decl in &sf.uses {
        check_use(decl, &uses, &mut out);
    }
    out
}

fn check_use(decl: &UseDecl, uses: &Uses, out: &mut Vec<Finding>) {
    if let Some(list) = &decl.list {
        for entry in list {
            check_entry(entry, decl.span, uses, out);
        }
        return;
    }
    let name = decl.alias.as_ref().map_or_else(|| {
        match &decl.target {
            UseTarget::Module(path) => path
                .segments
                .last()
                .map(|s| s.name.clone())
                .unwrap_or_default(),
            UseTarget::Project { module, id } => module
                .as_ref()
                .and_then(|p| p.segments.last().map(|s| s.name.clone()))
                .unwrap_or_else(|| id.clone()),
        }
    }, |a| a.name.clone());
    if !name.is_empty() && !uses.contains(&name) {
        out.push((
            decl.span,
            format!("unused import `{name}`"),
            Some("remove the `use` declaration".to_string()),
        ));
    }
}

fn check_entry(entry: &UseListEntry, span: Span, uses: &Uses, out: &mut Vec<Finding>) {
    let name = entry
        .alias
        .as_ref()
        .map_or_else(|| entry.name.name.clone(), |a| a.name.clone());
    if !uses.contains(&name) {
        out.push((
            span,
            format!("unused import `{name}`"),
            Some("remove it from the `use` list".to_string()),
        ));
    }
}

fn lint_needless_return(sf: &SourceFile) -> Vec<Finding> {
    let mut out = Vec::new();
    each_fn_body(sf, |body| {
        let Some(block) = as_block(body) else {
            return;
        };
        if let Some(tail) = &block.tail {
            if matches!(&tail.kind, ExprKind::Return(_)) {
                out.push((
                    tail.span,
                    "needless `return` at end of block".to_string(),
                    Some("drop the `return` and let the tail expression return".to_string()),
                ));
            }
        } else if let Some(last) = block.stmts.last() {
            if let StmtKind::Expr {
                expr,
                has_semi: false,
            } = &last.kind
            {
                if matches!(&expr.kind, ExprKind::Return(_)) {
                    out.push((
                        last.span,
                        "needless `return` at end of block".to_string(),
                        Some("drop the `return` and let the tail expression return".to_string()),
                    ));
                }
            }
        }
    });
    out
}

fn lint_needless_bool(sf: &SourceFile) -> Vec<Finding> {
    let mut out = Vec::new();
    each_fn_body(sf, |body| {
        walk_expr(body, &mut |expr| {
            if let ExprKind::If {
                then_branch,
                else_branch: Some(else_branch),
                ..
            } = &expr.kind
            {
                if is_bool_block(then_branch, true) && is_bool_block(else_branch, false) {
                    out.push((
                        expr.span,
                        "needless `if` returning a bool literal".to_string(),
                        Some("replace with the condition itself".to_string()),
                    ));
                } else if is_bool_block(then_branch, false) && is_bool_block(else_branch, true) {
                    out.push((
                        expr.span,
                        "needless `if` returning a bool literal".to_string(),
                        Some("replace with `!condition`".to_string()),
                    ));
                }
            }
        });
    });
    out
}

fn is_bool_block(expr: &Expr, target: bool) -> bool {
    if let ExprKind::Block(block) = &expr.kind {
        if block.stmts.is_empty() {
            if let Some(tail) = &block.tail {
                return is_bool_literal(tail, target);
            }
        }
    }
    is_bool_literal(expr, target)
}

fn is_bool_literal(expr: &Expr, target: bool) -> bool {
    matches!(
        &expr.kind,
        ExprKind::Literal(Literal::Bool(b)) if *b == target
    )
}

fn lint_bool_cmp(sf: &SourceFile) -> Vec<Finding> {
    let mut out = Vec::new();
    each_fn_body(sf, |body| {
        walk_expr(body, &mut |expr| {
            if let ExprKind::Binary { op, lhs, rhs } = &expr.kind {
                if matches!(op, BinaryOp::Eq | BinaryOp::Ne)
                    && (is_any_bool_lit(lhs) || is_any_bool_lit(rhs))
                {
                    out.push((
                        expr.span,
                        "comparing to a bool literal is noisy".to_string(),
                        Some(
                            "drop the comparison and use the value (or `!value`) directly"
                                .to_string(),
                        ),
                    ));
                }
            }
        });
    });
    out
}

fn is_any_bool_lit(expr: &Expr) -> bool {
    matches!(&expr.kind, ExprKind::Literal(Literal::Bool(_)))
}

fn lint_single_match(sf: &SourceFile) -> Vec<Finding> {
    let mut out = Vec::new();
    each_fn_body(sf, |body| {
        walk_expr(body, &mut |expr| {
            if let ExprKind::Match { arms, .. } = &expr.kind {
                if arms.len() == 1 {
                    out.push((
                        expr.span,
                        "`match` with a single arm is clearer as `if let`".to_string(),
                        Some("rewrite as `if let PATTERN = ...`".to_string()),
                    ));
                }
            }
        });
    });
    out
}

fn lint_shadowed_binding(sf: &SourceFile) -> Vec<Finding> {
    let mut out = Vec::new();
    each_fn_body(sf, |body| {
        let Some(block) = as_block(body) else {
            return;
        };
        let mut seen: BTreeMap<String, Span> = BTreeMap::new();
        for stmt in &block.stmts {
            if let StmtKind::Let { pattern, .. } = &stmt.kind {
                if let Some((name, span, _)) = ident_name(pattern) {
                    if seen.contains_key(name) {
                        out.push((
                            span,
                            format!("`{name}` shadows a prior binding"),
                            Some("rename to clarify which binding is in use".to_string()),
                        ));
                    } else {
                        seen.insert(name.to_string(), span);
                    }
                }
            }
        }
    });
    out
}

fn lint_unchecked_result(sf: &SourceFile) -> Vec<Finding> {
    let mut out = Vec::new();
    each_fn_body(sf, |body| {
        let Some(block) = as_block(body) else {
            return;
        };
        for stmt in &block.stmts {
            if let StmtKind::Let {
                pattern,
                init: Some(init),
                ..
            } = &stmt.kind
            {
                if matches!(&pattern.kind, PatternKind::Wildcard) && returns_result(init) {
                    out.push((
                        stmt.span,
                        "`Result` discarded with `let _`".to_string(),
                        Some(
                            "handle the `Err` explicitly or propagate with `?`".to_string(),
                        ),
                    ));
                }
            }
        }
    });
    out
}

fn returns_result(expr: &Expr) -> bool {
    if let ExprKind::Call { callee, .. } = &expr.kind {
        if let ExprKind::Path(path) = &callee.kind {
            if let Some(name) = last_path_seg(path) {
                return matches!(name, "Ok" | "Err");
            }
        }
    }
    false
}

fn lint_empty_block(sf: &SourceFile) -> Vec<Finding> {
    let mut out = Vec::new();
    each_fn_body(sf, |body| {
        walk_expr(body, &mut |expr| {
            if let ExprKind::Block(block) = &expr.kind {
                if block.stmts.is_empty() && block.tail.is_none() {
                    out.push((
                        expr.span,
                        "empty block is almost always a mistake".to_string(),
                        Some("remove the block or add an explicit `()` tail".to_string()),
                    ));
                }
            }
        });
    });
    out
}

fn lint_panic_in_main(sf: &SourceFile) -> Vec<Finding> {
    let mut out = Vec::new();
    for item in &sf.items {
        if let ItemKind::Fn(decl) = &item.kind {
            if decl.name.name != "main" {
                continue;
            }
            if let Some(body) = &decl.body {
                walk_expr(body, &mut |expr| {
                    if let ExprKind::Call { callee, .. } = &expr.kind {
                        if let ExprKind::Path(path) = &callee.kind {
                            if last_path_seg(path) == Some("panic") {
                                out.push((
                                    expr.span,
                                    "`panic!` inside `main` aborts without a clean exit code"
                                        .to_string(),
                                    Some(
                                        "return a `Result` or use `std::process::exit`"
                                            .to_string(),
                                    ),
                                ));
                            }
                        }
                    }
                    if let ExprKind::MacroCall(mc) = &expr.kind {
                        if mc.path.segments.last().map(|s| s.name.name.as_str()) == Some("panic") {
                            out.push((
                                expr.span,
                                "`panic!` inside `main` aborts without a clean exit code"
                                    .to_string(),
                                Some(
                                    "return a `Result` or use `std::process::exit`"
                                        .to_string(),
                                ),
                            ));
                        }
                    }
                });
            }
        }
    }
    out
}

fn lint_redundant_clone(sf: &SourceFile) -> Vec<Finding> {
    let mut out = Vec::new();
    each_fn_body(sf, |body| {
        walk_expr(body, &mut |expr| {
            if let ExprKind::MethodCall { name, receiver, args, .. } = &expr.kind {
                if name.name == "clone"
                    && args.is_empty()
                    && matches!(&receiver.kind, ExprKind::Literal(_))
                {
                    out.push((
                        expr.span,
                        "`.clone()` on a literal is redundant".to_string(),
                        Some("drop the `.clone()`".to_string()),
                    ));
                }
            }
        });
    });
    out
}

fn lint_double_negation(sf: &SourceFile) -> Vec<Finding> {
    let mut out = Vec::new();
    each_fn_body(sf, |body| {
        walk_expr(body, &mut |expr| {
            if let ExprKind::Unary {
                op: UnaryOp::Not,
                operand,
            } = &expr.kind
            {
                if matches!(
                    &operand.kind,
                    ExprKind::Unary {
                        op: UnaryOp::Not,
                        ..
                    }
                ) {
                    out.push((
                        expr.span,
                        "`!!x` is the same as `x` when `x: bool`".to_string(),
                        Some("drop the double negation".to_string()),
                    ));
                }
            }
        });
    });
    out
}

fn lint_self_assignment(sf: &SourceFile) -> Vec<Finding> {
    let mut out = Vec::new();
    each_fn_body(sf, |body| {
        walk_expr(body, &mut |expr| {
            if let ExprKind::Assign {
                op: AssignOp::Assign,
                place,
                value,
            } = &expr.kind
            {
                if path_eq(place, value) {
                    out.push((
                        expr.span,
                        "assignment to self is a no-op".to_string(),
                        Some("remove the statement".to_string()),
                    ));
                }
            }
        });
    });
    out
}

fn path_eq(a: &Expr, b: &Expr) -> bool {
    match (&a.kind, &b.kind) {
        (ExprKind::Path(p), ExprKind::Path(q)) => {
            p.segments.len() == q.segments.len()
                && p.segments
                    .iter()
                    .zip(&q.segments)
                    .all(|(x, y)| x.name.name == y.name.name)
        }
        _ => false,
    }
}

fn lint_todo_macro(sf: &SourceFile) -> Vec<Finding> {
    let mut out = Vec::new();
    each_fn_body(sf, |body| {
        walk_expr(body, &mut |expr| {
            match &expr.kind {
                ExprKind::MacroCall(mc) => {
                    if let Some(name) = mc.path.segments.last().map(|s| s.name.name.as_str()) {
                        if matches!(name, "todo" | "unimplemented") {
                            out.push((
                                expr.span,
                                format!("`{name}!` is a placeholder, not a shippable expression"),
                                Some("implement the branch before merging".to_string()),
                            ));
                        }
                    }
                }
                ExprKind::Call { callee, .. } => {
                    if let ExprKind::Path(path) = &callee.kind {
                        if let Some(name) = path.segments.last().map(|s| s.name.name.as_str()) {
                            if matches!(name, "todo" | "unimplemented") {
                                out.push((
                                    expr.span,
                                    format!("`{name}(...)` is a placeholder, not a shippable expression"),
                                    Some("implement the branch before merging".to_string()),
                                ));
                            }
                        }
                    }
                }
                _ => {}
            }
        });
    });
    out
}

// ---------------------------------------------------------------------
// Batch 2 lints.
// ---------------------------------------------------------------------

fn lint_bool_literal_in_condition(sf: &SourceFile) -> Vec<Finding> {
    let mut out = Vec::new();
    each_fn_body(sf, |body| {
        walk_expr(body, &mut |expr| {
            let check = |cond: &Expr, kw: &'static str, out: &mut Vec<Finding>| {
                if let ExprKind::Literal(Literal::Bool(b)) = &cond.kind {
                    out.push((
                        cond.span,
                        format!("{kw} condition is the literal `{b}`"),
                        Some(format!("drop the `{kw}` or replace with a real predicate")),
                    ));
                }
            };
            if let ExprKind::If { condition, .. } = &expr.kind {
                check(condition, "if", &mut out);
            }
            if let ExprKind::While { condition, .. } = &expr.kind {
                check(condition, "while", &mut out);
            }
        });
    });
    out
}

fn lint_let_and_return(sf: &SourceFile) -> Vec<Finding> {
    let mut out = Vec::new();
    each_fn_body(sf, |body| {
        walk_expr(body, &mut |expr| {
            let ExprKind::Block(block) = &expr.kind else {
                return;
            };
            let Some(tail) = block.tail.as_deref() else {
                return;
            };
            let ExprKind::Path(path) = &tail.kind else {
                return;
            };
            let Some(last) = block.stmts.last() else {
                return;
            };
            let StmtKind::Let { pattern, .. } = &last.kind else {
                return;
            };
            let PatternKind::Ident { name, .. } = &pattern.kind else {
                return;
            };
            if path.segments.len() == 1
                && path.segments.first().is_some_and(|s| s.name.name == name.name)
            {
                out.push((
                    last.span,
                    format!("`let {0} = ...; {0}` can be collapsed to the expression", name.name),
                    Some("return the value directly instead of binding it first".to_string()),
                ));
            }
        });
    });
    out
}

fn lint_collapsible_if(sf: &SourceFile) -> Vec<Finding> {
    let mut out = Vec::new();
    each_fn_body(sf, |body| {
        walk_expr(body, &mut |expr| {
            let ExprKind::If {
                then_branch,
                else_branch: None,
                ..
            } = &expr.kind
            else {
                return;
            };
            let ExprKind::Block(block) = &then_branch.kind else {
                return;
            };
            if !block.stmts.is_empty() {
                return;
            }
            let Some(inner) = block.tail.as_deref() else {
                return;
            };
            if let ExprKind::If {
                else_branch: None, ..
            } = &inner.kind
            {
                out.push((
                    expr.span,
                    "collapsible `if` — the outer and inner branches can be combined with `&&`"
                        .to_string(),
                    Some("rewrite as `if outer && inner { ... }`".to_string()),
                ));
            }
        });
    });
    out
}

fn lint_if_same_then_else(sf: &SourceFile) -> Vec<Finding> {
    let mut out = Vec::new();
    each_fn_body(sf, |body| {
        walk_expr(body, &mut |expr| {
            let ExprKind::If {
                then_branch,
                else_branch: Some(else_branch),
                ..
            } = &expr.kind
            else {
                return;
            };
            if spans_equal_text(then_branch.span, else_branch.span) {
                out.push((
                    expr.span,
                    "both branches of the `if` have identical bodies".to_string(),
                    Some("drop the `if` and keep the body once".to_string()),
                ));
            }
        });
    });
    out
}

fn spans_equal_text(a: Span, b: Span) -> bool {
    a.end - a.start == b.end - b.start
}

fn lint_redundant_field_init(sf: &SourceFile) -> Vec<Finding> {
    let mut out = Vec::new();
    each_fn_body(sf, |body| {
        walk_expr(body, &mut |expr| {
            let ExprKind::Struct { fields, .. } = &expr.kind else {
                return;
            };
            for field in fields {
                let Some(value) = &field.value else { continue };
                let ExprKind::Path(path) = &value.kind else { continue };
                if path.segments.len() == 1
                    && path.segments.first().is_some_and(|s| s.name.name == field.name.name)
                {
                    out.push((
                        value.span,
                        format!("redundant `{0}: {0}` — field shorthand suffices", field.name.name),
                        Some("drop the `: <name>` — shorthand `{ x, y }` works".to_string()),
                    ));
                }
            }
        });
    });
    out
}

fn lint_needless_else_after_return(sf: &SourceFile) -> Vec<Finding> {
    let mut out = Vec::new();
    each_fn_body(sf, |body| {
        walk_expr(body, &mut |expr| {
            let ExprKind::If {
                then_branch,
                else_branch: Some(else_branch),
                ..
            } = &expr.kind
            else {
                return;
            };
            if block_ends_in_return(then_branch) {
                out.push((
                    else_branch.span,
                    "needless `else` after a returning `if`".to_string(),
                    Some("un-nest the else body — the fallthrough is already unreachable".to_string()),
                ));
            }
        });
    });
    out
}

fn block_ends_in_return(branch: &Expr) -> bool {
    let ExprKind::Block(block) = &branch.kind else {
        return matches!(&branch.kind, ExprKind::Return(_));
    };
    if let Some(tail) = &block.tail {
        if matches!(&tail.kind, ExprKind::Return(_)) {
            return true;
        }
    }
    if let Some(last) = block.stmts.last() {
        if let StmtKind::Expr { expr, .. } = &last.kind {
            if matches!(&expr.kind, ExprKind::Return(_)) {
                return true;
            }
        }
    }
    false
}

fn lint_self_compare(sf: &SourceFile) -> Vec<Finding> {
    let mut out = Vec::new();
    each_fn_body(sf, |body| {
        walk_expr(body, &mut |expr| {
            let ExprKind::Binary { op, lhs, rhs } = &expr.kind else {
                return;
            };
            if !matches!(
                op,
                BinaryOp::Eq | BinaryOp::Ne | BinaryOp::Lt | BinaryOp::Le | BinaryOp::Gt | BinaryOp::Ge
            ) {
                return;
            }
            let (ExprKind::Path(lhs_path), ExprKind::Path(rhs_path)) = (&lhs.kind, &rhs.kind) else {
                return;
            };
            if path_text(lhs_path) == path_text(rhs_path) {
                out.push((
                    expr.span,
                    "comparing a value to itself — the result is a constant".to_string(),
                    Some("use the constant `true` / `false` directly if that is what you meant".to_string()),
                ));
            }
        });
    });
    out
}

fn path_text(path: &PathExpr) -> String {
    path.segments
        .iter()
        .map(|s| s.name.name.clone())
        .collect::<Vec<_>>()
        .join("::")
}

fn lint_identity_op(sf: &SourceFile) -> Vec<Finding> {
    let mut out = Vec::new();
    each_fn_body(sf, |body| {
        walk_expr(body, &mut |expr| {
            let ExprKind::Binary { op, lhs, rhs } = &expr.kind else {
                return;
            };
            let lhs_zero = is_int_literal(lhs, 0);
            let rhs_zero = is_int_literal(rhs, 0);
            let lhs_one = is_int_literal(lhs, 1);
            let rhs_one = is_int_literal(rhs, 1);
            let hit = match op {
                BinaryOp::Add | BinaryOp::Sub => rhs_zero || (matches!(op, BinaryOp::Add) && lhs_zero),
                BinaryOp::Mul | BinaryOp::Div => rhs_one || (matches!(op, BinaryOp::Mul) && lhs_one),
                _ => false,
            };
            if hit {
                out.push((
                    expr.span,
                    "arithmetic identity — the result is the other operand".to_string(),
                    Some("drop the redundant operation".to_string()),
                ));
            }
        });
    });
    out
}

fn is_int_literal(expr: &Expr, target: i128) -> bool {
    let ExprKind::Literal(Literal::Int(src)) = &expr.kind else {
        return false;
    };
    let stripped = src.trim_end_matches(|c: char| c.is_ascii_alphabetic() || c == '_');
    let clean: String = stripped.chars().filter(|c| *c != '_').collect();
    if let Ok(n) = clean.parse::<i128>() {
        return n == target;
    }
    if let Some(hex) = clean.strip_prefix("0x").or_else(|| clean.strip_prefix("0X")) {
        return i128::from_str_radix(hex, 16).ok() == Some(target);
    }
    if let Some(oct) = clean.strip_prefix("0o").or_else(|| clean.strip_prefix("0O")) {
        return i128::from_str_radix(oct, 8).ok() == Some(target);
    }
    if let Some(bin) = clean.strip_prefix("0b").or_else(|| clean.strip_prefix("0B")) {
        return i128::from_str_radix(bin, 2).ok() == Some(target);
    }
    false
}

fn lint_unit_let(sf: &SourceFile) -> Vec<Finding> {
    let mut out = Vec::new();
    each_fn_body(sf, |body| {
        let Some(block) = as_block_ref(body) else { return };
        for stmt in &block.stmts {
            let StmtKind::Let {
                init: Some(init), ..
            } = &stmt.kind
            else {
                continue;
            };
            if let ExprKind::Tuple(elems) = &init.kind {
                if elems.is_empty() {
                    out.push((
                        stmt.span,
                        "binding the unit value `()` — probably not intended".to_string(),
                        Some("drop the `let` and keep the expression".to_string()),
                    ));
                }
            }
        }
    });
    out
}

fn as_block_ref(expr: &Expr) -> Option<&Block> {
    if let ExprKind::Block(b) = &expr.kind {
        Some(b)
    } else {
        None
    }
}

fn lint_float_eq_zero(sf: &SourceFile) -> Vec<Finding> {
    let mut out = Vec::new();
    each_fn_body(sf, |body| {
        walk_expr(body, &mut |expr| {
            let ExprKind::Binary { op, lhs, rhs } = &expr.kind else {
                return;
            };
            if !matches!(op, BinaryOp::Eq | BinaryOp::Ne) {
                return;
            }
            let float_zero = |e: &Expr| matches!(&e.kind, ExprKind::Literal(Literal::Float { .. }));
            if float_zero(lhs) || float_zero(rhs) {
                out.push((
                    expr.span,
                    "equality against a float literal is almost never what you want".to_string(),
                    Some("compare `(x - y).abs() < eps` with an explicit tolerance".to_string()),
                ));
            }
        });
    });
    out
}

fn lint_empty_else(sf: &SourceFile) -> Vec<Finding> {
    let mut out = Vec::new();
    each_fn_body(sf, |body| {
        walk_expr(body, &mut |expr| {
            let ExprKind::If {
                else_branch: Some(else_branch),
                ..
            } = &expr.kind
            else {
                return;
            };
            if let ExprKind::Block(block) = &else_branch.kind {
                if block.stmts.is_empty() && block.tail.is_none() {
                    out.push((
                        else_branch.span,
                        "empty `else` block".to_string(),
                        Some("drop the `else` — an `if` without it is fine".to_string()),
                    ));
                }
            }
        });
    });
    out
}

fn lint_match_bool(sf: &SourceFile) -> Vec<Finding> {
    let mut out = Vec::new();
    each_fn_body(sf, |body| {
        walk_expr(body, &mut |expr| {
            let ExprKind::Match { arms, .. } = &expr.kind else {
                return;
            };
            if arms.len() != 2 {
                return;
            }
            let mut saw_true = false;
            let mut saw_false = false;
            for arm in arms {
                if let PatternKind::Literal(Literal::Bool(b)) = &arm.pattern.kind {
                    if *b {
                        saw_true = true;
                    } else {
                        saw_false = true;
                    }
                }
            }
            if saw_true && saw_false {
                out.push((
                    expr.span,
                    "`match` on a boolean is an `if` in disguise".to_string(),
                    Some("rewrite as `if cond { .. } else { .. }`".to_string()),
                ));
            }
        });
    });
    out
}

fn lint_needless_parens(sf: &SourceFile) -> Vec<Finding> {
    let mut out = Vec::new();
    each_fn_body(sf, |body| {
        walk_expr(body, &mut |expr| {
            let ExprKind::Tuple(elems) = &expr.kind else {
                return;
            };
            if elems.len() == 1 {
                out.push((
                    expr.span,
                    "`(x,)` is a one-tuple; `(x)` without the comma is a needless pair of parens"
                        .to_string(),
                    Some("drop the parens or add a trailing comma if you meant a one-tuple"
                        .to_string()),
                ));
            }
        });
    });
    out
}

fn lint_manual_not_equal(sf: &SourceFile) -> Vec<Finding> {
    let mut out = Vec::new();
    each_fn_body(sf, |body| {
        walk_expr(body, &mut |expr| {
            let ExprKind::Unary {
                op: UnaryOp::Not,
                operand,
            } = &expr.kind
            else {
                return;
            };
            let ExprKind::Binary {
                op: BinaryOp::Eq, ..
            } = &operand.kind
            else {
                return;
            };
            out.push((
                expr.span,
                "`!(a == b)` is just `a != b`".to_string(),
                Some("replace `!(... == ...)` with `... != ...`".to_string()),
            ));
        });
    });
    out
}

fn lint_nested_ternary_if(sf: &SourceFile) -> Vec<Finding> {
    let mut out = Vec::new();
    each_fn_body(sf, |body| {
        walk_expr(body, &mut |expr| {
            let ExprKind::If {
                else_branch: Some(else_branch),
                ..
            } = &expr.kind
            else {
                return;
            };
            let tail = match &else_branch.kind {
                ExprKind::Block(b) if b.stmts.is_empty() => b.tail.as_deref(),
                ExprKind::If { .. } => Some(else_branch.as_ref()),
                _ => None,
            };
            let Some(tail) = tail else { return };
            if let ExprKind::If {
                else_branch: Some(_), ..
            } = &tail.kind
            {
                if let ExprKind::If {
                    else_branch: Some(inner_else),
                    ..
                } = &tail.kind
                {
                    if matches!(&inner_else.kind, ExprKind::If { else_branch: Some(_), .. }) {
                        out.push((
                            expr.span,
                            "deeply nested `if/else if` chain reads better as `match`".to_string(),
                            Some("convert to `match` on the discriminant".to_string()),
                        ));
                    }
                }
            }
        });
    });
    out
}

// ---------------------------------------------------------------------
// Batch 3 lints.
// ---------------------------------------------------------------------

fn lint_absurd_range(sf: &SourceFile) -> Vec<Finding> {
    let mut out = Vec::new();
    each_fn_body(sf, |body| {
        walk_expr(body, &mut |expr| {
            let ExprKind::Range { start: Some(start), end: Some(end), .. } = &expr.kind else {
                return;
            };
            let (ExprKind::Literal(Literal::Int(a)), ExprKind::Literal(Literal::Int(b))) =
                (&start.kind, &end.kind)
            else {
                return;
            };
            let (Some(a), Some(b)) = (parse_int(a), parse_int(b)) else {
                return;
            };
            if a > b {
                out.push((
                    expr.span,
                    format!("absurd range: `{a}..{b}` is empty at runtime"),
                    Some("swap the bounds or double-check the intent".to_string()),
                ));
            }
        });
    });
    out
}

fn parse_int(src: &str) -> Option<i128> {
    let stripped = src.trim_end_matches(|c: char| c.is_ascii_alphabetic() || c == '_');
    let clean: String = stripped.chars().filter(|c| *c != '_').collect();
    clean.parse::<i128>().ok()
}

fn lint_string_literal_concat(sf: &SourceFile) -> Vec<Finding> {
    let mut out = Vec::new();
    each_fn_body(sf, |body| {
        walk_expr(body, &mut |expr| {
            let ExprKind::Binary { op: BinaryOp::Add, lhs, rhs } = &expr.kind else {
                return;
            };
            if matches!(&lhs.kind, ExprKind::Literal(Literal::String(_)))
                && matches!(&rhs.kind, ExprKind::Literal(Literal::String(_)))
            {
                out.push((
                    expr.span,
                    "concatenating two string literals — merge them at the source level"
                        .to_string(),
                    Some("write the combined literal directly".to_string()),
                ));
            }
        });
    });
    out
}

fn lint_chained_negation_literals(sf: &SourceFile) -> Vec<Finding> {
    let mut out = Vec::new();
    each_fn_body(sf, |body| {
        walk_expr(body, &mut |expr| {
            let ExprKind::Unary { op: UnaryOp::Neg, operand } = &expr.kind else {
                return;
            };
            if matches!(&operand.kind, ExprKind::Unary { op: UnaryOp::Neg, .. }) {
                out.push((
                    expr.span,
                    "`-(-x)` collapses to `x`".to_string(),
                    Some("drop the double negation".to_string()),
                ));
            }
        });
    });
    out
}

fn lint_if_not_else(sf: &SourceFile) -> Vec<Finding> {
    let mut out = Vec::new();
    each_fn_body(sf, |body| {
        walk_expr(body, &mut |expr| {
            let ExprKind::If { condition, else_branch: Some(_), .. } = &expr.kind else {
                return;
            };
            if matches!(&condition.kind, ExprKind::Unary { op: UnaryOp::Not, .. }) {
                out.push((
                    expr.span,
                    "`if !cond { A } else { B }` reads better as `if cond { B } else { A }`"
                        .to_string(),
                    Some("flip the branches and drop the leading `!`".to_string()),
                ));
            }
        });
    });
    out
}

fn lint_empty_string_concat(sf: &SourceFile) -> Vec<Finding> {
    let mut out = Vec::new();
    each_fn_body(sf, |body| {
        walk_expr(body, &mut |expr| {
            let ExprKind::Binary { op: BinaryOp::Add, lhs, rhs } = &expr.kind else {
                return;
            };
            let is_empty = |e: &Expr| matches!(&e.kind, ExprKind::Literal(Literal::String(s)) if s.is_empty());
            if is_empty(lhs) || is_empty(rhs) {
                out.push((
                    expr.span,
                    "concatenating an empty string literal does nothing".to_string(),
                    Some("drop the `\"\" +` or `+ \"\"`".to_string()),
                ));
            }
        });
    });
    out
}

fn lint_println_newline_only(sf: &SourceFile) -> Vec<Finding> {
    let mut out = Vec::new();
    each_fn_body(sf, |body| {
        walk_expr(body, &mut |expr| {
            let ExprKind::Call { callee, args } = &expr.kind else {
                return;
            };
            let ExprKind::Path(path) = &callee.kind else { return };
            let Some(last) = path.segments.last() else { return };
            if !matches!(last.name.name.as_str(), "println" | "print") {
                return;
            }
            let Some(first) = args.first() else { return };
            if let ExprKind::Literal(Literal::String(s)) = &first.kind {
                if s == "\n" || s.is_empty() {
                    out.push((
                        expr.span,
                        "`println(\"\")` / `println(\"\\n\")` is the same as `println(\"\")`"
                            .to_string(),
                        Some("call `println(\"\")` once with no argument-side whitespace".to_string()),
                    ));
                }
            }
        });
    });
    out
}

fn lint_match_same_arms(sf: &SourceFile) -> Vec<Finding> {
    let mut out = Vec::new();
    each_fn_body(sf, |body| {
        walk_expr(body, &mut |expr| {
            let ExprKind::Match { arms, .. } = &expr.kind else {
                return;
            };
            for i in 0..arms.len() {
                for j in (i + 1)..arms.len() {
                    let a = &arms[i].body;
                    let b = &arms[j].body;
                    if a.span.end - a.span.start == b.span.end - b.span.start
                        && a.span.end - a.span.start > 0
                    {
                        out.push((
                            arms[j].body.span,
                            "match arm has the same body as an earlier arm".to_string(),
                            Some("merge with `|` alternation or extract the shared body".to_string()),
                        ));
                        break;
                    }
                }
            }
        });
    });
    out
}

// ---------------------------------------------------------------------
// Batch 4 lints.
// ---------------------------------------------------------------------

fn lint_manual_swap(sf: &SourceFile) -> Vec<Finding> {
    let mut out = Vec::new();
    each_fn_body(sf, |body| {
        let Some(block) = as_block_ref(body) else { return };
        let stmts = &block.stmts;
        for i in 0..stmts.len().saturating_sub(2) {
            let (s1, s2, s3) = (&stmts[i], &stmts[i + 1], &stmts[i + 2]);
            let StmtKind::Let { pattern: tmp_pat, init: Some(tmp_init), .. } = &s1.kind else {
                continue;
            };
            let Some((tmp_name, _, _)) = ident_name_of(tmp_pat) else { continue };
            let ExprKind::Path(tmp_rhs) = &tmp_init.kind else { continue };
            let StmtKind::Expr { expr: assign1, .. } = &s2.kind else { continue };
            let StmtKind::Expr { expr: assign2, .. } = &s3.kind else { continue };
            let ExprKind::Assign { op: AssignOp::Assign, place: p1, value: v1 } = &assign1.kind else {
                continue;
            };
            let ExprKind::Assign { op: AssignOp::Assign, place: p2, value: v2 } = &assign2.kind else {
                continue;
            };
            let (ExprKind::Path(p1), ExprKind::Path(v1), ExprKind::Path(p2), ExprKind::Path(v2)) =
                (&p1.kind, &v1.kind, &p2.kind, &v2.kind)
            else {
                continue;
            };
            if path_text(p1) == path_text(tmp_rhs)
                && path_text(v1) == path_text(p2)
                && path_text(v2) == tmp_name
            {
                out.push((
                    s1.span,
                    "manual swap via a temporary — prefer an explicit `(a, b) = (b, a)` tuple destructuring when it lands, or at least document the intent"
                        .to_string(),
                    Some("three-line temporary swaps are a common refactor residue".to_string()),
                ));
            }
        }
    });
    out
}

fn ident_name_of(pattern: &Pattern) -> Option<(String, Span, Mutability)> {
    if let PatternKind::Ident { mutability, name, .. } = &pattern.kind {
        Some((name.name.clone(), pattern.span, *mutability))
    } else {
        None
    }
}

fn lint_consecutive_assignment(sf: &SourceFile) -> Vec<Finding> {
    let mut out = Vec::new();
    each_fn_body(sf, |body| {
        let Some(block) = as_block_ref(body) else { return };
        let stmts = &block.stmts;
        for i in 0..stmts.len().saturating_sub(1) {
            let (StmtKind::Expr { expr: e1, .. }, StmtKind::Expr { expr: e2, .. }) =
                (&stmts[i].kind, &stmts[i + 1].kind)
            else {
                continue;
            };
            let (ExprKind::Assign { op: o1, place: p1, value: v1 },
                 ExprKind::Assign { op: o2, place: p2, value: v2 }) = (&e1.kind, &e2.kind)
            else {
                continue;
            };
            if !matches!(o1, AssignOp::Assign) || !matches!(o2, AssignOp::Assign) {
                continue;
            }
            let (ExprKind::Path(p1_path), ExprKind::Path(p2_path)) = (&p1.kind, &p2.kind) else {
                continue;
            };
            if path_text(p1_path) != path_text(p2_path) {
                continue;
            }
            if e1.span.end - e1.span.start == e2.span.end - e2.span.start {
                out.push((
                    stmts[i + 1].span,
                    "two back-to-back assignments to the same place — the first one is dead"
                        .to_string(),
                    Some("drop the earlier assignment or consolidate the logic".to_string()),
                ));
            }
            let _ = (v1, v2);
        }
    });
    out
}

fn lint_large_unreadable_literal(sf: &SourceFile) -> Vec<Finding> {
    let mut out = Vec::new();
    each_fn_body(sf, |body| {
        walk_expr(body, &mut |expr| {
            let ExprKind::Literal(Literal::Int(src)) = &expr.kind else { return };
            if src.contains('_') {
                return;
            }
            if src.starts_with("0x") || src.starts_with("0b") || src.starts_with("0o") {
                return;
            }
            let digits_only = src.trim_end_matches(|c: char| c.is_ascii_alphabetic());
            if digits_only.chars().filter(char::is_ascii_digit).count() >= 5 {
                out.push((
                    expr.span,
                    format!("large integer literal `{src}` is hard to scan without underscores"),
                    Some(format!(
                        "insert `_` as thousands separators, e.g. `{}`",
                        humanize_int_literal(src)
                    )),
                ));
            }
        });
    });
    out
}

fn humanize_int_literal(src: &str) -> String {
    let suffix_start = src
        .find(|c: char| c.is_ascii_alphabetic())
        .unwrap_or(src.len());
    let (digits, suffix) = src.split_at(suffix_start);
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    let rev: Vec<char> = digits.chars().rev().collect();
    for (i, c) in rev.iter().enumerate() {
        if i > 0 && i % 3 == 0 {
            grouped.push('_');
        }
        grouped.push(*c);
    }
    let forward: String = grouped.chars().rev().collect();
    format!("{forward}{suffix}")
}

fn lint_redundant_closure(sf: &SourceFile) -> Vec<Finding> {
    let mut out = Vec::new();
    each_fn_body(sf, |body| {
        walk_expr(body, &mut |expr| {
            let ExprKind::Closure { params, body: closure_body, .. } = &expr.kind else {
                return;
            };
            if params.len() != 1 {
                return;
            }
            let ExprKind::Call { callee, args } = &closure_body.kind else { return };
            if args.len() != 1 {
                return;
            }
            let ExprKind::Path(arg_path) = &args[0].kind else { return };
            if arg_path.segments.len() != 1 {
                return;
            }
            let Some((param_name, _, _)) = ident_name_of(&params[0].pattern) else { return };
            if arg_path
                .segments
                .first()
                .is_some_and(|s| s.name.name == param_name)
            {
                let _ = callee;
                out.push((
                    expr.span,
                    "closure `|x| f(x)` can be replaced with `f`".to_string(),
                    Some("pass the function directly".to_string()),
                ));
            }
        });
    });
    out
}

fn lint_empty_if_body(sf: &SourceFile) -> Vec<Finding> {
    let mut out = Vec::new();
    each_fn_body(sf, |body| {
        walk_expr(body, &mut |expr| {
            let ExprKind::If { then_branch, else_branch, .. } = &expr.kind else {
                return;
            };
            if else_branch.is_none() {
                return;
            }
            let ExprKind::Block(block) = &then_branch.kind else { return };
            if block.stmts.is_empty() && block.tail.is_none() {
                out.push((
                    then_branch.span,
                    "empty `then` branch with a non-empty `else` — invert the condition and drop the `else`".to_string(),
                    Some("rewrite as `if !cond { <else body> }`".to_string()),
                ));
            }
        });
    });
    out
}

fn lint_bool_to_int_match(sf: &SourceFile) -> Vec<Finding> {
    let mut out = Vec::new();
    each_fn_body(sf, |body| {
        walk_expr(body, &mut |expr| {
            let ExprKind::Match { arms, .. } = &expr.kind else { return };
            if arms.len() != 2 {
                return;
            }
            let (Some(t_arm), Some(f_arm)) = (
                arms.iter().find(|a| matches!(a.pattern.kind, PatternKind::Literal(Literal::Bool(true)))),
                arms.iter().find(|a| matches!(a.pattern.kind, PatternKind::Literal(Literal::Bool(false)))),
            ) else {
                return;
            };
            if matches!(t_arm.body.kind, ExprKind::Literal(Literal::Int(_)))
                && matches!(f_arm.body.kind, ExprKind::Literal(Literal::Int(_)))
            {
                out.push((
                    expr.span,
                    "`match b { true => 1, false => 0 }` — use `if b { 1 } else { 0 }` or a numeric cast when the language exposes one".to_string(),
                    Some("a direct `if` is shorter and intent is clearer".to_string()),
                ));
            }
        });
    });
    out
}

fn lint_fn_returns_unit_explicit(sf: &SourceFile) -> Vec<Finding> {
    let mut out = Vec::new();
    for item in &sf.items {
        let ItemKind::Fn(func) = &item.kind else { continue };
        let Some(ret) = &func.ret else { continue };
        if matches!(ret.kind, gossamer_ast::TypeKind::Unit) {
            out.push((
                ret.span,
                format!("`fn {}() -> ()` is the same as `fn {}()`", func.name.name, func.name.name),
                Some("drop the explicit `-> ()` return type".to_string()),
            ));
        }
    }
    out
}

fn lint_let_with_unit_type(sf: &SourceFile) -> Vec<Finding> {
    let mut out = Vec::new();
    each_fn_body(sf, |body| {
        let Some(block) = as_block_ref(body) else { return };
        for stmt in &block.stmts {
            let StmtKind::Let { ty: Some(ty), .. } = &stmt.kind else { continue };
            if matches!(ty.kind, gossamer_ast::TypeKind::Unit) {
                out.push((
                    stmt.span,
                    "`let _: () = ...` binding carries the unit type explicitly — almost never useful".to_string(),
                    Some("drop the `: ()` annotation".to_string()),
                ));
            }
        }
    });
    out
}

fn lint_useless_default_only_match(sf: &SourceFile) -> Vec<Finding> {
    let mut out = Vec::new();
    each_fn_body(sf, |body| {
        walk_expr(body, &mut |expr| {
            let ExprKind::Match { arms, .. } = &expr.kind else { return };
            if arms.len() != 1 {
                return;
            }
            if matches!(arms[0].pattern.kind, PatternKind::Wildcard)
                && arms[0].guard.is_none()
            {
                out.push((
                    expr.span,
                    "`match x { _ => expr }` discards `x` — the scrutinee is evaluated but never inspected".to_string(),
                    Some("drop the match and just evaluate `expr` (plus `let _ = x` if the side effect matters)".to_string()),
                ));
            }
        });
    });
    out
}

fn lint_unnecessary_parens_in_condition(sf: &SourceFile) -> Vec<Finding> {
    let mut out = Vec::new();
    each_fn_body(sf, |body| {
        walk_expr(body, &mut |expr| {
            let check = |cond: &Expr, kw: &'static str, out: &mut Vec<Finding>| {
                if let ExprKind::Tuple(elems) = &cond.kind {
                    if elems.len() == 1 {
                        out.push((
                            cond.span,
                            format!("needless parens around `{kw}` condition"),
                            Some(format!("drop the parens: `{kw} cond {{ … }}`")),
                        ));
                    }
                }
            };
            if let ExprKind::If { condition, .. } = &expr.kind {
                check(condition, "if", &mut out);
            }
            if let ExprKind::While { condition, .. } = &expr.kind {
                check(condition, "while", &mut out);
            }
        });
    });
    out
}

fn lint_pattern_matching_unit(sf: &SourceFile) -> Vec<Finding> {
    let mut out = Vec::new();
    each_fn_body(sf, |body| {
        walk_expr(body, &mut |expr| {
            let ExprKind::Match { scrutinee, .. } = &expr.kind else { return };
            if let ExprKind::Tuple(elems) = &scrutinee.kind {
                if elems.is_empty() {
                    out.push((
                        expr.span,
                        "matching on the unit value `()` — there is exactly one possible arm".to_string(),
                        Some("drop the match and run the body directly".to_string()),
                    ));
                }
            }
        });
    });
    out
}

fn lint_panic_without_message(sf: &SourceFile) -> Vec<Finding> {
    let mut out = Vec::new();
    each_fn_body(sf, |body| {
        walk_expr(body, &mut |expr| {
            let ExprKind::Call { callee, args } = &expr.kind else { return };
            if !args.is_empty() {
                return;
            }
            let ExprKind::Path(path) = &callee.kind else { return };
            let Some(last) = path.segments.last() else { return };
            if last.name.name == "panic" {
                out.push((
                    expr.span,
                    "`panic()` without a message — the post-mortem has nothing to go on".to_string(),
                    Some("pass a brief explanation as the first argument".to_string()),
                ));
            }
        });
    });
    out
}

fn lint_empty_loop(sf: &SourceFile) -> Vec<Finding> {
    let mut out = Vec::new();
    each_fn_body(sf, |body| {
        walk_expr(body, &mut |expr| {
            let ExprKind::Loop { body: loop_body, .. } = &expr.kind else { return };
            let ExprKind::Block(block) = &loop_body.kind else { return };
            if block.stmts.is_empty() && block.tail.is_none() {
                out.push((
                    expr.span,
                    "empty `loop {}` will busy-wait forever".to_string(),
                    Some("put a `break`, a `continue`, or replace with a real wait primitive".to_string()),
                ));
            }
        });
    });
    out
}
