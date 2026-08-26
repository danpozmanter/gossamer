//! Rotates a written std free-function call into the argument order
//! every pass after the front end expects.
//!
//! Every std free function takes its data first, which is what makes
//! `iter::map(xs, f)` read like the `xs.map(f)` it stands for. The
//! lowering underneath does not: method lowering rewrites `xs.map(f)`
//! into the free call with the data LAST, and both tiers' free-call
//! lowering reads that slot. Rotating the written call once, after
//! resolution and before type checking, is what keeps a single order
//! below the front end - the same normalisation
//! [`gossamer_resolve::resolve_named_arguments`] performs for labelled
//! and defaulted arguments.
//!
//! A path the resolver bound to an item or a local of this compilation
//! unit is left alone: a user module is free to declare an `iter::map`
//! of its own, whose parameters are its declaration's.

#![forbid(unsafe_code)]

use gossamer_ast::visitor::{VisitorMut, walk_expr_mut};
use gossamer_ast::{Expr, ExprKind, PathExpr, SourceFile};
use gossamer_resolve::{Resolution, Resolutions, ResolveDiagnostic, ResolveError};

/// Rotates every catalogued data-first call into data-last order, and
/// reports a call still written in the order that ended with 0.55.
pub fn rotate_data_first_calls(
    sf: &mut SourceFile,
    resolutions: &Resolutions,
) -> Vec<ResolveDiagnostic> {
    let mut pass = Rotate {
        resolutions,
        diagnostics: Vec::new(),
    };
    pass.visit_source_file(sf);
    pass.diagnostics
}

struct Rotate<'a> {
    resolutions: &'a Resolutions,
    diagnostics: Vec<ResolveDiagnostic>,
}

impl Rotate<'_> {
    /// Whether `path` names a catalogued std free function whose data
    /// parameter is written first.
    fn is_data_first_std_call(&self, callee: &Expr, path: &PathExpr) -> bool {
        if matches!(
            self.resolutions.get(callee.id),
            Some(Resolution::Local(_) | Resolution::Def { .. })
        ) {
            return false;
        }
        let segments: Vec<&str> = path.segments.iter().map(|s| s.name.name.as_str()).collect();
        let Some((name, modules)) = segments.split_last() else {
            return false;
        };
        crate::stdlib_signatures::takes_data_first(modules, name)
    }
}

impl VisitorMut for Rotate<'_> {
    fn visit_expr(&mut self, expr: &mut Expr) {
        walk_expr_mut(self, expr);
        let ExprKind::Call { callee, args } = &mut expr.kind else {
            return;
        };
        let ExprKind::Path(path) = &callee.kind else {
            return;
        };
        // The arity is the catalogue's; a call that supplies a different
        // number of arguments is reported by the checker against the
        // signature the author read, so it is left as written.
        let Some(shape) = crate::stdlib_signatures::function_shape_for_path(
            &path
                .segments
                .iter()
                .map(|s| s.name.name.as_str())
                .collect::<Vec<_>>()[..path.segments.len().saturating_sub(1)],
            &path.segments[path.segments.len() - 1].name.name,
        ) else {
            return;
        };
        if shape.params.len() != args.len() || args.len() < 2 {
            return;
        }
        if !self.is_data_first_std_call(callee, path) {
            return;
        }
        // A callback slot that holds no closure while another argument
        // does is the argument list of the release before this one. The
        // call already reads in the order every pass below expects, so it
        // is left as written and only the spelling is reported.
        let callable: Vec<bool> = shape
            .params
            .iter()
            .map(|param| param.ty.trim_start().starts_with("Fn("))
            .collect();
        let written: Vec<bool> = args
            .iter()
            .map(|arg| matches!(arg.kind, ExprKind::Closure { .. }))
            .collect();
        if callable != written && callable.iter().zip(&written).any(|(c, w)| *c && !*w) {
            let mirrored: Vec<bool> = written
                .iter()
                .skip(1)
                .chain(&written[..1])
                .copied()
                .collect();
            if callable == mirrored {
                let callee_path = path
                    .segments
                    .iter()
                    .map(|s| s.name.name.as_str())
                    .collect::<Vec<_>>()
                    .join("::");
                let params = shape
                    .params
                    .iter()
                    .map(|param| format!("{}: {}", param.name, param.ty))
                    .collect::<Vec<_>>()
                    .join(", ");
                self.diagnostics.push(ResolveDiagnostic::new(
                    ResolveError::DataLastCallOrder {
                        callee: callee_path,
                        params: format!(
                            "{}({params})",
                            path.segments[path.segments.len() - 1].name.name
                        ),
                    },
                    expr.span,
                ));
                return;
            }
        }
        let data = args.remove(0);
        args.push(data);
    }
}
