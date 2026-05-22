#![allow(clippy::missing_safety_doc)]
#![allow(missing_docs)]
#![allow(clippy::too_many_lines)]
#![allow(clippy::needless_range_loop)]
#![allow(clippy::wildcard_imports)]
#![allow(clippy::similar_names)]
#![allow(clippy::many_single_char_names)]
#![allow(clippy::items_after_statements)]
#![allow(clippy::cast_lossless)]
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_possible_wrap)]
#![allow(clippy::cast_sign_loss)]
#![allow(clippy::doc_markdown)]
#![allow(clippy::option_if_let_else)]
#![allow(clippy::match_same_arms)]
#![allow(clippy::if_not_else)]
#![allow(clippy::single_match_else)]
#![allow(clippy::needless_pass_by_value)]
#![allow(clippy::manual_let_else)]
#![allow(clippy::redundant_else)]
#![allow(clippy::collapsible_if)]
#![allow(clippy::collapsible_else_if)]
#![allow(clippy::map_unwrap_or)]
#![allow(clippy::struct_excessive_bools)]
#![allow(clippy::module_name_repetitions)]
#![allow(clippy::unnecessary_wraps)]
#![allow(clippy::large_enum_variant)]
#![allow(clippy::if_same_then_else)]
#![allow(clippy::single_match)]
#![allow(clippy::useless_conversion)]

use std::collections::HashMap;

use gossamer_ast::Ident;
use gossamer_hir::{
    HirAdtKind, HirBinaryOp, HirBlock, HirExpr, HirExprKind, HirFn, HirItem, HirItemKind,
    HirLiteral, HirMatchArm, HirPat, HirPatKind, HirProgram, HirStmt, HirStmtKind, HirUnaryOp,
};
use gossamer_lex::Span;
use gossamer_types::{Ty, TyCtxt};

use crate::ir::{
    BasicBlock, BinOp, BlockId, Body, ConstValue, Local, LocalDecl, Operand, Place, Rvalue,
    Statement, StatementKind, Terminator, UnOp,
};

use super::*;

use super::Builder;

impl<'a> Builder<'a> {
    pub(crate) fn lower_block(&mut self, block: &HirBlock) -> Option<Local> {
        self.push_scope();
        for stmt in &block.stmts {
            self.lower_stmt(stmt);
            if self.current.is_none() {
                self.pop_scope();
                return None;
            }
        }
        let result = match block.tail.as_ref() {
            Some(tail) => self.lower_expr(tail),
            // Tail-less block whose flow didn't diverge yields the
            // unit value. Without this the caller (e.g. `lower_if`'s
            // else arm) sees `None` and skips the join `Goto`,
            // leaving the post-statement block with the default
            // `Unreachable` terminator — `let _ = fn_call()` as
            // the last statement of an else block crashed
            // compiled binaries with `ud2`.
            None => {
                if self.current.is_some() {
                    Some(self.lower_unit(block.span))
                } else {
                    None
                }
            }
        };
        self.pop_scope();
        if self.current.is_none() { None } else { result }
    }

