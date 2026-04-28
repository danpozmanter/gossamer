//! HIR → bytecode compiler.

#![forbid(unsafe_code)]
#![allow(
    clippy::too_many_lines,
    clippy::unnecessary_wraps,
    clippy::similar_names
)]

use std::collections::HashMap;
use std::sync::Arc;

use gossamer_ast::Ident;
use gossamer_hir::{
    HirBinaryOp, HirBlock, HirExpr, HirExprKind, HirFn, HirLiteral, HirPat, HirPatKind, HirStmt,
    HirStmtKind, HirUnaryOp,
};
use gossamer_types::{FloatTy, IntTy, Ty, TyCtxt, TyKind};

use crate::bytecode::{ConstIdx, FnChunk, GlobalIdx, InstrIdx, Op, Reg};
use crate::value::{RuntimeError, RuntimeResult, SmolStr, Value};

/// Kind of a virtual register. Phase-1 typed opcodes target
/// these kinds directly to skip the `Value` enum pack/unpack
/// that dominates numeric kernels. Unknown / aggregate
/// registers stay in [`RegKind::Value`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RegKind {
    /// Boxed [`crate::value::Value`] register (the default,
    /// and the ABI used for calls / returns / aggregates).
    Value,
    /// Unboxed `f64` register.
    F64,
    /// Unboxed `i64` register.
    I64,
}

/// A register plus the file it lives in. Typed opcodes read
/// from / write to one file per operand.
#[derive(Debug, Clone, Copy)]
pub(crate) struct TypedReg {
    pub reg: Reg,
    pub kind: RegKind,
}

/// Struct declaration-order field tables the compiler uses to
/// resolve field-access offsets at compile time.
pub(crate) type StructLayouts = std::collections::HashMap<gossamer_resolve::DefId, Vec<String>>;

/// Trivial-wrapper inlining table: for each user function
/// whose body is `return intrinsic(param)` (a pattern common
/// enough in library code that its call overhead shows up on
/// profiles), we record the target intrinsic's path segments.
/// Calls to the wrapper are rewritten to direct intrinsic
/// calls at compile time — no `Op::Call`, no push/pop of a
/// frame, no boxing across the call boundary.
pub(crate) type InlinableWrappers = std::collections::HashMap<String, Vec<String>>;

/// Compiles an [`HirFn`] body into a [`FnChunk`]. The caller owns the
/// resulting chunk; the compiler itself has no shared state.
pub fn compile_fn(
    decl: &HirFn,
    tcx: &TyCtxt,
    layouts: &StructLayouts,
    wrappers: &InlinableWrappers,
) -> RuntimeResult<FnChunk> {
    let Some(body) = decl.body.as_ref() else {
        return Ok(FnChunk {
            name: decl.name.name.clone(),
            arity: u16::try_from(decl.params.len()).unwrap_or(u16::MAX),
            register_count: 0,
            float_count: 0,
            int_count: 0,
            instrs: Vec::new(),
            consts: Vec::new(),
            f64_consts: Vec::new(),
            i64_consts: Vec::new(),
            globals: Vec::new(),
            deferred_exprs: Vec::new(),
            deferred_envs: Vec::new(),
            deferred_env_regs: Vec::new(),
            call_cache_count: 0,
            arith_cache_count: 0,
            field_cache_count: 0,
        });
    };
    let mut builder = FnBuilder::new(decl.name.name.clone(), tcx, layouts, wrappers);
    for param in &decl.params {
        let reg = builder.alloc_reg();
        builder.bind_param(&param.pattern, reg);
    }
    let result = builder.compile_block(&body.block)?;
    if matches!(result, BlockResult::ValueIn(_)) {
        let BlockResult::ValueIn(reg) = result else {
            unreachable!()
        };
        builder.emit(Op::Return { value: reg });
    } else {
        builder.emit(Op::ReturnUnit);
    }
    let arity = u16::try_from(decl.params.len()).unwrap_or(u16::MAX);
    Ok(builder.finish(arity))
}

#[derive(Debug, Clone, Copy)]
enum BlockResult {
    Unit,
    ValueIn(Reg),
    Diverges,
}

struct FnBuilder<'tcx> {
    name: String,
    tcx: &'tcx TyCtxt,
    layouts: &'tcx StructLayouts,
    wrappers: &'tcx InlinableWrappers,
    /// Value registers that are compile-time-proven to hold
    /// `Value::FloatArray` — populated by `BuildFloatArray`
    /// emission and cleared whenever the register is
    /// reassigned. When a read/write op's base is one of
    /// these, we emit `FlatGetF64` / `FlatSetF64` instead of
    /// the discriminant-checking `IndexedFieldGetF64ByOffset`.
    flat_locals: std::collections::HashMap<Reg, u16>,
    /// Value registers compile-time-proven to hold a
    /// `Value::IntArray` (a primitive `[i64; N]` literal). Reads
    /// against one of these registers route through
    /// [`Op::IntArrayGetI64`] into a typed `i64` register.
    flat_int_locals: std::collections::HashSet<Reg>,
    /// Mirror of [`Self::flat_int_locals`] for `Value::FloatVec` —
    /// `[f64; N]` literals built via [`Self::try_build_float_vec`].
    /// Lets indexed reads / writes route through the typed-`f64`
    /// fast path that skips the `Value::Float` round-trip.
    flat_float_locals: std::collections::HashSet<Reg>,
    instrs: Vec<Op>,
    consts: Vec<Value>,
    const_cache: HashMap<ConstKey, ConstIdx>,
    f64_consts: Vec<f64>,
    f64_const_cache: HashMap<u64, ConstIdx>,
    i64_consts: Vec<i64>,
    i64_const_cache: HashMap<i64, ConstIdx>,
    globals: Vec<String>,
    global_cache: HashMap<String, GlobalIdx>,
    next_reg: u16,
    next_float_reg: u16,
    next_int_reg: u16,
    scopes: Vec<Scope>,
    loop_stack: Vec<LoopCtx>,
    deferred_exprs: Vec<HirExpr>,
    deferred_envs: Vec<Vec<String>>,
    deferred_env_regs: Vec<Vec<Reg>>,
    /// Counter incremented every time we emit a dispatch op
    /// (`Op::Call` / `Op::MethodCall`) so each call site gets a
    /// unique inline-cache slot index. The `FnChunk` allocates a
    /// `Vec<CacheSlot>` of this size at finish time.
    next_cache_idx: u16,
    /// Counter for `Op::FieldGet` IC slots (T2.5).
    next_field_cache_idx: u16,
    /// Counter incremented every time we emit a generic-`Value`
    /// arith op (`Op::AddInt` / `Op::SubInt` / `Op::MulInt` /
    /// `Op::DivInt` / `Op::RemInt`) so each site gets its own
    /// `arith_caches` slot. The `FnChunk` allocates the cache
    /// vector to this size at `finish` time. Tier C2.
    next_arith_cache_idx: u16,
}

#[derive(Debug, Default)]
struct Scope {
    locals: HashMap<String, TypedReg>,
}

#[derive(Debug)]
struct LoopCtx {
    break_patches: Vec<InstrIdx>,
    result_reg: Reg,
    /// Address of the loop's re-entry point. `Op::Jump` to
    /// here for `continue`. For `while`, this is the
    /// condition check; for bare `loop`, the body's first op.
    loop_start: InstrIdx,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum ConstKey {
    Unit,
    Bool(bool),
    Int(i64),
    Float(u64),
    Char(char),
    String(String),
}

impl<'tcx> FnBuilder<'tcx> {
    fn new(
        name: String,
        tcx: &'tcx TyCtxt,
        layouts: &'tcx StructLayouts,
        wrappers: &'tcx InlinableWrappers,
    ) -> Self {
        Self {
            name,
            tcx,
            layouts,
            wrappers,
            flat_locals: std::collections::HashMap::new(),
            flat_int_locals: std::collections::HashSet::new(),
            flat_float_locals: std::collections::HashSet::new(),
            instrs: Vec::new(),
            consts: Vec::new(),
            const_cache: HashMap::new(),
            f64_consts: Vec::new(),
            f64_const_cache: HashMap::new(),
            i64_consts: Vec::new(),
            i64_const_cache: HashMap::new(),
            globals: Vec::new(),
            global_cache: HashMap::new(),
            next_reg: 0,
            next_float_reg: 0,
            next_int_reg: 0,
            scopes: vec![Scope::default()],
            loop_stack: Vec::new(),
            deferred_exprs: Vec::new(),
            deferred_envs: Vec::new(),
            deferred_env_regs: Vec::new(),
            next_cache_idx: 0,
            next_arith_cache_idx: 0,
            next_field_cache_idx: 0,
        }
    }

    /// Reserves a fresh inline-cache slot index for the current
    /// dispatch site and returns it. The matching slot is allocated
    /// by `finish` once the total count is known.
    fn alloc_cache_idx(&mut self) -> u16 {
        let idx = self.next_cache_idx;
        self.next_cache_idx = self.next_cache_idx.saturating_add(1);
        idx
    }

    /// Allocates a fresh `field_caches` slot for an
    /// `Op::FieldGet` site. Mirrors [`Self::alloc_cache_idx`]
    /// but for the PEP 659-style field-shape cache.
    fn alloc_field_cache_idx(&mut self) -> u16 {
        let idx = self.next_field_cache_idx;
        self.next_field_cache_idx = self.next_field_cache_idx.saturating_add(1);
        idx
    }

    fn f64_const_idx(&mut self, value: f64) -> ConstIdx {
        let key = value.to_bits();
        if let Some(idx) = self.f64_const_cache.get(&key) {
            return *idx;
        }
        let idx = ConstIdx::try_from(self.f64_consts.len()).expect("f64 const pool overflow");
        self.f64_consts.push(value);
        self.f64_const_cache.insert(key, idx);
        idx
    }

    fn i64_const_idx(&mut self, value: i64) -> ConstIdx {
        if let Some(idx) = self.i64_const_cache.get(&value) {
            return *idx;
        }
        let idx = ConstIdx::try_from(self.i64_consts.len()).expect("i64 const pool overflow");
        self.i64_consts.push(value);
        self.i64_const_cache.insert(value, idx);
        idx
    }

    fn alloc_reg(&mut self) -> Reg {
        let r = self.next_reg;
        self.next_reg = self.next_reg.checked_add(1).expect("register overflow");
        r
    }

    fn alloc_float(&mut self) -> Reg {
        let r = self.next_float_reg;
        self.next_float_reg = self
            .next_float_reg
            .checked_add(1)
            .expect("float register overflow");
        r
    }

    fn alloc_int(&mut self) -> Reg {
        let r = self.next_int_reg;
        self.next_int_reg = self
            .next_int_reg
            .checked_add(1)
            .expect("int register overflow");
        r
    }

    fn emit(&mut self, op: Op) -> InstrIdx {
        let idx = u32::try_from(self.instrs.len()).expect("instruction overflow");
        self.instrs.push(op);
        idx
    }

    fn cur_idx(&self) -> InstrIdx {
        u32::try_from(self.instrs.len()).expect("instruction overflow")
    }

    fn patch_jump(&mut self, idx: InstrIdx, target: InstrIdx) {
        match &mut self.instrs[idx as usize] {
            Op::Jump { target: t }
            | Op::BranchIf { target: t, .. }
            | Op::BranchIfNot { target: t, .. }
            | Op::BranchIfLtI64 { target: t, .. }
            | Op::BranchIfGeI64 { target: t, .. }
            | Op::BranchIfLtF64 { target: t, .. }
            | Op::BranchIfGeF64 { target: t, .. } => *t = target,
            other => panic!("cannot patch non-jump: {other:?}"),
        }
    }

    fn push_scope(&mut self) {
        self.scopes.push(Scope::default());
    }

    fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    fn bind_local(&mut self, name: &str, typed: TypedReg) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.locals.insert(name.to_string(), typed);
        }
    }

    fn lookup_local(&self, name: &str) -> Option<TypedReg> {
        for scope in self.scopes.iter().rev() {
            if let Some(typed) = scope.locals.get(name) {
                return Some(*typed);
            }
        }
        None
    }

    /// Classifies an expression's natural result kind from its
    /// HIR `Ty`. Unknown / aggregate / polymorphic types stay in
    /// `Value`; `f64` → `F64`, all integer types → `I64`. When
    /// the type is unknown (walker / error type) we default to
    /// `Value` so the generic path handles it.
    fn expr_kind(&self, expr: &HirExpr) -> RegKind {
        match self.tcx.kind(expr.ty) {
            Some(TyKind::Float(FloatTy::F64)) => RegKind::F64,
            Some(TyKind::Int(_)) => RegKind::I64,
            _ => RegKind::Value,
        }
    }

    /// If `ty` is a named struct whose layout is known, returns
    /// the offset of `field_name` within its declaration-order
    /// field list. Used to fold field reads/writes to the
    /// `ByOffset` op variants that skip the runtime name scan.
    fn resolve_struct_field_offset(&self, ty: gossamer_types::Ty, field_name: &str) -> Option<u16> {
        let ty = self.unwrap_ref(ty);
        let kind = self.tcx.kind(ty)?;
        let def = match kind {
            TyKind::Adt { def, .. } => *def,
            _ => return None,
        };
        let fields = self.layouts.get(&def)?;
        let idx = fields.iter().position(|f| f == field_name)?;
        u16::try_from(idx).ok()
    }

    /// Peels any `&T` / `&mut T` reference layers off a `Ty`,
    /// so type-directed optimisations work through reference
    /// binders (`fn energy(b: &[Body; 5])`).
    fn unwrap_ref(&self, mut ty: gossamer_types::Ty) -> gossamer_types::Ty {
        loop {
            match self.tcx.kind(ty) {
                Some(TyKind::Ref { inner, .. }) => ty = *inner,
                _ => return ty,
            }
        }
    }

    /// Returns the element type of an array / vec / slice,
    /// peeling reference layers first.
    fn array_elem_ty(&self, ty: gossamer_types::Ty) -> Option<gossamer_types::Ty> {
        let ty = self.unwrap_ref(ty);
        match self.tcx.kind(ty) {
            Some(TyKind::Array { elem, .. } | TyKind::Vec(elem) | TyKind::Slice(elem)) => {
                Some(*elem)
            }
            _ => None,
        }
    }

    /// Returns `true` when `ty` resolves to `HashMap<i64, i64>`,
    /// the typed shape that rides through `Value::IntMap`. The
    /// resolver may already have erased one or both of the
    /// generic args when the inference variable couldn't be
    /// pinned; callers fall back to the boxed `Value::Map` in
    /// that case rather than risk a typed op crashing on a
    /// non-`i64` payload.
    fn is_int_map_ty(&self, ty: gossamer_types::Ty) -> bool {
        let ty = self.unwrap_ref(ty);
        let Some(TyKind::HashMap { key, value }) = self.tcx.kind(ty) else {
            return false;
        };
        let key_is_i64 = matches!(
            self.tcx.kind(*key),
            Some(TyKind::Int(IntTy::I64 | IntTy::Isize | IntTy::Usize))
        );
        let value_is_i64 = matches!(
            self.tcx.kind(*value),
            Some(TyKind::Int(IntTy::I64 | IntTy::Isize | IntTy::Usize))
        );
        key_is_i64 && value_is_i64
    }

    /// Coerces a typed-reg into the `Value` register file,
    /// emitting `BoxF64` / `BoxI64` as required.
    fn as_value(&mut self, tr: TypedReg) -> Reg {
        match tr.kind {
            RegKind::Value => tr.reg,
            RegKind::F64 => {
                let dst = self.alloc_reg();
                self.emit(Op::BoxF64 {
                    dst_v: dst,
                    src_f: tr.reg,
                });
                dst
            }
            RegKind::I64 => {
                let dst = self.alloc_reg();
                self.emit(Op::BoxI64 {
                    dst_v: dst,
                    src_i: tr.reg,
                });
                dst
            }
        }
    }

    /// Coerces a typed-reg into the float register file.
    fn as_f64(&mut self, tr: TypedReg) -> Reg {
        if tr.kind == RegKind::F64 {
            tr.reg
        } else {
            let v = self.as_value(tr);
            let dst = self.alloc_float();
            self.emit(Op::UnboxF64 {
                dst_f: dst,
                src_v: v,
            });
            dst
        }
    }

    /// Coerces a typed-reg into the int register file.
    fn as_i64(&mut self, tr: TypedReg) -> Reg {
        if tr.kind == RegKind::I64 {
            tr.reg
        } else {
            let v = self.as_value(tr);
            let dst = self.alloc_int();
            self.emit(Op::UnboxI64 {
                dst_i: dst,
                src_v: v,
            });
            dst
        }
    }

    /// Allocates a fresh register of `tr`'s kind and emits the
    /// appropriate kind-specific move. Used by `let` bindings
    /// so subsequent reassignments can always target the
    /// local's fixed slot.
    fn bind_to_fresh(&mut self, tr: TypedReg) -> TypedReg {
        match tr.kind {
            RegKind::Value => {
                let dst = self.alloc_reg();
                self.emit(Op::Move { dst, src: tr.reg });
                TypedReg {
                    reg: dst,
                    kind: RegKind::Value,
                }
            }
            RegKind::F64 => {
                let dst = self.alloc_float();
                self.emit(Op::MoveF64 {
                    dst_f: dst,
                    src_f: tr.reg,
                });
                TypedReg {
                    reg: dst,
                    kind: RegKind::F64,
                }
            }
            RegKind::I64 => {
                let dst = self.alloc_int();
                self.emit(Op::MoveI64 {
                    dst_i: dst,
                    src_i: tr.reg,
                });
                TypedReg {
                    reg: dst,
                    kind: RegKind::I64,
                }
            }
        }
    }

    /// Moves a typed source into an existing destination
    /// register of the same kind. Used by `x = expr`
    /// reassignments so the local's slot stays put.
    fn emit_move_into(&mut self, dst: TypedReg, src: TypedReg) {
        match dst.kind {
            RegKind::Value => {
                let src_v = self.as_value(src);
                self.emit(Op::Move {
                    dst: dst.reg,
                    src: src_v,
                });
            }
            RegKind::F64 => {
                let src_f = self.as_f64(src);
                self.emit(Op::MoveF64 {
                    dst_f: dst.reg,
                    src_f,
                });
            }
            RegKind::I64 => {
                let src_i = self.as_i64(src);
                self.emit(Op::MoveI64 {
                    dst_i: dst.reg,
                    src_i,
                });
            }
        }
    }

    fn const_idx(&mut self, key: ConstKey, value: Value) -> ConstIdx {
        if let Some(idx) = self.const_cache.get(&key) {
            return *idx;
        }
        let idx = ConstIdx::try_from(self.consts.len()).expect("const pool overflow");
        self.consts.push(value);
        self.const_cache.insert(key, idx);
        idx
    }

    fn global_idx(&mut self, name: &str) -> GlobalIdx {
        if let Some(idx) = self.global_cache.get(name) {
            return *idx;
        }
        let idx = GlobalIdx::try_from(self.globals.len()).expect("global pool overflow");
        self.globals.push(name.to_string());
        self.global_cache.insert(name.to_string(), idx);
        idx
    }

    fn bind_param(&mut self, pattern: &HirPat, reg: Reg) {
        if let HirPatKind::Binding { name, .. } = &pattern.kind {
            self.bind_local(
                &name.name,
                TypedReg {
                    reg,
                    kind: RegKind::Value,
                },
            );
        }
    }

    fn finish(self, arity: u16) -> FnChunk {
        FnChunk {
            name: self.name,
            arity,
            register_count: self.next_reg,
            float_count: self.next_float_reg,
            int_count: self.next_int_reg,
            instrs: self.instrs,
            consts: self.consts,
            f64_consts: self.f64_consts,
            i64_consts: self.i64_consts,
            globals: self.globals,
            deferred_exprs: self.deferred_exprs,
            deferred_envs: self.deferred_envs,
            deferred_env_regs: self.deferred_env_regs,
            // The actual cache `Vec`s live in per-`Vm`
            // `ChunkState` and are sized from these counts. See
            // `vm::Vm::chunk_state_for`.
            call_cache_count: self.next_cache_idx,
            arith_cache_count: self.next_arith_cache_idx,
            field_cache_count: self.next_field_cache_idx,
        }
    }

    fn compile_block(&mut self, block: &HirBlock) -> RuntimeResult<BlockResult> {
        self.push_scope();
        let mut diverges = false;
        for stmt in &block.stmts {
            if self.compile_stmt(stmt)? {
                diverges = true;
            }
        }
        let result = if diverges {
            BlockResult::Diverges
        } else if let Some(tail) = &block.tail {
            let reg = self.compile_expr(tail)?;
            BlockResult::ValueIn(reg)
        } else {
            BlockResult::Unit
        };
        self.pop_scope();
        Ok(result)
    }

    /// Returns `true` when the statement diverges (return/break/continue).
    fn compile_stmt(&mut self, stmt: &HirStmt) -> RuntimeResult<bool> {
        match &stmt.kind {
            HirStmtKind::Let { pattern, init, .. } => {
                if let HirPatKind::Binding { name, .. } = &pattern.kind {
                    if let Some(init) = init {
                        // Compile the init in its natural kind.
                        // Most exprs produce a freshly-allocated
                        // reg we can bind directly; only a bare
                        // path lookup (`let y = x`) aliases an
                        // existing reg, so in that case copy
                        // into a fresh slot.
                        let tr = self.compile_expr_ex(init)?;
                        let typed = if is_path_expr(init) {
                            self.bind_to_fresh(tr)
                        } else {
                            tr
                        };
                        self.bind_local(&name.name, typed);
                    } else {
                        // Declared-only — default to Value; an
                        // assignment before read will overwrite.
                        let reg = self.alloc_reg();
                        self.bind_local(
                            &name.name,
                            TypedReg {
                                reg,
                                kind: RegKind::Value,
                            },
                        );
                    }
                } else if let Some(init) = init {
                    // Destructuring — fall back to Value and
                    // delegate the pattern match to the walker
                    // (kept on the generic path).
                    let _ = self.compile_expr(init)?;
                }
                Ok(false)
            }
            HirStmtKind::Expr { expr, .. } => {
                let _ = self.compile_expr(expr)?;
                Ok(expr_diverges(expr))
            }
            HirStmtKind::Go(expr) => {
                // Native goroutine spawn: compile the callee and
                // args directly into VM ops and emit `Op::Spawn`.
                // The dispatcher creates a fresh `Vm` on the new
                // thread that executes the call entirely in
                // bytecode — never re-entering the tree-walker.
                if self.try_compile_go_native(expr)? {
                    return Ok(false);
                }
                // Non-call shapes (e.g., `go { block }`) keep the
                // deferred path until we lower them too — but they
                // don't appear in the bench-game programs.
                let _ = self.compile_deferred(expr)?;
                Ok(false)
            }
            HirStmtKind::Defer(expr) => {
                // `defer` keeps tree-walker delegation: the VM
                // doesn't model the cleanup ordering it needs.
                let _ = self.compile_deferred(expr)?;
                Ok(false)
            }
            HirStmtKind::Item(_) => Err(RuntimeError::Unsupported("nested items")),
        }
    }

    /// Attempts a native lowering of `go callable(args)`. Returns
    /// `Ok(true)` when the spawn was emitted; `Ok(false)` when the
    /// shape is something the VM doesn't yet handle natively (and
    /// the caller should fall back to `compile_deferred`).
    fn try_compile_go_native(&mut self, expr: &HirExpr) -> RuntimeResult<bool> {
        // The HIR shape we native-lower is `go callable(args)` —
        // a `Call` whose callee is any expression and whose args
        // are arbitrary expressions. The runtime side accepts the
        // resulting callee `Value` (closure / global fn / builtin)
        // and walks the args vector exactly as the synchronous
        // dispatch path does.
        let HirExprKind::Call { callee, args } = &expr.kind else {
            return Ok(false);
        };
        let callee_reg = self.compile_expr(callee)?;
        let argc = u16::try_from(args.len()).map_err(|_| RuntimeError::Arity {
            expected: u16::MAX as usize,
            found: args.len(),
        })?;
        // Reserve a contiguous span before lowering args so
        // intermediate compiles don't grab overlapping registers.
        let args_start = self.next_reg;
        self.next_reg = self
            .next_reg
            .checked_add(argc)
            .expect("register overflow reserving spawn args");
        let arg_regs: Vec<Reg> = args
            .iter()
            .map(|arg| self.compile_expr(arg))
            .collect::<RuntimeResult<Vec<_>>>()?;
        for (i, arg_reg) in arg_regs.iter().enumerate() {
            let slot = args_start
                .checked_add(u16::try_from(i).unwrap())
                .expect("register overflow");
            self.emit(Op::Move {
                dst: slot,
                src: *arg_reg,
            });
        }
        self.emit(Op::Spawn {
            callee: callee_reg,
            args: args_start,
            argc,
        });
        Ok(true)
    }

    /// Typed counterpart to [`Self::compile_expr`]. Returns
    /// whatever kind the expression naturally produces,
    /// skipping the `BoxF64` / `BoxI64` round-trip when the
    /// result feeds into another typed consumer. Callers that
    /// need a `Value` register invoke [`Self::compile_expr`],
    /// which wraps this method and coerces via `as_value`.
    fn compile_expr_ex(&mut self, expr: &HirExpr) -> RuntimeResult<TypedReg> {
        match &expr.kind {
            // Numeric literals land in their typed reg file so
            // adjacent typed ops can consume them directly.
            HirExprKind::Literal(lit) => self.compile_literal_ex(lit, expr.ty),
            // Single-segment paths resolve to locals; the local
            // already carries its `TypedReg`, so we return it
            // as-is without boxing.
            HirExprKind::Path { segments, .. } if segments.len() == 1 => {
                if let Some(tr) = self.lookup_local(&segments[0].name) {
                    return Ok(tr);
                }
                let reg = self.compile_path(segments)?;
                Ok(TypedReg {
                    reg,
                    kind: RegKind::Value,
                })
            }
            HirExprKind::Binary { op, lhs, rhs } => self.compile_binary_ex(*op, lhs, rhs),
            HirExprKind::Unary { op, operand } => self.compile_unary_ex(*op, operand),
            HirExprKind::Call { callee, args } => {
                if let Some(tr) = self.try_intrinsic_call(callee, args)? {
                    return Ok(tr);
                }
                let reg = self.compile_call_ex(callee, args, expr.ty)?;
                Ok(TypedReg {
                    reg,
                    kind: RegKind::Value,
                })
            }
            HirExprKind::Field { receiver, name } => self.compile_field_ex(receiver, name, expr.ty),
            // Typed numeric cast. We classify the result and the
            // source by the existing `expr_kind` helper. The four
            // tractable combinations land directly in the right
            // typed register file:
            //
            //   i64 → f64   →  IntToFloatF64
            //   f64 → i64   →  FloatToIntI64
            //   i64 → i64   →  identity (already in I64 file)
            //   f64 → f64   →  identity (already in F64 file)
            //
            // Anything else (refs, custom From impls, trait dyn
            // casts) defers via the catch-all in `compile_expr`.
            HirExprKind::Cast { value, .. } => {
                let dst_kind = self.expr_kind(expr);
                let src_kind = self.expr_kind(value);
                match (dst_kind, src_kind) {
                    (RegKind::F64, RegKind::I64) => {
                        let src_tr = self.compile_expr_ex(value)?;
                        let src_i = self.as_i64(src_tr);
                        let dst_f = self.alloc_float();
                        self.emit(Op::IntToFloatF64 { dst_f, src_i });
                        Ok(TypedReg {
                            reg: dst_f,
                            kind: RegKind::F64,
                        })
                    }
                    (RegKind::I64, RegKind::F64) => {
                        let src_tr = self.compile_expr_ex(value)?;
                        let src_f = self.as_f64(src_tr);
                        let dst_i = self.alloc_int();
                        self.emit(Op::FloatToIntI64 { dst_i, src_f });
                        Ok(TypedReg {
                            reg: dst_i,
                            kind: RegKind::I64,
                        })
                    }
                    (RegKind::F64, RegKind::F64) | (RegKind::I64, RegKind::I64) => {
                        // No-op cast — re-classify the inner
                        // expression's typed result directly.
                        self.compile_expr_ex(value)
                    }
                    _ => {
                        let reg = self.compile_deferred(expr)?;
                        Ok(TypedReg {
                            reg,
                            kind: RegKind::Value,
                        })
                    }
                }
            }
            // Typed flat-i64 indexed read fast path. When the base
            // resolves to a local register marked as a
            // `Value::IntArray` (built via `try_build_int_array`)
            // and the parent expects an i64, we emit
            // `Op::IntArrayGetI64` which feeds the typed `i64`
            // register file directly — no `Value::Int` box/unbox.
            HirExprKind::Index { base, index }
                if matches!(self.tcx.kind(expr.ty), Some(TyKind::Int(_))) =>
            {
                let base_reg = self.compile_expr(base)?;
                if self.flat_int_locals.contains(&base_reg) {
                    let idx_tr = self.compile_expr_ex(index)?;
                    let idx_i = self.as_i64(idx_tr);
                    let dst_i = self.alloc_int();
                    self.emit(Op::IntArrayGetI64 {
                        dst_i,
                        base: base_reg,
                        index_i: idx_i,
                    });
                    return Ok(TypedReg {
                        reg: dst_i,
                        kind: RegKind::I64,
                    });
                }
                // Slow path: generic IndexGet → boxed Value reg.
                let idx_reg = self.compile_expr(index)?;
                let dst = self.alloc_reg();
                self.emit(Op::IndexGet {
                    dst,
                    base: base_reg,
                    index: idx_reg,
                });
                Ok(TypedReg {
                    reg: dst,
                    kind: RegKind::Value,
                })
            }
            // Typed flat-f64 indexed read fast path. Same shape as
            // the flat-i64 path above but for `Value::FloatVec` —
            // the inner-loop scratch arrays in nbody-style code
            // ride this branch.
            HirExprKind::Index { base, index }
                if matches!(self.tcx.kind(expr.ty), Some(TyKind::Float(FloatTy::F64))) =>
            {
                let base_reg = self.compile_expr(base)?;
                if self.flat_float_locals.contains(&base_reg) {
                    let idx_tr = self.compile_expr_ex(index)?;
                    let idx_i = self.as_i64(idx_tr);
                    let dst_f = self.alloc_float();
                    self.emit(Op::FloatVecGetF64 {
                        dst_f,
                        base: base_reg,
                        index_i: idx_i,
                    });
                    return Ok(TypedReg {
                        reg: dst_f,
                        kind: RegKind::F64,
                    });
                }
                // Slow path: generic IndexGet → boxed Value reg.
                let idx_reg = self.compile_expr(index)?;
                let dst = self.alloc_reg();
                self.emit(Op::IndexGet {
                    dst,
                    base: base_reg,
                    index: idx_reg,
                });
                Ok(TypedReg {
                    reg: dst,
                    kind: RegKind::Value,
                })
            }
            HirExprKind::Array(gossamer_hir::HirArrayExpr::List(elems)) => {
                if let Some(tr) = self.try_build_float_array(expr.ty, elems.as_slice())? {
                    return Ok(tr);
                }
                if let Some(tr) = self.try_build_int_array(expr.ty, elems.as_slice())? {
                    return Ok(tr);
                }
                if let Some(tr) = self.try_build_float_vec(expr.ty, elems.as_slice())? {
                    return Ok(tr);
                }
                let reg = self.compile_expr(expr)?;
                Ok(TypedReg {
                    reg,
                    kind: RegKind::Value,
                })
            }
            HirExprKind::Array(gossamer_hir::HirArrayExpr::Repeat { value, count }) => {
                if let Some(tr) = self.try_build_float_vec_repeat(expr.ty, value, count)? {
                    return Ok(tr);
                }
                if let Some(tr) = self.try_build_int_array_repeat(expr.ty, value, count)? {
                    return Ok(tr);
                }
                let reg = self.compile_expr(expr)?;
                Ok(TypedReg {
                    reg,
                    kind: RegKind::Value,
                })
            }
            // Everything else goes through the generic path,
            // which always yields a `Value` register.
            _ => {
                let reg = self.compile_expr(expr)?;
                Ok(TypedReg {
                    reg,
                    kind: RegKind::Value,
                })
            }
        }
    }

    fn compile_expr(&mut self, expr: &HirExpr) -> RuntimeResult<Reg> {
        match &expr.kind {
            HirExprKind::Literal(lit) => self.compile_literal(lit),
            HirExprKind::Path { segments, .. } => self.compile_path(segments),
            HirExprKind::Unary { op, operand } => self.compile_unary(*op, operand),
            HirExprKind::Binary { op, lhs, rhs } => self.compile_binary(*op, lhs, rhs),
            HirExprKind::Assign { place, value } => self.compile_assign(place, value),
            // Route through `_ex` so intrinsic-style calls
            // (`math::sqrt(x)`, etc.) get lowered to dedicated
            // opcodes when the arg kind is concrete f64, even
            // inside functions whose bodies are compiled via
            // the regular path (e.g. `fn fsqrt(x) { math::sqrt(x) }`).
            HirExprKind::Call { callee, args } => {
                let tr = {
                    let intr = self.try_intrinsic_call(callee, args)?;
                    if let Some(tr) = intr {
                        tr
                    } else {
                        let reg = self.compile_call_ex(callee, args, expr.ty)?;
                        TypedReg {
                            reg,
                            kind: RegKind::Value,
                        }
                    }
                };
                Ok(self.as_value(tr))
            }
            HirExprKind::If {
                condition,
                then_branch,
                else_branch,
            } => self.compile_if(condition, then_branch, else_branch.as_deref()),
            HirExprKind::While { condition, body } => self.compile_while(condition, body),
            // `Loop { body }` — native-compile only when the body
            // stays inside VM-handleable expression shapes.
            // Anything with an embedded Match (typically the
            // for-loop desugaring `loop { match iter.next() { ...
            // None => break } }`) defers the whole loop so the
            // walker can handle Break propagation correctly.
            HirExprKind::Loop { body } => {
                if body_contains_unsupported(body) {
                    self.compile_deferred(expr)
                } else {
                    self.compile_loop(body)
                }
            }
            HirExprKind::Block(block) => {
                let result = self.compile_block(block)?;
                Ok(match result {
                    BlockResult::Unit | BlockResult::Diverges => self.load_unit(),
                    BlockResult::ValueIn(reg) => reg,
                })
            }
            HirExprKind::Return(value) => self.compile_return(value.as_deref()),
            HirExprKind::Break(value) => self.compile_break(value.as_deref()),
            // Native `continue` — jump to the enclosing loop's
            // re-entry. Skips the `EvalDeferred` round trip the
            // walker would otherwise pay each iteration.
            HirExprKind::Continue => {
                let target = self
                    .loop_stack
                    .last()
                    .ok_or(RuntimeError::Unsupported("continue outside of loop"))?
                    .loop_start;
                self.emit(Op::Jump { target });
                Ok(self.load_unit())
            }
            // Native method dispatch — avoids the `EvalDeferred`
            // env-rebuild cost that dominated tight loops
            // (fasta's inner `out.write_byte(…)` etc.).
            HirExprKind::MethodCall {
                receiver,
                name,
                args,
                ..
            } => self.compile_method_call(receiver, name, args),
            // Native indexed read.
            HirExprKind::Index { base, index } => {
                let base_reg = self.compile_expr(base)?;
                let idx_reg = self.compile_expr(index)?;
                let dst = self.alloc_reg();
                self.emit(Op::IndexGet {
                    dst,
                    base: base_reg,
                    index: idx_reg,
                });
                Ok(dst)
            }
            // Native struct-field read.
            HirExprKind::Field { receiver, name } => {
                let recv_reg = self.compile_expr(receiver)?;
                let name_idx = self.const_idx(
                    ConstKey::String(name.name.clone()),
                    Value::String(SmolStr::from(name.name.clone())),
                );
                let dst = self.alloc_reg();
                let cache_idx = self.alloc_field_cache_idx();
                self.emit(Op::FieldGet {
                    dst,
                    receiver: recv_reg,
                    name_idx,
                    cache_idx,
                });
                Ok(dst)
            }
            // Native tuple / positional-field read.
            HirExprKind::TupleIndex { receiver, index } => {
                let recv_reg = self.compile_expr(receiver)?;
                let dst = self.alloc_reg();
                self.emit(Op::TupleIndex {
                    dst,
                    receiver: recv_reg,
                    index: *index,
                });
                Ok(dst)
            }
            // Cast — delegate to the typed compile path so the
            // typed-numeric arms fire, then box back into a
            // Value reg for whoever asked for one.
            HirExprKind::Cast { .. } => {
                let tr = self.compile_expr_ex(expr)?;
                Ok(self.as_value(tr))
            }
            // Native tuple literal — `(a, b, c)` lands in
            // `count` consecutive value registers, then
            // `Op::BuildTuple` packs them. No walker re-entry.
            HirExprKind::Tuple(elems) => {
                let n = elems.len();
                if n == 0 {
                    // Empty tuple is unit-shaped; just emit
                    // `Value::Tuple(Arc::new(vec![]))` via
                    // BuildTuple with count 0 to keep semantics
                    // honest.
                    let dst = self.alloc_reg();
                    self.emit(Op::BuildTuple {
                        dst,
                        first: 0,
                        count: 0,
                    });
                    return Ok(dst);
                }
                // Allocate a contiguous block of value registers
                // up front, then compile each elem into its
                // pre-assigned slot via Move. Doing it this way
                // (rather than naively `compile_expr` per elem
                // and hoping they land contiguously) keeps the
                // BuildTuple op's first-reg invariant.
                let first = self.alloc_reg();
                for _ in 1..n {
                    let _ = self.alloc_reg();
                }
                for (i, elem) in elems.iter().enumerate() {
                    let r = self.compile_expr(elem)?;
                    let slot = first + i as u16;
                    if r != slot {
                        self.emit(Op::Move { dst: slot, src: r });
                    }
                }
                let dst = self.alloc_reg();
                let count = u16::try_from(n).map_err(|_| {
                    RuntimeError::Unsupported("tuple literal exceeds 65535 elements")
                })?;
                self.emit(Op::BuildTuple { dst, first, count });
                Ok(dst)
            }
            // Anything the VM's native lowering doesn't handle
            // yet — match, closures, `go expr`, `continue`,
            // and the rest — falls through to `Op::EvalDeferred`.
            // The VM hands the expression + captured local
            // environment to a bundled tree-walker which
            // returns a Value. Result: the VM never fails at
            // compile time; it just does slower work for these
            // nodes until a native opcode is wired.
            _ => self.compile_deferred(expr),
        }
    }

    /// Captures the current locally-visible bindings (name → reg)
    /// and emits an `Op::EvalDeferred` that hands `expr` plus
    /// those values off to the bundled tree-walker. The reg
    /// list is stored in `deferred_env_regs` so the VM can both
    /// pass the values in and sync mutations back out.
    fn compile_deferred(&mut self, expr: &HirExpr) -> RuntimeResult<Reg> {
        // Snapshot the visible locals (inner scopes shadow
        // outer ones — overwrite slot for already-seen names).
        let mut entries: Vec<(String, TypedReg)> = Vec::new();
        for scope in &self.scopes {
            for (name, tr) in &scope.locals {
                if let Some(i) = entries.iter().position(|(n, _)| n == name) {
                    entries[i].1 = *tr;
                } else {
                    entries.push((name.clone(), *tr));
                }
            }
        }
        // Typed locals must cross the walker boundary as boxed
        // `Value`s. Box before the call, remember the (typed,
        // value_reg) pair, and unbox back after the walker runs
        // so mutations inside the deferred block propagate.
        let mut names: Vec<String> = Vec::with_capacity(entries.len());
        let mut regs: Vec<Reg> = Vec::with_capacity(entries.len());
        let mut writebacks: Vec<(TypedReg, Reg)> = Vec::new();
        for (name, tr) in entries {
            let value_reg = match tr.kind {
                RegKind::Value => tr.reg,
                RegKind::F64 => {
                    let dst = self.alloc_reg();
                    self.emit(Op::BoxF64 {
                        dst_v: dst,
                        src_f: tr.reg,
                    });
                    writebacks.push((tr, dst));
                    dst
                }
                RegKind::I64 => {
                    let dst = self.alloc_reg();
                    self.emit(Op::BoxI64 {
                        dst_v: dst,
                        src_i: tr.reg,
                    });
                    writebacks.push((tr, dst));
                    dst
                }
            };
            names.push(name);
            regs.push(value_reg);
        }
        let expr_idx =
            u32::try_from(self.deferred_exprs.len()).expect("deferred expression index overflow");
        self.deferred_exprs.push(expr.clone());
        self.deferred_envs.push(names);
        self.deferred_env_regs.push(regs);
        let dst = self.alloc_reg();
        self.emit(Op::EvalDeferred { dst, expr_idx });
        for (tr, vr) in writebacks {
            match tr.kind {
                RegKind::F64 => {
                    self.emit(Op::UnboxF64 {
                        dst_f: tr.reg,
                        src_v: vr,
                    });
                }
                RegKind::I64 => {
                    self.emit(Op::UnboxI64 {
                        dst_i: tr.reg,
                        src_v: vr,
                    });
                }
                RegKind::Value => {}
            }
        }
        Ok(dst)
    }

    fn compile_literal(&mut self, lit: &HirLiteral) -> RuntimeResult<Reg> {
        let (key, value) = literal_const(lit);
        let idx = self.const_idx(key, value);
        let dst = self.alloc_reg();
        self.emit(Op::LoadConst { dst, idx });
        Ok(dst)
    }

    fn compile_path(&mut self, segments: &[Ident]) -> RuntimeResult<Reg> {
        let Some(first) = segments.first() else {
            return Err(RuntimeError::UnresolvedName(String::new()));
        };
        if segments.len() == 1 {
            if let Some(tr) = self.lookup_local(&first.name) {
                return Ok(self.as_value(tr));
            }
        }
        // For multi-segment paths (`fmt::println`,
        // `http::Response::text`, ...) the VM has two builtins to
        // pick between: one registered under the tail name
        // (`text`) and one under the fully-qualified path
        // (`http::Response::text`). Emit a LoadGlobal keyed on the
        // full join — the global table has entries for both, and
        // the qualified key is unambiguous.
        let name = if segments.len() > 1 {
            segments
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>()
                .join("::")
        } else {
            first.name.clone()
        };
        let idx = self.global_idx(&name);
        let dst = self.alloc_reg();
        self.emit(Op::LoadGlobal { dst, idx });
        Ok(dst)
    }

    fn compile_unary(&mut self, op: HirUnaryOp, operand: &HirExpr) -> RuntimeResult<Reg> {
        let operand_reg = self.compile_expr(operand)?;
        let dst = self.alloc_reg();
        let instr = match op {
            HirUnaryOp::Neg => Op::Neg {
                dst,
                operand: operand_reg,
            },
            HirUnaryOp::Not => Op::Not {
                dst,
                operand: operand_reg,
            },
            HirUnaryOp::RefShared | HirUnaryOp::RefMut | HirUnaryOp::Deref => Op::Move {
                dst,
                src: operand_reg,
            },
        };
        self.emit(instr);
        Ok(dst)
    }

    fn compile_binary(
        &mut self,
        op: HirBinaryOp,
        lhs: &HirExpr,
        rhs: &HirExpr,
    ) -> RuntimeResult<Reg> {
        if matches!(op, HirBinaryOp::And | HirBinaryOp::Or) {
            return self.compile_short_circuit(op, lhs, rhs);
        }
        // Route through `_ex` so two-f64 / two-i64 binary
        // trees stay in the typed register file end-to-end.
        // The result gets boxed only if the caller needs a
        // `Value`.
        let tr = self.compile_binary_ex(op, lhs, rhs)?;
        Ok(self.as_value(tr))
    }

    /// Typed binary-op compile. Emits `AddF64` / `LtI64` /
    /// etc. when both operands share a concrete numeric kind;
    /// otherwise falls back to the generic `binary_op` path
    /// (which operates on `Value` regs).
    fn compile_binary_ex(
        &mut self,
        op: HirBinaryOp,
        lhs: &HirExpr,
        rhs: &HirExpr,
    ) -> RuntimeResult<TypedReg> {
        if matches!(op, HirBinaryOp::And | HirBinaryOp::Or) {
            let reg = self.compile_short_circuit(op, lhs, rhs)?;
            return Ok(TypedReg {
                reg,
                kind: RegKind::Value,
            });
        }
        let lk = self.expr_kind(lhs);
        let rk = self.expr_kind(rhs);
        // Both operands f64 — emit a typed f64 op. For `+-*/`
        // the result is also f64; for comparisons it's a
        // `Bool` Value.
        if lk == RegKind::F64 && rk == RegKind::F64 {
            // Peephole fuse `a * b + c` / `c + a * b` /
            // `c - a * b` into `MulAddF64` / `MulSubF64`
            // before touching operand evaluation. Halves the
            // op count on any vector-math-style expression
            // tree (`x + dt * vx`, `vx - dx * mag`, ...).
            if let Some(tr) = self.try_compile_fma(op, lhs, rhs)? {
                return Ok(tr);
            }
            let lhs_tr = self.compile_expr_ex(lhs)?;
            let rhs_tr = self.compile_expr_ex(rhs)?;
            let lhs_f = self.as_f64(lhs_tr);
            let rhs_f = self.as_f64(rhs_tr);
            return self.emit_binary_f64(op, lhs_f, rhs_f);
        }
        if lk == RegKind::I64 && rk == RegKind::I64 {
            let lhs_tr = self.compile_expr_ex(lhs)?;
            let rhs_tr = self.compile_expr_ex(rhs)?;
            let lhs_i = self.as_i64(lhs_tr);
            let rhs_i = self.as_i64(rhs_tr);
            return self.emit_binary_i64(op, lhs_i, rhs_i);
        }
        // Fallback: generic path on Value regs.
        let lhs_reg = self.compile_expr(lhs)?;
        let rhs_reg = self.compile_expr(rhs)?;
        let dst = self.alloc_reg();
        let instr = self
            .binary_op(op, dst, lhs_reg, rhs_reg)
            .ok_or(RuntimeError::Unsupported("binary op kind"))?;
        self.emit(instr);
        Ok(TypedReg {
            reg: dst,
            kind: RegKind::Value,
        })
    }

    /// Allocates a fresh `arith_caches` slot for a Tier-C2
    /// adaptive op and returns its index. Each emit site gets
    /// its own slot so observed shapes don't bleed across call
    /// sites that happen to flow through the same handler.
    fn next_arith_cache(&mut self) -> u16 {
        let idx = self.next_arith_cache_idx;
        self.next_arith_cache_idx = self.next_arith_cache_idx.saturating_add(1);
        idx
    }

    /// Builds the boxed-`Value` op for `op` on `(lhs, rhs)`
    /// destined for `dst`. Adaptive arith variants (Add/Sub/Mul/
    /// Div/Rem) allocate a fresh cache slot here so the runtime
    /// has somewhere to record the observed shape (Tier C2).
    fn binary_op(&mut self, op: HirBinaryOp, dst: Reg, lhs: Reg, rhs: Reg) -> Option<Op> {
        Some(match op {
            HirBinaryOp::Add => Op::AddInt {
                dst,
                lhs,
                rhs,
                cache_idx: self.next_arith_cache(),
            },
            HirBinaryOp::Sub => Op::SubInt {
                dst,
                lhs,
                rhs,
                cache_idx: self.next_arith_cache(),
            },
            HirBinaryOp::Mul => Op::MulInt {
                dst,
                lhs,
                rhs,
                cache_idx: self.next_arith_cache(),
            },
            HirBinaryOp::Div => Op::DivInt {
                dst,
                lhs,
                rhs,
                cache_idx: self.next_arith_cache(),
            },
            HirBinaryOp::Rem => Op::RemInt {
                dst,
                lhs,
                rhs,
                cache_idx: self.next_arith_cache(),
            },
            HirBinaryOp::Eq => Op::Eq { dst, lhs, rhs },
            HirBinaryOp::Ne => Op::Ne { dst, lhs, rhs },
            HirBinaryOp::Lt => Op::Lt { dst, lhs, rhs },
            HirBinaryOp::Le => Op::Le { dst, lhs, rhs },
            HirBinaryOp::Gt => Op::Gt { dst, lhs, rhs },
            HirBinaryOp::Ge => Op::Ge { dst, lhs, rhs },
            _ => return None,
        })
    }

    /// Matches `a * b + c`, `c + a * b`, or `c - a * b` in the
    /// HIR and emits a single fused-multiply-{add,sub} op
    /// instead of the two-op sequence. All three operands must
    /// resolve to concrete f64 kinds.
    fn try_compile_fma(
        &mut self,
        op: HirBinaryOp,
        lhs: &HirExpr,
        rhs: &HirExpr,
    ) -> RuntimeResult<Option<TypedReg>> {
        match op {
            HirBinaryOp::Add => {
                // a * b + c
                if let HirExprKind::Binary {
                    op: HirBinaryOp::Mul,
                    lhs: ma,
                    rhs: mb,
                } = &lhs.kind
                {
                    if self.expr_kind(ma) == RegKind::F64
                        && self.expr_kind(mb) == RegKind::F64
                        && self.expr_kind(rhs) == RegKind::F64
                    {
                        let a_tr = self.compile_expr_ex(ma)?;
                        let b_tr = self.compile_expr_ex(mb)?;
                        let c_tr = self.compile_expr_ex(rhs)?;
                        let a_f = self.as_f64(a_tr);
                        let b_f = self.as_f64(b_tr);
                        let c_f = self.as_f64(c_tr);
                        let dst = self.alloc_float();
                        self.emit(Op::MulAddF64 {
                            dst_f: dst,
                            a_f,
                            b_f,
                            c_f,
                        });
                        return Ok(Some(TypedReg {
                            reg: dst,
                            kind: RegKind::F64,
                        }));
                    }
                }
                // c + a * b
                if let HirExprKind::Binary {
                    op: HirBinaryOp::Mul,
                    lhs: ma,
                    rhs: mb,
                } = &rhs.kind
                {
                    if self.expr_kind(ma) == RegKind::F64
                        && self.expr_kind(mb) == RegKind::F64
                        && self.expr_kind(lhs) == RegKind::F64
                    {
                        let c_tr = self.compile_expr_ex(lhs)?;
                        let a_tr = self.compile_expr_ex(ma)?;
                        let b_tr = self.compile_expr_ex(mb)?;
                        let a_f = self.as_f64(a_tr);
                        let b_f = self.as_f64(b_tr);
                        let c_f = self.as_f64(c_tr);
                        let dst = self.alloc_float();
                        self.emit(Op::MulAddF64 {
                            dst_f: dst,
                            a_f,
                            b_f,
                            c_f,
                        });
                        return Ok(Some(TypedReg {
                            reg: dst,
                            kind: RegKind::F64,
                        }));
                    }
                }
            }
            HirBinaryOp::Sub => {
                // c - a * b
                if let HirExprKind::Binary {
                    op: HirBinaryOp::Mul,
                    lhs: ma,
                    rhs: mb,
                } = &rhs.kind
                {
                    if self.expr_kind(ma) == RegKind::F64
                        && self.expr_kind(mb) == RegKind::F64
                        && self.expr_kind(lhs) == RegKind::F64
                    {
                        let c_tr = self.compile_expr_ex(lhs)?;
                        let a_tr = self.compile_expr_ex(ma)?;
                        let b_tr = self.compile_expr_ex(mb)?;
                        let a_f = self.as_f64(a_tr);
                        let b_f = self.as_f64(b_tr);
                        let c_f = self.as_f64(c_tr);
                        let dst = self.alloc_float();
                        self.emit(Op::MulSubF64 {
                            dst_f: dst,
                            a_f,
                            b_f,
                            c_f,
                        });
                        return Ok(Some(TypedReg {
                            reg: dst,
                            kind: RegKind::F64,
                        }));
                    }
                }
            }
            _ => {}
        }
        Ok(None)
    }

    fn emit_binary_f64(
        &mut self,
        op: HirBinaryOp,
        lhs_f: Reg,
        rhs_f: Reg,
    ) -> RuntimeResult<TypedReg> {
        match op {
            HirBinaryOp::Add => {
                let dst = self.alloc_float();
                self.emit(Op::AddF64 {
                    dst_f: dst,
                    lhs_f,
                    rhs_f,
                });
                Ok(TypedReg {
                    reg: dst,
                    kind: RegKind::F64,
                })
            }
            HirBinaryOp::Sub => {
                let dst = self.alloc_float();
                self.emit(Op::SubF64 {
                    dst_f: dst,
                    lhs_f,
                    rhs_f,
                });
                Ok(TypedReg {
                    reg: dst,
                    kind: RegKind::F64,
                })
            }
            // (handled by the caller via `try_compile_fma` —
            // this arm is the non-fused fallback)
            HirBinaryOp::Mul => {
                let dst = self.alloc_float();
                self.emit(Op::MulF64 {
                    dst_f: dst,
                    lhs_f,
                    rhs_f,
                });
                Ok(TypedReg {
                    reg: dst,
                    kind: RegKind::F64,
                })
            }
            HirBinaryOp::Div => {
                let dst = self.alloc_float();
                self.emit(Op::DivF64 {
                    dst_f: dst,
                    lhs_f,
                    rhs_f,
                });
                Ok(TypedReg {
                    reg: dst,
                    kind: RegKind::F64,
                })
            }
            HirBinaryOp::Lt => {
                let dst = self.alloc_reg();
                self.emit(Op::LtF64 {
                    dst_v: dst,
                    lhs_f,
                    rhs_f,
                });
                Ok(TypedReg {
                    reg: dst,
                    kind: RegKind::Value,
                })
            }
            HirBinaryOp::Le => {
                let dst = self.alloc_reg();
                self.emit(Op::LeF64 {
                    dst_v: dst,
                    lhs_f,
                    rhs_f,
                });
                Ok(TypedReg {
                    reg: dst,
                    kind: RegKind::Value,
                })
            }
            HirBinaryOp::Gt => {
                let dst = self.alloc_reg();
                self.emit(Op::GtF64 {
                    dst_v: dst,
                    lhs_f,
                    rhs_f,
                });
                Ok(TypedReg {
                    reg: dst,
                    kind: RegKind::Value,
                })
            }
            HirBinaryOp::Ge => {
                let dst = self.alloc_reg();
                self.emit(Op::GeF64 {
                    dst_v: dst,
                    lhs_f,
                    rhs_f,
                });
                Ok(TypedReg {
                    reg: dst,
                    kind: RegKind::Value,
                })
            }
            HirBinaryOp::Eq => {
                let dst = self.alloc_reg();
                self.emit(Op::EqF64 {
                    dst_v: dst,
                    lhs_f,
                    rhs_f,
                });
                Ok(TypedReg {
                    reg: dst,
                    kind: RegKind::Value,
                })
            }
            HirBinaryOp::Ne => {
                let dst = self.alloc_reg();
                self.emit(Op::NeF64 {
                    dst_v: dst,
                    lhs_f,
                    rhs_f,
                });
                Ok(TypedReg {
                    reg: dst,
                    kind: RegKind::Value,
                })
            }
            _ => Err(RuntimeError::Unsupported("f64 binary op kind")),
        }
    }

    fn emit_binary_i64(
        &mut self,
        op: HirBinaryOp,
        lhs_i: Reg,
        rhs_i: Reg,
    ) -> RuntimeResult<TypedReg> {
        match op {
            HirBinaryOp::Add => {
                let dst = self.alloc_int();
                self.emit(Op::AddI64 {
                    dst_i: dst,
                    lhs_i,
                    rhs_i,
                });
                Ok(TypedReg {
                    reg: dst,
                    kind: RegKind::I64,
                })
            }
            HirBinaryOp::Sub => {
                let dst = self.alloc_int();
                self.emit(Op::SubI64 {
                    dst_i: dst,
                    lhs_i,
                    rhs_i,
                });
                Ok(TypedReg {
                    reg: dst,
                    kind: RegKind::I64,
                })
            }
            HirBinaryOp::Mul => {
                let dst = self.alloc_int();
                self.emit(Op::MulI64 {
                    dst_i: dst,
                    lhs_i,
                    rhs_i,
                });
                Ok(TypedReg {
                    reg: dst,
                    kind: RegKind::I64,
                })
            }
            HirBinaryOp::Div => {
                let dst = self.alloc_int();
                self.emit(Op::DivI64 {
                    dst_i: dst,
                    lhs_i,
                    rhs_i,
                });
                Ok(TypedReg {
                    reg: dst,
                    kind: RegKind::I64,
                })
            }
            HirBinaryOp::Rem => {
                let dst = self.alloc_int();
                self.emit(Op::RemI64 {
                    dst_i: dst,
                    lhs_i,
                    rhs_i,
                });
                Ok(TypedReg {
                    reg: dst,
                    kind: RegKind::I64,
                })
            }
            HirBinaryOp::Lt => {
                let dst = self.alloc_reg();
                self.emit(Op::LtI64 {
                    dst_v: dst,
                    lhs_i,
                    rhs_i,
                });
                Ok(TypedReg {
                    reg: dst,
                    kind: RegKind::Value,
                })
            }
            HirBinaryOp::Le => {
                let dst = self.alloc_reg();
                self.emit(Op::LeI64 {
                    dst_v: dst,
                    lhs_i,
                    rhs_i,
                });
                Ok(TypedReg {
                    reg: dst,
                    kind: RegKind::Value,
                })
            }
            HirBinaryOp::Gt => {
                let dst = self.alloc_reg();
                self.emit(Op::GtI64 {
                    dst_v: dst,
                    lhs_i,
                    rhs_i,
                });
                Ok(TypedReg {
                    reg: dst,
                    kind: RegKind::Value,
                })
            }
            HirBinaryOp::Ge => {
                let dst = self.alloc_reg();
                self.emit(Op::GeI64 {
                    dst_v: dst,
                    lhs_i,
                    rhs_i,
                });
                Ok(TypedReg {
                    reg: dst,
                    kind: RegKind::Value,
                })
            }
            HirBinaryOp::Eq => {
                let dst = self.alloc_reg();
                self.emit(Op::EqI64 {
                    dst_v: dst,
                    lhs_i,
                    rhs_i,
                });
                Ok(TypedReg {
                    reg: dst,
                    kind: RegKind::Value,
                })
            }
            HirBinaryOp::Ne => {
                let dst = self.alloc_reg();
                self.emit(Op::NeI64 {
                    dst_v: dst,
                    lhs_i,
                    rhs_i,
                });
                Ok(TypedReg {
                    reg: dst,
                    kind: RegKind::Value,
                })
            }
            HirBinaryOp::BitAnd => {
                let dst = self.alloc_int();
                self.emit(Op::BitAndI64 {
                    dst_i: dst,
                    lhs_i,
                    rhs_i,
                });
                Ok(TypedReg {
                    reg: dst,
                    kind: RegKind::I64,
                })
            }
            HirBinaryOp::BitOr => {
                let dst = self.alloc_int();
                self.emit(Op::BitOrI64 {
                    dst_i: dst,
                    lhs_i,
                    rhs_i,
                });
                Ok(TypedReg {
                    reg: dst,
                    kind: RegKind::I64,
                })
            }
            HirBinaryOp::BitXor => {
                let dst = self.alloc_int();
                self.emit(Op::BitXorI64 {
                    dst_i: dst,
                    lhs_i,
                    rhs_i,
                });
                Ok(TypedReg {
                    reg: dst,
                    kind: RegKind::I64,
                })
            }
            HirBinaryOp::Shl => {
                let dst = self.alloc_int();
                self.emit(Op::ShlI64 {
                    dst_i: dst,
                    lhs_i,
                    rhs_i,
                });
                Ok(TypedReg {
                    reg: dst,
                    kind: RegKind::I64,
                })
            }
            HirBinaryOp::Shr => {
                let dst = self.alloc_int();
                self.emit(Op::ShrI64 {
                    dst_i: dst,
                    lhs_i,
                    rhs_i,
                });
                Ok(TypedReg {
                    reg: dst,
                    kind: RegKind::I64,
                })
            }
            _ => Err(RuntimeError::Unsupported("i64 binary op kind")),
        }
    }

    fn compile_unary_ex(&mut self, op: HirUnaryOp, operand: &HirExpr) -> RuntimeResult<TypedReg> {
        let kind = self.expr_kind(operand);
        match (op, kind) {
            (HirUnaryOp::Neg, RegKind::F64) => {
                let tr = self.compile_expr_ex(operand)?;
                let src_f = self.as_f64(tr);
                let dst = self.alloc_float();
                self.emit(Op::NegF64 { dst_f: dst, src_f });
                Ok(TypedReg {
                    reg: dst,
                    kind: RegKind::F64,
                })
            }
            (HirUnaryOp::Neg, RegKind::I64) => {
                let tr = self.compile_expr_ex(operand)?;
                let src_i = self.as_i64(tr);
                let dst = self.alloc_int();
                self.emit(Op::NegI64 { dst_i: dst, src_i });
                Ok(TypedReg {
                    reg: dst,
                    kind: RegKind::I64,
                })
            }
            _ => {
                let reg = self.compile_unary(op, operand)?;
                Ok(TypedReg {
                    reg,
                    kind: RegKind::Value,
                })
            }
        }
    }

    fn compile_literal_ex(&mut self, lit: &HirLiteral, _ty: Ty) -> RuntimeResult<TypedReg> {
        match lit {
            HirLiteral::Float(text) => {
                let value = strip_float_suffix(text).parse::<f64>().unwrap_or(0.0);
                let idx = self.f64_const_idx(value);
                let dst = self.alloc_float();
                self.emit(Op::LoadConstF64 { dst_f: dst, idx });
                Ok(TypedReg {
                    reg: dst,
                    kind: RegKind::F64,
                })
            }
            HirLiteral::Int(text) => {
                if let Some(n) = parse_int(text) {
                    let idx = self.i64_const_idx(n);
                    let dst = self.alloc_int();
                    self.emit(Op::LoadConstI64 { dst_i: dst, idx });
                    return Ok(TypedReg {
                        reg: dst,
                        kind: RegKind::I64,
                    });
                }
                let reg = self.compile_literal(lit)?;
                Ok(TypedReg {
                    reg,
                    kind: RegKind::Value,
                })
            }
            _ => {
                let reg = self.compile_literal(lit)?;
                Ok(TypedReg {
                    reg,
                    kind: RegKind::Value,
                })
            }
        }
    }

    /// Detects `[S; N]` array literals where `S` is a struct
    /// whose fields are all `f64`, and emits a flat-f64
    /// `Op::BuildFloatArray` instead of constructing the
    /// boxed `Value::Array<Value::Struct>` form. Subsequent
    /// indexed field access on the resulting local routes
    /// through the flat fast path in the VM.
    fn try_build_float_array(
        &mut self,
        array_ty: Ty,
        elems: &[HirExpr],
    ) -> RuntimeResult<Option<TypedReg>> {
        // Require a concrete array-of-struct shape.
        let elem_ty = match self.tcx.kind(array_ty) {
            Some(TyKind::Array { elem, .. } | TyKind::Vec(elem) | TyKind::Slice(elem)) => *elem,
            _ => return Ok(None),
        };
        let (def, struct_name) = match self.tcx.kind(elem_ty) {
            Some(TyKind::Adt { def, .. }) => {
                let Some(layout) = self.layouts.get(def) else {
                    return Ok(None);
                };
                // Need a name for rehydration; grab it from
                // any layout key. We don't have a DefId→Name
                // table here, so rely on each element `__struct`
                // call carrying the name string.
                let _ = layout;
                (*def, "")
            }
            _ => return Ok(None),
        };
        let Some(field_names) = self.layouts.get(&def).cloned() else {
            return Ok(None);
        };
        // If the type context knows the declared field types,
        // require every one to be `f64`. When the types aren't
        // registered (e.g. for programs whose resolver didn't
        // populate `struct_field_tys`) we still try the fast
        // path as long as every element in the literal is
        // clearly the same struct — the `__struct` parse below
        // sees the actual field values, so a later type mismatch
        // would just fall back at runtime.
        if let Some(tys) = self.tcx.struct_field_tys(def) {
            let all_f64 = tys
                .iter()
                .all(|t| matches!(self.tcx.kind(*t), Some(TyKind::Float(FloatTy::F64))));
            if !all_f64 {
                return Ok(None);
            }
        }
        if field_names.is_empty() {
            return Ok(None);
        }
        let Ok(stride) = u16::try_from(field_names.len()) else {
            return Ok(None);
        };
        let Ok(elem_count) = u16::try_from(elems.len()) else {
            return Ok(None);
        };
        // Pick up the struct name from the first element's
        // `__struct(name, ...)` call; fall back to the layout
        // map if we've seen an explicit name before.
        let _ = struct_name;
        let mut struct_name_found: Option<String> = None;
        // Each element must be a `Call(__struct, args)` whose
        // arg layout matches `name, fname, value, fname, value, …`.
        // Collect the per-element field expressions, keyed by
        // field name.
        let mut per_elem: Vec<std::collections::HashMap<String, &HirExpr>> =
            Vec::with_capacity(elems.len());
        for elem in elems {
            let HirExprKind::Call { callee, args } = &elem.kind else {
                return Ok(None);
            };
            let HirExprKind::Path { segments, .. } = &callee.kind else {
                return Ok(None);
            };
            if segments.len() != 1 || segments[0].name != "__struct" {
                return Ok(None);
            }
            // args: [String(name), String(field1), Value1, ...]
            if args.is_empty() {
                return Ok(None);
            }
            if let HirExprKind::Literal(HirLiteral::String(s)) = &args[0].kind {
                if struct_name_found.is_none() {
                    struct_name_found = Some(s.clone());
                }
            }
            let mut map = std::collections::HashMap::new();
            let rest = &args[1..];
            let mut i = 0;
            while i + 1 < rest.len() {
                let HirExprKind::Literal(HirLiteral::String(fname)) = &rest[i].kind else {
                    return Ok(None);
                };
                map.insert(fname.clone(), &rest[i + 1]);
                i += 2;
            }
            per_elem.push(map);
        }
        let struct_name = struct_name_found.unwrap_or_default();
        // Allocate `stride * elem_count` contiguous float regs.
        let first_f = self.next_float_reg;
        let total = u32::from(stride) * u32::from(elem_count);
        if total > u32::from(u16::MAX - first_f) {
            return Ok(None);
        }
        self.next_float_reg = first_f + total as u16;
        // Compile each field's value expression into the matching
        // float slot.
        for (elem_idx, fields) in per_elem.iter().enumerate() {
            for (field_idx, fname) in field_names.iter().enumerate() {
                let target = first_f + elem_idx as u16 * stride + field_idx as u16;
                if let Some(value_expr) = fields.get(fname) {
                    let tr = self.compile_expr_ex(value_expr)?;
                    let src_f = self.as_f64(tr);
                    self.emit(Op::MoveF64 {
                        dst_f: target,
                        src_f,
                    });
                } else {
                    let idx = self.f64_const_idx(0.0);
                    self.emit(Op::LoadConstF64 { dst_f: target, idx });
                }
            }
        }
        // Intern the struct name + field-name metadata in the
        // const pool so the `BuildFloatArray` op can rehydrate
        // lazily.
        let name_idx = self.const_idx(
            ConstKey::String(struct_name.clone()),
            Value::String(struct_name.into()),
        );
        let fields_key = field_names.join("\0");
        let fields_value = Value::Array(Arc::new(
            field_names
                .iter()
                .map(|n| Value::String(SmolStr::from(n.clone())))
                .collect::<Vec<_>>(),
        ));
        let fields_idx = self.const_idx(ConstKey::String(fields_key), fields_value);
        let dst = self.alloc_reg();
        self.emit(Op::BuildFloatArray {
            dst_v: dst,
            name_idx,
            fields_idx,
            stride,
            elem_count,
            first_f,
        });
        // Record the register as known-flat so subsequent
        // indexed-field reads / writes can emit
        // `Flat{Get,Set}F64` and skip the runtime
        // discriminant check.
        self.flat_locals.insert(dst, stride);
        Ok(Some(TypedReg {
            reg: dst,
            kind: RegKind::Value,
        }))
    }

    /// Mirror of [`Self::try_build_float_array`] for the
    /// primitive `[i64; N]` shape. When the literal's element
    /// type is `i64` we emit `Op::BuildIntArray` (writing into
    /// the typed `i64` register file) instead of the
    /// general-purpose boxed-`Value::Array<Value::Int>` form.
    /// fasta's TWO/THREE inner loops index two such arrays
    /// (`iub_cut`, `iub_ch`) several times per output byte;
    /// keeping their storage as raw `Vec<i64>` lets
    /// [`Op::IntArrayGetI64`] feed the typed `i64` registers
    /// directly.
    fn try_build_int_array(
        &mut self,
        array_ty: Ty,
        elems: &[HirExpr],
    ) -> RuntimeResult<Option<TypedReg>> {
        let elem_ty = match self.tcx.kind(array_ty) {
            Some(TyKind::Array { elem, .. } | TyKind::Vec(elem) | TyKind::Slice(elem)) => *elem,
            _ => return Ok(None),
        };
        let elem_is_i64 = matches!(
            self.tcx.kind(elem_ty),
            Some(TyKind::Int(IntTy::I64 | IntTy::Isize | IntTy::Usize))
        );
        if !elem_is_i64 {
            return Ok(None);
        }
        let Ok(count) = u16::try_from(elems.len()) else {
            return Ok(None);
        };
        // Allocate `count` contiguous i64 registers. `compile_expr_ex`
        // on each element returns a TypedReg; we coerce to i64 via
        // `as_i64`.
        let first_i = self.next_int_reg;
        if u32::from(count) > u32::from(u16::MAX - first_i) {
            return Ok(None);
        }
        self.next_int_reg = first_i + count;
        for (i, elem) in elems.iter().enumerate() {
            let target = first_i + u16::try_from(i).expect("count overflow");
            let tr = self.compile_expr_ex(elem)?;
            let src_i = self.as_i64(tr);
            self.emit(Op::MoveI64 {
                dst_i: target,
                src_i,
            });
        }
        let dst = self.alloc_reg();
        self.emit(Op::BuildIntArray {
            dst_v: dst,
            first_i,
            count,
        });
        // Track for the indexing fast path so subsequent
        // `arr[k]` reads route through `Op::IntArrayGetI64`.
        self.flat_int_locals.insert(dst);
        Ok(Some(TypedReg {
            reg: dst,
            kind: RegKind::Value,
        }))
    }

    /// Repeat-form variant of [`Self::try_build_float_vec`] for
    /// `[value; count]` shapes where the count is a literal that
    /// fits in `u16`. Evaluates `value` once into an f64 register
    /// and broadcasts it across the `FloatVec`'s storage with a
    /// constant-fill loop.
    fn try_build_float_vec_repeat(
        &mut self,
        array_ty: Ty,
        value: &HirExpr,
        count: &HirExpr,
    ) -> RuntimeResult<Option<TypedReg>> {
        let elem_ty = match self.tcx.kind(array_ty) {
            Some(TyKind::Array { elem, .. } | TyKind::Vec(elem) | TyKind::Slice(elem)) => *elem,
            _ => return Ok(None),
        };
        if !matches!(self.tcx.kind(elem_ty), Some(TyKind::Float(FloatTy::F64))) {
            return Ok(None);
        }
        let Some(n) = resolve_const_count(count) else {
            return Ok(None);
        };
        let Ok(count_u) = u16::try_from(n) else {
            return Ok(None);
        };
        let first_f = self.next_float_reg;
        if u32::from(count_u) > u32::from(u16::MAX - first_f) {
            return Ok(None);
        }
        self.next_float_reg = first_f + count_u;
        // Compile the source value once; broadcast into every slot.
        let src_tr = self.compile_expr_ex(value)?;
        let src_f = self.as_f64(src_tr);
        for i in 0..count_u {
            let target = first_f + i;
            self.emit(Op::MoveF64 {
                dst_f: target,
                src_f,
            });
        }
        let dst = self.alloc_reg();
        self.emit(Op::BuildFloatVec {
            dst_v: dst,
            first_f,
            count: count_u,
        });
        self.flat_float_locals.insert(dst);
        Ok(Some(TypedReg {
            reg: dst,
            kind: RegKind::Value,
        }))
    }

    /// Repeat-form mirror of [`Self::try_build_int_array`] for
    /// `[value; count]` `[i64]` literals — used by integer scratch
    /// buffers initialised at function entry.
    fn try_build_int_array_repeat(
        &mut self,
        array_ty: Ty,
        value: &HirExpr,
        count: &HirExpr,
    ) -> RuntimeResult<Option<TypedReg>> {
        let elem_ty = match self.tcx.kind(array_ty) {
            Some(TyKind::Array { elem, .. } | TyKind::Vec(elem) | TyKind::Slice(elem)) => *elem,
            _ => return Ok(None),
        };
        if !matches!(
            self.tcx.kind(elem_ty),
            Some(TyKind::Int(IntTy::I64 | IntTy::Isize | IntTy::Usize))
        ) {
            return Ok(None);
        }
        let Some(n) = resolve_const_count(count) else {
            return Ok(None);
        };
        let Ok(count_u) = u16::try_from(n) else {
            return Ok(None);
        };
        let first_i = self.next_int_reg;
        if u32::from(count_u) > u32::from(u16::MAX - first_i) {
            return Ok(None);
        }
        self.next_int_reg = first_i + count_u;
        let src_tr = self.compile_expr_ex(value)?;
        let src_i = self.as_i64(src_tr);
        for i in 0..count_u {
            let target = first_i + i;
            self.emit(Op::MoveI64 {
                dst_i: target,
                src_i,
            });
        }
        let dst = self.alloc_reg();
        self.emit(Op::BuildIntArray {
            dst_v: dst,
            first_i,
            count: count_u,
        });
        self.flat_int_locals.insert(dst);
        Ok(Some(TypedReg {
            reg: dst,
            kind: RegKind::Value,
        }))
    }

    /// Mirror of [`Self::try_build_int_array`] for `[f64; N]`
    /// literals. Compiles each element into a contiguous f64
    /// register span and emits [`Op::BuildFloatVec`], which wraps
    /// the span into a `Value::FloatVec`. Subsequent indexed reads
    /// / writes route through [`Op::FloatVecGetF64`] and
    /// [`Op::FloatVecSetF64`] so each element load lands directly
    /// in the typed-`f64` register file.
    fn try_build_float_vec(
        &mut self,
        array_ty: Ty,
        elems: &[HirExpr],
    ) -> RuntimeResult<Option<TypedReg>> {
        let elem_ty = match self.tcx.kind(array_ty) {
            Some(TyKind::Array { elem, .. } | TyKind::Vec(elem) | TyKind::Slice(elem)) => *elem,
            _ => return Ok(None),
        };
        let elem_is_f64 = matches!(self.tcx.kind(elem_ty), Some(TyKind::Float(FloatTy::F64)));
        if !elem_is_f64 {
            return Ok(None);
        }
        let Ok(count) = u16::try_from(elems.len()) else {
            return Ok(None);
        };
        let first_f = self.next_float_reg;
        if u32::from(count) > u32::from(u16::MAX - first_f) {
            return Ok(None);
        }
        self.next_float_reg = first_f + count;
        for (i, elem) in elems.iter().enumerate() {
            let target = first_f + u16::try_from(i).expect("count overflow");
            let tr = self.compile_expr_ex(elem)?;
            let src_f = self.as_f64(tr);
            self.emit(Op::MoveF64 {
                dst_f: target,
                src_f,
            });
        }
        let dst = self.alloc_reg();
        self.emit(Op::BuildFloatVec {
            dst_v: dst,
            first_f,
            count,
        });
        self.flat_float_locals.insert(dst);
        Ok(Some(TypedReg {
            reg: dst,
            kind: RegKind::Value,
        }))
    }

    /// Phase-2 field-read fast path. When the field's own
    /// type is `f64`, emit `IndexedFieldGetF64` /
    /// `FieldGetF64` so the scalar skips a `Value::Float`
    /// wrap and lands directly in the float register file —
    /// critical for nbody's inner loop, where every
    /// `bodies[i].x` read feeds straight into f64 math.
    fn compile_field_ex(
        &mut self,
        receiver: &HirExpr,
        name: &Ident,
        field_ty: Ty,
    ) -> RuntimeResult<TypedReg> {
        let field_is_f64 = matches!(self.tcx.kind(field_ty), Some(TyKind::Float(FloatTy::F64)));
        // Try to resolve the receiver's struct field layout
        // for a compile-time offset. When present, emit an
        // offset-based op so the runtime skips the field-name
        // scan entirely.
        let elem_ty = match &receiver.kind {
            HirExprKind::Index { base, .. } => self.array_elem_ty(base.ty),
            _ => Some(self.unwrap_ref(receiver.ty)),
        };
        let offset = elem_ty.and_then(|t| self.resolve_struct_field_offset(t, name.name.as_str()));
        let name_idx = self.const_idx(
            ConstKey::String(name.name.clone()),
            Value::String(SmolStr::from(name.name.clone())),
        );
        // Fused `base[i].field` — avoids cloning the inner
        // struct `Arc`.
        if let HirExprKind::Index { base, index } = &receiver.kind {
            let base_reg = self.compile_expr(base)?;
            let idx_reg = self.compile_expr(index)?;
            if field_is_f64 {
                if let Some(offset) = offset {
                    // Known-flat local: emit the dedicated
                    // FloatArray-only read that skips the
                    // discriminant check.
                    if let Some(&stride) = self.flat_locals.get(&base_reg) {
                        let dst = self.alloc_float();
                        self.emit(Op::FlatGetF64 {
                            dst_f: dst,
                            base: base_reg,
                            index: idx_reg,
                            stride,
                            offset,
                        });
                        return Ok(TypedReg {
                            reg: dst,
                            kind: RegKind::F64,
                        });
                    }
                    let dst = self.alloc_float();
                    self.emit(Op::IndexedFieldGetF64ByOffset {
                        dst_f: dst,
                        base: base_reg,
                        index: idx_reg,
                        offset,
                    });
                    return Ok(TypedReg {
                        reg: dst,
                        kind: RegKind::F64,
                    });
                }
                let dst = self.alloc_float();
                self.emit(Op::IndexedFieldGetF64 {
                    dst_f: dst,
                    base: base_reg,
                    index: idx_reg,
                    name_idx,
                });
                return Ok(TypedReg {
                    reg: dst,
                    kind: RegKind::F64,
                });
            }
            let dst = self.alloc_reg();
            self.emit(Op::IndexedFieldGet {
                dst,
                base: base_reg,
                index: idx_reg,
                name_idx,
            });
            return Ok(TypedReg {
                reg: dst,
                kind: RegKind::Value,
            });
        }
        // Plain `value.field` — the receiver itself is a
        // single value, so we already avoid the indexed
        // clone. The remaining win is unboxing the scalar
        // into a float reg.
        let recv_reg = self.compile_expr(receiver)?;
        if field_is_f64 {
            if let Some(offset) = offset {
                let dst = self.alloc_float();
                self.emit(Op::FieldGetF64ByOffset {
                    dst_f: dst,
                    receiver: recv_reg,
                    offset,
                });
                return Ok(TypedReg {
                    reg: dst,
                    kind: RegKind::F64,
                });
            }
            let dst = self.alloc_float();
            self.emit(Op::FieldGetF64 {
                dst_f: dst,
                receiver: recv_reg,
                name_idx,
            });
            return Ok(TypedReg {
                reg: dst,
                kind: RegKind::F64,
            });
        }
        let dst = self.alloc_reg();
        let cache_idx = self.alloc_field_cache_idx();
        self.emit(Op::FieldGet {
            dst,
            receiver: recv_reg,
            name_idx,
            cache_idx,
        });
        Ok(TypedReg {
            reg: dst,
            kind: RegKind::Value,
        })
    }

    /// Recognise pure single-arg f64 math intrinsics
    /// (`math::sqrt`, `math::sin`, …) and emit the dedicated
    /// typed opcode instead of going through `Op::Call`.
    /// Both the bare and `math::` spellings are accepted to
    /// match the stdlib's dual registration.
    fn try_intrinsic_call(
        &mut self,
        callee: &HirExpr,
        args: &[HirExpr],
    ) -> RuntimeResult<Option<TypedReg>> {
        if args.len() != 1 {
            return Ok(None);
        }
        let HirExprKind::Path { segments, .. } = &callee.kind else {
            return Ok(None);
        };
        let full: Vec<String> = segments.iter().map(|s| s.name.clone()).collect();
        // If the callee is a single-segment user function that
        // the prepass flagged as a trivial wrapper around an
        // intrinsic (`fn f(x) { math::sqrt(x) }`), redirect to
        // the intrinsic's path and inline directly.
        let effective_segs = if full.len() == 1 {
            match self.wrappers.get(&full[0]) {
                Some(target) => target.clone(),
                None => full.clone(),
            }
        } else {
            full.clone()
        };
        let segs_str: Vec<&str> = effective_segs
            .iter()
            .map(std::string::String::as_str)
            .collect();
        let kind = match segs_str.as_slice() {
            ["math", "sqrt"] | ["sqrt"] => "sqrt",
            ["math", "sin"] | ["sin"] => "sin",
            ["math", "cos"] | ["cos"] => "cos",
            ["math", "abs"] | ["abs"] => "abs",
            ["math", "floor"] | ["floor"] => "floor",
            ["math", "ceil"] | ["ceil"] => "ceil",
            ["math", "exp"] | ["exp"] => "exp",
            ["math", "ln" | "log"] | ["ln"] => "ln",
            _ => return Ok(None),
        };
        if self.expr_kind(&args[0]) != RegKind::F64 {
            return Ok(None);
        }
        let arg_tr = self.compile_expr_ex(&args[0])?;
        let src_f = self.as_f64(arg_tr);
        let dst = self.alloc_float();
        let op = match kind {
            "sqrt" => Op::SqrtF64 { dst_f: dst, src_f },
            "sin" => Op::SinF64 { dst_f: dst, src_f },
            "cos" => Op::CosF64 { dst_f: dst, src_f },
            "abs" => Op::AbsF64 { dst_f: dst, src_f },
            "floor" => Op::FloorF64 { dst_f: dst, src_f },
            "ceil" => Op::CeilF64 { dst_f: dst, src_f },
            "exp" => Op::ExpF64 { dst_f: dst, src_f },
            "ln" => Op::LnF64 { dst_f: dst, src_f },
            _ => unreachable!(),
        };
        self.emit(op);
        Ok(Some(TypedReg {
            reg: dst,
            kind: RegKind::F64,
        }))
    }

    fn compile_short_circuit(
        &mut self,
        op: HirBinaryOp,
        lhs: &HirExpr,
        rhs: &HirExpr,
    ) -> RuntimeResult<Reg> {
        let result = self.alloc_reg();
        let lhs_reg = self.compile_expr(lhs)?;
        self.emit(Op::Move {
            dst: result,
            src: lhs_reg,
        });
        let branch_idx = match op {
            HirBinaryOp::And => self.emit(Op::BranchIfNot {
                cond: result,
                target: 0,
            }),
            HirBinaryOp::Or => self.emit(Op::BranchIf {
                cond: result,
                target: 0,
            }),
            _ => unreachable!(),
        };
        let rhs_reg = self.compile_expr(rhs)?;
        self.emit(Op::Move {
            dst: result,
            src: rhs_reg,
        });
        let after = self.cur_idx();
        self.patch_jump(branch_idx, after);
        Ok(result)
    }

    fn compile_assign(&mut self, place: &HirExpr, value: &HirExpr) -> RuntimeResult<Reg> {
        if let HirExprKind::Path { segments, .. } = &place.kind {
            if let Some(first) = segments.first() {
                if let Some(target) = self.lookup_local(&first.name) {
                    // Typed-local reassignment: compile the
                    // RHS in the destination kind so no box /
                    // unbox round-trip happens in hot loops.
                    let src_tr = self.compile_expr_ex(value)?;
                    self.emit_move_into(target, src_tr);
                    return Ok(self.load_unit());
                }
            }
        }
        // Native `local.field = value` and `local[i] = value`
        // writes: emit FieldSet / IndexSet directly so the VM's
        // hot loops (nbody's body.vx / body.x updates) don't
        // incur the `Op::EvalDeferred` env-rebuild cost.
        if let HirExprKind::Field { receiver, name } = &place.kind {
            if let HirExprKind::Path { segments, .. } = &receiver.kind {
                if let Some(first) = segments.first() {
                    if let Some(target) = self.lookup_local(&first.name) {
                        if target.kind == RegKind::Value {
                            let value_reg = self.compile_expr(value)?;
                            let name_idx = self.const_idx(
                                ConstKey::String(name.name.clone()),
                                Value::String(SmolStr::from(name.name.clone())),
                            );
                            self.emit(Op::FieldSet {
                                receiver: target.reg,
                                name_idx,
                                value: value_reg,
                            });
                            return Ok(self.load_unit());
                        }
                        // Typed local can't be a struct;
                        // fall through to the deferred path.
                    }
                }
            }
        }
        if let HirExprKind::Index { base, index } = &place.kind {
            if let HirExprKind::Path { segments, .. } = &base.kind {
                if let Some(first) = segments.first() {
                    if let Some(target) = self.lookup_local(&first.name) {
                        if target.kind == RegKind::Value {
                            // Typed flat-f64 store fast path: when
                            // the receiver is a `Value::FloatVec`
                            // and the RHS is f64-typed, write
                            // straight from the f64 register file.
                            // Mirrors the IntArray IndexSet bypass
                            // in `IntArray` users.
                            let value_is_f64 = matches!(
                                self.tcx.kind(value.ty),
                                Some(TyKind::Float(FloatTy::F64))
                            );
                            if value_is_f64 && self.flat_float_locals.contains(&target.reg) {
                                let idx_tr = self.compile_expr_ex(index)?;
                                let idx_i = self.as_i64(idx_tr);
                                let value_tr = self.compile_expr_ex(value)?;
                                let value_f = self.as_f64(value_tr);
                                self.emit(Op::FloatVecSetF64 {
                                    base: target.reg,
                                    index_i: idx_i,
                                    value_f,
                                });
                                return Ok(self.load_unit());
                            }
                            let idx_reg = self.compile_expr(index)?;
                            let value_reg = self.compile_expr(value)?;
                            self.emit(Op::IndexSet {
                                base: target.reg,
                                index: idx_reg,
                                value: value_reg,
                            });
                            return Ok(self.load_unit());
                        }
                    }
                }
            }
        }
        // `local[idx].field = value` — fused in-place write.
        // The `IndexedFieldSet` op mutates the array and the
        // body's field vec via `Arc::make_mut`, which is O(1)
        // here because `target` is the sole holder of the
        // array's Arc. This is the nbody inner-loop hot path
        // (`bodies[i].vx = ...`).
        if let HirExprKind::Field { receiver, name } = &place.kind {
            if let HirExprKind::Index { base, index } = &receiver.kind {
                if let HirExprKind::Path { segments, .. } = &base.kind {
                    if let Some(first) = segments.first() {
                        if let Some(target) = self.lookup_local(&first.name) {
                            if target.kind == RegKind::Value {
                                let name_idx = self.const_idx(
                                    ConstKey::String(name.name.clone()),
                                    Value::String(SmolStr::from(name.name.clone())),
                                );
                                // Phase-2 typed store: when the RHS is
                                // an f64 expression, write straight
                                // from the float register file into
                                // `base[i].field`, skipping the
                                // `BoxF64` that the generic path
                                // would emit.
                                let value_is_f64 = matches!(
                                    self.tcx.kind(value.ty),
                                    Some(TyKind::Float(FloatTy::F64))
                                );
                                if value_is_f64 {
                                    // Resolve the struct's field
                                    // offset for this write; emit the
                                    // offset-based op when possible.
                                    let elem_ty = self.array_elem_ty(base.ty);
                                    let offset = elem_ty.and_then(|t| {
                                        self.resolve_struct_field_offset(t, name.name.as_str())
                                    });
                                    let idx_reg = self.compile_expr(index)?;
                                    let value_tr = self.compile_expr_ex(value)?;
                                    let value_f = self.as_f64(value_tr);
                                    // Known-flat fast path.
                                    if let (Some(offset), Some(&stride)) =
                                        (offset, self.flat_locals.get(&target.reg))
                                    {
                                        self.emit(Op::FlatSetF64 {
                                            base: target.reg,
                                            index: idx_reg,
                                            stride,
                                            offset,
                                            value_f,
                                        });
                                        return Ok(self.load_unit());
                                    }
                                    if let Some(offset) = offset {
                                        self.emit(Op::IndexedFieldSetF64ByOffset {
                                            base: target.reg,
                                            index: idx_reg,
                                            offset,
                                            value_f,
                                        });
                                    } else {
                                        self.emit(Op::IndexedFieldSetF64 {
                                            base: target.reg,
                                            index: idx_reg,
                                            name_idx,
                                            value_f,
                                        });
                                    }
                                    return Ok(self.load_unit());
                                }
                                let idx_reg = self.compile_expr(index)?;
                                let value_reg = self.compile_expr(value)?;
                                self.emit(Op::IndexedFieldSet {
                                    base: target.reg,
                                    index: idx_reg,
                                    name_idx,
                                    value: value_reg,
                                });
                                return Ok(self.load_unit());
                            }
                        }
                    }
                }
            }
        }
        // Anything more complex (e.g. `a.b.c = x`, indexed
        // assignment through a temporary) still delegates to
        // the tree-walker via a synthetic Assign expression.
        let synthetic = HirExpr {
            id: place.id,
            span: place.span,
            ty: place.ty,
            kind: HirExprKind::Assign {
                place: Box::new(place.clone()),
                value: Box::new(value.clone()),
            },
        };
        self.compile_deferred(&synthetic)
    }

    fn compile_method_call(
        &mut self,
        receiver: &HirExpr,
        name: &Ident,
        args: &[HirExpr],
    ) -> RuntimeResult<Reg> {
        // Super-instruction fast path for the canonical
        // `m.insert(k, m.get_or(k, 0) + by)` counter-bump.
        // Detected here (before compiling args) so the inner
        // `get_or` call is never lowered.
        if name.name == "insert" && args.len() == 2 {
            if let Some((key_expr, by_expr)) = match_map_inc_pattern(receiver, &args[0], &args[1]) {
                if matches!(self.tcx.kind(receiver.ty), Some(TyKind::HashMap { .. })) {
                    // Typed `HashMap<i64, i64>` route: use
                    // `Op::IntMapInc` so the key + delta stay in
                    // the i64 register file the whole time.
                    if self.is_int_map_ty(receiver.ty) {
                        let map_reg = self.compile_expr(receiver)?;
                        let key_tr = self.compile_expr_ex(key_expr)?;
                        let key_i = self.as_i64(key_tr);
                        let by_tr = self.compile_expr_ex(by_expr)?;
                        let by_i = self.as_i64(by_tr);
                        let dst_i = self.alloc_int();
                        self.emit(Op::IntMapInc {
                            dst_i,
                            map_reg,
                            key_i,
                            by_i,
                        });
                        // Caller wants a `Value` register; box the
                        // post-increment value back so the existing
                        // statement-context code keeps working.
                        let dst = self.alloc_reg();
                        self.emit(Op::BoxI64 {
                            dst_v: dst,
                            src_i: dst_i,
                        });
                        return Ok(dst);
                    }
                    let map_reg = self.compile_expr(receiver)?;
                    let key_reg = self.compile_expr(key_expr)?;
                    let by_reg = self.compile_expr(by_expr)?;
                    let dst = self.alloc_reg();
                    self.emit(Op::MapInc {
                        dst,
                        map_reg,
                        key_reg,
                        by_reg,
                    });
                    return Ok(dst);
                }
            }
        }
        // `m.inc_at(seq, start, len, by)` super-instruction for a
        // string-keyed integer-valued `HashMap`. Inlines the
        // slice-hash + entry-increment so a sliding-window
        // counter update doesn't pay the generic builtin-call
        // dispatch on each iteration.
        if name.name == "inc_at"
            && args.len() == 4
            && matches!(self.tcx.kind(receiver.ty), Some(TyKind::HashMap { .. }))
        {
            let map_reg = self.compile_expr(receiver)?;
            let seq_reg = self.compile_expr(&args[0])?;
            let start_reg = self.compile_expr(&args[1])?;
            let len_reg = self.compile_expr(&args[2])?;
            let by_reg = self.compile_expr(&args[3])?;
            let dst = self.alloc_reg();
            self.emit(Op::MapIncAt {
                dst,
                map_reg,
                seq_reg,
                start_reg,
                len_reg,
                by_reg,
            });
            return Ok(dst);
        }
        // Typed-IntMap method dispatch fast paths. Skip the
        // generic builtin-IC route for the handful of HashMap
        // methods that hot counter loops drive.
        if self.is_int_map_ty(receiver.ty) {
            if let Some(reg) = self.try_compile_int_map_method(receiver, &name.name, args)? {
                return Ok(reg);
            }
        }
        let receiver_reg = self.compile_expr(receiver)?;
        // Super-instruction fast path for `<stream>.write_byte(<b>)`.
        // The runtime handler in `vm.rs::Op::StreamWriteByte`
        // verifies the receiver is a Stream and the byte is an
        // integer; if not, it falls through to a normal MethodCall
        // dispatch. Skipping the args-buf + IC + builtin-extract
        // chain saves the dominant per-character overhead in
        // fasta's hot output loop. Mirrors CPython 3.11's
        // `CALL_NO_KW_BUILTIN_O` specialisation.
        if name.name == "write_byte" && args.len() == 1 {
            // Use the typed compile path so a typed-i64 result (from
            // e.g. `Op::IntArrayGetI64`) can flow through an
            // explicit `BoxI64` rather than being re-fetched as a
            // boxed `Value::Int`. The handler still expects a
            // `Value` register, but `BoxI64` is a single op.
            let byte_tr = self.compile_expr_ex(&args[0])?;
            let byte_reg = self.as_value(byte_tr);
            let dst = self.alloc_reg();
            self.emit(Op::StreamWriteByte {
                dst,
                stream_reg: receiver_reg,
                byte_reg,
            });
            return Ok(dst);
        }
        // Mirror super-instruction for `<u8vec>.set_byte(<idx>, <byte>)`.
        // fasta's per-byte buffer fill drives this op millions of
        // times per phase; the inline handler skips the
        // MethodCall + IC + `&[Value]` round-trip.
        if name.name == "set_byte" && args.len() == 2 {
            let idx_tr = self.compile_expr_ex(&args[0])?;
            let idx_reg = self.as_value(idx_tr);
            let byte_tr = self.compile_expr_ex(&args[1])?;
            let byte_reg = self.as_value(byte_tr);
            let dst = self.alloc_reg();
            self.emit(Op::U8VecSetByte {
                dst,
                u8vec_reg: receiver_reg,
                idx_reg,
                byte_reg,
            });
            return Ok(dst);
        }
        // Mirror super-instruction for `<u8vec>.get_byte(<idx>) -> i64`.
        // The handler writes into a typed `i64` register, so a
        // downstream `Op::Add` etc. picks the result up without an
        // intermediate `Value::Int` round-trip. Caller still
        // expects a `Value` register, so we box back through
        // `Op::BoxI64` — the register allocator and downstream
        // typed-arith specialisation usually elide that pair.
        if name.name == "get_byte" && args.len() == 1 {
            let idx_tr = self.compile_expr_ex(&args[0])?;
            let idx_reg = self.as_value(idx_tr);
            let dst_i = self.alloc_int();
            self.emit(Op::U8VecGetByte {
                dst_i,
                u8vec_reg: receiver_reg,
                idx_reg,
            });
            let dst = self.alloc_reg();
            self.emit(Op::BoxI64 {
                dst_v: dst,
                src_i: dst_i,
            });
            return Ok(dst);
        }
        let arg_regs: Vec<Reg> = args
            .iter()
            .map(|a| self.compile_expr(a))
            .collect::<RuntimeResult<Vec<_>>>()?;
        let args_start = self.next_reg;
        for (i, r) in arg_regs.iter().enumerate() {
            let slot = args_start
                .checked_add(u16::try_from(i).expect("argc overflow"))
                .expect("reg overflow");
            self.ensure_reg_slot(slot);
            self.emit(Op::Move { dst: slot, src: *r });
        }
        let argc = u16::try_from(args.len()).map_err(|_| RuntimeError::Arity {
            expected: u16::MAX as usize,
            found: args.len(),
        })?;
        let name_idx = self.global_idx(&name.name);
        let dst = self.alloc_reg();
        let cache_idx = self.alloc_cache_idx();
        self.emit(Op::MethodCall {
            dst,
            receiver: receiver_reg,
            name_idx,
            args: args_start,
            argc,
            cache_idx,
        });
        // Mutating-method writeback. The interp builtins for
        // `push` / `insert` / etc. return the *new* aggregate
        // rather than mutating in place, so the VM has to thread
        // the result back into the receiver's storage. The tree-
        // walker handles this via `maybe_writeback`; the VM has
        // no equivalent dispatcher, so we splice the move here
        // when the receiver is a bindable local. Field / Index
        // receivers fall through with no writeback today.
        if Self::is_mutating_method_name(name.name.as_str()) {
            if let HirExprKind::Path { segments, .. } = &receiver.kind {
                if segments.len() == 1 {
                    if let Some(target) = self.lookup_local(&segments[0].name) {
                        if target.kind == RegKind::Value && target.reg == receiver_reg {
                            self.emit(Op::Move {
                                dst: target.reg,
                                src: dst,
                            });
                        }
                    }
                }
            }
        }
        Ok(dst)
    }

    /// Mirrors the tree-walker's `is_mutating_method` list. Methods
    /// here have a "returns the new aggregate" interp builtin that
    /// the VM has to thread back into the receiver's slot.
    fn is_mutating_method_name(name: &str) -> bool {
        matches!(
            name,
            "push"
                | "pop"
                | "insert"
                | "remove"
                | "clear"
                | "extend"
                | "append"
                | "truncate"
                | "sort"
                | "reverse"
                | "retain"
                | "drain"
                | "swap"
        )
    }

    /// Routes the typed-`HashMap<i64, i64>` method-call surface
    /// through dedicated typed ops. Returns `Some(reg)` when the
    /// method is handled here; the caller falls through to the
    /// generic dispatch otherwise.
    fn try_compile_int_map_method(
        &mut self,
        receiver: &HirExpr,
        method: &str,
        args: &[HirExpr],
    ) -> RuntimeResult<Option<Reg>> {
        match (method, args.len()) {
            ("insert", 2) => {
                let map_reg = self.compile_expr(receiver)?;
                let key_tr = self.compile_expr_ex(&args[0])?;
                let key_i = self.as_i64(key_tr);
                let val_tr = self.compile_expr_ex(&args[1])?;
                let val_i = self.as_i64(val_tr);
                let dst = self.alloc_reg();
                self.emit(Op::IntMapInsert {
                    dst_v: dst,
                    map_reg,
                    key_i,
                    value_i: val_i,
                });
                Ok(Some(dst))
            }
            ("get_or", 2) => {
                let map_reg = self.compile_expr(receiver)?;
                let key_tr = self.compile_expr_ex(&args[0])?;
                let key_i = self.as_i64(key_tr);
                let def_tr = self.compile_expr_ex(&args[1])?;
                let def_i = self.as_i64(def_tr);
                let dst_i = self.alloc_int();
                self.emit(Op::IntMapGetOr {
                    dst_i,
                    map_reg,
                    key_i,
                    default_i: def_i,
                });
                let dst = self.alloc_reg();
                self.emit(Op::BoxI64 {
                    dst_v: dst,
                    src_i: dst_i,
                });
                Ok(Some(dst))
            }
            ("len", 0) => {
                let map_reg = self.compile_expr(receiver)?;
                let dst_i = self.alloc_int();
                self.emit(Op::IntMapLen { dst_i, map_reg });
                let dst = self.alloc_reg();
                self.emit(Op::BoxI64 {
                    dst_v: dst,
                    src_i: dst_i,
                });
                Ok(Some(dst))
            }
            ("contains_key", 1) => {
                let map_reg = self.compile_expr(receiver)?;
                let key_tr = self.compile_expr_ex(&args[0])?;
                let key_i = self.as_i64(key_tr);
                let dst = self.alloc_reg();
                self.emit(Op::IntMapContainsKey {
                    dst_v: dst,
                    map_reg,
                    key_i,
                });
                Ok(Some(dst))
            }
            _ => Ok(None),
        }
    }

    /// Variant of [`Self::compile_call`] that takes the call's
    /// **result** type. Used by callers that have it on hand (for
    /// example `HirExprKind::Call`'s `expr.ty`) so the typed
    /// `HashMap<i64, i64>` construction can route to
    /// [`Op::BuildIntMap`] instead of the generic
    /// `builtin_map_new` path.
    fn compile_call_ex(
        &mut self,
        callee: &HirExpr,
        args: &[HirExpr],
        result_ty: Ty,
    ) -> RuntimeResult<Reg> {
        // Typed-IntMap construction fast path: when the callee is
        // `HashMap::new` and the result type is `HashMap<i64, i64>`,
        // emit a dedicated `Op::BuildIntMap` so the receiver lands
        // as `Value::IntMap` and downstream typed ops fire.
        if args.is_empty() {
            if let HirExprKind::Path { segments, .. } = &callee.kind {
                let segs: Vec<&str> = segments.iter().map(|s| s.name.as_str()).collect();
                if matches!(segs.as_slice(), ["HashMap", "new"]) && self.is_int_map_ty(result_ty) {
                    let dst = self.alloc_reg();
                    self.emit(Op::BuildIntMap { dst_v: dst });
                    return Ok(dst);
                }
            }
        }
        let callee_reg = self.compile_expr(callee)?;
        let argc = u16::try_from(args.len()).map_err(|_| RuntimeError::Arity {
            expected: u16::MAX as usize,
            found: args.len(),
        })?;
        // Reserve `argc` contiguous Value-register slots for the call's
        // argument vector before compiling any arg expression. Without
        // this, an arg whose `compile_expr` allocates a fresh register
        // (e.g. a literal or call result) lands inside the not-yet-
        // populated args region, and the subsequent `Move dst=slot
        // src=arg_reg` clobbers earlier args before they reach the
        // callee.
        let args_start = self.next_reg;
        self.next_reg = self
            .next_reg
            .checked_add(argc)
            .expect("register overflow reserving call args");
        let arg_regs: Vec<Reg> = args
            .iter()
            .map(|arg| self.compile_expr(arg))
            .collect::<RuntimeResult<Vec<_>>>()?;
        for (i, arg_reg) in arg_regs.iter().enumerate() {
            let slot = args_start
                .checked_add(u16::try_from(i).unwrap())
                .expect("register overflow");
            self.emit(Op::Move {
                dst: slot,
                src: *arg_reg,
            });
        }
        let dst = self.alloc_reg();
        let cache_idx = self.alloc_cache_idx();
        self.emit(Op::Call {
            dst,
            callee: callee_reg,
            args: args_start,
            argc,
            cache_idx,
        });
        Ok(dst)
    }

    fn ensure_reg_slot(&mut self, slot: Reg) {
        if slot >= self.next_reg {
            self.next_reg = slot.checked_add(1).expect("register overflow");
        }
    }

    fn compile_if(
        &mut self,
        condition: &HirExpr,
        then_branch: &HirExpr,
        else_branch: Option<&HirExpr>,
    ) -> RuntimeResult<Reg> {
        let cond_reg = self.compile_expr(condition)?;
        let result = self.alloc_reg();
        let branch_idx = self.emit(Op::BranchIfNot {
            cond: cond_reg,
            target: 0,
        });
        let then_reg = self.compile_expr(then_branch)?;
        self.emit(Op::Move {
            dst: result,
            src: then_reg,
        });
        let jump_end = self.emit(Op::Jump { target: 0 });
        let else_start = self.cur_idx();
        self.patch_jump(branch_idx, else_start);
        if let Some(else_branch) = else_branch {
            let else_reg = self.compile_expr(else_branch)?;
            self.emit(Op::Move {
                dst: result,
                src: else_reg,
            });
        } else {
            let unit_reg = self.load_unit();
            self.emit(Op::Move {
                dst: result,
                src: unit_reg,
            });
        }
        let after = self.cur_idx();
        self.patch_jump(jump_end, after);
        Ok(result)
    }

    fn compile_while(&mut self, condition: &HirExpr, body: &HirExpr) -> RuntimeResult<Reg> {
        // Fused-branch fast path: `while lhs < rhs` / `while
        // lhs >= rhs` etc. on typed i64 / f64 operands gets
        // lowered to a single `BranchIfGeI64` / `BranchIfLtI64`
        // that pairs the comparison with the exit jump. Cuts
        // two dispatched ops (typed compare + BranchIfNot /
        // BranchIf) down to one per loop iteration.
        //
        // Loop-invariant literal operands get hoisted above
        // `loop_start` so the LoadConst ops don't re-execute
        // per iteration.
        let hoisted = self.try_hoist_condition_literals(condition)?;
        let loop_start = self.cur_idx();
        let exit_patch = if let Some((lhs_reg, rhs_reg, op, kind)) = hoisted {
            Some(self.emit_fused_exit_branch(op, kind, lhs_reg, rhs_reg))
        } else {
            self.try_compile_fused_exit_branch(condition)?
        }
        .unwrap_or_else(|| {
            let cond_reg = self.compile_expr(condition).unwrap_or(0);
            self.emit(Op::BranchIfNot {
                cond: cond_reg,
                target: 0,
            })
        });
        let result = self.alloc_reg();
        self.loop_stack.push(LoopCtx {
            break_patches: Vec::new(),
            result_reg: result,
            loop_start,
        });
        let _ = self.compile_expr(body)?;
        self.emit(Op::Jump { target: loop_start });
        let after = self.cur_idx();
        self.patch_jump(exit_patch, after);
        let ctx = self
            .loop_stack
            .pop()
            .expect("loop stack underflow on while");
        for patch in ctx.break_patches {
            self.patch_jump(patch, after);
        }
        Ok(self.load_unit())
    }

    /// Hoists literal-and-local comparison operands out of
    /// `while` loops so the compare operands are evaluated
    /// once up front rather than per iteration. Returns
    /// `(lhs_reg, rhs_reg, op, kind)` when the condition
    /// has a hoistable shape — specifically
    /// `Path(local) <op> Literal` or `Literal <op> Path(local)`
    /// over typed numeric kinds.
    fn try_hoist_condition_literals(
        &mut self,
        condition: &HirExpr,
    ) -> RuntimeResult<Option<(Reg, Reg, HirBinaryOp, RegKind)>> {
        let HirExprKind::Binary { op, lhs, rhs } = &condition.kind else {
            return Ok(None);
        };
        if !matches!(
            op,
            HirBinaryOp::Lt | HirBinaryOp::Le | HirBinaryOp::Gt | HirBinaryOp::Ge
        ) {
            return Ok(None);
        }
        let lk = self.expr_kind(lhs);
        let rk = self.expr_kind(rhs);
        if lk != rk || lk == RegKind::Value {
            return Ok(None);
        }
        // Hoist only when neither operand would require an
        // `Unbox*` at evaluation: that would snapshot a
        // `Value::Int` local into a typed int reg once,
        // and subsequent writes back through the `Value`
        // reg wouldn't update it. Safe cases:
        //   * typed literals — always produce a typed reg
        //   * locals whose stored `TypedReg` already matches
        //     the operand kind — reads update through the
        //     same typed reg the compare uses.
        if !self.is_hoistable_operand(lhs, lk) {
            return Ok(None);
        }
        if !self.is_hoistable_operand(rhs, lk) {
            return Ok(None);
        }
        let lhs_tr = self.compile_expr_ex(lhs)?;
        let rhs_tr = self.compile_expr_ex(rhs)?;
        let (lhs_reg, rhs_reg) = match lk {
            RegKind::I64 => (self.as_i64(lhs_tr), self.as_i64(rhs_tr)),
            RegKind::F64 => (self.as_f64(lhs_tr), self.as_f64(rhs_tr)),
            RegKind::Value => unreachable!(),
        };
        Ok(Some((lhs_reg, rhs_reg, *op, lk)))
    }

    /// Returns `true` when `expr`'s operand register can be
    /// pre-computed before a loop body without going stale.
    /// Typed literals qualify (their reg is write-once), as do
    /// locals already bound in the matching typed register
    /// file. Anything else — most importantly a local bound as
    /// `Value` that would need an `Unbox*` snapshot — is
    /// rejected so the fused branch re-emits it each iteration.
    fn is_hoistable_operand(&self, expr: &HirExpr, kind: RegKind) -> bool {
        match &expr.kind {
            HirExprKind::Literal(_) => true,
            HirExprKind::Path { segments, .. } if segments.len() == 1 => {
                match self.lookup_local(&segments[0].name) {
                    Some(tr) => tr.kind == kind,
                    None => false,
                }
            }
            _ => false,
        }
    }

    /// Emits the inverted compare-and-branch op that exits a
    /// loop when `lhs <op> rhs` is false. Callers have already
    /// computed the operand registers.
    fn emit_fused_exit_branch(
        &mut self,
        op: HirBinaryOp,
        kind: RegKind,
        lhs_reg: Reg,
        rhs_reg: Reg,
    ) -> InstrIdx {
        let op_emit = match (kind, op) {
            (RegKind::I64, HirBinaryOp::Lt) => Op::BranchIfGeI64 {
                lhs_i: lhs_reg,
                rhs_i: rhs_reg,
                target: 0,
            },
            (RegKind::I64, HirBinaryOp::Le) => Op::BranchIfLtI64 {
                lhs_i: rhs_reg,
                rhs_i: lhs_reg,
                target: 0,
            },
            (RegKind::I64, HirBinaryOp::Gt) => Op::BranchIfGeI64 {
                lhs_i: rhs_reg,
                rhs_i: lhs_reg,
                target: 0,
            },
            (RegKind::I64, HirBinaryOp::Ge) => Op::BranchIfLtI64 {
                lhs_i: lhs_reg,
                rhs_i: rhs_reg,
                target: 0,
            },
            (RegKind::F64, HirBinaryOp::Lt) => Op::BranchIfGeF64 {
                lhs_f: lhs_reg,
                rhs_f: rhs_reg,
                target: 0,
            },
            (RegKind::F64, HirBinaryOp::Le) => Op::BranchIfLtF64 {
                lhs_f: rhs_reg,
                rhs_f: lhs_reg,
                target: 0,
            },
            (RegKind::F64, HirBinaryOp::Gt) => Op::BranchIfGeF64 {
                lhs_f: rhs_reg,
                rhs_f: lhs_reg,
                target: 0,
            },
            (RegKind::F64, HirBinaryOp::Ge) => Op::BranchIfLtF64 {
                lhs_f: lhs_reg,
                rhs_f: rhs_reg,
                target: 0,
            },
            _ => unreachable!(),
        };
        self.emit(op_emit)
    }

    /// Recognises `while lhs <op> rhs { ... }` where `lhs` and
    /// `rhs` share a concrete numeric kind and emits a fused
    /// "branch to loop exit when the inverted predicate holds"
    /// op. Returns the patch index so the caller can fix up
    /// the target once the loop-end address is known.
    fn try_compile_fused_exit_branch(
        &mut self,
        condition: &HirExpr,
    ) -> RuntimeResult<Option<InstrIdx>> {
        let HirExprKind::Binary { op, lhs, rhs } = &condition.kind else {
            return Ok(None);
        };
        let lk = self.expr_kind(lhs);
        let rk = self.expr_kind(rhs);
        if lk != rk || lk == RegKind::Value {
            return Ok(None);
        }
        // Check supported op kinds BEFORE compiling operands —
        // otherwise we'd emit dead operand-evaluation ops when
        // the comparison falls back to the generic path.
        if !matches!(
            op,
            HirBinaryOp::Lt | HirBinaryOp::Le | HirBinaryOp::Gt | HirBinaryOp::Ge
        ) {
            return Ok(None);
        }
        if lk == RegKind::I64 {
            let lhs_tr = self.compile_expr_ex(lhs)?;
            let rhs_tr = self.compile_expr_ex(rhs)?;
            let lhs_i = self.as_i64(lhs_tr);
            let rhs_i = self.as_i64(rhs_tr);
            // Fire when the predicate is FALSE (the loop
            // wants to exit). `while lhs < rhs` → exit when
            // `lhs >= rhs`, etc.
            //   < → Ge(lhs, rhs)
            //   <= → Lt(rhs, lhs)      [NOT (lhs <= rhs) ⟺ rhs < lhs]
            //   > → Ge(rhs, lhs)       [NOT (lhs > rhs) ⟺ rhs >= lhs]
            //   >= → Lt(lhs, rhs)
            let op_emit = match op {
                HirBinaryOp::Lt => Op::BranchIfGeI64 {
                    lhs_i,
                    rhs_i,
                    target: 0,
                },
                HirBinaryOp::Le => Op::BranchIfLtI64 {
                    lhs_i: rhs_i,
                    rhs_i: lhs_i,
                    target: 0,
                },
                HirBinaryOp::Gt => Op::BranchIfGeI64 {
                    lhs_i: rhs_i,
                    rhs_i: lhs_i,
                    target: 0,
                },
                HirBinaryOp::Ge => Op::BranchIfLtI64 {
                    lhs_i,
                    rhs_i,
                    target: 0,
                },
                _ => unreachable!(),
            };
            return Ok(Some(self.emit(op_emit)));
        }
        if lk == RegKind::F64 {
            let lhs_tr = self.compile_expr_ex(lhs)?;
            let rhs_tr = self.compile_expr_ex(rhs)?;
            let lhs_f = self.as_f64(lhs_tr);
            let rhs_f = self.as_f64(rhs_tr);
            let op_emit = match op {
                HirBinaryOp::Lt => Op::BranchIfGeF64 {
                    lhs_f,
                    rhs_f,
                    target: 0,
                },
                HirBinaryOp::Le => Op::BranchIfLtF64 {
                    lhs_f: rhs_f,
                    rhs_f: lhs_f,
                    target: 0,
                },
                HirBinaryOp::Gt => Op::BranchIfGeF64 {
                    lhs_f: rhs_f,
                    rhs_f: lhs_f,
                    target: 0,
                },
                HirBinaryOp::Ge => Op::BranchIfLtF64 {
                    lhs_f,
                    rhs_f,
                    target: 0,
                },
                _ => unreachable!(),
            };
            return Ok(Some(self.emit(op_emit)));
        }
        Ok(None)
    }

    fn compile_loop(&mut self, body: &HirExpr) -> RuntimeResult<Reg> {
        let loop_start = self.cur_idx();
        let result = self.alloc_reg();
        self.loop_stack.push(LoopCtx {
            break_patches: Vec::new(),
            result_reg: result,
            loop_start,
        });
        let _ = self.compile_expr(body)?;
        self.emit(Op::Jump { target: loop_start });
        let after = self.cur_idx();
        let ctx = self.loop_stack.pop().expect("loop stack underflow on loop");
        for patch in ctx.break_patches {
            self.patch_jump(patch, after);
        }
        Ok(result)
    }

    fn compile_return(&mut self, value: Option<&HirExpr>) -> RuntimeResult<Reg> {
        let reg = match value {
            Some(value) => self.compile_expr(value)?,
            None => self.load_unit(),
        };
        self.emit(Op::Return { value: reg });
        Ok(reg)
    }

    fn compile_break(&mut self, value: Option<&HirExpr>) -> RuntimeResult<Reg> {
        let reg = match value {
            Some(value) => self.compile_expr(value)?,
            None => self.load_unit(),
        };
        let ctx = self
            .loop_stack
            .last_mut()
            .ok_or(RuntimeError::Unsupported("break outside of loop"))?;
        let result_reg = ctx.result_reg;
        self.emit(Op::Move {
            dst: result_reg,
            src: reg,
        });
        let patch = self.emit(Op::Jump { target: 0 });
        self.loop_stack
            .last_mut()
            .expect("loop ctx")
            .break_patches
            .push(patch);
        Ok(reg)
    }

    fn load_unit(&mut self) -> Reg {
        let idx = self.const_idx(ConstKey::Unit, Value::Unit);
        let dst = self.alloc_reg();
        self.emit(Op::LoadConst { dst, idx });
        dst
    }
}

