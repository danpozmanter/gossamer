//! Source rewriters `gos fix` applies to bring a project forward.
//!
//! A migration is not a lint. A lint says something about the code the
//! author wrote and is reported whether or not they act on it; a
//! migration is a mechanical upgrade the toolchain owns, and running it
//! is the whole interaction. They share [`crate::Fix`] as the edit
//! representation and nothing else.
//!
//! Every rewriter here must be **deterministic** - the same input yields
//! the same edits - and **idempotent** - applying it to its own output
//! produces no further edits. `gos fix` re-runs the front end afterwards
//! and keeps the result only when the file still checks, so a rewriter
//! that breaks a program cannot land, but a rewriter that is not
//! idempotent would still churn a repository on every run.

use gossamer_ast::{Expr, ExprKind, SourceFile};
use gossamer_lex::Span;

use crate::Fix;
use crate::lints::walk_expr;

/// One named migration.
pub struct Rewriter {
    /// Stable identifier, used to select and to report.
    pub id: &'static str,
    /// One line describing what the rewrite does.
    pub summary: &'static str,
    /// Editions this rewriter prepares a project for. Empty means it
    /// applies to every edition.
    pub editions: &'static [&'static str],
    /// Collects the edits this rewriter would make.
    pub collect: fn(&SourceFile, &str, &mut Vec<Fix>),
}

/// Every registered migration, in application order.
pub const REWRITERS: &[Rewriter] = &[Rewriter {
    id: "method_form_combinators",
    summary: "Rewrite `iter::map(f, xs)` into the canonical `xs.map(f)`.",
    editions: &[],
    collect: method_form_combinator_fixes,
}];

/// Looks up a rewriter by id.
#[must_use]
pub fn rewriter(id: &str) -> Option<&'static Rewriter> {
    REWRITERS.iter().find(|r| r.id == id)
}

/// Collects the edits every rewriter in `selected` would make.
#[must_use]
pub fn migrations(sf: &SourceFile, source: &str, selected: &[&Rewriter]) -> Vec<Fix> {
    let mut out = Vec::new();
    for rewriter in selected {
        (rewriter.collect)(sf, source, &mut out);
    }
    out
}

/// Sequence combinators that read the same in method form and in the
/// data-last free form, with the argument count the direct call takes.
///
/// The method form is canonical: it is the spelling the skill card
/// teaches and the one a reader meets first. The count is what separates
/// a direct call from a pipeline step, which supplies the sequence
/// through the pipe and so writes one argument fewer.
const COMBINATORS: &[(&str, usize)] = &[
    ("map", 2),
    ("filter", 2),
    ("fold", 3),
    ("any", 2),
    ("all", 2),
    ("find", 2),
    ("position", 2),
    ("take", 2),
    ("skip", 2),
    ("step_by", 2),
    ("count", 1),
    ("sum", 1),
    ("min", 1),
    ("max", 1),
    ("rev", 1),
];

/// `iter::map(f, xs)` -> `xs.map(f)`.
///
/// The free form stays valid, and stays idiomatic as a `|>` pipeline
/// target where the piped value fills the last slot. Only a direct call
/// with every argument written out is rewritten; a pipeline step has
/// fewer arguments than the combinator takes and is left alone.
pub(crate) fn method_form_combinator_fixes(sf: &SourceFile, source: &str, out: &mut Vec<Fix>) {
    each_expr(sf, &mut |expr| {
        let ExprKind::Call { callee, args } = &expr.kind else {
            return;
        };
        let ExprKind::Path(path) = &callee.kind else {
            return;
        };
        let [module, name] = path.segments.as_slice() else {
            return;
        };
        if module.name.name != "iter" {
            return;
        }
        let Some((_, arity)) = COMBINATORS.iter().find(|(c, _)| *c == name.name.name) else {
            return;
        };
        if args.len() != *arity {
            return;
        }
        // Data-last: the sequence is the final argument and becomes the
        // receiver. A single-argument call is the sequence alone.
        let Some((data, leading)) = args.split_last() else {
            return;
        };
        // `x |> iter::map(f)` parses into this same shape, with the piped
        // value appended as an argument that lies outside the call's own
        // span. Rewriting that would consume the function as the receiver
        // and drop the sequence entirely.
        if data.span.start < expr.span.start || data.span.end > expr.span.end {
            return;
        }
        // A receiver written as a bare name, a path, a field, an index,
        // or a call keeps its meaning next to `.`; anything looser would
        // need parentheses to preserve precedence, and guessing where
        // they go is how a rewriter corrupts a program.
        if !is_self_delimiting(data) {
            return;
        }
        let mut rewritten = String::new();
        rewritten.push_str(slice(source, data.span));
        rewritten.push('.');
        rewritten.push_str(&name.name.name);
        rewritten.push('(');
        for (i, arg) in leading.iter().enumerate() {
            if i > 0 {
                rewritten.push_str(", ");
            }
            rewritten.push_str(slice(source, arg.span));
        }
        rewritten.push(')');
        out.push(Fix {
            span: expr.span,
            replacement: rewritten,
            lint_id: "method_form_combinators",
        });
    });
}

/// Whether `expr` reads unambiguously to the left of a `.` without
/// added parentheses.
fn is_self_delimiting(expr: &Expr) -> bool {
    matches!(
        expr.kind,
        ExprKind::Path(_)
            | ExprKind::Call { .. }
            | ExprKind::MethodCall { .. }
            | ExprKind::FieldAccess { .. }
            | ExprKind::Index { .. }
            | ExprKind::Literal(_)
            | ExprKind::Array(_)
            | ExprKind::FixedArray(_)
            | ExprKind::Tuple(_)
    )
}

fn slice(source: &str, span: Span) -> &str {
    let start = span.start as usize;
    let end = (span.end as usize).min(source.len());
    source.get(start..end).unwrap_or("")
}

/// Visits every expression in every function body of `sf`.
fn each_expr(sf: &SourceFile, visit: &mut dyn FnMut(&Expr)) {
    for item in &sf.items {
        if let gossamer_ast::ItemKind::Fn(decl) = &item.kind
            && let Some(body) = &decl.body
        {
            walk_expr(body, visit);
        }
    }
}
