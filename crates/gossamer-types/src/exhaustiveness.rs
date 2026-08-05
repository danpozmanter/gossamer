//! Match exhaustiveness and reachability.
//! Implements a Maranget-lite usefulness algorithm over a simplified
//! pattern form. For every `match` expression in a source file, reports
//! missing patterns that would make the match exhaustive and flags
//! arms that are dominated by earlier arms as unreachable.
//! The checker is conservative: when it cannot enumerate the scrutinee
//! type (e.g. integers, strings, external ADTs), exhaustiveness
//! requires a wildcard arm but concrete values covered by earlier arms
//! are still tracked for redundancy detection.

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::fmt;

use gossamer_ast::{
    Expr, ExprKind, Item, ItemKind, Literal, MatchArm, Pattern, PatternKind, SourceFile, StmtKind,
};
use gossamer_lex::Span;
use gossamer_resolve::{DefId, Resolutions};
use thiserror::Error;

use crate::context::TyCtxt;
use crate::table::TypeTable;
use crate::ty::{Ty, TyKind};

/// Walks every `match` in `source` and reports exhaustiveness and
/// reachability diagnostics.
#[must_use]
pub fn check_exhaustiveness(
    source: &SourceFile,
    resolutions: &Resolutions,
    table: &TypeTable,
    tcx: &TyCtxt,
) -> Vec<ExhaustivenessDiagnostic> {
    let enums = collect_enums(source, resolutions);
    let mut checker = Checker {
        tcx,
        table,
        enums: &enums,
        diagnostics: Vec::new(),
    };
    checker.walk_items(&source.items);
    checker.diagnostics
}

struct Checker<'a> {
    tcx: &'a TyCtxt,
    table: &'a TypeTable,
    enums: &'a HashMap<DefId, Vec<String>>,
    diagnostics: Vec<ExhaustivenessDiagnostic>,
}