fn expr_diverges(expr: &HirExpr) -> bool {
    matches!(
        expr.kind,
        HirExprKind::Return(_) | HirExprKind::Break(_) | HirExprKind::Continue
    )
}

/// Returns `true` when `expr` is a bare single-segment path.
/// Used by `let` binding to detect the aliasing case — binding
/// a local to the reg of an existing local would share storage
/// and propagate future writes through the alias. Every other
/// expression produces a freshly-allocated reg we can bind
/// directly.
fn is_path_expr(expr: &HirExpr) -> bool {
    matches!(&expr.kind, HirExprKind::Path { .. })
}

fn literal_const(lit: &HirLiteral) -> (ConstKey, Value) {
    match lit {
        HirLiteral::Unit => (ConstKey::Unit, Value::Unit),
        HirLiteral::Bool(b) => (ConstKey::Bool(*b), Value::Bool(*b)),
        HirLiteral::Int(text) => {
            let value = parse_int(text).unwrap_or(0);
            (ConstKey::Int(value), Value::Int(value))
        }
        HirLiteral::Float(text) => {
            let parsed = strip_float_suffix(text).parse::<f64>().unwrap_or(0.0);
            (ConstKey::Float(parsed.to_bits()), Value::Float(parsed))
        }
        HirLiteral::Char(c) => (ConstKey::Char(*c), Value::Char(*c)),
        HirLiteral::String(text) => (
            ConstKey::String(text.clone()),
            Value::String(SmolStr::from(std::sync::Arc::new(text.clone()))),
        ),
        HirLiteral::Byte(b) => (ConstKey::Int(i64::from(*b)), Value::Int(i64::from(*b))),
        HirLiteral::ByteString(bytes) => {
            let parts = bytes.iter().map(|b| Value::Int(i64::from(*b))).collect();
            (
                ConstKey::String(format!("bytes:{bytes:?}")),
                Value::Array(std::sync::Arc::new(parts)),
            )
        }
    }
}

