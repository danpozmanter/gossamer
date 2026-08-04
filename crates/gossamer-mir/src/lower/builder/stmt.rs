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
        // Function bodies begin with Builder's one root scope. Only nested
        // blocks may acquire an implicit lexical region: a function's result
        // is an externally visible escape boundary, while a nested block is
        // accepted only when its tail is Copy (checked by the analysis).
        // Never layer an automatic region inside a source-visible one.
        let lexical_region = self.scopes.len() > 1
            && self.region_depth == 0
            && matches!(
                crate::lower::helpers::LoopEligibility::new(&*self.tcx, self.region_unsafe)
                    .decide_lexical_block(block),
                crate::lower::helpers::RegionDecision::Region
            );
        if lexical_region {
            self.emit_region_call("gos_rt_arena_push", block.span);
            self.region_depth += 1;
            self.deferred_auto_region_collections.push(false);
        }
        self.push_scope();
        self.defer_stack.push(Vec::new());
        for stmt in &block.stmts {
            self.lower_stmt(stmt);
            if self.current.is_none() {
                // Diverged mid-block (a `return` / `break` / `continue` inside
                // a statement). That construct already emitted the defers it
                // needed; drop this frame without re-emitting.
                self.defer_stack.pop();
                self.pop_scope();
                // Eligibility rejects all early exits, so this is defensive
                // only; keeping the pop here preserves the region stack if a
                // future lowering rule gains another diverging expression.
                self.end_loop_region(lexical_region, block.span);
                return None;
            }
        }
        let result = match block.tail.as_ref() {
            Some(tail) => self.lower_expr(tail),
            // Tail-less block whose flow didn't diverge yields the
            // unit value. Without this the caller (e.g. `lower_if`'s
            // else arm) sees `None` and skips the join `Goto`,
            // leaving the post-statement block with the default
            // `Unreachable` terminator - `let _ = fn_call()` as
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
        // Block-scoped `defer`: on a normal (non-diverging) exit, run this
        // block's deferred expressions LIFO after the block's value is
        // computed. A diverging tail (e.g. `return`) leaves `current` None and
        // has already emitted the frames itself.
        let frame = self.defer_stack.pop().unwrap_or_default();
        // Snapshot the block's value into a fresh local before the deferred
        // expressions run, so a defer that mutates a binding the tail names
        // (`{ defer t += 1; t }`) cannot change the value the block yields -
        // the value is the binding's state at the tail, not after the defer.
        let result = if self.current.is_some() && !frame.is_empty() {
            result.map(|r| {
                let ty = self.locals[r.0 as usize].ty;
                let snap = self.fresh(ty);
                self.emit_assign(
                    Place::local(snap),
                    Rvalue::Use(Operand::Copy(Place::local(r))),
                    block.span,
                );
                snap
            })
        } else {
            result
        };
        if self.current.is_some() {
            self.emit_defer_frame(&frame);
        }
        self.pop_scope();
        self.end_loop_region(lexical_region, block.span);
        if self.current.is_none() { None } else { result }
    }

    /// If `value` is the freshly-produced result of a call or inline enum
    /// constructor, rewrite that producer to write `binding` directly. The
    /// result temporary has no source-language identity and its only use is
    /// this binding, so copying it cannot be observed. This is ordinary return
    /// value copy elision, not ownership transfer between user bindings.
    ///
    /// Besides avoiding a redundant scalar or aggregate copy, direct binding
    /// is essential for aggregates containing Vec fields. Deep-cloning the Vec
    /// out of a fresh function result allocated a new non-region buffer on
    /// every loop iteration, while the dead temporary's region cleanup could
    /// not release that detached buffer.
    fn try_rebind_ctor_call(&mut self, value: Local, binding: Local) -> bool {
        let Some(cur) = self.current else {
            return false;
        };
        // Calls lower to a terminator in a prior block whose continuation is
        // the current block. Their destination temporary is fresh by
        // construction and is consumed immediately by this let binding.
        for blk in &mut self.blocks {
            if let Terminator::Call {
                callee,
                destination,
                target: Some(t),
                ..
            } = &mut blk.terminator
                && *t == cur
                && destination.local == value
                && destination.projection.is_empty()
                && matches!(callee, Operand::Const(ConstValue::Str(name)) if is_container_ctor(name))
            {
                *destination = Place::local(binding);
                return true;
            }
        }
        // `Some(..)` / `Ok(..)` / `Err(..)` lower to a `gos_rt_result_new`
        // `CallIntrinsic` assignment - the last statement of the current block
        // (the binding copy has not been emitted yet).
        let cur_idx = cur.0 as usize;
        if cur_idx < self.blocks.len()
            && let Some(last) = self.blocks[cur_idx].stmts.last_mut()
            && let StatementKind::Assign {
                place,
                rvalue: Rvalue::CallIntrinsic { name, .. },
            } = &mut last.kind
            && *name == "gos_rt_result_new"
            && place.local == value
            && place.projection.is_empty()
        {
            place.local = binding;
            return true;
        }
        false
    }

    fn is_fresh_user_call_result(&self, value: Local) -> bool {
        let Some(cur) = self.current else {
            return false;
        };
        self.blocks.iter().any(|block| {
            matches!(
                &block.terminator,
                Terminator::Call {
                    callee: Operand::FnRef { .. },
                    destination,
                    target: Some(target),
                    ..
                } if *target == cur
                    && destination.local == value
                    && destination.projection.is_empty()
            )
        })
    }

    fn is_vec_like_ty(&self, ty: gossamer_types::Ty) -> bool {
        matches!(self.tcx.kind_of(ty), gossamer_types::TyKind::Vec(_))
    }

    pub(crate) fn emit_vec_clone_binding(&mut self, value: Local, binding: Local, span: Span) {
        let next = self.new_block(span);
        self.terminate(Terminator::Call {
            callee: Operand::Const(ConstValue::Str("gos_rt_vec_clone".to_string())),
            args: vec![Operand::Copy(Place::local(value))],
            destination: Place::local(binding),
            target: Some(next),
        });
        self.set_current(next);
    }

    /// Copies an owned value into `binding`, cloning every growable vector
    /// header nested in a by-value struct or tuple. A flat aggregate memcpy is
    /// sufficient for fixed storage and RC-managed scalar children, but it
    /// would otherwise let a later `push` through the copy replace the source
    /// value's vector buffer in the LLVM and Cranelift tiers.
    pub(crate) fn emit_owned_clone_binding(&mut self, value: Local, binding: Local, span: Span) {
        use gossamer_types::TyKind;

        let ty = self.locals[binding.0 as usize].ty;
        if matches!(self.tcx.kind_of(ty), TyKind::Vec(_)) {
            self.emit_vec_clone_binding(value, binding, span);
            return;
        }

        self.emit_assign(
            Place::local(binding),
            Rvalue::Use(Operand::Copy(Place::local(value))),
            span,
        );

        for (path, kind) in crate::lower::aggregate_rc_field_paths(self.tcx, ty) {
            if kind != crate::lower::FieldRcKind::Vec {
                continue;
            }
            let mut field_ty = ty;
            let mut place = Place::local(binding);
            let mut valid = true;
            for index in path {
                field_ty = match self.tcx.kind_of(field_ty) {
                    TyKind::Adt { def, substs } => self
                        .tcx
                        .adt_field_tys(*def, substs)
                        .and_then(|fields| fields.get(index as usize).copied())
                        .unwrap_or_else(|| {
                            valid = false;
                            field_ty
                        }),
                    TyKind::Tuple(fields) => {
                        fields.get(index as usize).copied().unwrap_or_else(|| {
                            valid = false;
                            field_ty
                        })
                    }
                    TyKind::Array { elem, len } if (index as usize) < len.to_usize() => *elem,
                    _ => {
                        valid = false;
                        field_ty
                    }
                };
                place.projection.push(crate::ir::Projection::Field(index));
            }
            if !valid
                || !matches!(
                    self.tcx.kind_of(field_ty),
                    TyKind::Vec(_) | TyKind::Slice(_)
                )
            {
                continue;
            }
            let cloned = self.fresh(field_ty);
            let next = self.new_block(span);
            self.terminate(Terminator::Call {
                callee: Operand::Const(ConstValue::Str("gos_rt_vec_clone".to_string())),
                args: vec![Operand::Copy(place.clone())],
                destination: Place::local(cloned),
                target: Some(next),
            });
            self.set_current(next);
            self.emit_assign(
                place,
                Rvalue::Use(Operand::Copy(Place::local(cloned))),
                span,
            );
        }
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
                    let binding_wants_vec = matches!(self.tcx.kind_of(*ty), TyKind::Vec(_));
                    if binding_wants_vec {
                        if let Some(init_expr) = init.as_ref() {
                            if let HirExprKind::Array(gossamer_hir::HirArrayExpr::List(elems)) =
                                &init_expr.kind
                            {
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
                // A non-rebindable direct reference binding aliases its source
                // place. Do not materialise a copied fixed array or scalar in
                // `local`: bind the user name to the source local so reads and
                // projected stores share the same storage.
                //
                // A mutable binding of an aggregate reference is different:
                // `let mut cursor = &head; cursor = next` rebinds the pointer,
                // it does not assign through the reference. Aggregate values
                // already have pointer representation, so keep that pointer in
                // the binding's own `&T` local. Aliasing `cursor` directly to
                // the owning `head` local made the later rebind release and
                // overwrite `head`, corrupting recursive-list ownership in
                // compiled tiers.
                if let HirPatKind::Binding { name, .. } = &pattern.kind
                    && let Some(HirExpr {
                        kind: HirExprKind::Unary { op, operand },
                        ..
                    }) = init
                    && matches!(op, HirUnaryOp::RefShared | HirUnaryOp::RefMut)
                    && let HirExprKind::Path { segments, .. } = &operand.kind
                    && let [source] = segments.as_slice()
                    && let Some(source_local) = self.lookup_local(&source.name)
                {
                    if param_mutable(pattern)
                        && !matches!(
                            self.tcx.kind_of(self.locals[source_local.0 as usize].ty),
                            gossamer_types::TyKind::Int(_)
                                | gossamer_types::TyKind::Float(_)
                                | gossamer_types::TyKind::Bool
                                | gossamer_types::TyKind::Char
                                | gossamer_types::TyKind::String
                        )
                    {
                        self.emit_assign(
                            Place::local(local),
                            Rvalue::Ref {
                                mutable: matches!(op, HirUnaryOp::RefMut),
                                place: Place::local(source_local),
                            },
                            stmt.span,
                        );
                        self.bind_local(&name.name, local);
                    } else {
                        self.bind_local(&name.name, source_local);
                        self.bind_reference_alias(&name.name, source_local);
                    }
                    return;
                }
                if let Some(init) = init {
                    // A runtime-sized repeat (`let a = [value; n]`) is a heap
                    // Vec. Build it directly in the binding. Lowering it as a
                    // general expression first produced a temporary Vec and
                    // then applied ordinary Vec value semantics, deep-cloning
                    // the complete buffer into `a`. Large numeric buffers
                    // therefore used twice their required memory and paid an
                    // avoidable full-buffer copy before useful work began.
                    if let HirExprKind::Array(gossamer_hir::HirArrayExpr::Repeat { value, count }) =
                        &init.kind
                        && (matches!(self.tcx.kind_of(init.ty), gossamer_types::TyKind::Vec(_))
                            || literal_u64(count).is_none())
                        && self
                            .lower_array_repeat_into(value, count, init.ty, stmt.span, Some(local))
                            .is_some()
                    {
                        if let HirPatKind::Binding { name, .. } = &pattern.kind {
                            self.bind_local(&name.name, local);
                        }
                        return;
                    }
                    if let Some(mut value) = self.lower_expr(init) {
                        // Coerce a `json::Value`-typed initialiser
                        // when the binding has an explicit primitive
                        // / String annotation. `let low: i64 =
                        // root.latency.low_ms` becomes
                        // `gos_rt_json_as_i64(root.get("latency").get("low_ms"))`
                        // - keeps the user's natural notation while
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
                        // initialiser settled on - keeps downstream
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
                            // to i64 - keeping it as bool/i8 makes
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
                            let promote_vec_elem = match (binding_kind, init_kind) {
                                (TyKind::Vec(be), TyKind::Vec(ie)) => {
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
                                    | TyKind::HashMap { .. }
                            ) || promote_inner
                                || promote_handle
                                || promote_array_elem
                                || promote_vec_elem
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
                        // Bind fresh call and constructor results directly.
                        if !self.try_rebind_ctor_call(value, local) {
                            let init_ty = self.locals[value.0 as usize].ty;
                            let binding_ty = self.locals[local.0 as usize].ty;
                            if self.is_fresh_user_call_result(value) {
                                // The call-result temporary has no
                                // source-language identity. Copy its aggregate
                                // words and managed child handles, allowing RC
                                // insertion to retain the children before the
                                // temporary dies. Deep-cloning a nested Vec
                                // here detached a fresh buffer on every loop
                                // iteration.
                                self.emit_assign(
                                    Place::local(local),
                                    Rvalue::Use(Operand::Copy(Place::local(value))),
                                    stmt.span,
                                );
                            } else if self.is_vec_like_ty(init_ty)
                                && self.is_vec_like_ty(binding_ty)
                                || matches!(
                                    self.tcx.kind_of(binding_ty),
                                    gossamer_types::TyKind::Adt { .. }
                                        | gossamer_types::TyKind::Tuple(_)
                                        | gossamer_types::TyKind::Array { .. }
                                )
                            {
                                self.emit_owned_clone_binding(value, local, stmt.span);
                            } else {
                                self.emit_assign(
                                    Place::local(local),
                                    Rvalue::Use(Operand::Copy(Place::local(value))),
                                    stmt.span,
                                );
                            }
                        }
                        match &pattern.kind {
                            HirPatKind::Tuple(sub_patterns) => {
                                self.bind_tuple_pattern(local, sub_patterns, stmt.span);
                            }
                            HirPatKind::Struct { .. } | HirPatKind::Variant { .. } => {
                                self.bind_aggregate_let_pattern(local, pattern, stmt.span);
                            }
                            HirPatKind::Or(branches) => {
                                self.bind_or_let_pattern(local, branches, stmt.span);
                            }
                            _ => {}
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
            HirStmtKind::Defer(expr) => {
                // Register for block-scoped execution: the expression runs
                // (LIFO) when control leaves the enclosing block, emitted by
                // `lower_block` (normal exit) or by `return` / `break` /
                // `continue` (the exit edges).
                if let Some(frame) = self.defer_stack.last_mut() {
                    frame.push(expr.clone());
                }
            }
            HirStmtKind::Go(expr) => {
                // `go f(args);` - spawn `f` on a fresh OS
                // thread via the runtime's
                // `gos_rt_go_spawn_call_N(fn_addr, args…)`
                // helper. Mirrors the expression-position
                // lowering below so a goroutine spawned at
                // statement level fans out the same way as
                // one used as an expression. Any shape that is
                // not a direct named-function call with ≤ 6
                // arguments has been wrapped by the front-end
                // (`lift_go_inner`) into a zero-argument
                // closure, which spawns fire-and-forget through
                // `lower_go_spawn_closure`.
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
                                    if matches!(
                                        self.tcx.kind_of(lt),
                                        gossamer_types::TyKind::Vec(_)
                                            | gossamer_types::TyKind::Adt { .. }
                                            | gossamer_types::TyKind::Tuple(_)
                                    ) && matches!(
                                        &arg.kind,
                                        HirExprKind::Path { .. }
                                            | HirExprKind::Field { .. }
                                            | HirExprKind::TupleIndex { .. }
                                            | HirExprKind::Index { .. }
                                    ) {
                                        let cloned = self.fresh(lt);
                                        self.emit_owned_clone_binding(a, cloned, expr.span);
                                        a = cloned;
                                    }
                                    // The arg escapes to the spawned goroutine:
                                    // switch any RC-managed value to atomic
                                    // reference counting and flip a shared map to
                                    // its synchronized path before the spawn
                                    // publishes it (mirrors the expression-position
                                    // `go` lowering).
                                    self.emit_mark_shared_if_rc(a, expr.span);
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
                    let _ = self.lower_go_spawn_closure(expr, stmt.span);
                }
            }
            HirStmtKind::Item(_) => {}
        }
    }
}

/// Runtime constructors whose results are fresh owned values. Other runtime
/// calls may return a borrowed or write-back-related handle and must keep their
/// original temporary so the drop pass can honor that ABI contract.
fn is_container_ctor(name: &str) -> bool {
    matches!(
        name,
        "Vec::new"
            | "Vec::with_capacity"
            | "gos_rt_vec_new"
            | "gos_rt_vec_new_typed"
            | "gos_rt_vec_with_capacity"
            | "gos_rt_vec_with_capacity_typed"
            | "gos_rt_vec_from_arr"
            | "gos_rt_nested_arr_to_vec"
            | "gos_rt_vec_clone"
            | "gos_rt_str_chars"
            | "gos_rt_i64_chars"
            | "gos_rt_arr_iter"
            | "gos_rt_lazy_iter_collect_i64"
            | "gos_rt_lazy_iter_collect_pair_i64"
            | "HashMap::new"
            | "HashMap::with_capacity"
            | "collections::HashMap::new"
            | "HashSet::new"
            | "collections::HashSet::new"
            | "BTreeSet::new"
            | "collections::BTreeSet::new"
            | "BTreeMap::new"
            | "collections::BTreeMap::new"
            | "gos_rt_map_new"
            | "gos_rt_map_new_with_capacity"
            | "gos_rt_set_new"
            | "gos_rt_btmap_new"
    )
}