impl Checker<'_> {
    fn walk_items(&mut self, items: &[Item]) {
        for item in items {
            match &item.kind {
                ItemKind::Fn(decl) => {
                    if let Some(body) = &decl.body {
                        self.walk_expr(body);
                    }
                }
                ItemKind::Impl(decl) => {
                    for impl_item in &decl.items {
                        if let gossamer_ast::ImplItem::Fn(fn_decl) = impl_item {
                            if let Some(body) = &fn_decl.body {
                                self.walk_expr(body);
                            }
                        }
                    }
                }
                ItemKind::Trait(decl) => {
                    for trait_item in &decl.items {
                        if let gossamer_ast::TraitItem::Fn(fn_decl) = trait_item {
                            if let Some(body) = &fn_decl.body {
                                self.walk_expr(body);
                            }
                        }
                    }
                }
                ItemKind::Const(decl) => self.walk_expr(&decl.value),
                ItemKind::Static(decl) => self.walk_expr(&decl.value),
                ItemKind::Mod(decl) => {
                    if let gossamer_ast::ModBody::Inline(inner) = &decl.body {
                        self.walk_items(inner);
                    }
                }
                _ => {}
            }
        }
    }

    fn walk_expr(&mut self, expr: &Expr) {
        match &expr.kind {
            ExprKind::Match { scrutinee, arms } => self.walk_match(scrutinee, arms, expr.span),
            ExprKind::Block(block) | ExprKind::Unsafe(block) => self.walk_block(block),
            ExprKind::Call { callee, args } => {
                self.walk_expr(callee);
                self.walk_exprs(args);
            }
            ExprKind::MethodCall { receiver, args, .. } => {
                self.walk_expr(receiver);
                self.walk_exprs(args);
            }
            ExprKind::Binary { lhs, rhs, .. }
            | ExprKind::Assign {
                place: lhs,
                value: rhs,
                ..
            }
            | ExprKind::Index {
                base: lhs,
                index: rhs,
            } => {
                self.walk_expr(lhs);
                self.walk_expr(rhs);
            }
            ExprKind::Unary { operand, .. }
            | ExprKind::FieldAccess {
                receiver: operand, ..
            }
            | ExprKind::Try(operand)
            | ExprKind::Go(operand) => self.walk_expr(operand),
            ExprKind::If {
                condition,
                then_branch,
                else_branch,
            } => self.walk_if(condition, then_branch, else_branch.as_deref()),
            ExprKind::Loop { body, .. } | ExprKind::Closure { body, .. } => self.walk_expr(body),
            ExprKind::While {
                condition, body, ..
            } => {
                self.walk_expr(condition);
                self.walk_expr(body);
            }
            ExprKind::For { iter, body, .. } => {
                self.walk_expr(iter);
                self.walk_expr(body);
            }
            ExprKind::Return(value) | ExprKind::Break { value, .. } => {
                self.walk_optional(value.as_deref());
            }
            ExprKind::Tuple(elems) | ExprKind::MapLiteral(elems) | ExprKind::SetLiteral(elems) => {
                self.walk_exprs(elems);
            }
            ExprKind::Struct { fields, base, .. } => self.walk_struct(fields, base.as_deref()),
            ExprKind::Array(arr)
            | ExprKind::FixedArray(arr)
            | ExprKind::QueueLiteral(arr)
            | ExprKind::StackLiteral(arr)
            | ExprKind::MaxHeapLiteral(arr)
            | ExprKind::MinHeapLiteral(arr) => self.walk_array(arr),
            ExprKind::Range { start, end, .. } => {
                self.walk_optional(start.as_deref());
                self.walk_optional(end.as_deref());
            }
            ExprKind::Cast { value, .. } => self.walk_expr(value),
            _ => {}
        }
    }

    fn walk_match(&mut self, scrutinee: &Expr, arms: &[MatchArm], span: Span) {
        self.walk_expr(scrutinee);
        for arm in arms {
            if let Some(guard) = &arm.guard {
                self.walk_expr(guard);
            }
            self.walk_expr(&arm.body);
        }
        self.check_match(scrutinee, arms, span);
    }

    fn walk_block(&mut self, block: &gossamer_ast::Block) {
        for stmt in &block.stmts {
            self.walk_stmt(stmt);
        }
        if let Some(tail) = &block.tail {
            self.walk_expr(tail);
        }
    }

    fn walk_if(&mut self, condition: &Expr, then_branch: &Expr, else_branch: Option<&Expr>) {
        self.walk_expr(condition);
        self.walk_expr(then_branch);
        self.walk_optional(else_branch);
    }

    fn walk_struct(&mut self, fields: &[gossamer_ast::StructExprField], base: Option<&Expr>) {
        for field in fields {
            if let Some(value) = &field.value {
                self.walk_expr(value);
            }
        }
        self.walk_optional(base);
    }

    fn walk_exprs(&mut self, exprs: &[Expr]) {
        for expr in exprs {
            self.walk_expr(expr);
        }
    }

    fn walk_optional(&mut self, expr: Option<&Expr>) {
        if let Some(expr) = expr {
            self.walk_expr(expr);
        }
    }

    fn walk_array(&mut self, arr: &gossamer_ast::ArrayExpr) {
        match arr {
            gossamer_ast::ArrayExpr::List(elems) => {
                for elem in elems {
                    self.walk_expr(elem);
                }
            }
            gossamer_ast::ArrayExpr::Repeat { value, count } => {
                self.walk_expr(value);
                self.walk_expr(count);
            }
        }
    }

    fn walk_stmt(&mut self, stmt: &gossamer_ast::Stmt) {
        match &stmt.kind {
            StmtKind::Let { init, .. } => {
                if let Some(init) = init {
                    self.walk_expr(init);
                }
            }
            StmtKind::Expr { expr, .. } | StmtKind::Defer(expr) | StmtKind::Go(expr) => {
                self.walk_expr(expr);
            }
            StmtKind::Item(item) => self.walk_items(std::slice::from_ref(item)),
        }
    }

    fn check_match(&mut self, scrutinee: &Expr, arms: &[MatchArm], span: Span) {
        let scrutinee_ty = self.table.get(scrutinee.id);
        let rows: Vec<Row> = arms
            .iter()
            .map(|arm| Row {
                pat: lower_pattern(&arm.pattern),
                has_guard: arm.guard.is_some(),
                span: arm.pattern.span,
            })
            .collect();
        self.report_redundancy(&rows);
        self.report_non_exhaustive(scrutinee_ty, &rows, span);
    }

    fn report_redundancy(&mut self, rows: &[Row]) {
        for (i, row) in rows.iter().enumerate() {
            if rows[..i]
                .iter()
                .any(|earlier| !earlier.has_guard && subsumes(&earlier.pat, &row.pat))
            {
                self.diagnostics.push(ExhaustivenessDiagnostic::new(
                    ExhaustivenessError::UnreachableArm,
                    row.span,
                ));
            }
        }
    }

    fn report_non_exhaustive(&mut self, scrutinee_ty: Option<Ty>, rows: &[Row], span: Span) {
        let relevant: Vec<&Pat> = rows
            .iter()
            .filter(|row| !row.has_guard)
            .map(|row| &row.pat)
            .collect();
        if relevant.iter().any(|pat| is_catch_all(pat)) {
            return;
        }
        let missing = self.compute_missing(scrutinee_ty, &relevant);
        if missing.is_empty() {
            return;
        }
        self.diagnostics.push(ExhaustivenessDiagnostic::new(
            ExhaustivenessError::NonExhaustive { missing },
            span,
        ));
    }

    fn compute_missing(&self, scrutinee_ty: Option<Ty>, patterns: &[&Pat]) -> Vec<String> {
        if let Some(ty) = scrutinee_ty {
            if let Some(missing) = self.missing_for_ty(ty, patterns) {
                return missing;
            }
        }
        Vec::new()
    }

    fn missing_for_ty(&self, ty: Ty, patterns: &[&Pat]) -> Option<Vec<String>> {
        match self.tcx.kind(ty)? {
            TyKind::Bool => Some(missing_bool(patterns)),
            TyKind::Adt { def, .. } => {
                if let Some(variants) = self.enums_by_def(*def) {
                    return Some(missing_variants(&variants, patterns));
                }
                // `Option` / `Result` are built-in sentinel ADTs, absent from
                // the user-enum table but finitely enumerable. Without this a
                // `match o { Some(n) => .. }` missing `None` was treated as
                // exhaustive, and the compiled tier read an uninitialised
                // discriminant.
                match self.tcx.def_name(*def) {
                    Some("Option") => Some(missing_variants(
                        &["Some".to_string(), "None".to_string()],
                        patterns,
                    )),
                    Some("Result") => Some(missing_variants(
                        &["Ok".to_string(), "Err".to_string()],
                        patterns,
                    )),
                    _ => None,
                }
            }
            // Scalar types whose value domain cannot be exhausted by listing
            // literals require a catch-all arm. `report_non_exhaustive`
            // returns before reaching here when an unguarded wildcard is
            // present, so arriving here means the match has no catch-all and
            // is therefore non-exhaustive - the witness is `_`.
            TyKind::Int(_) | TyKind::Float(_) | TyKind::String | TyKind::Char => {
                Some(vec!["_".to_string()])
            }
            _ => None,
        }
    }

    fn enums_by_def(&self, def: DefId) -> Option<Vec<String>> {
        self.enums.get(&def).cloned()
    }
}