/// Resolves a `[value; count]` count expression to an integer at
/// compile time. Only matches plain `i64` / `usize` literals so the
/// bytecode emitter can pre-allocate exactly `count` registers.
/// Other shapes (`const`-folded path, function call) fall back to
/// the deferred path.
fn resolve_const_count(expr: &HirExpr) -> Option<i64> {
    use gossamer_hir::{HirExprKind as H, HirLiteral as L};
    if let H::Literal(L::Int(s)) = &expr.kind {
        // The HIR preserves source-form integer literals; strip the
        // optional type suffix and underscore separators before parsing.
        let trimmed = s
            .trim_end_matches("i64")
            .trim_end_matches("usize")
            .trim_end_matches("u64")
            .trim_end_matches("isize")
            .trim_end_matches("u32")
            .trim_end_matches("i32");
        let cleaned: String = trimmed.chars().filter(|c| *c != '_').collect();
        if let Some(stripped) = cleaned.strip_prefix("0x") {
            return i64::from_str_radix(stripped, 16).ok();
        }
        if let Some(stripped) = cleaned.strip_prefix("0o") {
            return i64::from_str_radix(stripped, 8).ok();
        }
        if let Some(stripped) = cleaned.strip_prefix("0b") {
            return i64::from_str_radix(stripped, 2).ok();
        }
        return cleaned.parse::<i64>().ok();
    }
    None
}

