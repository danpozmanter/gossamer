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

    /// If `value` is the freshly-produced result of a recognised constructor
    /// (a container `Call` like `HashMap::new`, or a `gos_rt_result_new`
    /// `CallIntrinsic` for `Some`/`Ok`/`Err`), rewrite that constructor to
    /// write `binding` directly and return true - the caller then skips the
    /// redundant `binding = Copy(value)`. This keeps the constructor result
    /// un-aliased so the drop pass treats the binding as the single owner: a
    /// loop-local map is reclaimed like a directly-bound `Vec`, and a by-value
    /// enum's payload is released exactly once (the copy would otherwise mark
    /// the binding an alias and defeat the single-use ownership check).
    fn try_rebind_ctor_call(&mut self, value: Local, binding: Local) -> bool {
        let Some(cur) = self.current else {
            return false;
        };
        // Container constructors lower to a terminator `Call` in a prior block
        // whose continuation is the current block.
        for blk in &mut self.blocks {
            if let Terminator::Call {
                callee: Operand::Const(ConstValue::Str(name)),
                destination,
                target: Some(t),
                ..
            } = &mut blk.terminator
                && *t == cur
                && destination.local == value
                && destination.projection.is_empty()
                && is_container_ctor(name)
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

    fn is_vec_like_ty(&self, ty: gossamer_types::Ty) -> bool {
        matches!(
            self.tcx.kind_of(ty),
            gossamer_types::TyKind::Vec(_) | gossamer_types::TyKind::Slice(_)
        )
    }

    fn emit_vec_clone_binding(&mut self, value: Local, binding: Local, span: Span) {
        let next = self.new_block(span);
        self.terminate(Terminator::Call {
            callee: Operand::Const(ConstValue::Str("gos_rt_vec_clone".to_string())),
            args: vec![Operand::Copy(Place::local(value))],
            destination: Place::local(binding),
            target: Some(next),
        });
        self.set_current(next);
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
                    // `let mut xs = [literal]` - the user wrote `mut`,
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
                    // keep its inline fixed-array layout - promoting
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
                                // resolved `Array`/`Vec`/`Slice` element -
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
                // A direct reference binding aliases its
                // source place. Do not materialise a copied fixed array or
                // scalar in `local`: bind the user name to the source local
                // so reads and projected stores share the same storage.
                if let HirPatKind::Binding { name, .. } = &pattern.kind
                    && let Some(HirExpr {
                        kind:
                            HirExprKind::Unary {
                                op: HirUnaryOp::RefShared | HirUnaryOp::RefMut,
                                operand,
                            },
                        ..
                    }) = init
                    && let HirExprKind::Path { segments, .. } = &operand.kind
                    && let [source] = segments.as_slice()
                    && let Some(source_local) = self.lookup_local(&source.name)
                {
                    self.bind_local(&name.name, source_local);
                    self.bind_reference_alias(&name.name, source_local);
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
                        && (matches!(
                            self.tcx.kind_of(init.ty),
                            gossamer_types::TyKind::Vec(_) | gossamer_types::TyKind::Slice(_)
                        ) || literal_u64(count).is_none())
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
                        // `let m = HashMap::new()` lowers `value = map_new();`
                        // then a copy into the binding. Array literals bind
                        // direct (see `lower_let_array_as_vec`); maps/sets do
                        // not, and the copy pins the constructor result as
                        // aliased so the drop pass cannot reclaim a loop-local
                        // one. Rewrite the constructor call to write the binding
                        // directly and drop the redundant copy - `value` is the
                        // freshly-lowered init, used only by this copy, so this
                        // is sound and leaves the result un-aliased.
                        if !self.try_rebind_ctor_call(value, local) {
                            let init_ty = self.locals[value.0 as usize].ty;
                            let binding_ty = self.locals[local.0 as usize].ty;
                            if self.is_vec_like_ty(init_ty) && self.is_vec_like_ty(binding_ty) {
                                self.emit_vec_clone_binding(value, local, stmt.span);
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

/// Recognised container constructors whose `let`-binding result the lowerer
/// rewrites to bind directly (see `try_rebind_ctor_call`).
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
            | "BTreeMap::new"
            | "collections::BTreeMap::new"
            | "gos_rt_map_new"
            | "gos_rt_map_new_with_capacity"
            | "gos_rt_set_new"
            | "gos_rt_btmap_new"
    )
}