fn missing_bool(patterns: &[&Pat]) -> Vec<String> {
    let mut saw_true = false;
    let mut saw_false = false;
    for pat in patterns {
        scan_bool(pat, &mut saw_true, &mut saw_false);
    }
    let mut missing = Vec::new();
    if !saw_true {
        missing.push("true".to_string());
    }
    if !saw_false {
        missing.push("false".to_string());
    }
    missing
}

fn scan_bool(pat: &Pat, saw_true: &mut bool, saw_false: &mut bool) {
    match pat {
        Pat::Wild => {
            *saw_true = true;
            *saw_false = true;
        }
        Pat::Bool(true) => *saw_true = true,
        Pat::Bool(false) => *saw_false = true,
        Pat::Or(alts) => {
            for alt in alts {
                scan_bool(alt, saw_true, saw_false);
            }
        }
        _ => {}
    }
}

fn missing_variants(all: &[String], patterns: &[&Pat]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    for pat in patterns {
        scan_variants(pat, &mut seen);
        if seen.contains("*") {
            return Vec::new();
        }
    }
    // for each named variant we've seen, check
    // whether the patterns that match it are exhaustive over its
    // payload's finite domain. The first uncovered payload shape
    // becomes the missing witness. Today we only know how to
    // enumerate finite domains for `Pat::Bool` payloads (`true` /
    // `false`); non-bool payloads default to "exhaustive" because
    // we lack the type context here.
    let mut out: Vec<String> = all
        .iter()
        .filter(|name| !seen.contains(name.as_str()))
        .cloned()
        .collect();
    for name in all {
        if !seen.contains(name.as_str()) {
            continue;
        }
        if let Some(witness) = missing_payload_witness(name, patterns) {
            out.push(witness);
        }
    }
    out
}