/// Recursively walks `expr` looking for expression kinds the VM
/// compiler doesn't handle natively. When a `Loop { body }` body
/// contains one, the whole loop defers to the tree-walker so
/// Break/Continue flow out correctly.
fn body_contains_unsupported(expr: &HirExpr) -> bool {
    use gossamer_hir::{HirArrayExpr, HirExprKind as H};
    match &expr.kind {
        H::Match { .. }
        | H::Closure { .. }
        | H::Go(_)
        | H::Range { .. }
        | H::Select { .. }
        | H::LiftedClosure { .. } => true,
        // Tuples, Casts, and Continue are now natively lowered,
        // so a loop containing one stays on the bytecode path
        // instead of deferring the whole loop body.
        H::Tuple(elems) => elems.iter().any(body_contains_unsupported),
        H::Cast { value, .. } => body_contains_unsupported(value),
        H::MethodCall { receiver, args, .. } => {
            body_contains_unsupported(receiver) || args.iter().any(body_contains_unsupported)
        }
        H::Field { receiver, .. } => body_contains_unsupported(receiver),
        H::TupleIndex { receiver, .. } => body_contains_unsupported(receiver),
        H::Index { base, index } => {
            body_contains_unsupported(base) || body_contains_unsupported(index)
        }
        H::Literal(_) | H::Path { .. } => false,
        H::Unary { operand, .. } => body_contains_unsupported(operand),
        H::Binary { lhs, rhs, .. } => {
            body_contains_unsupported(lhs) || body_contains_unsupported(rhs)
        }
        H::Assign { place, value } => {
            body_contains_unsupported(place) || body_contains_unsupported(value)
        }
        H::Call { callee, args } => {
            body_contains_unsupported(callee) || args.iter().any(body_contains_unsupported)
        }
        H::If {
            condition,
            then_branch,
            else_branch,
        } => {
            body_contains_unsupported(condition)
                || body_contains_unsupported(then_branch)
                || else_branch
                    .as_deref()
                    .is_some_and(body_contains_unsupported)
        }
        H::While { condition, body } => {
            body_contains_unsupported(condition) || body_contains_unsupported(body)
        }
        H::Loop { body } => body_contains_unsupported(body),
        H::Block(block) => {
            block.stmts.iter().any(stmt_contains_unsupported)
                || block
                    .tail
                    .as_ref()
                    .is_some_and(|t| body_contains_unsupported(t))
        }
        H::Return(v) | H::Break(v) => v.as_ref().is_some_and(|e| body_contains_unsupported(e)),
        H::Placeholder => true,
        // Native `continue` jumps to loop_start; only an
        // unsupported sub-expression in the surrounding shape
        // would force a defer.
        H::Continue => false,
        H::Array(arr) => match arr {
            HirArrayExpr::List(elems) => elems.iter().any(body_contains_unsupported),
            HirArrayExpr::Repeat { value, count } => {
                body_contains_unsupported(value) || body_contains_unsupported(count)
            }
        },
    }
}