    pub(crate) fn lower_stmt(&mut self, stmt: &HirStmt) {
        match &stmt.kind {
            HirStmtKind::Let { pattern, ty, init } => {
                let local = self.push_local(*ty, param_name(pattern), param_mutable(pattern));
                // NOTE: do NOT bind the name yet. `let x = expr`
                // must evaluate `expr` in the *outer* scope so a
                // shadowing form like `let x = x + 1` reads the
                // previous binding. We `bind_local` only after
                // `lower_expr(init)` has resolved every name.
                // `lower_let_array_as_vec` (the Vec annotation
                // shortcut) below also defers the bind to its own
                // post-init point.
                {
                    use gossamer_types::TyKind;
                    let binding_wants_vec =
                        matches!(self.tcx.kind_of(*ty), TyKind::Vec(_) | TyKind::Slice(_),);
                    // `let mut xs = [literal]` — the user wrote `mut`,
                    // so they want a growable Vec, not a fixed-size
                    // array. Without this promotion `xs.push(...)`
                    // calls `gos_rt_vec_push(stack_array_ptr, ...)`
                    // which interprets the stack array as a GosVec
                    // header and corrupts memory.
                    // Only promote a `let mut xs = [literal]` to a
                    // heap Vec when `xs` is actually grown / reshaped
                    // (push / pop / insert / …) somewhere in the
                    // function. An explicitly-sized `let mut bodies:
                    // [Body; 5]` that is only indexed, field-mutated,
                    // or passed to a `[T; N]`-typed parameter must
                    // keep its inline fixed-array layout — promoting
                    // it to `Vec<Body>` desynchronises the element
                    // stride at call boundaries (`energy(&bodies)`
                    // expecting `&[Body; 5]`) and corrupts reads.
                    let binding_name = match &pattern.kind {
                        HirPatKind::Binding { name, .. } => Some(name.name.clone()),
                        _ => None,
                    };
                    let mut_with_array_literal = param_mutable(pattern)
                        && binding_name
                            .as_ref()
                            .is_some_and(|n| self.grows_bindings.contains(n))
                        && init.as_ref().is_some_and(|init_expr| {
                            matches!(
                                init_expr.kind,
                                HirExprKind::Array(gossamer_hir::HirArrayExpr::List(_))
                            )
                        });
                    if binding_wants_vec || mut_with_array_literal {
                        if let Some(init_expr) = init.as_ref() {
                            if let HirExprKind::Array(gossamer_hir::HirArrayExpr::List(elems)) =
                                &init_expr.kind
                            {
                                // Pin the local's type to Vec<elem>
                                // before calling `lower_let_array_as_vec`
                                // so the codegen lays out the slot as
                                // an 8-byte heap pointer instead of a
                                // multi-slot stack array. The element type
                                // comes from the first element; for an empty
                                // literal (`let mut xs = []`) there is no
                                // element, so fall back to the binding's
                                // resolved `Array`/`Vec`/`Slice` element —
                                // defaulting to i64 would size the vec's
                                // elements at 8 bytes and corrupt the heap on
                                // `xs.push(t)` of a wider element (e.g. a
                                // `[i64; 2]` or a tuple).
                                if mut_with_array_literal && !binding_wants_vec {
                                    let concrete = |b: &Self, t: gossamer_types::Ty| {
                                        !matches!(b.tcx.kind_of(t), TyKind::Var(_) | TyKind::Error)
                                    };
                                    let elem_ty = elems
                                        .first()
                                        .map(|e| e.ty)
                                        .filter(|t| concrete(self, *t))
                                        .or_else(|| match self.tcx.kind_of(*ty) {
                                            TyKind::Array { elem, .. }
                                            | TyKind::Vec(elem)
                                            | TyKind::Slice(elem) => Some(*elem),
                                            _ => None,
                                        })
                                        .filter(|t| concrete(self, *t))
                                        // For an empty `[]` neither the literal
                                        // nor the binding annotation names a
                                        // concrete element. Recover it from the
                                        // `push(x)` / `insert(_, x)` sites so a
                                        // multi-slot element ([i64; 2], tuple,
                                        // struct) is sized correctly instead of
                                        // truncated to one 8-byte slot.
                                        .or_else(|| {
                                            binding_name
                                                .as_ref()
                                                .and_then(|n| self.grows_elem_ty.get(n).copied())
                                                .filter(|t| concrete(self, *t))
                                        })
                                        .unwrap_or_else(|| {
                                            self.tcx.int_ty(gossamer_types::IntTy::I64)
                                        });
                                    let vec_ty = self.tcx.intern(TyKind::Vec(elem_ty));
                                    self.locals[local.0 as usize].ty = vec_ty;
                                }
                                if self.lower_let_array_as_vec(local, elems, stmt.span) {
                                    if let HirPatKind::Binding { name, .. } = &pattern.kind {
                                        self.bind_local(&name.name, local);
                                    }
                                    return;
                                }
                            }
                        }
                    }
                }
                if let Some(init) = init {
                    if let Some(mut value) = self.lower_expr(init) {
                        // Coerce a `json::Value`-typed initialiser
                        // when the binding has an explicit primitive
                        // / String annotation. `let low: i64 =
                        // root.latency.low_ms` becomes
                        // `gos_rt_json_as_i64(root.get("latency").get("low_ms"))`
                        // — keeps the user's natural notation while
                        // funnelling the dynamic-shape tax through
                        // the runtime helpers.
                        let value_ty = self.locals[value.0 as usize].ty;
                        if self.is_json_value_ty(value_ty) && !self.is_json_value_ty(*ty) {
                            if let Some(coerced) =
                                self.maybe_coerce_json_value(value, *ty, stmt.span)
                            {
                                value = coerced;
                            }
                        }
                        // Callable-shape coercion: when `let f:
                        // fn(...) -> ... = bare_fn` (or
                        // `Fn(...) -> ...`) is written, wrap the
                        // bare fn item in the env+code blob so
                        // every callable slot in the program
                        // uniformly carries an env_ptr. Without
                        // this, a later `f(...)` call site would
                        // see the raw fn address and skip the
                        // env-load step, segfaulting on access.
                        {
                            use gossamer_types::TyKind;
                            let value_ty_now = self.locals[value.0 as usize].ty;
                            let dest_callable = matches!(
                                self.tcx.kind_of(*ty),
                                TyKind::FnPtr(_) | TyKind::FnTrait(_)
                            );
                            let src_is_fn_def =
                                matches!(self.tcx.kind_of(value_ty_now), TyKind::FnDef { .. });
                            // Lift-closed produces a Path lowered
                            // to `Const(Str(name))` whose local is
                            // marked in `local_fn_name`. Treat it
                            // the same as a bare fn item for
                            // callable-slot wrapping.
                            let src_names_fn = self.local_fn_name.contains_key(&value);
                            if dest_callable && (src_is_fn_def || src_names_fn) {
                                value = self.coerce_to_fn_trait_if_needed(value, *ty, stmt.span);
                            }
                        }
                        // When the HIR-recorded type is an
                        // unresolved inference variable, pin the
                        // binding's MIR type to whatever the lowered
                        // initialiser settled on — keeps downstream
                        // passes (string-concat, codegen cl-type
                        // inference) grounded on concrete kinds.
                        let init_ty = self.locals[value.0 as usize].ty;
                        {
                            use gossamer_types::TyKind;
                            let binding_kind = self.tcx.kind_of(self.locals[local.0 as usize].ty);
                            let init_kind = self.tcx.kind_of(init_ty);
                            // When the binding's annotation is an
                            // Adt wrapper but the initialiser
                            // settled on a concrete scalar / String
                            // (typical of `let v = r.unwrap()` for
                            // `r: Result<T, E>` where the compiled
                            // tier flattens the wrapper), promote
                            // the binding to the scalar type so
                            // downstream printing + arithmetic find
                            // the right kind. The other concrete
                            // annotations (struct, vec, tuple, …)
                            // are kept verbatim because they
                            // typically come from explicit user
                            // annotations the typechecker has
                            // already validated against the value.
                            let promote_inner = matches!(binding_kind, TyKind::Adt { .. })
                                && matches!(
                                    init_kind,
                                    TyKind::Bool
                                        | TyKind::Char
                                        | TyKind::Int(_)
                                        | TyKind::Float(_)
                                        | TyKind::String
                                );
                            // The initialiser's runtime kind tells us
                            // when the binding is a runtime handle
                            // (flag cell, http response, regex
                            // pattern, …) that the typechecker has
                            // collapsed to a primitive. The cell
                            // value is pointer-shaped at runtime, so
                            // the binding's MIR type must be widened
                            // to i64 — keeping it as bool/i8 makes
                            // cranelift store the byte-truncated
                            // pointer value, and later loads return
                            // garbage. This is the cli_args / flag
                            // bool reproducer the test suite caught.
                            let init_rk = self.local_runtime_kind.get(&value).copied();
                            let promote_handle = init_rk.is_some_and(|rk| {
                                rk.starts_with("flag::Cell::")
                                    || rk.starts_with("http::")
                                    || rk.starts_with("regex::")
                                    || rk.starts_with("bufio::")
                                    || rk == "errors::Error"
                                    || rk == "flag::Set"
                            }) && matches!(
                                binding_kind,
                                TyKind::Bool
                                    | TyKind::Char
                                    | TyKind::Int(_)
                                    | TyKind::Float(_)
                                    | TyKind::String
                            );
                            // Promote `Array { elem: Var/Error, len }`
                            // bindings to the init's resolved Array
                            // type so {:?} dispatch can classify the
                            // elem (otherwise the print path falls
                            // back to the `<value>` placeholder).
                            let promote_array_elem = match (binding_kind, init_kind) {
                                (
                                    TyKind::Array { elem: be, .. },
                                    TyKind::Array { elem: ie, .. },
                                ) => {
                                    let be_unresolved = matches!(
                                        self.tcx.kind_of(*be),
                                        TyKind::Var(_) | TyKind::Error
                                    );
                                    let ie_resolved = !matches!(
                                        self.tcx.kind_of(*ie),
                                        TyKind::Var(_) | TyKind::Error
                                    );
                                    be_unresolved && ie_resolved
                                }
                                _ => false,
                            };
                            if !matches!(
                                binding_kind,
                                TyKind::Bool
                                    | TyKind::Char
                                    | TyKind::Int(_)
                                    | TyKind::Float(_)
                                    | TyKind::String
                                    | TyKind::Vec(_)
                                    | TyKind::Array { .. }
                                    | TyKind::Slice(_)
                                    | TyKind::Adt { .. }
                                    | TyKind::Tuple(_)
                                    | TyKind::Ref { .. }
                            ) || promote_inner
                                || promote_handle
                                || promote_array_elem
                            {
                                self.locals[local.0 as usize].ty = init_ty;
                            }
                        }
                        if let Some(struct_name) = self.local_struct.get(&value).cloned() {
                            self.local_struct.insert(local, struct_name);
                        }
                        if let Some(elem) = self.local_elem_struct.get(&value).cloned() {
                            self.local_elem_struct.insert(local, elem);
                        }
                        if let Some(closure_name) = self.local_closure.get(&value).cloned() {
                            self.local_closure.insert(local, closure_name);
                        }
                        if let Some(fn_name) = self.local_fn_name.get(&value).cloned() {
                            self.local_fn_name.insert(local, fn_name);
                        }
                        if let Some(rk) = self.local_runtime_kind.get(&value).copied() {
                            self.local_runtime_kind.insert(local, rk);
                        }
                        if let Some(layout) = self.local_define_layout.get(&value).cloned() {
                            self.local_define_layout.insert(local, layout);
                        }
                        self.emit_assign(
                            Place::local(local),
                            Rvalue::Use(Operand::Copy(Place::local(value))),
                            stmt.span,
                        );
                        if let HirPatKind::Tuple(sub_patterns) = &pattern.kind {
                            self.bind_tuple_pattern(local, sub_patterns, stmt.span);
                        }
                    }
                }
                // Bind the user-name AFTER the init has been
                // lowered, so a shadowing form like
                // `let x = x + 1` reads the previous `x` while
                // evaluating the RHS.
                if let HirPatKind::Binding { name, .. } = &pattern.kind {
                    self.bind_local(&name.name, local);
                }
            }
            HirStmtKind::Expr { expr, .. } => {
                let _ = self.lower_expr(expr);
            }
            HirStmtKind::Defer(_) => {
                // Deferred calls are lowered to no-ops at the MIR
                // level for now; full support lands with the
                // runtime's unwind-and-run machinery.
            }
            HirStmtKind::Go(expr) => {
                // `go f(args);` — spawn `f` on a fresh OS
                // thread via the runtime's
                // `gos_rt_go_spawn_call_N(fn_addr, args…)`
                // helper. Mirrors the expression-position
                // lowering below so a goroutine spawned at
                // statement level fans out the same way as
                // one used as an expression. Falls back to
                // synchronous execution when the inner shape
                // doesn't match a direct `f(args)` call with
                // ≤ 4 scalar arguments.
                let mut handled = false;
                if let HirExprKind::Call { callee, args } = &expr.kind {
                    if let HirExprKind::Path { def: Some(def), .. } = &callee.kind {
                        if args.len() <= 6 {
                            let sym: &'static str = match args.len() {
                                0 => "gos_rt_go_spawn_call_0",
                                1 => "gos_rt_go_spawn_call_1",
                                2 => "gos_rt_go_spawn_call_2",
                                3 => "gos_rt_go_spawn_call_3",
                                4 => "gos_rt_go_spawn_call_4",
                                5 => "gos_rt_go_spawn_call_5",
                                _ => "gos_rt_go_spawn_call_6",
                            };
                            let i64_ty = self.tcx.int_ty(gossamer_types::IntTy::I64);
                            let fn_addr_local = self.fresh(i64_ty);
                            let substs = self.substs_of(callee.ty);
                            self.emit_assign(
                                Place::local(fn_addr_local),
                                Rvalue::Use(Operand::FnRef { def: *def, substs }),
                                expr.span,
                            );
                            let mut operands = Vec::with_capacity(args.len() + 1);
                            operands.push(Operand::Copy(Place::local(fn_addr_local)));
                            for arg in args {
                                if let Some(mut a) = self.lower_expr(arg) {
                                    // Array literals are flat stack aggregates.
                                    // Goroutine bodies run on a separate stack, so
                                    // passing a pointer to the caller's stack frame
                                    // is unsafe. Coerce Array→Vec to heap-allocate
                                    // the data before the spawn.
                                    let lt = self.locals[a.0 as usize].ty;
                                    if let gossamer_types::TyKind::Array { elem, len } =
                                        self.tcx.kind_of(lt).clone()
                                    {
                                        a = self.coerce_array_to_vec(a, elem, len, expr.span);
                                    }
                                    operands.push(Operand::Copy(Place::local(a)));
                                }
                            }
                            let unit_ty = self.tcx.unit();
                            let dest = self.fresh(unit_ty);
                            let next = self.new_block(expr.span);
                            self.terminate(Terminator::Call {
                                callee: Operand::Const(ConstValue::Str(sym.to_string())),
                                args: operands,
                                destination: Place::local(dest),
                                target: Some(next),
                            });
                            self.set_current(next);
                            handled = true;
                        }
                    }
                }
                if !handled {
                    let _ = self.lower_expr(expr);
                }
            }
            HirStmtKind::Item(_) => {
                // Nested items are not supported in the MIR yet.
            }
        }
    }
}