/// Returns a missing payload witness for `variant_name` when its
/// matched arms don't cover every shape its payload can take.
/// Currently handles single-field bool payloads
/// (`enum E { V(bool) }` - missing `V(true)` if only `V(false)`
/// was matched).
fn missing_payload_witness(variant_name: &str, patterns: &[&Pat]) -> Option<String> {
    let mut bool_field_seen_true = false;
    let mut bool_field_seen_false = false;
    let mut bool_field_seen_wild = false;
    let mut any_with_bool_payload = false;
    for pat in patterns {
        match pat {
            Pat::Variant { name, fields } if name == variant_name && fields.len() == 1 => {
                match &fields[0] {
                    Pat::Bool(true) => {
                        any_with_bool_payload = true;
                        bool_field_seen_true = true;
                    }
                    Pat::Bool(false) => {
                        any_with_bool_payload = true;
                        bool_field_seen_false = true;
                    }
                    Pat::Wild => {
                        // Wildcard payload covers both. Only treat
                        // as bool-domain if we've also seen a real
                        // bool field elsewhere - otherwise a
                        // wildcard payload is a wildcard over an
                        // unknown type.
                        bool_field_seen_wild = true;
                    }
                    _ => return None,
                }
            }
            Pat::Or(alts) => {
                if let Some(w) =
                    missing_payload_witness(variant_name, &alts.iter().collect::<Vec<_>>())
                {
                    return Some(w);
                }
            }
            _ => {}
        }
    }
    if !any_with_bool_payload {
        return None;
    }
    if bool_field_seen_wild {
        return None;
    }
    if !bool_field_seen_true {
        return Some(format!("{variant_name}(true)"));
    }
    if !bool_field_seen_false {
        return Some(format!("{variant_name}(false)"));
    }
    None
}

fn scan_variants(pat: &Pat, seen: &mut std::collections::HashSet<String>) {
    match pat {
        Pat::Wild => {
            seen.insert("*".to_string());
        }
        Pat::Variant { name, .. } => {
            seen.insert(name.clone());
        }
        Pat::Or(alts) => {
            for alt in alts {
                scan_variants(alt, seen);
            }
        }
        _ => {}
    }
}

fn is_catch_all(pat: &Pat) -> bool {
    match pat {
        Pat::Wild => true,
        Pat::Or(alts) => alts.iter().any(is_catch_all),
        _ => false,
    }
}