fn stmt_contains_unsupported(stmt: &gossamer_hir::HirStmt) -> bool {
    use gossamer_hir::HirStmtKind as S;
    match &stmt.kind {
        S::Let { init, .. } => init.as_ref().is_some_and(body_contains_unsupported),
        S::Expr { expr, .. } => body_contains_unsupported(expr),
        S::Defer(_) | S::Go(_) => true,
        S::Item(_) => false,
    }
}

fn parse_int(text: &str) -> Option<i64> {
    let cleaned = strip_int_suffix(text).replace('_', "");
    if let Some(rest) = cleaned
        .strip_prefix("0x")
        .or_else(|| cleaned.strip_prefix("0X"))
    {
        return i64::from_str_radix(rest, 16).ok();
    }
    if let Some(rest) = cleaned
        .strip_prefix("0b")
        .or_else(|| cleaned.strip_prefix("0B"))
    {
        return i64::from_str_radix(rest, 2).ok();
    }
    if let Some(rest) = cleaned
        .strip_prefix("0o")
        .or_else(|| cleaned.strip_prefix("0O"))
    {
        return i64::from_str_radix(rest, 8).ok();
    }
    cleaned.parse::<i64>().ok()
}

fn strip_int_suffix(text: &str) -> String {
    const SUFFIXES: &[&str] = &[
        "i128", "u128", "isize", "usize", "i64", "u64", "i32", "u32", "i16", "u16", "i8", "u8",
    ];
    for suffix in SUFFIXES {
        if let Some(stripped) = text.strip_suffix(suffix) {
            return stripped.to_string();
        }
    }
    text.to_string()
}

fn strip_float_suffix(text: &str) -> String {
    for suffix in &["f32", "f64"] {
        if let Some(stripped) = text.strip_suffix(suffix) {
            return stripped.to_string();
        }
    }
    text.to_string()
}

/// Detects `m.insert(k, m.get_or(k, 0) + by)`. Returns `(key, by)`
/// (borrowed from the original HIR) when the surrounding insert
/// receiver and key match the inner `get_or`'s receiver and key
/// structurally and the inner default arg is literal `0`.
pub(crate) fn match_map_inc_pattern<'a>(
    receiver: &'a HirExpr,
    insert_key: &'a HirExpr,
    insert_value: &'a HirExpr,
) -> Option<(&'a HirExpr, &'a HirExpr)> {
    let HirExprKind::Binary { op, lhs, rhs } = &insert_value.kind else {
        return None;
    };
    if !matches!(op, HirBinaryOp::Add) {
        return None;
    }
    if is_get_or_zero(receiver, insert_key, lhs) {
        return Some((insert_key, rhs));
    }
    if is_get_or_zero(receiver, insert_key, rhs) {
        return Some((insert_key, lhs));
    }
    None
}

fn is_get_or_zero(receiver: &HirExpr, key: &HirExpr, candidate: &HirExpr) -> bool {
    let HirExprKind::MethodCall {
        receiver: inner_recv,
        name: inner_name,
        args: inner_args,
    } = &candidate.kind
    else {
        return false;
    };
    inner_name.name == "get_or"
        && inner_args.len() == 2
        && exprs_equiv(receiver, inner_recv)
        && exprs_equiv(key, &inner_args[0])
        && is_zero_literal(&inner_args[1])
}