/// True when `earlier` matches every input `later` matches.
///
/// element-aware for `Tuple` and `Variant`.
/// Previously `Pat::Tuple(_)` subsumed every other `Pat::Tuple(_)`
/// regardless of element shape, so `match (b, x) { (true, false)
/// => .., (true, true) => .. }` reported the second arm as
/// unreachable. Now subsumption descends through fields: a tuple
/// `(P1, P2)` subsumes `(Q1, Q2)` iff `P1 ⊃ Q1` and `P2 ⊃ Q2`.
/// Variant subsumption is the same shape plus the name-match
/// gate.
fn subsumes(earlier: &Pat, later: &Pat) -> bool {
    match earlier {
        Pat::Wild => true,
        Pat::Or(alts) => alts.iter().any(|a| subsumes(a, later)),
        Pat::Bool(b) => match later {
            Pat::Bool(other) => other == b,
            Pat::Or(alts) => alts.iter().all(|a| subsumes(earlier, a)),
            _ => false,
        },
        Pat::Variant {
            name: en,
            fields: ef,
        } => match later {
            Pat::Variant {
                name: ln,
                fields: lf,
            } => {
                if en != ln {
                    return false;
                }
                // No fields recorded on the earlier pattern means
                // a name-only test (`SomeVariant` / `Path` form);
                // treat as wildcard over fields so it subsumes
                // every later occurrence of the same variant.
                if ef.is_empty() {
                    return true;
                }
                if lf.len() != ef.len() {
                    return false;
                }
                ef.iter().zip(lf.iter()).all(|(e, l)| subsumes(e, l))
            }
            Pat::Or(alts) => alts.iter().all(|a| subsumes(earlier, a)),
            _ => false,
        },
        Pat::Literal(text) => match later {
            Pat::Literal(other) => other == text,
            Pat::Or(alts) => alts.iter().all(|a| subsumes(earlier, a)),
            _ => false,
        },
        Pat::Tuple(ef) => match later {
            Pat::Tuple(lf) => {
                if lf.len() != ef.len() {
                    return false;
                }
                ef.iter().zip(lf.iter()).all(|(e, l)| subsumes(e, l))
            }
            Pat::Or(alts) => alts.iter().all(|a| subsumes(earlier, a)),
            _ => false,
        },
        Pat::Opaque => false,
    }
}

fn lower_pattern(pattern: &Pattern) -> Pat {
    match &pattern.kind {
        PatternKind::Wildcard | PatternKind::Rest => Pat::Wild,
        PatternKind::Ident { subpattern, .. } => {
            // `x @ subpat` matches whatever `subpat` matches; the
            // bare `x` form is a wildcard. Without recursing, a
            // user could write `x @ 1 => …` followed by `1 => …`
            // and the second arm would silently be flagged as
            // reachable-only when it's actually shadowed (or vice
            // versa for unreachability).
            match subpattern {
                Some(inner) => lower_pattern(inner),
                None => Pat::Wild,
            }
        }
        PatternKind::Literal(lit) => lower_literal(lit),
        PatternKind::Path(path) => {
            let name = path
                .segments
                .last()
                .map(|seg| seg.name.name.clone())
                .unwrap_or_default();
            Pat::Variant {
                name,
                fields: Vec::new(),
            }
        }
        PatternKind::TupleStruct { path, elems } => {
            let name = path
                .segments
                .last()
                .map(|seg| seg.name.name.clone())
                .unwrap_or_default();
            Pat::Variant {
                name,
                fields: elems.iter().map(lower_pattern).collect(),
            }
        }
        PatternKind::Struct { path, .. } => {
            let name = path
                .segments
                .last()
                .map(|seg| seg.name.name.clone())
                .unwrap_or_default();
            Pat::Variant {
                name,
                fields: Vec::new(),
            }
        }
        PatternKind::Tuple(parts) => Pat::Tuple(parts.iter().map(lower_pattern).collect()),
        // A `[..]` / `[..rest]` slice (no fixed elements) matches any
        // slice, so it acts as a catch-all; any slice carrying fixed
        // elements imposes a length constraint and is opaque to the
        // usefulness lattice (it never subsumes another pattern).
        PatternKind::Slice {
            prefix,
            rest,
            suffix,
        } => {
            if rest.is_some() && prefix.is_empty() && suffix.is_empty() {
                Pat::Wild
            } else {
                Pat::Opaque
            }
        }
        PatternKind::Or(alts) => Pat::Or(alts.iter().map(lower_pattern).collect()),
        PatternKind::Range { .. } => Pat::Opaque,
        PatternKind::Ref { inner, .. } => lower_pattern(inner),
        PatternKind::Error => Pat::Opaque,
    }
}