/// Structural equivalence over the HIR shapes that can safely be
/// re-evaluated zero times (i.e. compiled once and reused for both
/// the outer `insert` and the elided inner `get_or`). Limited to
/// pure single-segment `Path` reads and primitive literals so we
/// never elide a side-effecting expression.
fn exprs_equiv(a: &HirExpr, b: &HirExpr) -> bool {
    match (&a.kind, &b.kind) {
        (HirExprKind::Path { segments: sa, .. }, HirExprKind::Path { segments: sb, .. }) => {
            sa.len() == sb.len() && sa.iter().zip(sb).all(|(x, y)| x.name == y.name)
        }
        (HirExprKind::Literal(la), HirExprKind::Literal(lb)) => literals_equal(la, lb),
        _ => false,
    }
}

fn literals_equal(a: &HirLiteral, b: &HirLiteral) -> bool {
    match (a, b) {
        (HirLiteral::Int(x), HirLiteral::Int(y)) => x == y,
        (HirLiteral::Float(x), HirLiteral::Float(y)) => x == y,
        (HirLiteral::String(x), HirLiteral::String(y)) => x == y,
        (HirLiteral::Char(x), HirLiteral::Char(y)) => x == y,
        (HirLiteral::Byte(x), HirLiteral::Byte(y)) => x == y,
        (HirLiteral::ByteString(x), HirLiteral::ByteString(y)) => x == y,
        (HirLiteral::Bool(x), HirLiteral::Bool(y)) => x == y,
        (HirLiteral::Unit, HirLiteral::Unit) => true,
        _ => false,
    }
}

fn is_zero_literal(expr: &HirExpr) -> bool {
    match &expr.kind {
        HirExprKind::Literal(HirLiteral::Int(text)) => parse_int(text) == Some(0),
        _ => false,
    }
}