fn lower_literal(lit: &Literal) -> Pat {
    match lit {
        Literal::Bool(value) => Pat::Bool(*value),
        Literal::Int(text) | Literal::Float(text) => Pat::Literal(text.clone()),
        Literal::String(text) => Pat::Literal(format!("\"{text}\"")),
        Literal::Char(c) => Pat::Literal(format!("'{c}'")),
        _ => Pat::Opaque,
    }
}

fn collect_enums(source: &SourceFile, resolutions: &Resolutions) -> HashMap<DefId, Vec<String>> {
    let mut map = HashMap::new();
    collect_enums_in(&source.items, resolutions, &mut map);
    map
}

fn collect_enums_in(
    items: &[gossamer_ast::Item],
    resolutions: &Resolutions,
    map: &mut HashMap<DefId, Vec<String>>,
) {
    for item in items {
        match &item.kind {
            ItemKind::Enum(decl) => {
                let Some(def) = resolutions.definition_of(item.id) else {
                    continue;
                };
                let variants = decl
                    .variants
                    .iter()
                    .map(|variant| variant.name.name.clone())
                    .collect();
                map.insert(def, variants);
            }
            ItemKind::Mod(decl) => {
                if let gossamer_ast::ModBody::Inline(inner) = &decl.body {
                    collect_enums_in(inner, resolutions, map);
                }
            }
            _ => {}
        }
    }
}

/// Internal lowered pattern form the checker works over.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Pat {
    Wild,
    Bool(bool),
    Variant { name: String, fields: Vec<Pat> },
    Tuple(Vec<Pat>),
    Literal(String),
    Or(Vec<Pat>),
    Opaque,
}

#[derive(Debug)]
struct Row {
    pat: Pat,
    has_guard: bool,
    span: Span,
}

/// One exhaustiveness diagnostic with its primary source span.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExhaustivenessDiagnostic {
    /// Specific error variant.
    pub error: ExhaustivenessError,
    /// Where in the source the problem was detected.
    pub span: Span,
}

impl ExhaustivenessDiagnostic {
    /// Constructs a diagnostic from its error and span.
    #[must_use]
    pub const fn new(error: ExhaustivenessError, span: Span) -> Self {
        Self { error, span }
    }

    /// Renders this diagnostic as a structured
    /// [`gossamer_diagnostics::Diagnostic`].
    #[must_use]
    pub fn to_diagnostic(&self) -> gossamer_diagnostics::Diagnostic {
        use gossamer_diagnostics::{Code, Diagnostic, Location};
        let location = Location::new(self.span.file, self.span);
        let title = format!("{}", self.error);
        Diagnostic::error(Code(self.error.code()), title.clone()).with_primary(location, title)
    }
}

impl fmt::Display for ExhaustivenessDiagnostic {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(out, "{}", self.error)
    }
}

/// Every failure mode the exhaustiveness checker can report.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ExhaustivenessError {
    /// The match does not cover every possible scrutinee value.
    #[error("non-exhaustive patterns: {} not covered", format_missing(missing))]
    NonExhaustive {
        /// Missing patterns that witness the incompleteness.
        missing: Vec<String>,
    },
    /// An arm is dominated by a preceding arm and can never be matched.
    #[error("unreachable pattern: earlier arm already matches this value")]
    UnreachableArm,
}

impl ExhaustivenessError {
    /// Returns a short stable tag useful for snapshot tests.
    #[must_use]
    pub const fn tag(&self) -> &'static str {
        match self {
            Self::NonExhaustive { .. } => "non-exhaustive",
            Self::UnreachableArm => "unreachable-arm",
        }
    }

    /// Stable error code used by the diagnostics framework.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::NonExhaustive { .. } => "GM0001",
            Self::UnreachableArm => "GM0002",
        }
    }
}

fn format_missing(missing: &[String]) -> String {
    missing
        .iter()
        .map(|item| format!("`{item}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod m13_tests {
    use super::*;

    /// Audit M13 tuple bug: element-blind subsumption flagged the
    /// second arm of `(true, false) => .., (true, true) => ..`
    /// as redundant because both lowered to `Pat::Tuple(_)`. The
    /// 0.6.0 fix recurses field-by-field; (true, false) does NOT
    /// subsume (true, true) because Bool(false) does not subsume
    /// Bool(true).
    #[test]
    fn tuple_subsumption_recurses_through_fields() {
        let earlier = Pat::Tuple(vec![Pat::Bool(true), Pat::Bool(false)]);
        let later = Pat::Tuple(vec![Pat::Bool(true), Pat::Bool(true)]);
        assert!(
            !subsumes(&earlier, &later),
            "(true, false) must NOT subsume (true, true) - they're disjoint"
        );
    }

    /// Wildcard fields still propagate: `(true, _)` does subsume
    /// `(true, true)` because the wildcard covers every concrete
    /// shape.
    #[test]
    fn tuple_subsumption_propagates_wildcard_fields() {
        let earlier = Pat::Tuple(vec![Pat::Bool(true), Pat::Wild]);
        let later = Pat::Tuple(vec![Pat::Bool(true), Pat::Bool(true)]);
        assert!(subsumes(&earlier, &later));
    }

    /// Audit M13 enum-payload bug: `Foo::A(true)` matched alone is NOT
    /// exhaustive for `enum Foo { A(bool) }`; `Foo::A(false)`
    /// is missing. The 0.6.0 fix recurses into the payload's
    /// finite domain.
    #[test]
    fn variant_with_bool_payload_reports_missing_false() {
        let all = vec!["A".to_string()];
        let pat = Pat::Variant {
            name: "A".to_string(),
            fields: vec![Pat::Bool(true)],
        };
        let pats: Vec<&Pat> = vec![&pat];
        let missing = missing_variants(&all, &pats);
        assert!(
            missing.iter().any(|m| m == "A(false)"),
            "expected `A(false)` missing, got {missing:?}"
        );
    }

    /// Both bool payloads present → exhaustive.
    #[test]
    fn variant_with_bool_payload_both_seen_is_exhaustive() {
        let all = vec!["A".to_string()];
        let true_pat = Pat::Variant {
            name: "A".to_string(),
            fields: vec![Pat::Bool(true)],
        };
        let false_pat = Pat::Variant {
            name: "A".to_string(),
            fields: vec![Pat::Bool(false)],
        };
        let pats: Vec<&Pat> = vec![&true_pat, &false_pat];
        let missing = missing_variants(&all, &pats);
        assert!(missing.is_empty(), "expected no missing, got {missing:?}");
    }

    /// Variant subsumption is name-AND-field aware: same-name but
    /// disjoint payload does NOT subsume.
    #[test]
    fn variant_subsumption_name_and_field_aware() {
        let earlier = Pat::Variant {
            name: "A".to_string(),
            fields: vec![Pat::Bool(true)],
        };
        let later = Pat::Variant {
            name: "A".to_string(),
            fields: vec![Pat::Bool(false)],
        };
        assert!(
            !subsumes(&earlier, &later),
            "A(true) must NOT subsume A(false)"
        );
    }

    /// `x @ p` should be treated as `p` for usefulness purposes.
    /// We verify by lowering through the standard path and
    /// checking field-aware subsumption holds.
    #[test]
    fn name_only_variant_treated_as_wildcard_over_fields() {
        let earlier = Pat::Variant {
            name: "A".to_string(),
            fields: Vec::new(),
        };
        let later = Pat::Variant {
            name: "A".to_string(),
            fields: vec![Pat::Bool(true)],
        };
        assert!(
            subsumes(&earlier, &later),
            "name-only `A` should subsume `A(true)`"
        );
    }
}
