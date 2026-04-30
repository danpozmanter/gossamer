//! MIR → LLVM IR text lowering.
//!
//! One [`Lowerer`] per [`gossamer_mir::Body`]. It walks the
//! MIR in block order, allocates a single SSA value per MIR
//! local via an `alloca` in the entry block, and emits
//! `load` / `store` instructions around each statement. This
//! matches what `rustc` does at its `-O0` setting; `llc -O3`
//! folds the redundant loads away during mem2reg.

use std::fmt::Write;

use gossamer_mir::{
    BinOp, Body, ConstValue, Local, Operand, Place, Projection, Rvalue, Statement, StatementKind,
    Terminator, UnOp,
};
use gossamer_types::{FloatTy, Ty, TyCtxt, TyKind};

use crate::emit::BuildError;
use crate::ty::{
    NumericKind, elem_slots, int_signed, int_width, is_aggregate, is_pure_primitive_aggregate,
    is_unit, numeric_kind, render_ty, slot_count,
};

/// Emits one function's LLVM IR text, including the required
/// `declare` statements for any `gos_rt_*` symbols it calls.
pub(crate) struct Lowerer<'a> {
    pub(crate) body: &'a Body,
    pub(crate) tcx: &'a TyCtxt,
    /// Accumulator for the function body text.
    pub(crate) out: String,
    /// Monotonically increasing counter for SSA value names
    /// (`%t0`, `%t1`, …) — LLVM requires unique numbering
    /// within a function.
    pub(crate) next_ssa: u32,
    /// Runtime function signatures we've referenced so the
    /// enclosing module can emit the matching `declare`s.
    pub(crate) runtime_refs: std::collections::BTreeSet<String>,
    /// `DefId.local` → function name map so `Operand::FnRef`
    /// resolves to the exported symbol. Populated by the
    /// emitter before calling [`Lowerer::lower`].
    pub(crate) fn_name_by_def: std::collections::HashMap<u32, String>,
    /// String-constant pool — the emitter materialises each
    /// entry as a `@.str_N = private unnamed_addr constant
    /// [len x i8] c"..."` module-level global so
    /// `ConstValue::Str(_)` operands can reference real
    /// `.rodata` bytes instead of `null`. Entries are shared
    /// with the module-wide pool via an `Rc<RefCell<...>>`
    /// populated by the emitter before calling
    /// [`Lowerer::lower`].
    pub(crate) strings: std::rc::Rc<std::cell::RefCell<StringPool>>,
    /// Tracks which MIR block we're currently lowering so the
    /// safepoint emitter can place its `!dbg`-style comment in the
    /// right place. Track 3 / §6.3 of the audit owns the actual
    /// safepoint semantics; this slot is the bookkeeping the
    /// codegen needs while that work threads through.
    pub(crate) current_block: Option<u32>,
    /// Monotonically-increasing counter for safepoint label
    /// suffixes so the LLVM IR has unique block names per
    /// preempt-check call site.
    pub(crate) preempt_seq: u32,
}

/// Module-scoped string intern pool.
#[derive(Debug, Default)]
pub(crate) struct StringPool {
    /// Source-text → (`global_name`, `byte_length`) map.
    entries: std::collections::HashMap<String, (String, usize)>,
    next_id: u32,
}

impl StringPool {
    pub(crate) fn intern(&mut self, text: &str) -> (String, usize) {
        if let Some(hit) = self.entries.get(text) {
            return hit.clone();
        }
        let id = self.next_id;
        self.next_id += 1;
        let name = format!("@.gstr_{id}");
        let entry = (name, text.len() + 1);
        self.entries.insert(text.to_string(), entry.clone());
        entry
    }

    /// Renders every interned string as an LLVM global. The
    /// emitter calls this after every body has lowered.
    pub(crate) fn render(&self) -> String {
        let mut out = String::new();
        for (text, (name, size)) in &self.entries {
            let escaped = escape_c_string(text);
            let _ = writeln!(
                out,
                "{name} = private unnamed_addr constant [{size} x i8] c\"{escaped}\\00\""
            );
        }
        out
    }
}

/// Operand classification for `__concat`'s per-arg dispatch.
///
/// `Unsupported` covers operand types we can't print without a
/// Display impl (tuples, structs, Vec, HashMap, Option, Result,
/// etc.). The LLVM backend turns this into a generic
/// `BuildError::Unsupported` so the per-function driver routes
/// the body to Cranelift; Cranelift then bails with a user-facing
/// message naming the specific operand kind.
#[derive(Debug, Clone, Copy)]
enum ConcatKind {
    StrPtr,
    Int,
    Float,
    Bool,
    Char,
    Unsupported,
}

impl<'a> Lowerer<'a> {
    pub(crate) fn new(body: &'a Body, tcx: &'a TyCtxt) -> Self {
        Self {
            body,
            tcx,
            out: String::new(),
            next_ssa: 0,
            runtime_refs: std::collections::BTreeSet::new(),
            fn_name_by_def: std::collections::HashMap::new(),
            strings: std::rc::Rc::new(std::cell::RefCell::new(StringPool::default())),
            current_block: None,
            preempt_seq: 0,
        }
    }

    /// Main entry point — emits the function's IR text in its
    /// entirety. The module-level global declarations (panic
    /// message constants, etc.) accumulated during lowering
    /// remain in `self.runtime_refs` and are read by the
    /// caller to prepend to the module.
    pub(crate) fn lower(&mut self) -> Result<String, BuildError> {
        // Refuse closure bodies — `__closure_N` functions ship
        // with their MIR return slot still typed `Unit` (a
        // typechecker quirk pending follow-up) but the call
        // sites expect the inner expression's value type. The
        // Cranelift backend handles closures correctly already,
        // so route them via the per-function fallback rather
        // than emit a `void`-returning LLVM function whose
        // callers read zero out of an unrelated register.
        if self.body.name.starts_with("__closure_") {
            return Err(BuildError::Unsupported("closure body"));
        }
        self.emit_prelude();
        // Entry block opens with `alloca`s for every local.
        self.emit_allocas();
        // Copy function parameters into their local slots so
        // the rest of the body uniformly reads through
        // `local_slot`. MIR reserves `_1..=_arity` as
        // parameter locals.
        self.emit_param_stores();
        // Unconditional jump into the MIR entry block.
        writeln!(self.out, "  br label %bb0").unwrap();
        for block in &self.body.blocks {
            self.lower_block(block)?;
        }
        writeln!(self.out, "}}").unwrap();
        Ok(std::mem::take(&mut self.out))
    }

    /// Drains the module-level globals this body introduced
    /// (string constants for panic messages, etc.). Called by
    /// the emitter once the body text is in the module.
    pub(crate) fn take_module_globals(&mut self) -> Vec<String> {
        std::mem::take(&mut self.runtime_refs).into_iter().collect()
    }

    fn emit_prelude(&mut self) {
        let ret_ty = render_ty(self.tcx, self.body.local_ty(Local::RETURN));
        let mut params = String::new();
        for i in 0..self.body.arity {
            if i > 0 {
                params.push_str(", ");
            }
            let local = Local(i + 1);
            let p_ty = render_ty(self.tcx, self.body.local_ty(local));
            let _ = write!(params, "{p_ty} %p{i}");
        }
        writeln!(
            self.out,
            "define {ret_ty} @\"{name}\"({params}) {{",
            name = escape_ident(mangle_fn_name(&self.body.name)),
            ret_ty = ret_ty,
            params = params,
        )
        .unwrap();
        writeln!(self.out, "entry:").unwrap();
    }

    fn emit_allocas(&mut self) {
        for (i, decl) in self.body.locals.iter().enumerate() {
            if is_unit(self.tcx, decl.ty) {
                // Zero-sized: skip. Reads return the singleton
                // `()` directly via emit-time folding.
                continue;
            }
            let slot = local_slot(Local(i as u32));
            if is_aggregate(self.tcx, decl.ty) {
                // Aggregates use Cranelift's flat layout:
                // 8-byte i64-sized slots, one per scalar
                // field, struct-of-struct flattened in
                // declaration order. `alloca [N x i64]`
                // gets us the same footprint and honours
                // 8-byte alignment the runtime expects.
                let slots = slot_count(self.tcx, decl.ty).unwrap_or(1).max(1);
                writeln!(self.out, "  {slot} = alloca [{slots} x i64]").unwrap();
            } else {
                let ty = render_ty(self.tcx, decl.ty);
                writeln!(self.out, "  {slot} = alloca {ty}").unwrap();
            }
        }
    }

    fn emit_param_stores(&mut self) {
        for i in 0..self.body.arity {
            let local = Local(i + 1);
            let local_ty = self.body.local_ty(local);
            if is_unit(self.tcx, local_ty) {
                continue;
            }
            let slot = local_slot(local);
            if is_aggregate(self.tcx, local_ty) {
                // Aggregates are passed by pointer (the caller hands us
                // the address of its flat-slot storage). Copy that data
                // into our own slot so subsequent reads land on the
                // aggregate's inline data — matching how locally-built
                // aggregates are populated by `emit_aggregate_store`.
                let bytes = u64::from(slot_count(self.tcx, local_ty).unwrap_or(1).max(1)) * 8;
                writeln!(
                    self.out,
                    "  call void @llvm.memcpy.p0.p0.i64(ptr {slot}, ptr %p{i}, i64 {bytes}, i1 false)"
                )
                .unwrap();
            } else {
                let ty = render_ty(self.tcx, local_ty);
                writeln!(self.out, "  store {ty} %p{i}, ptr {slot}").unwrap();
            }
        }
    }

    fn lower_block(&mut self, block: &gossamer_mir::BasicBlock) -> Result<(), BuildError> {
        writeln!(self.out, "bb{}:", block.id.as_u32()).unwrap();
        for stmt in &block.stmts {
            self.lower_stmt(stmt)?;
        }
        self.current_block = Some(block.id.as_u32());
        self.lower_terminator(&block.terminator)?;
        self.current_block = None;
        Ok(())
    }

    /// Back-edge safepoint for cooperative preemption. Currently a
    /// no-op: a runtime call inserted on every loop back-edge
    /// blocks `opt -O3` from vectorising tight numeric inner loops
    /// (the call is opaque to alias analysis and the loop
    /// vectoriser refuses to lift it across iterations), which is
    /// the difference between sub-1-second and 5+ second runs on
    /// spectral-norm and n-body. Mirrors the Cranelift backend,
    /// where the matching insertion point in
    /// `crates/gossamer-codegen-cranelift/src/native.rs:1723` is
    /// also a stub. Both backends will gain a real safepoint when
    /// the runtime grows SIGURG-based async preemption (Track 3
    /// follow-up); until then the function-level safepoint at the
    /// scheduler boundary is what limits runaway goroutines.
    fn emit_preempt_check(&mut self) {
        let _ = self.preempt_seq;
    }

    fn lower_stmt(&mut self, stmt: &Statement) -> Result<(), BuildError> {
        match &stmt.kind {
            StatementKind::Assign { place, rvalue } => {
                self.lower_assign(place, rvalue)?;
            }
            StatementKind::StorageLive(local) => {
                // Hint to LLVM's register allocator that the
                // alloca's storage becomes live. Treat unit /
                // zero-sized locals as no-ops since they have no
                // alloca.
                if !is_unit(self.tcx, self.body.local_ty(*local)) {
                    let slot = local_slot(*local);
                    let bytes =
                        u64::from(slot_count(self.tcx, self.body.local_ty(*local)).unwrap_or(1))
                            * 8;
                    writeln!(
                        self.out,
                        "  call void @llvm.lifetime.start.p0(i64 {bytes}, ptr {slot})"
                    )
                    .unwrap();
                }
            }
            StatementKind::StorageDead(local) => {
                if !is_unit(self.tcx, self.body.local_ty(*local)) {
                    let slot = local_slot(*local);
                    let bytes =
                        u64::from(slot_count(self.tcx, self.body.local_ty(*local)).unwrap_or(1))
                            * 8;
                    writeln!(
                        self.out,
                        "  call void @llvm.lifetime.end.p0(i64 {bytes}, ptr {slot})"
                    )
                    .unwrap();
                }
            }
            StatementKind::Nop => {}
            StatementKind::SetDiscriminant { place, variant } => {
                // Stores the variant index at offset 0 of the
                // enum's backing place. Matches the Cranelift
                // convention: tag at slot 0, payload at +8.
                let addr = if place.projection.is_empty() {
                    local_slot(place.local)
                } else {
                    self.lower_place_address(place)
                };
                writeln!(
                    self.out,
                    "  store i64 {variant}, ptr {addr}",
                    variant = *variant,
                )
                .unwrap();
            }
            StatementKind::GcWriteBarrier { .. } => {
                // No-op until the tri-color GC lands; matches
                // Cranelift's behaviour. Code is correct
                // without it.
            }
        }
        Ok(())
    }

    fn lower_assign(&mut self, place: &Place, rvalue: &Rvalue) -> Result<(), BuildError> {
        let dest_ty_mir = self.body.local_ty(place.local);
        if is_unit(self.tcx, dest_ty_mir) {
            return Ok(());
        }
        // Aggregate constructions (`Aggregate`, `Repeat`) are
        // routed straight at the destination slot — they
        // populate the stack aggregate in-place rather than
        // producing a scalar value to store.
        match rvalue {
            Rvalue::Aggregate { operands, .. } => {
                return self.emit_aggregate_store(place, operands);
            }
            Rvalue::Repeat { value, count } => {
                return self.emit_repeat_store(place, value, *count);
            }
            _ => {}
        }
        // Whole-aggregate copy: when the destination is an
        // aggregate local and the rvalue is a plain `Use(Copy)`
        // of another aggregate local, neither side has a
        // scalar representation — memcpy the flat storage
        // rather than trying to load/store it as a single
        // value.
        if place.projection.is_empty() && is_aggregate(self.tcx, dest_ty_mir) {
            if let Rvalue::Use(Operand::Copy(src_place)) = rvalue {
                if src_place.projection.is_empty()
                    && is_aggregate(self.tcx, self.body.local_ty(src_place.local))
                {
                    let bytes = u64::from(slot_count(self.tcx, dest_ty_mir).unwrap_or(1)) * 8;
                    writeln!(
                        self.out,
                        "  call void @llvm.memcpy.p0.p0.i64(ptr {dst}, ptr {src}, i64 {bytes}, i1 false)",
                        dst = local_slot(place.local),
                        src = local_slot(src_place.local),
                    )
                    .unwrap();
                    return Ok(());
                }
            }
        }
        let leaf_ty = self.place_leaf_ty(place);
        let leaf_llvm = render_ty(self.tcx, leaf_ty);
        let value = self.lower_rvalue(rvalue, place.local)?;
        let addr = if place.projection.is_empty() {
            local_slot(place.local)
        } else {
            self.lower_place_address(place)
        };
        writeln!(self.out, "  store {leaf_llvm} {value}, ptr {addr}").unwrap();
        // Write barrier: when the *destination* is heap-resident
        // (i.e. the place projects through a deref / heap pointer)
        // and the *value* is itself a heap pointer, the concurrent
        // collector needs to know about the new edge so its mark
        // phase doesn't lose track of it. The barrier is a no-op
        // while the collector is idle (a single load + branch in
        // the runtime helper) so we emit it unconditionally for
        // qualifying stores rather than try to model GC liveness
        // statically here.
        if !place.projection.is_empty() && Self::is_pointer_local_ty(self.tcx, leaf_ty) {
            self.emit_write_barrier(&value);
        }
        Ok(())
    }

    /// Emits a `gos_rt_write_barrier` call for the supplied
    /// LLVM value (an `i64` representation of a heap reference).
    /// Idempotent registration of the runtime declaration.
    fn emit_write_barrier(&mut self, value: &str) {
        self.runtime_refs
            .insert("declare void @gos_rt_write_barrier(i32)".to_string());
        let truncated = self.fresh();
        // Heap refs are stored as 64-bit values in the flat ABI;
        // the runtime symbol takes a 32-bit index. Truncating is
        // safe here: the compiled tier never produces refs above
        // the u32 boundary (heap slots are u32-indexed in
        // gossamer-gc).
        writeln!(self.out, "  {truncated} = trunc i64 {value} to i32").unwrap();
        writeln!(
            self.out,
            "  call void @gos_rt_write_barrier(i32 {truncated})"
        )
        .unwrap();
    }

    /// Populates an aggregate stack slot (the destination
    /// `place`'s flat layout) with each operand in order.
    /// Each operand occupies one i64-wide slot for scalar
    /// fields; nested aggregates add their own slot count.
    fn emit_aggregate_store(
        &mut self,
        place: &Place,
        operands: &[Operand],
    ) -> Result<(), BuildError> {
        if !place.projection.is_empty() {
            return Err(BuildError::Unsupported(
                "Aggregate assignment through projection",
            ));
        }
        let base = local_slot(place.local);
        let mut slot_idx = 0u32;
        for operand in operands {
            let op_ty = self.operand_ty(operand);
            let op_slots = slot_count(self.tcx, op_ty).unwrap_or(1);
            if op_slots == 1 {
                let v = self.lower_operand(operand)?;
                let op_llvm = self.operand_llvm_ty(operand);
                let dst = self.fresh();
                writeln!(
                    self.out,
                    "  {dst} = getelementptr i64, ptr {base}, i64 {slot_idx}"
                )
                .unwrap();
                writeln!(self.out, "  store {op_llvm} {v}, ptr {dst}").unwrap();
            } else {
                // Nested aggregate: the operand is a local
                // whose stack slot we memcpy. Use
                // `llvm.memcpy.p0.p0.i64` — lowered by `llc` to
                // the platform's best sequence.
                let src_place = match operand {
                    Operand::Copy(p) if p.projection.is_empty() => p,
                    _ => {
                        return Err(BuildError::Unsupported(
                            "nested aggregate operand must be a local copy",
                        ));
                    }
                };
                let src = local_slot(src_place.local);
                let dst = self.fresh();
                writeln!(
                    self.out,
                    "  {dst} = getelementptr i64, ptr {base}, i64 {slot_idx}"
                )
                .unwrap();
                let bytes = u64::from(op_slots) * 8;
                writeln!(
                    self.out,
                    "  call void @llvm.memcpy.p0.p0.i64(ptr {dst}, ptr {src}, i64 {bytes}, i1 false)"
                )
                .unwrap();
            }
            slot_idx += op_slots;
        }
        Ok(())
    }

    /// `[value; count]` — fills `count` slots with the same
    /// scalar `value`. Small counts are unrolled (`llc -O3`
    /// later SLP-vectorises); larger counts drop into a
    /// tight loop to keep module text small.
    fn emit_repeat_store(
        &mut self,
        place: &Place,
        value: &Operand,
        count: u64,
    ) -> Result<(), BuildError> {
        if !place.projection.is_empty() {
            return Err(BuildError::Unsupported(
                "Repeat assignment through projection",
            ));
        }
        let v = self.lower_operand(value)?;
        let v_llvm = self.operand_llvm_ty(value);
        let base = local_slot(place.local);
        if count <= 16 {
            for i in 0..count {
                let dst = self.fresh();
                writeln!(self.out, "  {dst} = getelementptr i64, ptr {base}, i64 {i}").unwrap();
                writeln!(self.out, "  store {v_llvm} {v}, ptr {dst}").unwrap();
            }
        } else {
            let head = self.fresh_label("repeat_head");
            let body = self.fresh_label("repeat_body");
            let done = self.fresh_label("repeat_done");
            let counter = self.fresh();
            writeln!(self.out, "  {counter} = alloca i64").unwrap();
            writeln!(self.out, "  store i64 0, ptr {counter}").unwrap();
            writeln!(self.out, "  br label %{head}").unwrap();
            writeln!(self.out, "{head}:").unwrap();
            let cur = self.fresh();
            writeln!(self.out, "  {cur} = load i64, ptr {counter}").unwrap();
            let cond = self.fresh();
            writeln!(self.out, "  {cond} = icmp ult i64 {cur}, {count}").unwrap();
            writeln!(self.out, "  br i1 {cond}, label %{body}, label %{done}").unwrap();
            writeln!(self.out, "{body}:").unwrap();
            let dst = self.fresh();
            writeln!(
                self.out,
                "  {dst} = getelementptr i64, ptr {base}, i64 {cur}"
            )
            .unwrap();
            writeln!(self.out, "  store {v_llvm} {v}, ptr {dst}").unwrap();
            let next = self.fresh();
            writeln!(self.out, "  {next} = add i64 {cur}, 1").unwrap();
            writeln!(self.out, "  store i64 {next}, ptr {counter}").unwrap();
            writeln!(self.out, "  br label %{head}").unwrap();
            writeln!(self.out, "{done}:").unwrap();
        }
        Ok(())
    }

    fn fresh_label(&mut self, prefix: &str) -> String {
        let n = self.next_ssa;
        self.next_ssa += 1;
        format!("{prefix}_{n}")
    }

    fn lower_rvalue(&mut self, rvalue: &Rvalue, dest_local: Local) -> Result<String, BuildError> {
        match rvalue {
            Rvalue::Use(op) => self.lower_operand(op),
            Rvalue::UnaryOp { op, operand } => self.lower_unary(*op, operand, dest_local),
            Rvalue::BinaryOp { op, lhs, rhs } => self.lower_binary(*op, lhs, rhs, dest_local),
            Rvalue::Cast { operand, target } => self.lower_cast(operand, *target, dest_local),
            Rvalue::CallIntrinsic { name, args } => {
                self.lower_call_intrinsic(name, args, dest_local)
            }
            Rvalue::Ref { place, .. } => {
                // `&place` — we return the address of the
                // projection walk (or the bare stack slot when
                // there's no projection). In Gossamer's
                // runtime shape references are just raw
                // pointers, so the store at the caller simply
                // takes the address value as `ptr`.
                if place.projection.is_empty() {
                    Ok(local_slot(place.local))
                } else {
                    Ok(self.lower_place_address(place))
                }
            }
            Rvalue::Len(place) => {
                // `Rvalue::Len` reports the length of a
                // runtime-managed sequence. Stack-allocated
                // arrays have static lengths the compiler
                // folds from the type; heap-backed
                // `Vec`/`Slice`/`String` values go through
                // `gos_rt_len`.
                let ty = self.place_leaf_ty(place);
                if let Some(TyKind::Array { len, .. }) = self.tcx.kind(ty) {
                    return Ok(format!("{len}"));
                }
                // For heap-backed shapes the operand is the
                // opaque pointer; call the runtime.
                self.runtime_refs
                    .insert("declare i64 @gos_rt_len(ptr)".to_string());
                let ptr = if place.projection.is_empty() {
                    let tmp = self.fresh();
                    writeln!(
                        self.out,
                        "  {tmp} = load ptr, ptr {slot}",
                        slot = local_slot(place.local),
                    )
                    .unwrap();
                    tmp
                } else {
                    self.lower_place_address(place)
                };
                let tmp = self.fresh();
                writeln!(self.out, "  {tmp} = call i64 @gos_rt_len(ptr {ptr})").unwrap();
                Ok(tmp)
            }
            Rvalue::Aggregate { .. } | Rvalue::Repeat { .. } => {
                // These rvalues are only legal on the right-hand
                // side of an `Assign` statement, and
                // `lower_assign` routes them directly to the
                // dedicated in-place aggregate store. Reaching
                // them here means the MIR used them as an
                // operand, which the MVP doesn't cover.
                Err(BuildError::Unsupported(
                    "Aggregate / Repeat as non-assignment rvalue",
                ))
            }
        }
    }

    /// MIR's `CallIntrinsic` is used for stdlib math and
    /// conversion calls the lowerer wants inline (no separate
    /// Call terminator). The MVP covers the single-argument
    /// f64 functions the nbody-shape programs call through
    /// `std::math::sqrt` etc. — each maps to an LLVM intrinsic
    /// (`llvm.sqrt.f64`, `llvm.sin.f64`, …) which `llc -O3`
    /// lowers to the matching SSE/AVX instruction.
    fn lower_call_intrinsic(
        &mut self,
        name: &str,
        args: &[Operand],
        dest_local: Local,
    ) -> Result<String, BuildError> {
        let (llvm_intrinsic, expected_arity) = match name {
            "f64.sqrt" | "sqrt" => ("llvm.sqrt.f64", 1),
            "f64.sin" | "sin" => ("llvm.sin.f64", 1),
            "f64.cos" | "cos" => ("llvm.cos.f64", 1),
            "f64.abs" | "abs" => ("llvm.fabs.f64", 1),
            "f64.floor" | "floor" => ("llvm.floor.f64", 1),
            "f64.ceil" | "ceil" => ("llvm.ceil.f64", 1),
            "f64.exp" | "exp" => ("llvm.exp.f64", 1),
            "f64.ln" | "ln" | "f64.log" | "log" => ("llvm.log.f64", 1),
            _ => {
                return Err(BuildError::Unsupported("unknown CallIntrinsic name"));
            }
        };
        if args.len() != expected_arity {
            return Err(BuildError::Unsupported("CallIntrinsic arity mismatch"));
        }
        // Ensure a `declare` for this intrinsic lands in the
        // module header.
        self.runtime_refs
            .insert(format!("declare double @{llvm_intrinsic}(double)"));
        let arg_v = self.lower_operand(&args[0])?;
        let dest_llvm = render_ty(self.tcx, self.body.local_ty(dest_local));
        let tmp = self.fresh();
        writeln!(
            self.out,
            "  {tmp} = call {dest_llvm} @{llvm_intrinsic}(double {arg_v})"
        )
        .unwrap();
        Ok(tmp)
    }

    fn lower_operand(&mut self, op: &Operand) -> Result<String, BuildError> {
        match op {
            Operand::Copy(place) => Ok(self.lower_place_read(place)),
            Operand::Const(ConstValue::Str(text)) => {
                let (name, _len) = self.strings.borrow_mut().intern(text);
                Ok(name)
            }
            Operand::Const(value) => Ok(render_const(value)),
            Operand::FnRef { def, .. } => {
                // `Operand::FnRef` as an *argument* — the
                // address of the named function. The MIR
                // lowerer types its containing local as
                // `i64` (`go expr` stuffs the address into a
                // register-sized scalar to pass through
                // `gos_rt_go_spawn_call_N`). LLVM globals are
                // pointer-typed, so we have to `ptrtoint` the
                // global to an i64 before the surrounding
                // `store` / `call` consumes it.
                if let Some(name) = self.fn_name_by_def.get(&def.local).cloned() {
                    let tmp = self.fresh();
                    writeln!(self.out, "  {tmp} = ptrtoint ptr @\"{name}\" to i64").unwrap();
                    return Ok(tmp);
                }
                Err(BuildError::Unsupported("FnRef operand not yet lowered"))
            }
        }
    }

    /// Reads a place, walking its `projection` chain. For a
    /// plain local (no projection) this is a single `load`
    /// against the stack slot. For `a[i].field` chains we
    /// compute the byte-offset address via `getelementptr i64`
    /// steps and then `load` the leaf scalar at its native
    /// type — matching the flat-slot layout the Cranelift
    /// backend emits. String byte indexing (`s[i]` where `s`
    /// is `String` or `&String`) is short-circuited through
    /// `gos_rt_str_byte_at`.
    fn lower_place_read(&mut self, place: &Place) -> String {
        if let Some(value) = self.try_string_byte_read(place) {
            return value;
        }
        let leaf_ty = self.place_leaf_ty(place);
        let leaf_llvm = render_ty(self.tcx, leaf_ty);
        if leaf_llvm == "void" {
            return String::new();
        }
        if place.projection.is_empty() {
            // Aggregate locals (`[Body; 5]`, struct, tuple) are
            // stored as a flat `[N x i64]` slab — the "value"
            // representation downstream code consumes is the
            // slot address itself. Loading the first 8 bytes as
            // a pointer is incorrect (mistakes the aggregate's
            // first scalar field for a pointer). When the local
            // *is* an aggregate, return its address; assignments
            // and call args treat that as the by-reference handle
            // the runtime / lowered code expects.
            if is_aggregate(self.tcx, self.body.local_ty(place.local)) {
                return local_slot(place.local);
            }
            let tmp = self.fresh();
            writeln!(
                self.out,
                "  {tmp} = load {leaf_llvm}, ptr {slot}",
                slot = local_slot(place.local)
            )
            .unwrap();
            return tmp;
        }
        let addr = self.lower_place_address(place);
        let tmp = self.fresh();
        writeln!(self.out, "  {tmp} = load {leaf_llvm}, ptr {addr}").unwrap();
        tmp
    }

    /// If `place`'s final projection is an `Index` whose
    /// preceding type resolves to `String` / `&String`, emit
    /// a runtime call to `gos_rt_str_byte_at(ptr, idx) ->
    /// i64` and return the byte value. Returns `None` for
    /// non-string indexing so the caller falls through to the
    /// generic aggregate walk.
    fn try_string_byte_read(&mut self, place: &Place) -> Option<String> {
        if place.projection.is_empty() {
            return None;
        }
        // Walk every projection except the last, resolving
        // the type after each step — the final one must be
        // `Index` on a `String`.
        let (prefix, last) = place.projection.split_at(place.projection.len() - 1);
        let Projection::Index(idx_local) = &last[0] else {
            return None;
        };
        // Compute the type the last-step operates on by
        // walking `prefix`.
        let mut ty = self.body.local_ty(place.local);
        for proj in prefix {
            ty = self.unwrap_ref(ty);
            ty = match proj {
                Projection::Field(i) => match self.tcx.kind(ty) {
                    Some(TyKind::Adt { def, .. }) => self
                        .tcx
                        .struct_field_tys(*def)
                        .and_then(|tys| tys.get(*i as usize).copied())
                        .unwrap_or(ty),
                    Some(TyKind::Tuple(elems)) => elems.get(*i as usize).copied().unwrap_or(ty),
                    _ => ty,
                },
                Projection::Index(_) => match self.tcx.kind(ty) {
                    Some(TyKind::Array { elem, .. } | TyKind::Slice(elem) | TyKind::Vec(elem)) => {
                        *elem
                    }
                    _ => ty,
                },
                Projection::Deref => self.unwrap_ref(ty),
                Projection::Downcast(_) | Projection::Discriminant => ty,
            };
        }
        ty = self.unwrap_ref(ty);
        if !matches!(self.tcx.kind(ty), Some(TyKind::String)) {
            return None;
        }
        // Resolve the pointer to the string. With no prefix
        // projections, that's the local's loaded value; with
        // prefix projections it's the projected address, then
        // a load of `ptr`.
        let str_ptr = if prefix.is_empty() {
            let tmp = self.fresh();
            writeln!(
                self.out,
                "  {tmp} = load ptr, ptr {slot}",
                slot = local_slot(place.local),
            )
            .unwrap();
            tmp
        } else {
            let prefix_place = Place {
                local: place.local,
                projection: prefix.to_vec(),
            };
            let addr = self.lower_place_address(&prefix_place);
            let tmp = self.fresh();
            writeln!(self.out, "  {tmp} = load ptr, ptr {addr}").unwrap();
            tmp
        };
        // Load the i64 index value.
        let idx_tmp = self.fresh();
        writeln!(
            self.out,
            "  {idx_tmp} = load i64, ptr {slot}",
            slot = local_slot(*idx_local)
        )
        .unwrap();
        self.runtime_refs
            .insert("declare i64 @gos_rt_str_byte_at(ptr, i64)".to_string());
        let out = self.fresh();
        writeln!(
            self.out,
            "  {out} = call i64 @gos_rt_str_byte_at(ptr {str_ptr}, i64 {idx_tmp})"
        )
        .unwrap();
        Some(out)
    }

    /// Computes the pointer address for a projected place.
    /// Walks `Field` / `Index` / `Deref` steps as byte-offset
    /// `getelementptr` instructions against the root local's
    /// stack slot (or a dereferenced pointer).
    fn lower_place_address(&mut self, place: &Place) -> String {
        let mut current = local_slot(place.local);
        let mut current_ty = self.body.local_ty(place.local);
        // If the root local is a reference (`&[Body; 5]`) or a
        // runtime-managed pointer (Vec, Slice, String,
        // HashMap, …), the local's *slot* holds a pointer
        // to the actual storage; load it once so subsequent
        // projections walk the referent rather than the
        // alloca itself. Stack-allocated aggregates ([Body;5]
        // declared inline) hold the data directly in their slot
        // so we leave `current` pointing at the alloca.
        if Self::is_pointer_local_ty(self.tcx, current_ty) {
            let next = self.fresh();
            writeln!(self.out, "  {next} = load ptr, ptr {current}").unwrap();
            current = next;
            current_ty = self.unwrap_ref(current_ty);
        }
        let mut stride_slots: u32 = elem_slots(self.tcx, current_ty);
        for proj in &place.projection {
            match proj {
                Projection::Field(idx) => {
                    let next = self.fresh();
                    writeln!(
                        self.out,
                        "  {next} = getelementptr i64, ptr {current}, i64 {idx}"
                    )
                    .unwrap();
                    current = next;
                }
                Projection::Index(index_local) => {
                    // Load the index value, widen to i64, then
                    // multiply by the per-element slot count
                    // and add to the base pointer.
                    let idx_slot = local_slot(*index_local);
                    let idx_raw = self.fresh();
                    writeln!(self.out, "  {idx_raw} = load i64, ptr {idx_slot}").unwrap();
                    let next = self.fresh();
                    if stride_slots == 1 {
                        writeln!(
                            self.out,
                            "  {next} = getelementptr i64, ptr {current}, i64 {idx_raw}"
                        )
                        .unwrap();
                    } else {
                        let scaled = self.fresh();
                        writeln!(self.out, "  {scaled} = mul i64 {idx_raw}, {stride_slots}")
                            .unwrap();
                        writeln!(
                            self.out,
                            "  {next} = getelementptr i64, ptr {current}, i64 {scaled}"
                        )
                        .unwrap();
                    }
                    current = next;
                    // After indexing, the remaining projections
                    // walk inside one element — subsequent Field
                    // offsets use the base stride (8 bytes per
                    // scalar field).
                    stride_slots = 1;
                }
                Projection::Deref => {
                    let next = self.fresh();
                    writeln!(self.out, "  {next} = load ptr, ptr {current}").unwrap();
                    current = next;
                    stride_slots = 1;
                }
                Projection::Discriminant => {
                    // Discriminant at offset 0; no pointer
                    // change, but later Field offsets walk past
                    // the tag word.
                    stride_slots = 1;
                }
                Projection::Downcast(_) => {
                    // Skip the 8-byte tag word to land on the
                    // payload.
                    let next = self.fresh();
                    writeln!(
                        self.out,
                        "  {next} = getelementptr i8, ptr {current}, i64 8"
                    )
                    .unwrap();
                    current = next;
                    stride_slots = 1;
                }
            }
        }
        current
    }

    /// Resolves the leaf type of a projection chain: the type
    /// the final `load`/`store` should use. Walks the MIR
    /// projections the same way the runtime does — an `Index`
    /// on an array yields the element type, a `Field` on a
    /// struct yields the field's type, etc. Auto-peels `&T` /
    /// `&mut T` reference layers at each step so the same
    /// code path handles `fn energy(b: &[Body; 5])`-style
    /// reference parameters whose MIR may or may not carry
    /// an explicit `Deref` projection.
    fn place_leaf_ty(&self, place: &Place) -> Ty {
        let mut ty = self.body.local_ty(place.local);
        for proj in &place.projection {
            ty = self.unwrap_ref(ty);
            ty = match proj {
                Projection::Field(idx) => match self.tcx.kind(ty) {
                    Some(TyKind::Adt { def, .. }) => self
                        .tcx
                        .struct_field_tys(*def)
                        .and_then(|tys| tys.get(*idx as usize).copied())
                        .unwrap_or(ty),
                    Some(TyKind::Tuple(elems)) => elems.get(*idx as usize).copied().unwrap_or(ty),
                    _ => ty,
                },
                Projection::Index(_) => match self.tcx.kind(ty) {
                    Some(TyKind::Array { elem, .. } | TyKind::Slice(elem) | TyKind::Vec(elem)) => {
                        *elem
                    }
                    _ => ty,
                },
                Projection::Deref => match self.tcx.kind(ty) {
                    Some(TyKind::Ref { inner, .. }) => *inner,
                    _ => ty,
                },
                Projection::Downcast(_) | Projection::Discriminant => ty,
            };
        }
        ty
    }

    /// True when the local's slot holds a pointer to the actual
    /// data (rather than the data itself). Reference types,
    /// runtime-managed shapes (`Vec`, `Slice`, `String`,
    /// `HashMap`, channels, dyn objects), and function pointers
    /// all live as `ptr` in the slot. Stack-allocated
    /// aggregates (Array / Tuple / Adt declared inline) hold
    /// their data in-place. Anything classified as opaque
    /// `ptr` by the type renderer that *isn't* a stack
    /// aggregate is treated as a pointer-bearing slot — this
    /// catches inference variables that left the typeck pipeline
    /// unresolved (a runtime call like `os::args()` whose return
    /// type is materialised at MIR time but never gets a concrete
    /// `Vec` resolution).
    fn is_pointer_local_ty(tcx: &TyCtxt, ty: Ty) -> bool {
        if matches!(
            tcx.kind(ty),
            Some(
                TyKind::Ref { .. }
                    | TyKind::Vec(_)
                    | TyKind::Slice(_)
                    | TyKind::String
                    | TyKind::HashMap { .. }
                    | TyKind::Sender(_)
                    | TyKind::Receiver(_)
                    | TyKind::Dyn(_)
                    | TyKind::FnPtr(_)
                    | TyKind::FnDef { .. }
            )
        ) {
            return true;
        }
        // For unresolved inference variables / opaque shapes,
        // the alloca was built as `ptr` (see `emit_allocas`).
        // Treat those as pointer-bearing too.
        !is_aggregate(tcx, ty) && render_ty(tcx, ty) == "ptr" && !is_unit(tcx, ty)
    }

    /// Peels any `&T` / `&mut T` layers off `ty` so subsequent
    /// type-dependent work (struct-field offset lookup, array
    /// stride calculation) sees the underlying aggregate.
    fn unwrap_ref(&self, mut ty: Ty) -> Ty {
        loop {
            match self.tcx.kind(ty) {
                Some(TyKind::Ref { inner, .. }) => ty = *inner,
                _ => return ty,
            }
        }
    }

    fn lower_unary(
        &mut self,
        op: UnOp,
        operand: &Operand,
        dest_local: Local,
    ) -> Result<String, BuildError> {
        let operand_v = self.lower_operand(operand)?;
        let dest_ty = self.body.local_ty(dest_local);
        let kind = numeric_kind(self.tcx, dest_ty);
        let tmp = self.fresh();
        match (op, kind) {
            (UnOp::Neg, NumericKind::Int(_)) => {
                writeln!(self.out, "  {tmp} = sub i64 0, {operand_v}").unwrap();
            }
            (UnOp::Neg, NumericKind::Float(f)) => {
                let ty = match f {
                    FloatTy::F32 => "float",
                    FloatTy::F64 => "double",
                };
                writeln!(self.out, "  {tmp} = fneg {ty} {operand_v}").unwrap();
            }
            (UnOp::Not, _) => {
                // `Not` is bitwise on integers, logical on bool.
                // Both map to `xor` with an all-ones mask for the
                // operand's width — `-1` covers both `i1` and
                // wider integer types.
                let ty = render_ty(self.tcx, dest_ty);
                writeln!(self.out, "  {tmp} = xor {ty} {operand_v}, -1").unwrap();
            }
            _ => {
                return Err(BuildError::Unsupported("unary op on non-numeric type"));
            }
        }
        Ok(tmp)
    }

    fn lower_binary(
        &mut self,
        op: BinOp,
        lhs: &Operand,
        rhs: &Operand,
        dest_local: Local,
    ) -> Result<String, BuildError> {
        let mut lhs_v = self.lower_operand(lhs)?;
        let mut rhs_v = self.lower_operand(rhs)?;
        // Comparisons return `i1`; everything else returns the
        // operands' shared type. Pick the operand type off
        // either side — both are the same kind by MIR
        // invariant.
        let operand_ty = self.operand_ty(lhs);
        let mut kind = numeric_kind(self.tcx, operand_ty);
        let mut operand_llvm = render_ty(self.tcx, operand_ty);
        // The MIR lowering of `||` / `&&` evaluates both
        // operands eagerly and folds into `Add` / similar
        // arithmetic on `i1`, with a `SwitchInt(0, false_arm)`
        // in the default branch acting as the boolean reduce.
        // On `i1`, `1 + 1` wraps to `0` and breaks the
        // semantics. Widen both operands to `i64` for any
        // non-bitwise / non-comparison arith on `i1` so the
        // Cranelift backend's `i8`-style "extend then add"
        // shape is preserved.
        if operand_llvm == "i1"
            && matches!(
                op,
                BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem
            )
        {
            let zlhs = self.fresh();
            writeln!(self.out, "  {zlhs} = zext i1 {lhs_v} to i64").unwrap();
            let zrhs = self.fresh();
            writeln!(self.out, "  {zrhs} = zext i1 {rhs_v} to i64").unwrap();
            lhs_v = zlhs;
            rhs_v = zrhs;
            operand_llvm = "i64".to_string();
            kind = NumericKind::Int(gossamer_types::IntTy::I64);
        }
        let tmp = self.fresh();
        let instr = match (op, kind) {
            (BinOp::Add, NumericKind::Int(_)) => format!("add {operand_llvm}"),
            (BinOp::Sub, NumericKind::Int(_)) => format!("sub {operand_llvm}"),
            (BinOp::Mul, NumericKind::Int(_)) => format!("mul {operand_llvm}"),
            (BinOp::Div, NumericKind::Int(i)) => {
                if int_signed(i) {
                    format!("sdiv {operand_llvm}")
                } else {
                    format!("udiv {operand_llvm}")
                }
            }
            (BinOp::Rem, NumericKind::Int(i)) => {
                if int_signed(i) {
                    format!("srem {operand_llvm}")
                } else {
                    format!("urem {operand_llvm}")
                }
            }
            (BinOp::BitAnd, _) => format!("and {operand_llvm}"),
            (BinOp::BitOr, _) => format!("or {operand_llvm}"),
            (BinOp::BitXor, _) => format!("xor {operand_llvm}"),
            (BinOp::Shl, _) => format!("shl {operand_llvm}"),
            (BinOp::Shr, NumericKind::Int(i)) => {
                if int_signed(i) {
                    format!("ashr {operand_llvm}")
                } else {
                    format!("lshr {operand_llvm}")
                }
            }
            (BinOp::Add, NumericKind::Float(_)) => format!("fadd {operand_llvm}"),
            (BinOp::Sub, NumericKind::Float(_)) => format!("fsub {operand_llvm}"),
            (BinOp::Mul, NumericKind::Float(_)) => format!("fmul {operand_llvm}"),
            (BinOp::Div, NumericKind::Float(_)) => format!("fdiv {operand_llvm}"),
            (BinOp::Rem, NumericKind::Float(_)) => format!("frem {operand_llvm}"),
            (cmp, NumericKind::Int(i)) if is_cmp(cmp) => {
                let pred = int_cmp_pred(cmp, int_signed(i));
                format!("icmp {pred} {operand_llvm}")
            }
            (cmp, NumericKind::Float(_)) if is_cmp(cmp) => {
                let pred = float_cmp_pred(cmp);
                format!("fcmp {pred} {operand_llvm}")
            }
            (cmp, _) if matches!(cmp, BinOp::Eq | BinOp::Ne) => {
                // Equality on non-numeric types (bool, char,
                // opaque pointers) uses `icmp`.
                let pred = if matches!(cmp, BinOp::Eq) { "eq" } else { "ne" };
                format!("icmp {pred} {operand_llvm}")
            }
            _ => {
                if std::env::var("GOS_LLVM_TRACE").is_ok() {
                    eprintln!(
                        "llvm backend: binop fallback: op={op:?} kind={kind:?} \
                         operand_ty={operand_llvm}"
                    );
                }
                return Err(BuildError::Unsupported(
                    "binary op / operand-type combination",
                ));
            }
        };
        writeln!(self.out, "  {tmp} = {instr} {lhs_v}, {rhs_v}").unwrap();
        // Coerce the result back to the destination type.
        //
        // * Comparison ops (Eq/Ne/Lt/Le/Gt/Ge): result is
        //   always `i1`. If the destination is wider, `zext`
        //   to its width.
        // * Arithmetic ops on `i1`-widened operands (the
        //   `&&` / `||` shape): result is `i64`. If the
        //   destination is `i1`, narrow via `icmp ne 0`.
        // Other (operand_llvm == dest_llvm): no coercion.
        let dest_ty = self.body.local_ty(dest_local);
        let dest_llvm = render_ty(self.tcx, dest_ty);
        let is_cmp = matches!(
            op,
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge
        );
        if is_cmp {
            // Result is `i1`.
            if dest_llvm == "i1" {
                return Ok(tmp);
            }
            let widened = self.fresh();
            writeln!(self.out, "  {widened} = zext i1 {tmp} to {dest_llvm}").unwrap();
            return Ok(widened);
        }
        // Arithmetic — result type matches `operand_llvm`.
        if operand_llvm == "i64" && dest_llvm == "i1" {
            let narrowed = self.fresh();
            writeln!(self.out, "  {narrowed} = icmp ne i64 {tmp}, 0").unwrap();
            return Ok(narrowed);
        }
        Ok(tmp)
    }

    fn lower_cast(
        &mut self,
        operand: &Operand,
        target: gossamer_types::Ty,
        _dest_local: Local,
    ) -> Result<String, BuildError> {
        let src_v = self.lower_operand(operand)?;
        let src_ty = self.operand_ty(operand);
        let src_kind = numeric_kind(self.tcx, src_ty);
        let dst_kind = numeric_kind(self.tcx, target);
        let src_llvm = render_ty(self.tcx, src_ty);
        let dst_llvm = render_ty(self.tcx, target);
        if src_llvm == dst_llvm {
            return Ok(src_v);
        }
        let tmp = self.fresh();
        let instr = match (src_kind, dst_kind) {
            (NumericKind::Int(a), NumericKind::Int(b)) => {
                let aw = int_width(a);
                let bw = int_width(b);
                if bw == aw {
                    // Same width, different signedness → bitcast.
                    format!("bitcast {src_llvm} {src_v} to {dst_llvm}")
                } else if bw < aw {
                    format!("trunc {src_llvm} {src_v} to {dst_llvm}")
                } else if int_signed(a) {
                    format!("sext {src_llvm} {src_v} to {dst_llvm}")
                } else {
                    format!("zext {src_llvm} {src_v} to {dst_llvm}")
                }
            }
            (NumericKind::Int(i), NumericKind::Float(_)) => {
                if int_signed(i) {
                    format!("sitofp {src_llvm} {src_v} to {dst_llvm}")
                } else {
                    format!("uitofp {src_llvm} {src_v} to {dst_llvm}")
                }
            }
            (NumericKind::Float(_), NumericKind::Int(i)) => {
                if int_signed(i) {
                    format!("fptosi {src_llvm} {src_v} to {dst_llvm}")
                } else {
                    format!("fptoui {src_llvm} {src_v} to {dst_llvm}")
                }
            }
            (NumericKind::Float(FloatTy::F32), NumericKind::Float(FloatTy::F64)) => {
                format!("fpext {src_llvm} {src_v} to {dst_llvm}")
            }
            (NumericKind::Float(FloatTy::F64), NumericKind::Float(FloatTy::F32)) => {
                format!("fptrunc {src_llvm} {src_v} to {dst_llvm}")
            }
            _ => {
                return Err(BuildError::Unsupported("cast between non-numeric types"));
            }
        };
        writeln!(self.out, "  {tmp} = {instr}").unwrap();
        Ok(tmp)
    }

    fn lower_terminator(&mut self, term: &Terminator) -> Result<(), BuildError> {
        match term {
            Terminator::Return => {
                // Emit cleanup calls for owning heap-typed locals before
                // the actual `ret`. Mirrors the Cranelift Return path —
                // see `gossamer_mir::plan_cleanup` for the analysis.
                // Each entry is `(local, free_fn)`: load the alloca
                // backing the local and call the runtime reclamation
                // helper. Without this loop the `_free` symbols ship in
                // the runtime but are never called and every owning
                // `Vec<i64>` / `Vec<u8>` / channel leaks until process
                // exit (C2 in `~/dev/contexts/lang/adversarial_analysis.md`).
                let cleanup = gossamer_mir::plan_cleanup(self.body);
                for entry in cleanup.entries() {
                    let tmp = self.fresh();
                    writeln!(
                        self.out,
                        "  {tmp} = load ptr, ptr {slot}",
                        slot = local_slot(entry.local)
                    )
                    .unwrap();
                    writeln!(
                        self.out,
                        "  call void @{free}(ptr {tmp})",
                        free = entry.free_fn
                    )
                    .unwrap();
                }
                let ret_ty = self.body.local_ty(Local::RETURN);
                let ret_llvm = render_ty(self.tcx, ret_ty);
                if is_unit(self.tcx, ret_ty) {
                    writeln!(self.out, "  ret void").unwrap();
                } else if is_aggregate(self.tcx, ret_ty) {
                    // Aggregate return: the callee's `%l0` is a stack
                    // alloca whose storage dies when the frame pops.
                    // Heap-allocate so the returned pointer outlives
                    // the call, copy the inline data over, and return
                    // the heap pointer. Both LLVM and Cranelift
                    // callers can dereference the result safely.
                    let bytes = u64::from(slot_count(self.tcx, ret_ty).unwrap_or(1).max(1)) * 8;
                    let heap = self.fresh();
                    writeln!(
                        self.out,
                        "  {heap} = call ptr @gos_rt_gc_alloc(i64 {bytes})"
                    )
                    .unwrap();
                    writeln!(
                        self.out,
                        "  call void @llvm.memcpy.p0.p0.i64(ptr {heap}, ptr {slot}, i64 {bytes}, i1 false)",
                        slot = local_slot(Local::RETURN)
                    )
                    .unwrap();
                    writeln!(self.out, "  ret ptr {heap}").unwrap();
                } else {
                    let tmp = self.fresh();
                    writeln!(
                        self.out,
                        "  {tmp} = load {ret_llvm}, ptr {slot}",
                        slot = local_slot(Local::RETURN)
                    )
                    .unwrap();
                    writeln!(self.out, "  ret {ret_llvm} {tmp}").unwrap();
                }
                Ok(())
            }
            Terminator::Goto { target } => {
                if self.current_block.is_some_and(|src| target.as_u32() <= src) {
                    self.emit_preempt_check();
                }
                writeln!(self.out, "  br label %bb{}", target.as_u32()).unwrap();
                Ok(())
            }
            Terminator::SwitchInt {
                discriminant,
                arms,
                default,
            } => {
                let src = self.current_block.unwrap_or(u32::MAX);
                let has_back_edge =
                    arms.iter().any(|(_, t)| t.as_u32() <= src) || default.as_u32() <= src;
                if has_back_edge {
                    self.emit_preempt_check();
                }
                let v = self.lower_operand(discriminant)?;
                let ty = render_ty(self.tcx, self.operand_ty(discriminant));
                writeln!(
                    self.out,
                    "  switch {ty} {v}, label %bb{default} [",
                    default = default.as_u32()
                )
                .unwrap();
                for (cst, target) in arms {
                    writeln!(self.out, "    {ty} {cst}, label %bb{}", target.as_u32()).unwrap();
                }
                writeln!(self.out, "  ]").unwrap();
                Ok(())
            }
            Terminator::Unreachable => {
                writeln!(self.out, "  unreachable").unwrap();
                Ok(())
            }
            Terminator::Panic { message } => {
                self.lower_panic(message);
                Ok(())
            }
            Terminator::Drop { target, .. } => {
                // Gossamer runtime manages drops through the GC
                // hooks; the MIR `Drop` terminator is a
                // sequencing point that the LLVM backend can
                // treat as a plain `Goto` without calling any
                // destructor (no-op drop).
                writeln!(self.out, "  br label %bb{}", target.as_u32()).unwrap();
                Ok(())
            }
            Terminator::Assert {
                cond,
                expected,
                target,
                msg,
            } => self.lower_assert(cond, *expected, *target, msg),
            Terminator::Call {
                callee,
                args,
                destination,
                target,
            } => self.lower_call(callee, args, destination, target.as_ref()),
        }
    }

    /// Emits the runtime call + `unreachable` for a MIR
    /// `Terminator::Panic`. The message is interned as a
    /// private rodata global; `gos_rt_panic` is `noreturn`.
    fn lower_panic(&mut self, message: &str) {
        let msg_global = self.runtime_refs.len();
        let msg_name = format!("@.panic_msg_{msg_global}");
        let escaped = escape_c_string(message);
        let size = message.len() + 1;
        self.runtime_refs.insert(format!(
            "{msg_name} = private unnamed_addr constant [{size} x i8] c\"{escaped}\\00\""
        ));
        writeln!(self.out, "  call void @gos_rt_panic(ptr {msg_name})").unwrap();
        writeln!(self.out, "  unreachable").unwrap();
    }

    /// Lowers `Terminator::Assert`: branches to the success
    /// target on the expected condition; on the other branch
    /// emits a category-specific panic message. Mirrors the
    /// Cranelift backend's `BoundsCheck` / `Overflow` /
    /// `DivideByZero` strings so panic output stays consistent
    /// across backends.
    fn lower_assert(
        &mut self,
        cond: &Operand,
        expected: bool,
        target: gossamer_mir::BlockId,
        msg: &gossamer_mir::AssertMessage,
    ) -> Result<(), BuildError> {
        let v = self.lower_operand(cond)?;
        let cond_ty = render_ty(self.tcx, self.operand_ty(cond));
        let cond_bit = if cond_ty == "i1" {
            v
        } else {
            let t = self.fresh();
            writeln!(self.out, "  {t} = icmp ne {cond_ty} {v}, 0").unwrap();
            t
        };
        let ok_label = format!("bb{}", target.as_u32());
        let fail_label = format!("assert_fail_{}", self.next_ssa);
        self.next_ssa += 1;
        let br_true = if expected { &ok_label } else { &fail_label };
        let br_false = if expected { &fail_label } else { &ok_label };
        writeln!(
            self.out,
            "  br i1 {cond_bit}, label %{br_true}, label %{br_false}"
        )
        .unwrap();
        let msg_text = match msg {
            gossamer_mir::AssertMessage::BoundsCheck => "index out of bounds\n",
            gossamer_mir::AssertMessage::Overflow => "arithmetic overflow\n",
            gossamer_mir::AssertMessage::DivideByZero => "divide by zero\n",
        };
        let msg_global = self.runtime_refs.len();
        let msg_name = format!("@.assert_msg_{msg_global}");
        let escaped = escape_c_string(msg_text);
        let size = msg_text.len() + 1;
        self.runtime_refs.insert(format!(
            "{msg_name} = private unnamed_addr constant [{size} x i8] c\"{escaped}\\00\""
        ));
        writeln!(self.out, "{fail_label}:").unwrap();
        writeln!(self.out, "  call void @gos_rt_panic(ptr {msg_name})").unwrap();
        writeln!(self.out, "  unreachable").unwrap();
        Ok(())
    }

    /// Indirect call lowering for `f(args…)` where `f` is a
    /// local variable holding either a plain function pointer
    /// or a closure-environment record. The callee classifier
    /// follows what the Cranelift backend does in its
    /// `call_indirect` arm:
    ///   1. `FnDef` / `FnPtr`-typed local → value is the fn
    ///      address; call directly with the plain arg list.
    ///   2. Closure env (other reference / opaque ptr local) →
    ///      load fn pointer from `env[0]`, then call with `env`
    ///      as the implicit first arg followed by the user
    ///      args.
    fn lower_indirect_call(
        &mut self,
        place: &Place,
        args: &[Operand],
        destination: &Place,
        target: Option<&gossamer_mir::BlockId>,
    ) -> Result<(), BuildError> {
        if !destination.projection.is_empty() {
            return Err(BuildError::Unsupported(
                "indirect call destination cannot have projections",
            ));
        }
        let callee_ty = self.body.local_ty(place.local);
        // Mirrors the Cranelift narrowing: only `FnDef`-typed
        // locals (the result of `Operand::FnRef`) hold a raw
        // function address. `FnPtr` / `FnTrait` locals carry an
        // env pointer post the MIR's let / return / assign
        // coercion, so they share the closure dispatch shape.
        let is_plain_fn = matches!(self.tcx.kind(callee_ty), Some(TyKind::FnDef { .. }));
        // Read the local's value: for a function pointer the
        // load yields the callable address; for a closure env
        // it yields the env pointer.
        let env_value = self.lower_place_read(place);
        let fn_ptr = if is_plain_fn {
            env_value.clone()
        } else {
            let tmp = self.fresh();
            writeln!(self.out, "  {tmp} = load ptr, ptr {env_value}").unwrap();
            tmp
        };
        let dest_ty_mir = self.body.local_ty(destination.local);
        let dest_llvm = render_ty(self.tcx, dest_ty_mir);
        let mut arg_text = String::new();
        if !is_plain_fn {
            // Closure: env is the first arg.
            arg_text.push_str("ptr ");
            arg_text.push_str(&env_value);
        }
        for arg in args {
            if !arg_text.is_empty() {
                arg_text.push_str(", ");
            }
            let a_ty = self.operand_llvm_ty(arg);
            let a_v = self.lower_operand(arg)?;
            let _ = write!(arg_text, "{a_ty} {a_v}");
        }
        if dest_llvm == "void" || is_unit(self.tcx, dest_ty_mir) {
            writeln!(self.out, "  call void {fn_ptr}({arg_text})").unwrap();
        } else {
            let tmp = self.fresh();
            writeln!(self.out, "  {tmp} = call {dest_llvm} {fn_ptr}({arg_text})").unwrap();
            let slot = local_slot(destination.local);
            writeln!(self.out, "  store {dest_llvm} {tmp}, ptr {slot}").unwrap();
        }
        match target {
            Some(t) => {
                writeln!(self.out, "  br label %bb{}", t.as_u32()).unwrap();
            }
            None => {
                writeln!(self.out, "  unreachable").unwrap();
            }
        }
        Ok(())
    }

    /// Lowers `println` / `print` / `eprintln` / `eprint` by
    /// dispatching each argument through the runtime helper
    /// matching its MIR type (`gos_rt_print_str` for strings,
    /// `_i64` for integers, `_f64` for floats, `_bool`, `_char`).
    /// Mirrors the per-arg shape of `lower_concat_call` so that
    /// bare `println(5i64)` and interpolated `println!("{n}")`
    /// share one code path. `*ln` variants append a trailing
    /// `gos_rt_println()` for the newline + flush.
    fn lower_print_call(
        &mut self,
        name: &str,
        args: &[Operand],
        destination: &Place,
        target: Option<&gossamer_mir::BlockId>,
    ) -> Result<(), BuildError> {
        if !destination.projection.is_empty() {
            return Err(BuildError::Unsupported(
                "println destination cannot have projections",
            ));
        }
        // Hold the stdout lock for the whole sequence — every
        // per-arg print + the trailing newline is one atomic
        // unit so concurrent goroutines on other OS threads
        // can't interleave their output mid-line. The lock is
        // reentrant, so the inner runtime helpers (which also
        // acquire) coexist with this outer acquire on the same
        // thread. On `Unsupported` we abandon the whole build
        // and fall back to Cranelift, so the dangling acquire
        // is harmless — the LLVM module itself is dropped.
        writeln!(self.out, "  call void @gos_rt_stdout_acquire()").unwrap();
        // Spec: each arg is space-separated. Mirrors the
        // interpreter's `render_args` (which inserts a `' '`
        // between each pair).
        self.emit_per_arg_print(args, " ")?;
        if matches!(name, "println" | "eprintln") {
            writeln!(self.out, "  call void @gos_rt_println()").unwrap();
        }
        writeln!(self.out, "  call void @gos_rt_stdout_release()").unwrap();
        if !is_unit(self.tcx, self.body.local_ty(destination.local)) {
            let dest_llvm = render_ty(self.tcx, self.body.local_ty(destination.local));
            let slot = local_slot(destination.local);
            // `println`'s return value is `()` per the prelude;
            // give the destination slot a zero value of its
            // declared type so any unexpected reader sees a sane
            // bit pattern.
            let zero = match dest_llvm.as_str() {
                "double" | "float" => "0.0".to_string(),
                "ptr" => "null".to_string(),
                _ => "0".to_string(),
            };
            writeln!(self.out, "  store {dest_llvm} {zero}, ptr {slot}").unwrap();
        }
        match target {
            Some(t) => {
                writeln!(self.out, "  br label %bb{}", t.as_u32()).unwrap();
            }
            None => {
                writeln!(self.out, "  unreachable").unwrap();
            }
        }
        Ok(())
    }

    /// Emits one runtime print call per argument, dispatching
    /// by the operand's MIR type. When `separator` is non-empty,
    /// emits a `gos_rt_print_str(separator)` call between each
    /// pair of args (used by `println(a, b, c)` for the
    /// space-separated form; empty for `__concat`'s tight join).
    fn emit_per_arg_print(&mut self, args: &[Operand], separator: &str) -> Result<(), BuildError> {
        let sep_name = if separator.is_empty() {
            None
        } else {
            Some(self.strings.borrow_mut().intern(separator).0)
        };
        for (idx, arg) in args.iter().enumerate() {
            if idx > 0 {
                if let Some(name) = &sep_name {
                    writeln!(self.out, "  call void @gos_rt_print_str(ptr {name})").unwrap();
                }
            }
            let kind = self.concat_print_kind(arg);
            if matches!(kind, ConcatKind::Unsupported) {
                // Surface a generic "unsupported" so the driver
                // routes this body to Cranelift, whose `bail!`
                // emits a user-facing message naming the
                // specific operand kind (tuple, Vec, struct, …).
                return Err(BuildError::Unsupported(
                    "println/format of aggregate or variant types",
                ));
            }
            let value = self.lower_operand(arg)?;
            match kind {
                ConcatKind::StrPtr => {
                    writeln!(self.out, "  call void @gos_rt_print_str(ptr {value})").unwrap();
                }
                ConcatKind::Int => {
                    let widened = self.widen_to_i64(arg, &value);
                    writeln!(self.out, "  call void @gos_rt_print_i64(i64 {widened})").unwrap();
                }
                ConcatKind::Float => {
                    let widened = self.widen_to_f64(arg, &value);
                    writeln!(self.out, "  call void @gos_rt_print_f64(double {widened})").unwrap();
                }
                ConcatKind::Bool => {
                    let widened = self.widen_bool_to_i32(arg, &value);
                    writeln!(self.out, "  call void @gos_rt_print_bool(i32 {widened})").unwrap();
                }
                ConcatKind::Char => {
                    let widened = self.widen_char_to_i32(arg, &value);
                    writeln!(self.out, "  call void @gos_rt_print_char(i32 {widened})").unwrap();
                }
                ConcatKind::Unsupported => unreachable!("checked above"),
            }
        }
        Ok(())
    }

    /// Builds a single concatenated c-string from every argument
    /// and stores its pointer in `dest_ssa`. Each arg is
    /// converted through `gos_rt_*_to_str` (or passed through for
    /// strings); pieces are joined with `separator` via
    /// `gos_rt_str_concat`. Used by multi-arg `panic(...)` where
    /// the runtime takes a single message pointer.
    fn emit_args_to_concat_string(
        &mut self,
        args: &[Operand],
        separator: &str,
    ) -> Result<String, BuildError> {
        let (empty_name, _) = self.strings.borrow_mut().intern("");
        if args.is_empty() {
            return Ok(empty_name);
        }
        let sep_name = if separator.is_empty() {
            None
        } else {
            Some(self.strings.borrow_mut().intern(separator).0)
        };
        let mut acc = self.lower_arg_to_str_ptr(&args[0])?;
        for arg in &args[1..] {
            if let Some(name) = &sep_name {
                let next = self.fresh();
                writeln!(
                    self.out,
                    "  {next} = call ptr @gos_rt_str_concat(ptr {acc}, ptr {name})"
                )
                .unwrap();
                acc = next;
            }
            let piece = self.lower_arg_to_str_ptr(arg)?;
            let next = self.fresh();
            writeln!(
                self.out,
                "  {next} = call ptr @gos_rt_str_concat(ptr {acc}, ptr {piece})"
            )
            .unwrap();
            acc = next;
        }
        Ok(acc)
    }

    /// Lowers a single operand to a `ptr` SSA holding the
    /// argument's stringification. Strings pass through; numeric
    /// types route through their `gos_rt_*_to_str` helper.
    fn lower_arg_to_str_ptr(&mut self, arg: &Operand) -> Result<String, BuildError> {
        let kind = self.concat_print_kind(arg);
        if matches!(kind, ConcatKind::Unsupported) {
            return Err(BuildError::Unsupported(
                "stringify of aggregate or variant types",
            ));
        }
        let value = self.lower_operand(arg)?;
        let dest = self.fresh();
        match kind {
            ConcatKind::StrPtr => Ok(value),
            ConcatKind::Int => {
                let widened = self.widen_to_i64(arg, &value);
                writeln!(
                    self.out,
                    "  {dest} = call ptr @gos_rt_i64_to_str(i64 {widened})"
                )
                .unwrap();
                Ok(dest)
            }
            ConcatKind::Float => {
                let widened = self.widen_to_f64(arg, &value);
                writeln!(
                    self.out,
                    "  {dest} = call ptr @gos_rt_f64_to_str(double {widened})"
                )
                .unwrap();
                Ok(dest)
            }
            ConcatKind::Bool => {
                let widened = self.widen_bool_to_i32(arg, &value);
                writeln!(
                    self.out,
                    "  {dest} = call ptr @gos_rt_bool_to_str(i32 {widened})"
                )
                .unwrap();
                Ok(dest)
            }
            ConcatKind::Char => {
                let widened = self.widen_char_to_i32(arg, &value);
                writeln!(
                    self.out,
                    "  {dest} = call ptr @gos_rt_char_to_str(i32 {widened})"
                )
                .unwrap();
                Ok(dest)
            }
            ConcatKind::Unsupported => unreachable!("checked above"),
        }
    }

    /// Lowers a `__concat(...)` call by appending each arg to
    /// the runtime's thread-local concat buffer, then storing
    /// the finished string pointer in `destination`. Mirrors the
    /// Cranelift backend so `format!(...)` produces a real value
    /// the caller can store / return; the previous inline-print
    /// shortcut printed pieces eagerly and reordered output
    /// whenever a `format!` result outlived its emission point.
    fn lower_concat_call(
        &mut self,
        args: &[Operand],
        destination: &Place,
        target: Option<&gossamer_mir::BlockId>,
    ) -> Result<(), BuildError> {
        if !destination.projection.is_empty() {
            return Err(BuildError::Unsupported(
                "__concat destination cannot have projections",
            ));
        }
        writeln!(self.out, "  call void @gos_rt_concat_init()").unwrap();
        for arg in args {
            let kind = self.concat_print_kind(arg);
            if matches!(kind, ConcatKind::Unsupported) {
                return Err(BuildError::Unsupported(
                    "println/format of aggregate or variant types",
                ));
            }
            let value = self.lower_operand(arg)?;
            match kind {
                ConcatKind::StrPtr => {
                    writeln!(self.out, "  call void @gos_rt_concat_str(ptr {value})").unwrap();
                }
                ConcatKind::Int => {
                    let widened = self.widen_to_i64(arg, &value);
                    writeln!(self.out, "  call void @gos_rt_concat_i64(i64 {widened})").unwrap();
                }
                ConcatKind::Float => {
                    let widened = self.widen_to_f64(arg, &value);
                    writeln!(self.out, "  call void @gos_rt_concat_f64(double {widened})").unwrap();
                }
                ConcatKind::Bool => {
                    let widened = self.widen_bool_to_i32(arg, &value);
                    writeln!(self.out, "  call void @gos_rt_concat_bool(i32 {widened})").unwrap();
                }
                ConcatKind::Char => {
                    let widened = self.widen_char_to_i32(arg, &value);
                    writeln!(self.out, "  call void @gos_rt_concat_char(i32 {widened})").unwrap();
                }
                ConcatKind::Unsupported => unreachable!("checked above"),
            }
        }
        let result = self.fresh();
        writeln!(self.out, "  {result} = call ptr @gos_rt_concat_finish()").unwrap();
        if !is_unit(self.tcx, self.body.local_ty(destination.local)) {
            let dest_ty = render_ty(self.tcx, self.body.local_ty(destination.local));
            let slot = local_slot(destination.local);
            writeln!(self.out, "  store {dest_ty} {result}, ptr {slot}").unwrap();
        }
        match target {
            Some(t) => {
                writeln!(self.out, "  br label %bb{}", t.as_u32()).unwrap();
            }
            None => {
                writeln!(self.out, "  unreachable").unwrap();
            }
        }
        Ok(())
    }

    /// Lowers `__fmt_prec(value, prec)` as a call into
    /// `gos_rt_f64_prec_to_str`. The value is widened to `f64` and
    /// the precision to `i64` to match the runtime ABI; the returned
    /// pointer becomes the destination's String value.
    fn lower_fmt_prec_call(
        &mut self,
        args: &[Operand],
        destination: &Place,
        target: Option<&gossamer_mir::BlockId>,
    ) -> Result<(), BuildError> {
        if !destination.projection.is_empty() {
            return Err(BuildError::Unsupported(
                "__fmt_prec destination cannot have projections",
            ));
        }
        if args.len() != 2 {
            return Err(BuildError::Unsupported(
                "__fmt_prec expects exactly two arguments",
            ));
        }
        let value_raw = self.lower_operand(&args[0])?;
        let value = self.coerce_to_f64(&args[0], &value_raw);
        let prec_raw = self.lower_operand(&args[1])?;
        let prec = self.widen_to_i64(&args[1], &prec_raw);
        let result = self.fresh();
        writeln!(
            self.out,
            "  {result} = call ptr @gos_rt_f64_prec_to_str(double {value}, i64 {prec})"
        )
        .unwrap();
        if !is_unit(self.tcx, self.body.local_ty(destination.local)) {
            let dest_ty = render_ty(self.tcx, self.body.local_ty(destination.local));
            let slot = local_slot(destination.local);
            writeln!(self.out, "  store {dest_ty} {result}, ptr {slot}").unwrap();
        }
        match target {
            Some(t) => {
                writeln!(self.out, "  br label %bb{}", t.as_u32()).unwrap();
            }
            None => {
                writeln!(self.out, "  unreachable").unwrap();
            }
        }
        Ok(())
    }

    fn concat_print_kind(&self, op: &Operand) -> ConcatKind {
        match op {
            Operand::Const(ConstValue::Str(_)) => ConcatKind::StrPtr,
            Operand::Const(ConstValue::Int(_)) => ConcatKind::Int,
            Operand::Const(ConstValue::Float(_)) => ConcatKind::Float,
            Operand::Const(ConstValue::Bool(_)) => ConcatKind::Bool,
            Operand::Const(ConstValue::Char(_)) => ConcatKind::Char,
            Operand::Const(ConstValue::Unit) => ConcatKind::Int,
            Operand::Copy(p) => {
                let ty = self.unwrap_ref(self.place_leaf_ty(p));
                match self.tcx.kind(ty) {
                    Some(TyKind::Bool) => ConcatKind::Bool,
                    Some(TyKind::Char) => ConcatKind::Char,
                    Some(TyKind::Float(_)) => ConcatKind::Float,
                    Some(TyKind::String | TyKind::Ref { .. }) => ConcatKind::StrPtr,
                    Some(TyKind::Int(_) | TyKind::Unit | TyKind::Never) => ConcatKind::Int,
                    // Unresolved inference variable: the dominant
                    // producer that flows into println is
                    // `__concat`, which returns a String pointer
                    // at runtime. Default to StrPtr so
                    // `println!("a={n}")` doesn't reprint the
                    // empty-string pointer as a giant integer.
                    Some(TyKind::Var(_)) => ConcatKind::StrPtr,
                    // Aggregate / collection / variant types
                    // need a Display impl. Refuse loudly so the
                    // backend triggers fallback to Cranelift,
                    // whose bail emits the user-facing message.
                    Some(
                        TyKind::Tuple(_)
                        | TyKind::Array { .. }
                        | TyKind::Slice(_)
                        | TyKind::Vec(_)
                        | TyKind::HashMap { .. }
                        | TyKind::Sender(_)
                        | TyKind::Receiver(_)
                        | TyKind::JsonValue
                        | TyKind::Adt { .. }
                        | TyKind::Closure { .. }
                        | TyKind::FnDef { .. }
                        | TyKind::FnPtr(_)
                        | TyKind::FnTrait(_)
                        | TyKind::Dyn(_),
                    ) => ConcatKind::Unsupported,
                    Some(TyKind::Param { .. } | TyKind::Alias { .. } | TyKind::Error) | None => {
                        ConcatKind::Int
                    }
                }
            }
            Operand::FnRef { .. } => ConcatKind::Int,
        }
    }

    /// Sign- or zero-extends a value to `i64` for the Int print
    /// path. Looks at the operand's source type so a `u8` byte
    /// extends differently than an `i32`.
    fn widen_to_i64(&mut self, op: &Operand, v: &str) -> String {
        let src_llvm = self.operand_llvm_ty(op);
        if src_llvm == "i64" {
            return v.to_string();
        }
        let signed = match op {
            Operand::Copy(p) => {
                let ty = self.unwrap_ref(self.place_leaf_ty(p));
                matches!(self.tcx.kind(ty), Some(TyKind::Int(i)) if int_signed(*i))
                    || !matches!(self.tcx.kind(ty), Some(TyKind::Int(_)))
            }
            _ => true,
        };
        let tmp = self.fresh();
        let instr = if signed { "sext" } else { "zext" };
        writeln!(self.out, "  {tmp} = {instr} {src_llvm} {v} to i64").unwrap();
        tmp
    }

    fn widen_to_f64(&mut self, op: &Operand, v: &str) -> String {
        let src_llvm = self.operand_llvm_ty(op);
        if src_llvm == "double" {
            return v.to_string();
        }
        if src_llvm == "float" {
            let tmp = self.fresh();
            writeln!(self.out, "  {tmp} = fpext float {v} to double").unwrap();
            return tmp;
        }
        v.to_string()
    }

    /// Like [`widen_to_f64`] but also converts integer operands via
    /// `sitofp`. Used by `__fmt_prec`, which accepts a numeric value
    /// regardless of MIR type and renders it as a float.
    fn coerce_to_f64(&mut self, op: &Operand, v: &str) -> String {
        let src_llvm = self.operand_llvm_ty(op);
        match src_llvm.as_str() {
            "double" => v.to_string(),
            "float" => {
                let tmp = self.fresh();
                writeln!(self.out, "  {tmp} = fpext float {v} to double").unwrap();
                tmp
            }
            "i1" | "i8" | "i16" | "i32" | "i64" => {
                let tmp = self.fresh();
                writeln!(self.out, "  {tmp} = sitofp {src_llvm} {v} to double").unwrap();
                tmp
            }
            _ => v.to_string(),
        }
    }

    fn widen_bool_to_i32(&mut self, op: &Operand, v: &str) -> String {
        let src_llvm = self.operand_llvm_ty(op);
        if src_llvm == "i32" {
            return v.to_string();
        }
        let tmp = self.fresh();
        if src_llvm == "i1" || src_llvm == "i8" || src_llvm == "i16" {
            writeln!(self.out, "  {tmp} = zext {src_llvm} {v} to i32").unwrap();
        } else if src_llvm == "i64" {
            writeln!(self.out, "  {tmp} = trunc i64 {v} to i32").unwrap();
        } else {
            return v.to_string();
        }
        tmp
    }

    fn widen_char_to_i32(&mut self, op: &Operand, v: &str) -> String {
        let src_llvm = self.operand_llvm_ty(op);
        if src_llvm == "i32" {
            return v.to_string();
        }
        let tmp = self.fresh();
        if src_llvm == "i64" {
            writeln!(self.out, "  {tmp} = trunc i64 {v} to i32").unwrap();
        } else if src_llvm == "i8" || src_llvm == "i16" {
            writeln!(self.out, "  {tmp} = zext {src_llvm} {v} to i32").unwrap();
        } else {
            return v.to_string();
        }
        tmp
    }

    /// Direct-call lowering for `Operand::FnRef` and
    /// simple prelude-name calls (the MIR lowerer leaves
    /// prelude targets as `ConstValue::Str("println")` etc.).
    /// Closure indirect calls aren't covered yet.
    fn lower_call(
        &mut self,
        callee: &Operand,
        args: &[Operand],
        destination: &Place,
        target: Option<&gossamer_mir::BlockId>,
    ) -> Result<(), BuildError> {
        if !destination.projection.is_empty() {
            return Err(BuildError::Unsupported("call with projected destination"));
        }
        let target_name: Option<String> = match callee {
            Operand::FnRef { def, .. } => {
                // Resolve through the per-module
                // `DefId.local` → name map populated by the
                // emitter. Unknown `def.local` means the
                // referenced function isn't in this MIR
                // module — could be a stdlib function the
                // frontend is expected to monomorphise, but
                // didn't. Fall back to unsupported so the
                // driver routes this body to Cranelift.
                self.fn_name_by_def.get(&def.local).cloned()
            }
            Operand::Const(ConstValue::Str(name)) => Some(name.clone()),
            Operand::Copy(place) => {
                // `Copy(local)` callee: indirect call through a
                // function pointer. Two shapes:
                //   1. `FnDef`/`FnPtr` typed local — the value
                //      *is* the callable address.
                //   2. Closure env pointer — first heap word is
                //      the function pointer; env doubles as the
                //      implicit first argument.
                // Mirror Cranelift's `call_indirect` handling.
                self.lower_indirect_call(place, args, destination, target)?;
                return Ok(());
            }
            Operand::Const(_) => None,
        };
        let Some(name) = target_name else {
            return Err(BuildError::Unsupported(
                "indirect / closure call not yet lowered",
            ));
        };
        // `__concat` is the parser's lowering of `println!`-style
        // formatted output: it takes a heterogeneous arg list,
        // prints each piece directly to stdout, and produces an
        // empty-string pointer for the surrounding `println` call
        // to consume. Mirror the Cranelift backend's per-arg
        // dispatch (one runtime print call per operand keyed off
        // the operand's MIR kind).
        if name == "__concat" {
            self.lower_concat_call(args, destination, target)?;
            return Ok(());
        }
        // `__fmt_prec(value, prec)` — emitted by macro expansion for
        // `{:.N}` specs. Routes through `gos_rt_f64_prec_to_str` so
        // the result is a heap String that the surrounding `__concat`
        // pipeline consumes like any other string operand.
        if name == "__fmt_prec" {
            self.lower_fmt_prec_call(args, destination, target)?;
            return Ok(());
        }
        // `println` / `print` / `eprintln` / `eprint` route to
        // the runtime's `gos_rt_print_str` for each arg, plus a
        // trailing `gos_rt_println()` for the `*ln` variants.
        // This mirrors what the Cranelift backend does in
        // `lower_intrinsic_call` — the runtime's println is
        // arity-0, so an inline `gos_rt_print_str(arg)` then
        // `gos_rt_println()` reproduces the user-level
        // `println(s)` semantics.
        if matches!(name.as_str(), "println" | "print" | "eprintln" | "eprint") {
            self.lower_print_call(&name, args, destination, target)?;
            return Ok(());
        }
        // `panic(args...)` builds a single concatenated message
        // (space-joined to mirror the interpreter's
        // `render_args`) and routes it through `gos_rt_panic`,
        // which is `noreturn` and emits the GX0005 prefix +
        // aborts. Fall back to an empty-string pointer when no
        // args were given.
        if name == "panic" {
            // `panic(args...)` builds a single space-joined
            // message via `gos_rt_str_concat` over the
            // per-arg `to_str` helpers, then calls
            // `gos_rt_panic` (noreturn). Empty arg list panics
            // with an empty message — `gos_rt_panic` then
            // emits its default "panic" string.
            let msg = self.emit_args_to_concat_string(args, " ")?;
            writeln!(self.out, "  call void @gos_rt_panic(ptr {msg})").unwrap();
            writeln!(self.out, "  unreachable").unwrap();
            return Ok(());
        }
        // Recognise `math::*` calls and emit a direct
        // LLVM intrinsic invocation instead of routing
        // through an undefined `@"math::sqrt"` symbol. These
        // lower to the host's SSE/AVX instruction via `llc`.
        if let Some(intrinsic_name) = math_intrinsic(&name)
            && args.len() == 1
        {
            self.lower_math_intrinsic(intrinsic_name, &args[0], destination, target)?;
            return Ok(());
        }
        // Hot path inlining for byte-at-a-time stdout writes.
        // The runtime exposes the stdout buffer as a pair of
        // global symbols (`@GOS_RT_STDOUT_BYTES` / `@GOS_RT_STDOUT_LEN`);
        // we emit the buffer-append fast path directly so that
        // fasta-style inner loops (50M+ calls) don't pay one
        // FFI call per byte. Only the slow path (full buffer)
        // falls through to `gos_rt_stream_write_byte`.
        if name == "gos_rt_stream_write_byte" && args.len() == 2 {
            self.lower_stream_write_byte_inline(args, destination, target)?;
            return Ok(());
        }
        // Bulk byte-array write for stdout: pack `len` low-bytes
        // of an `[i64; N]` array into the global stdout buffer
        // inline. The fasta_block / fasta_mt programs call
        // `out.write_byte_array(&line, line_len + 1)` once per
        // 60-char line; without inlining each line pays one
        // FFI call. With inlining the loop bound is usually
        // a compile-time-known small integer, so LLVM unrolls
        // and the per-byte pack drops to one `mov` + `inc`.
        if name == "gos_rt_stream_write_byte_array" && args.len() == 3 {
            self.lower_stream_write_byte_array_inline(args, destination, target)?;
            return Ok(());
        }
        // Hot path inlining for `s[i]` byte reads. Strings are
        // null-terminated `*const u8` in the runtime; the
        // out-of-bounds case (which would need `strlen`) costs
        // O(strlen) per access for fasta-style `alu[idx %
        // alu_len]` loops, so we inline the simple `addr+i`
        // load assuming the user's modulus keeps `i` in range.
        if name == "gos_rt_str_byte_at" && args.len() == 2 {
            self.lower_str_byte_at_inline(args, destination, target)?;
            return Ok(());
        }
        // Hot path inlining for `s.len()` on strings. Lowers
        // to `i64 @strlen(ptr s)` (a libc call LLVM
        // constant-folds for compile-time-known string
        // constants). With the constant in hand, modulus
        // operations like `idx % alu_len` reduce to
        // multiply-by-magic instead of `idiv`, which dominates
        // the fasta inner loop.
        if name == "gos_rt_str_len" && args.len() == 1 {
            self.lower_str_len_inline(&args[0], destination, target)?;
            return Ok(());
        }
        // Heap-Vec inline fast paths. The runtime returns a
        // `*mut GosI64Vec { len: i64, data: *mut i64 }`; user
        // code accesses elements via `vec.set_at(i, v)` and
        // `vec.get_at(i)`. Without inlining each access pays
        // one FFI call (~5-20 ns), which dominates the
        // multi-threaded fasta hot loop. The inline shape
        // skips the runtime's bounds check (caller is
        // expected to keep `i` in range, same convention as
        // `str_byte_at`).
        if name == "gos_rt_heap_i64_set" && args.len() == 3 {
            self.lower_heap_i64_set_inline(args, destination, target)?;
            return Ok(());
        }
        if name == "gos_rt_heap_i64_get" && args.len() == 2 {
            self.lower_heap_i64_get_inline(args, destination, target)?;
            return Ok(());
        }
        // Raw heap intrinsics that the cranelift tier handles
        // inline. Lower them to a direct LLVM `getelementptr +
        // load/store` here so the LLVM tier doesn't bail and
        // route the body to cranelift just for these calls.
        if matches!(
            name.as_str(),
            "gos_load" | "gos_store" | "gos_alloc" | "gos_fn_addr"
        ) {
            self.lower_raw_intrinsic(&name, args, destination, target)?;
            return Ok(());
        }
        // Variant constructor stubs: `Ok(v)`, `Some(v)`, `Err(e)`
        // pass the wrapped value through unchanged (the compiled
        // tier flattens Option/Result, so `unwrap` is identity).
        // `None` and other no-payload variants resolve to a zero
        // value. Mirrors the Cranelift backend's variant-stub
        // branch so escaped Result/Option values don't end up
        // calling a non-existent `@"Ok"` symbol at link time.
        let is_variant_stub = matches!(name.as_str(), "Ok" | "Some" | "Err" | "None")
            || (name.chars().next().is_some_and(|c| c.is_ascii_uppercase())
                && !name.contains("::"));
        if is_variant_stub {
            self.emit_variant_stub(&name, args, destination, target)?;
            return Ok(());
        }
        let symbol = map_prelude_symbol(&name);
        self.emit_named_call(symbol, args, destination, target)
    }

    /// Lowers the raw-pointer intrinsics (`gos_load`,
    /// `gos_store`, `gos_alloc`, `gos_fn_addr`) directly to
    /// LLVM IR so the LLVM tier doesn't have to fall back to
    /// cranelift for closure envs / vec iteration / fn-pointer
    /// trampolines. Mirrors the cranelift-side handlers in
    /// `lower_intrinsic_outcome`.
    fn lower_raw_intrinsic(
        &mut self,
        name: &str,
        args: &[Operand],
        destination: &Place,
        target: Option<&gossamer_mir::BlockId>,
    ) -> Result<(), BuildError> {
        let dest_ty_mir = self.body.local_ty(destination.local);
        let dest_ty = render_ty(self.tcx, dest_ty_mir);
        match name {
            "gos_load" => {
                // gos_load(ptr_i64, offset_i64) -> i64
                if args.len() < 2 {
                    return Err(BuildError::Unsupported("gos_load arity"));
                }
                let ptr_v = self.lower_operand(&args[0])?;
                let off_v = self.lower_operand(&args[1])?;
                let ptr_ty = self.operand_llvm_ty(&args[0]);
                let off_ty = self.operand_llvm_ty(&args[1]);
                // ptr might be i64 or ptr; coerce to ptr.
                let p = if ptr_ty == "ptr" {
                    ptr_v
                } else {
                    let tmp = self.fresh();
                    writeln!(self.out, "  {tmp} = inttoptr {ptr_ty} {ptr_v} to ptr").unwrap();
                    tmp
                };
                // gep i8, p, off → addr
                let off64 = if off_ty == "i64" {
                    off_v
                } else {
                    let tmp = self.fresh();
                    writeln!(self.out, "  {tmp} = sext {off_ty} {off_v} to i64").unwrap();
                    tmp
                };
                let addr = self.fresh();
                writeln!(
                    self.out,
                    "  {addr} = getelementptr i8, ptr {p}, i64 {off64}"
                )
                .unwrap();
                let loaded = self.fresh();
                writeln!(self.out, "  {loaded} = load i64, ptr {addr}").unwrap();
                let coerced = self.coerce_llvm_value(&loaded, "i64", &dest_ty);
                let slot = local_slot(destination.local);
                writeln!(self.out, "  store {dest_ty} {coerced}, ptr {slot}").unwrap();
            }
            "gos_store" => {
                // gos_store(ptr, offset, value) — writes 8 bytes.
                if args.len() < 3 {
                    return Err(BuildError::Unsupported("gos_store arity"));
                }
                let ptr_v = self.lower_operand(&args[0])?;
                let off_v = self.lower_operand(&args[1])?;
                let val_v = self.lower_operand(&args[2])?;
                let ptr_ty = self.operand_llvm_ty(&args[0]);
                let off_ty = self.operand_llvm_ty(&args[1]);
                let val_ty = self.operand_llvm_ty(&args[2]);
                let p = if ptr_ty == "ptr" {
                    ptr_v
                } else {
                    let tmp = self.fresh();
                    writeln!(self.out, "  {tmp} = inttoptr {ptr_ty} {ptr_v} to ptr").unwrap();
                    tmp
                };
                let off64 = if off_ty == "i64" {
                    off_v
                } else {
                    let tmp = self.fresh();
                    writeln!(self.out, "  {tmp} = sext {off_ty} {off_v} to i64").unwrap();
                    tmp
                };
                let val64 = self.coerce_llvm_value(&val_v, &val_ty, "i64");
                let addr = self.fresh();
                writeln!(
                    self.out,
                    "  {addr} = getelementptr i8, ptr {p}, i64 {off64}"
                )
                .unwrap();
                writeln!(self.out, "  store i64 {val64}, ptr {addr}").unwrap();
                if dest_ty != "void" && !is_unit(self.tcx, dest_ty_mir) {
                    let slot = local_slot(destination.local);
                    writeln!(self.out, "  store {dest_ty} 0, ptr {slot}").unwrap();
                }
            }
            "gos_alloc" => {
                // gos_alloc(size_i64) -> ptr (via libc malloc).
                let size_v = if args.is_empty() {
                    "0".to_string()
                } else {
                    let v = self.lower_operand(&args[0])?;
                    let t = self.operand_llvm_ty(&args[0]);
                    if t == "i64" {
                        v
                    } else {
                        let tmp = self.fresh();
                        writeln!(self.out, "  {tmp} = sext {t} {v} to i64").unwrap();
                        tmp
                    }
                };
                let tmp = self.fresh();
                writeln!(self.out, "  {tmp} = call ptr @malloc(i64 {size_v})").unwrap();
                let coerced = self.coerce_llvm_value(&tmp, "ptr", &dest_ty);
                let slot = local_slot(destination.local);
                writeln!(self.out, "  store {dest_ty} {coerced}, ptr {slot}").unwrap();
            }
            "gos_fn_addr" => {
                // gos_fn_addr("name") -> ptr to that function.
                let Some(Operand::Const(ConstValue::Str(fname))) = args.first() else {
                    return Err(BuildError::Unsupported("gos_fn_addr arg"));
                };
                // LLVM IR pointer-to-function constants are written
                // as the function symbol itself; declare-only is OK
                // because the cranelift companion (or another LLVM
                // body) provides the definition.
                let tmp = self.fresh();
                writeln!(self.out, "  {tmp} = bitcast ptr @\"{fname}\" to ptr").unwrap();
                let coerced = self.coerce_llvm_value(&tmp, "ptr", &dest_ty);
                let slot = local_slot(destination.local);
                writeln!(self.out, "  store {dest_ty} {coerced}, ptr {slot}").unwrap();
            }
            _ => {
                return Err(BuildError::Unsupported("unrecognised raw intrinsic"));
            }
        }
        emit_terminator_branch(&mut self.out, target);
        Ok(())
    }

    /// Emits the result of a variant constructor call without
    /// going through a real function symbol. `Ok(v)`, `Some(v)`,
    /// and `Err(e)` write the inner value to the destination;
    /// payload-less variants write zero. Coerces the inner
    /// value's LLVM type to the destination's slot type so the
    /// emitted store is well-formed even when the wrapper Adt
    /// renders as `ptr` and the inner value is a plain `i64` /
    /// `double`.
    fn emit_variant_stub(
        &mut self,
        name: &str,
        args: &[Operand],
        destination: &Place,
        target: Option<&gossamer_mir::BlockId>,
    ) -> Result<(), BuildError> {
        let dest_ty_mir = self.body.local_ty(destination.local);
        let dest_ty = render_ty(self.tcx, dest_ty_mir);
        if dest_ty == "void" || is_unit(self.tcx, dest_ty_mir) {
            emit_terminator_branch(&mut self.out, target);
            return Ok(());
        }
        let (value, value_ty): (String, String) =
            if matches!(name, "Ok" | "Some" | "Err") && !args.is_empty() {
                let v = self.lower_operand(&args[0])?;
                let vt = self.operand_llvm_ty(&args[0]);
                (v, vt)
            } else {
                let zero = match dest_ty.as_str() {
                    "ptr" => "null".to_string(),
                    "double" | "float" => "0.0".to_string(),
                    _ => "0".to_string(),
                };
                (zero, dest_ty.clone())
            };
        let coerced = self.coerce_llvm_value(&value, &value_ty, &dest_ty);
        let slot = local_slot(destination.local);
        writeln!(self.out, "  store {dest_ty} {coerced}, ptr {slot}").unwrap();
        emit_terminator_branch(&mut self.out, target);
        Ok(())
    }

    /// Inserts the LLVM cast that brings `value` (of type
    /// `from_ty`) over to `to_ty`, returning the new SSA name.
    /// No-op when the types already match. Handles the common
    /// scalar-to-pointer / pointer-to-scalar / int-width and
    /// float-width permutations the variant-stub path needs.
    fn coerce_llvm_value(&mut self, value: &str, from_ty: &str, to_ty: &str) -> String {
        if from_ty == to_ty {
            return value.to_string();
        }
        let tmp = self.fresh();
        let op = match (from_ty, to_ty) {
            ("ptr", _) if to_ty.starts_with('i') => "ptrtoint",
            (_, "ptr") if from_ty.starts_with('i') => "inttoptr",
            ("ptr", "double") => {
                // Through i64 — LLVM has no direct ptr→double.
                let mid = self.fresh();
                writeln!(self.out, "  {mid} = ptrtoint ptr {value} to i64").unwrap();
                writeln!(self.out, "  {tmp} = bitcast i64 {mid} to double").unwrap();
                return tmp;
            }
            ("double", "ptr") => {
                let mid = self.fresh();
                writeln!(self.out, "  {mid} = bitcast double {value} to i64").unwrap();
                writeln!(self.out, "  {tmp} = inttoptr i64 {mid} to ptr").unwrap();
                return tmp;
            }
            _ if from_ty.starts_with('i') && to_ty.starts_with('i') => {
                let from_w: u32 = from_ty[1..].parse().unwrap_or(64);
                let to_w: u32 = to_ty[1..].parse().unwrap_or(64);
                if to_w > from_w {
                    "zext"
                } else if to_w < from_w {
                    "trunc"
                } else {
                    return value.to_string();
                }
            }
            _ => "bitcast",
        };
        writeln!(self.out, "  {tmp} = {op} {from_ty} {value} to {to_ty}").unwrap();
        tmp
    }

    /// Inline fast path for `gos_rt_stream_write_byte(stream, b)`.
    ///
    /// The stdout case dominates the fasta benchmark (50M+
    /// calls). Going through an FFI call for every byte spends
    /// hundreds of millions of nanoseconds in PLT + stack-frame
    /// setup alone. Inlining the buffer-append (load len,
    /// bounds check, store byte, increment len) cuts those
    /// hot-loop calls down to ~5 instructions each.
    ///
    /// Shape:
    /// ```llvm
    ///   %fd = load i32, ptr %stream
    ///   %is_stdout = icmp eq i32 %fd, 1
    ///   br i1 %is_stdout, label %fast_check, label %slow
    /// fast_check:
    ///   %len = load i64, ptr @GOS_RT_STDOUT_LEN
    ///   %full = icmp uge i64 %len, 8192
    ///   br i1 %full, label %slow, label %append
    /// append:
    ///   %dst = getelementptr i8, ptr @GOS_RT_STDOUT_BYTES, i64 %len
    ///   %byte = trunc i64 %b to i8
    ///   store i8 %byte, ptr %dst
    ///   %newlen = add i64 %len, 1
    ///   store i64 %newlen, ptr @GOS_RT_STDOUT_LEN
    ///   br label %end
    /// slow:
    ///   call void @gos_rt_stream_write_byte_slow(ptr %stream, i64 %b)
    ///   br label %end
    /// end:
    /// ```
    fn lower_stream_write_byte_inline(
        &mut self,
        args: &[Operand],
        destination: &Place,
        target: Option<&gossamer_mir::BlockId>,
    ) -> Result<(), BuildError> {
        let stream_v = self.lower_operand(&args[0])?;
        let byte_v = self.lower_operand(&args[1])?;
        // Suffix to keep block labels unique within a function.
        let suffix = self.next_ssa;
        self.next_ssa += 1;
        let fast_check = format!("wb_check_{suffix}");
        let append = format!("wb_append_{suffix}");
        let slow = format!("wb_slow_{suffix}");
        let end = format!("wb_end_{suffix}");

        // Read fd and route stdout (fd==1) to the fast path.
        // `!invariant.load` tells LLVM the fd field of a stream
        // never changes after construction (every stream the
        // runtime exposes is a static `&STREAM_*`), so the load
        // can be hoisted out of containing loops. Without the
        // hint LLVM keeps a per-iteration `cmpl $1, (%stream)`
        // which is the hot path of fasta's inner loop.
        let fd = self.fresh();
        writeln!(
            self.out,
            "  {fd} = load i32, ptr {stream_v}, !invariant.load !0"
        )
        .unwrap();
        let is_stdout = self.fresh();
        writeln!(self.out, "  {is_stdout} = icmp eq i32 {fd}, 1").unwrap();
        writeln!(
            self.out,
            "  br i1 {is_stdout}, label %{fast_check}, label %{slow}"
        )
        .unwrap();

        // fast_check: bounds-check the buffer. Take the
        // process-global stdout lock first so this thread's
        // load+store on `@GOS_RT_STDOUT_LEN` cannot tear against
        // a concurrent goroutine on another worker thread.
        // `gos_rt_stdout_acquire` / `_release` wrap a
        // `parking_lot::RawMutex`; uncontended cost is ~10 ns.
        writeln!(self.out, "{fast_check}:").unwrap();
        writeln!(self.out, "  call void @gos_rt_stdout_acquire()").unwrap();
        let len = self.fresh();
        writeln!(self.out, "  {len} = load i64, ptr @GOS_RT_STDOUT_LEN").unwrap();
        let full = self.fresh();
        writeln!(self.out, "  {full} = icmp uge i64 {len}, 8192").unwrap();
        // On overflow we still hold the lock — release before
        // routing to the slow call path so the slow path can
        // re-acquire through the safe Rust guard.
        let full_release = format!("wb_full_rel_{suffix}");
        writeln!(
            self.out,
            "  br i1 {full}, label %{full_release}, label %{append}"
        )
        .unwrap();
        writeln!(self.out, "{full_release}:").unwrap();
        writeln!(self.out, "  call void @gos_rt_stdout_release()").unwrap();
        writeln!(self.out, "  br label %{slow}").unwrap();

        // append: store the byte at bytes[len], bump len, release.
        writeln!(self.out, "{append}:").unwrap();
        let dst = self.fresh();
        writeln!(
            self.out,
            "  {dst} = getelementptr i8, ptr @GOS_RT_STDOUT_BYTES, i64 {len}"
        )
        .unwrap();
        let byte_8 = self.fresh();
        writeln!(self.out, "  {byte_8} = trunc i64 {byte_v} to i8").unwrap();
        writeln!(self.out, "  store i8 {byte_8}, ptr {dst}").unwrap();
        let newlen = self.fresh();
        writeln!(self.out, "  {newlen} = add i64 {len}, 1").unwrap();
        writeln!(self.out, "  store i64 {newlen}, ptr @GOS_RT_STDOUT_LEN").unwrap();
        writeln!(self.out, "  call void @gos_rt_stdout_release()").unwrap();
        writeln!(self.out, "  br label %{end}").unwrap();

        // slow: full-call path. The runtime helper acquires the
        // lock itself through the safe `StdoutGuard`.
        writeln!(self.out, "{slow}:").unwrap();
        writeln!(
            self.out,
            "  call void @gos_rt_stream_write_byte(ptr {stream_v}, i64 {byte_v})"
        )
        .unwrap();
        writeln!(self.out, "  br label %{end}").unwrap();

        // Merge.
        writeln!(self.out, "{end}:").unwrap();
        // Destination is `()`; nothing to store.
        let _ = destination;
        match target {
            Some(t) => writeln!(self.out, "  br label %bb{}", t.as_u32()).unwrap(),
            None => writeln!(self.out, "  unreachable").unwrap(),
        }
        Ok(())
    }

    /// Inline fast path for `gos_rt_heap_i64_set(v, idx,
    /// val)`. The `GosI64Vec` is laid out as
    /// `{ i64 len; ptr data }` (8-byte aligned); we load
    /// `data` from offset 8, index it by `idx`, store `val`.
    /// Skips bounds checks — user code is expected to keep
    /// `idx` in range.
    fn lower_heap_i64_set_inline(
        &mut self,
        args: &[Operand],
        destination: &Place,
        target: Option<&gossamer_mir::BlockId>,
    ) -> Result<(), BuildError> {
        let v = self.lower_operand(&args[0])?;
        let idx = self.lower_operand(&args[1])?;
        let val = self.lower_operand(&args[2])?;
        let data_ptr_addr = self.fresh();
        writeln!(
            self.out,
            "  {data_ptr_addr} = getelementptr i8, ptr {v}, i64 8"
        )
        .unwrap();
        let data = self.fresh();
        writeln!(self.out, "  {data} = load ptr, ptr {data_ptr_addr}").unwrap();
        let dst = self.fresh();
        writeln!(
            self.out,
            "  {dst} = getelementptr i64, ptr {data}, i64 {idx}"
        )
        .unwrap();
        writeln!(self.out, "  store i64 {val}, ptr {dst}").unwrap();
        let _ = destination;
        emit_terminator_branch(&mut self.out, target);
        Ok(())
    }

    /// Inline fast path for `gos_rt_heap_i64_get(v, idx) ->
    /// i64`. Mirror of `lower_heap_i64_set_inline`.
    fn lower_heap_i64_get_inline(
        &mut self,
        args: &[Operand],
        destination: &Place,
        target: Option<&gossamer_mir::BlockId>,
    ) -> Result<(), BuildError> {
        let v = self.lower_operand(&args[0])?;
        let idx = self.lower_operand(&args[1])?;
        let data_ptr_addr = self.fresh();
        writeln!(
            self.out,
            "  {data_ptr_addr} = getelementptr i8, ptr {v}, i64 8"
        )
        .unwrap();
        let data = self.fresh();
        writeln!(self.out, "  {data} = load ptr, ptr {data_ptr_addr}").unwrap();
        let src = self.fresh();
        writeln!(
            self.out,
            "  {src} = getelementptr i64, ptr {data}, i64 {idx}"
        )
        .unwrap();
        let val = self.fresh();
        writeln!(self.out, "  {val} = load i64, ptr {src}").unwrap();
        if !is_unit(self.tcx, self.body.local_ty(destination.local)) {
            let slot = local_slot(destination.local);
            writeln!(self.out, "  store i64 {val}, ptr {slot}").unwrap();
        }
        emit_terminator_branch(&mut self.out, target);
        Ok(())
    }

    /// Inline fast path for `gos_rt_str_len(s) -> i64`. Strings
    /// are null-terminated, so the length is `strlen(s)` —
    /// LLVM has a builtin `@strlen` that constant-folds against
    /// rodata literals. Folding is critical because user code
    /// like `let alu_len = alu.len()` becomes a compile-time
    /// constant, which collapses every `idx % alu_len` modulus
    /// in the hot loop from a real `idiv` (~20-40 cycles) to a
    /// multiply-by-magic.
    fn lower_str_len_inline(
        &mut self,
        arg: &Operand,
        destination: &Place,
        target: Option<&gossamer_mir::BlockId>,
    ) -> Result<(), BuildError> {
        let s_v = self.lower_operand(arg)?;
        // `strlen` returns `size_t` (assumed 64-bit on the
        // targets we care about). Declare it once at the
        // module level via the runtime-refs set.
        self.runtime_refs
            .insert("declare i64 @strlen(ptr)".to_string());
        let tmp = self.fresh();
        writeln!(self.out, "  {tmp} = call i64 @strlen(ptr {s_v})").unwrap();
        if !is_unit(self.tcx, self.body.local_ty(destination.local)) {
            let slot = local_slot(destination.local);
            writeln!(self.out, "  store i64 {tmp}, ptr {slot}").unwrap();
        }
        emit_terminator_branch(&mut self.out, target);
        Ok(())
    }

    /// Inline fast path for
    /// `gos_rt_stream_write_byte_array(stream, arr, len)`.
    ///
    /// Pack the low byte of every i64 slot in `arr[..len]`
    /// directly into the stdout buffer. The stream-fd check is
    /// hoisted (via `!invariant.load !0`); when we know the fd
    /// is 1 we drop into a tight pack loop that LLVM unrolls
    /// when `len` is compile-time-known. For the fasta_block /
    /// fasta_mt programs `len` is `line_len + 1` ≤ 61 and the
    /// buffer is rarely full, so the slow path almost never
    /// fires.
    ///
    /// Layout summary:
    /// ```llvm
    ///   %fd = load i32, ptr %stream, !invariant.load !0
    ///   %is_stdout = icmp eq i32 %fd, 1
    ///   br i1 %is_stdout, label %fast_check, label %slow_call
    /// fast_check:
    ///   %len = load i64, ptr @GOS_RT_STDOUT_LEN
    ///   %sum = add i64 %len, %wlen
    ///   %fits = icmp ule i64 %sum, 8192
    ///   br i1 %fits, label %pack, label %slow_call
    /// pack:
    ///   %i = phi i64 [0, %fast_check], [%inext, %pack_body]
    ///   %done = icmp uge i64 %i, %wlen
    ///   br i1 %done, label %store_len, label %pack_body
    /// pack_body:
    ///   %src = getelementptr i64, ptr %arr, i64 %i
    ///   %v = load i64, ptr %src
    ///   %byte = trunc i64 %v to i8
    ///   %dst = getelementptr i8, ptr @GOS_RT_STDOUT_BYTES, i64 %newlen
    ///   store i8 %byte, ptr %dst
    ///   ; loop
    /// store_len:
    ///   store i64 %sum, ptr @GOS_RT_STDOUT_LEN
    ///   br label %end
    /// slow_call:
    ///   call void @gos_rt_stream_write_byte_array(...)
    ///   br label %end
    /// end:
    /// ```
    fn lower_stream_write_byte_array_inline(
        &mut self,
        args: &[Operand],
        destination: &Place,
        target: Option<&gossamer_mir::BlockId>,
    ) -> Result<(), BuildError> {
        let stream_v = self.lower_operand(&args[0])?;
        let arr_v = self.lower_operand(&args[1])?;
        let len_v = self.lower_operand(&args[2])?;
        let suffix = self.next_ssa;
        self.next_ssa += 1;
        let fast_check = format!("wba_check_{suffix}");
        let pack_header = format!("wba_pack_{suffix}");
        let pack_body = format!("wba_body_{suffix}");
        let store_len_lbl = format!("wba_store_{suffix}");
        let slow = format!("wba_slow_{suffix}");
        let end = format!("wba_end_{suffix}");

        // fd check
        let fd = self.fresh();
        writeln!(
            self.out,
            "  {fd} = load i32, ptr {stream_v}, !invariant.load !0"
        )
        .unwrap();
        let is_stdout = self.fresh();
        writeln!(self.out, "  {is_stdout} = icmp eq i32 {fd}, 1").unwrap();
        writeln!(
            self.out,
            "  br i1 {is_stdout}, label %{fast_check}, label %{slow}"
        )
        .unwrap();

        // Capacity check. Acquire the stdout lock before the
        // `LEN` load so the read + `LEN` store on the inline
        // path are atomic with respect to other goroutines.
        // The lock is released along every exit (store_len,
        // and the slow-call branch).
        writeln!(self.out, "{fast_check}:").unwrap();
        writeln!(self.out, "  call void @gos_rt_stdout_acquire()").unwrap();
        let cur_len = self.fresh();
        writeln!(self.out, "  {cur_len} = load i64, ptr @GOS_RT_STDOUT_LEN").unwrap();
        let new_len = self.fresh();
        writeln!(self.out, "  {new_len} = add i64 {cur_len}, {len_v}").unwrap();
        let fits = self.fresh();
        writeln!(self.out, "  {fits} = icmp ule i64 {new_len}, 8192").unwrap();
        let fits_release = format!("wba_nofit_rel_{suffix}");
        writeln!(
            self.out,
            "  br i1 {fits}, label %{pack_header}, label %{fits_release}"
        )
        .unwrap();
        writeln!(self.out, "{fits_release}:").unwrap();
        writeln!(self.out, "  call void @gos_rt_stdout_release()").unwrap();
        writeln!(self.out, "  br label %{slow}").unwrap();

        // Pack loop header (PHI for the loop counter).
        writeln!(self.out, "{pack_header}:").unwrap();
        let i_phi = self.fresh();
        writeln!(
            self.out,
            "  {i_phi} = phi i64 [ 0, %{fast_check} ], [ %t_inext_{suffix}, %{pack_body} ]",
        )
        .unwrap();
        let done = self.fresh();
        writeln!(self.out, "  {done} = icmp uge i64 {i_phi}, {len_v}").unwrap();
        writeln!(
            self.out,
            "  br i1 {done}, label %{store_len_lbl}, label %{pack_body}"
        )
        .unwrap();

        // Pack body — read arr[i], pack into buf[cur_len + i].
        writeln!(self.out, "{pack_body}:").unwrap();
        let src = self.fresh();
        writeln!(
            self.out,
            "  {src} = getelementptr i64, ptr {arr_v}, i64 {i_phi}"
        )
        .unwrap();
        let raw = self.fresh();
        writeln!(self.out, "  {raw} = load i64, ptr {src}").unwrap();
        let byte = self.fresh();
        writeln!(self.out, "  {byte} = trunc i64 {raw} to i8").unwrap();
        let dst_off = self.fresh();
        writeln!(self.out, "  {dst_off} = add i64 {cur_len}, {i_phi}").unwrap();
        let dst = self.fresh();
        writeln!(
            self.out,
            "  {dst} = getelementptr i8, ptr @GOS_RT_STDOUT_BYTES, i64 {dst_off}"
        )
        .unwrap();
        writeln!(self.out, "  store i8 {byte}, ptr {dst}").unwrap();
        // increment counter — must use the exact name we
        // forward-referenced in the PHI above.
        writeln!(self.out, "  %t_inext_{suffix} = add i64 {i_phi}, 1").unwrap();
        writeln!(self.out, "  br label %{pack_header}").unwrap();

        // Store the new length once we've packed the whole block,
        // then release the stdout lock acquired in fast_check.
        writeln!(self.out, "{store_len_lbl}:").unwrap();
        writeln!(self.out, "  store i64 {new_len}, ptr @GOS_RT_STDOUT_LEN").unwrap();
        writeln!(self.out, "  call void @gos_rt_stdout_release()").unwrap();
        writeln!(self.out, "  br label %{end}").unwrap();

        // Slow path: fall back to the runtime helper.
        writeln!(self.out, "{slow}:").unwrap();
        writeln!(
            self.out,
            "  call void @gos_rt_stream_write_byte_array(ptr {stream_v}, ptr {arr_v}, i64 {len_v})"
        )
        .unwrap();
        writeln!(self.out, "  br label %{end}").unwrap();

        // End — destination is `()`; nothing to store.
        writeln!(self.out, "{end}:").unwrap();
        let _ = destination;
        match target {
            Some(t) => writeln!(self.out, "  br label %bb{}", t.as_u32()).unwrap(),
            None => writeln!(self.out, "  unreachable").unwrap(),
        }
        Ok(())
    }

    /// Inline fast path for `gos_rt_str_byte_at(s, i) -> i64`.
    ///
    /// The bytecode is `*((s as *const u8) + i)` zero-extended
    /// to i64. We skip the runtime's null check since the
    /// caller already validated that the string handle is
    /// non-null at construction; null pointers will segfault
    /// rather than silently returning 0, but that matches
    /// every other byte-load path in the language.
    fn lower_str_byte_at_inline(
        &mut self,
        args: &[Operand],
        destination: &Place,
        target: Option<&gossamer_mir::BlockId>,
    ) -> Result<(), BuildError> {
        let s_v = self.lower_operand(&args[0])?;
        let i_v = self.lower_operand(&args[1])?;
        let addr = self.fresh();
        writeln!(
            self.out,
            "  {addr} = getelementptr i8, ptr {s_v}, i64 {i_v}"
        )
        .unwrap();
        let byte = self.fresh();
        writeln!(self.out, "  {byte} = load i8, ptr {addr}").unwrap();
        let ext = self.fresh();
        writeln!(self.out, "  {ext} = zext i8 {byte} to i64").unwrap();
        if !is_unit(self.tcx, self.body.local_ty(destination.local)) {
            let slot = local_slot(destination.local);
            writeln!(self.out, "  store i64 {ext}, ptr {slot}").unwrap();
        }
        emit_terminator_branch(&mut self.out, target);
        Ok(())
    }

    /// Single-arg LLVM math intrinsic dispatch: emits the call
    /// + result store + outgoing terminator branch.
    fn lower_math_intrinsic(
        &mut self,
        intrinsic_name: &str,
        arg: &Operand,
        destination: &Place,
        target: Option<&gossamer_mir::BlockId>,
    ) -> Result<(), BuildError> {
        let arg_v = self.lower_operand(arg)?;
        let dest_ty = render_ty(self.tcx, self.body.local_ty(destination.local));
        self.runtime_refs
            .insert(format!("declare double @{intrinsic_name}(double)"));
        let tmp = self.fresh();
        writeln!(
            self.out,
            "  {tmp} = call {dest_ty} @{intrinsic_name}(double {arg_v})"
        )
        .unwrap();
        if !is_unit(self.tcx, self.body.local_ty(destination.local)) {
            let slot = local_slot(destination.local);
            writeln!(self.out, "  store {dest_ty} {tmp}, ptr {slot}").unwrap();
        }
        emit_terminator_branch(&mut self.out, target);
        Ok(())
    }

    /// Direct call to a named symbol: renders args, emits the
    /// `call`, stores the result if non-unit, and writes the
    /// outgoing branch / unreachable.
    fn emit_named_call(
        &mut self,
        symbol: &str,
        args: &[Operand],
        destination: &Place,
        target: Option<&gossamer_mir::BlockId>,
    ) -> Result<(), BuildError> {
        let mut arg_text = String::new();
        for (i, arg) in args.iter().enumerate() {
            if i > 0 {
                arg_text.push_str(", ");
            }
            let a_ty = self.operand_llvm_ty(arg);
            let a_v = self.lower_operand(arg)?;
            let _ = write!(arg_text, "{a_ty} {a_v}");
        }
        let dest_ty_mir = self.body.local_ty(destination.local);
        let dest_ty = render_ty(self.tcx, dest_ty_mir);

        // Detect the call-site arena scoping pattern: a callee
        // returning a *pure primitive* aggregate (no heap-pointer
        // fields) hands us a heap pointer; we `memcpy` it into the
        // caller's stack alloca and the heap copy is dead. Wrap the
        // call+memcpy in a `gos_rt_arena_save`/`_restore` pair so
        // the heap copy and any callee-internal allocations are
        // reclaimed immediately. Drives the spectral-norm matvec
        // RAM win.
        let scope_arena = is_aggregate(self.tcx, dest_ty_mir)
            && is_pure_primitive_aggregate(self.tcx, dest_ty_mir)
            && !symbol.starts_with("gos_rt_");
        let saved_tmp = if scope_arena {
            let s = self.fresh();
            writeln!(self.out, "  {s} = call i64 @gos_rt_arena_save()").unwrap();
            Some(s)
        } else {
            None
        };

        if dest_ty == "void" || is_unit(self.tcx, dest_ty_mir) {
            writeln!(self.out, "  call void @\"{symbol}\"({arg_text})").unwrap();
        } else {
            let tmp = self.fresh();
            writeln!(
                self.out,
                "  {tmp} = call {dest_ty} @\"{symbol}\"({arg_text})"
            )
            .unwrap();
            let slot = local_slot(destination.local);
            if is_aggregate(self.tcx, dest_ty_mir) {
                // Aggregate return: the callee handed us a heap
                // pointer to fresh storage. Copy it into our
                // destination's inline alloca so subsequent field
                // reads use the same flat-slot shape that locally
                // built aggregates use.
                let bytes = u64::from(slot_count(self.tcx, dest_ty_mir).unwrap_or(1).max(1)) * 8;
                writeln!(
                    self.out,
                    "  call void @llvm.memcpy.p0.p0.i64(ptr {slot}, ptr {tmp}, i64 {bytes}, i1 false)"
                )
                .unwrap();
            } else {
                writeln!(self.out, "  store {dest_ty} {tmp}, ptr {slot}").unwrap();
            }
        }
        if let Some(saved) = saved_tmp {
            writeln!(self.out, "  call void @gos_rt_arena_restore(i64 {saved})").unwrap();
        }
        emit_terminator_branch(&mut self.out, target);
        Ok(())
    }

    fn fresh(&mut self) -> String {
        let n = self.next_ssa;
        self.next_ssa += 1;
        format!("%t{n}")
    }

    /// Looks up the rendered LLVM type for an operand,
    /// walking any projection chain so `p.x + p.y` sees the
    /// field type rather than the struct-ptr one. String-byte
    /// reads (`s[i]`) classify as `i64`.
    fn operand_llvm_ty(&self, op: &Operand) -> String {
        match op {
            Operand::Copy(p) => {
                if self.place_is_string_byte(p) {
                    return "i64".to_string();
                }
                render_ty(self.tcx, self.place_leaf_ty(p))
            }
            Operand::Const(c) => const_llvm_ty(c).to_string(),
            Operand::FnRef { .. } => "ptr".to_string(),
        }
    }

    /// Returns the [`gossamer_types::Ty`] behind an operand,
    /// used where the caller needs to do kind-aware dispatch
    /// (arithmetic vs comparison, integer signedness).
    /// Respects projection chains. For string-byte reads we
    /// scan the body's locals for an existing `i64`-kind
    /// handle so downstream numeric classifiers see an int.
    /// For constants we scan the body's locals for an
    /// existing handle of the same kind, so a float constant
    /// in a float context classifies as `f64`.
    fn operand_ty(&self, op: &Operand) -> Ty {
        match op {
            Operand::Copy(p) => {
                if self.place_is_string_byte(p) {
                    if let Some(ty) = self.borrow_i64_ty() {
                        return ty;
                    }
                }
                self.place_leaf_ty(p)
            }
            Operand::Const(value) => match value {
                ConstValue::Float(_) => self
                    .borrow_kind_ty(|k| matches!(k, TyKind::Float(gossamer_types::FloatTy::F64)))
                    .unwrap_or_else(|| self.body.local_ty(Local::RETURN)),
                ConstValue::Int(_) | ConstValue::Char(_) | ConstValue::Bool(_) => self
                    .borrow_i64_ty()
                    .unwrap_or_else(|| self.body.local_ty(Local::RETURN)),
                _ => self.body.local_ty(Local::RETURN),
            },
            Operand::FnRef { .. } => self.body.local_ty(Local::RETURN),
        }
    }

    fn borrow_kind_ty(&self, want: impl Fn(&TyKind) -> bool) -> Option<Ty> {
        for decl in &self.body.locals {
            if let Some(k) = self.tcx.kind(decl.ty) {
                if want(k) {
                    return Some(decl.ty);
                }
            }
        }
        None
    }

    /// Returns `true` when the final projection step of
    /// `place` indexes into a `String` / `&String`. Used to
    /// reclassify the operand type as `i64` without needing
    /// to mint a fresh `Ty` handle.
    fn place_is_string_byte(&self, place: &Place) -> bool {
        if place.projection.is_empty() {
            return false;
        }
        let (prefix, last) = place.projection.split_at(place.projection.len() - 1);
        if !matches!(last[0], Projection::Index(_)) {
            return false;
        }
        let mut ty = self.body.local_ty(place.local);
        for proj in prefix {
            ty = self.unwrap_ref(ty);
            ty = match proj {
                Projection::Field(i) => match self.tcx.kind(ty) {
                    Some(TyKind::Adt { def, .. }) => self
                        .tcx
                        .struct_field_tys(*def)
                        .and_then(|tys| tys.get(*i as usize).copied())
                        .unwrap_or(ty),
                    Some(TyKind::Tuple(elems)) => elems.get(*i as usize).copied().unwrap_or(ty),
                    _ => ty,
                },
                Projection::Index(_) => match self.tcx.kind(ty) {
                    Some(TyKind::Array { elem, .. } | TyKind::Slice(elem) | TyKind::Vec(elem)) => {
                        *elem
                    }
                    _ => ty,
                },
                Projection::Deref => self.unwrap_ref(ty),
                Projection::Downcast(_) | Projection::Discriminant => ty,
            };
        }
        matches!(self.tcx.kind(self.unwrap_ref(ty)), Some(TyKind::String))
    }

    /// Scans the body's locals for an existing `i64`-kinded
    /// [`Ty`] handle. Used to answer `operand_ty` queries for
    /// string-byte reads without needing to mint a fresh
    /// interner entry.
    fn borrow_i64_ty(&self) -> Option<Ty> {
        for decl in &self.body.locals {
            if matches!(
                self.tcx.kind(decl.ty),
                Some(TyKind::Int(gossamer_types::IntTy::I64))
            ) {
                return Some(decl.ty);
            }
        }
        None
    }
}

fn local_slot(local: Local) -> String {
    format!("%l{}", local.as_u32())
}

fn render_const(cv: &ConstValue) -> String {
    match cv {
        ConstValue::Unit => String::new(),
        ConstValue::Bool(false) => "false".to_string(),
        ConstValue::Bool(true) => "true".to_string(),
        ConstValue::Int(n) => n.to_string(),
        ConstValue::Float(bits) => {
            // `ConstValue::Float` already stores the bit
            // pattern of an IEEE-754 binary64. LLVM accepts
            // hex-encoded literals via `0xH…` — use that for
            // exact round-tripping.
            format!("0x{bits:016X}")
        }
        ConstValue::Char(c) => (*c as u32).to_string(),
        ConstValue::Str(_) => {
            // Strings go through the runtime; MVP doesn't
            // support them yet as value-level constants.
            "null".to_string()
        }
    }
}

/// Textual LLVM type for a constant. The MIR `ConstValue`
/// always carries enough tag to pick the right LLVM family;
/// we bake the default widths (`i64`, `double`) that the
/// frontend's literal-lowering produces.
fn const_llvm_ty(cv: &ConstValue) -> &'static str {
    match cv {
        ConstValue::Unit => "void",
        ConstValue::Bool(_) => "i1",
        ConstValue::Int(_) => "i64",
        ConstValue::Float(_) => "double",
        ConstValue::Char(_) => "i32",
        ConstValue::Str(_) => "ptr",
    }
}

fn is_cmp(op: BinOp) -> bool {
    matches!(
        op,
        BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge
    )
}

fn int_cmp_pred(op: BinOp, signed: bool) -> &'static str {
    match (op, signed) {
        (BinOp::Eq, _) => "eq",
        (BinOp::Ne, _) => "ne",
        (BinOp::Lt, true) => "slt",
        (BinOp::Lt, false) => "ult",
        (BinOp::Le, true) => "sle",
        (BinOp::Le, false) => "ule",
        (BinOp::Gt, true) => "sgt",
        (BinOp::Gt, false) => "ugt",
        (BinOp::Ge, true) => "sge",
        (BinOp::Ge, false) => "uge",
        _ => "eq",
    }
}

fn float_cmp_pred(op: BinOp) -> &'static str {
    match op {
        BinOp::Eq => "oeq",
        BinOp::Ne => "one",
        BinOp::Lt => "olt",
        BinOp::Le => "ole",
        BinOp::Gt => "ogt",
        BinOp::Ge => "oge",
        _ => "oeq",
    }
}

/// Rewrites `::` and other path punctuation so the resulting
/// identifier is a legal LLVM function name when rendered
/// inside quotes.
fn escape_ident(name: &str) -> String {
    name.replace('"', "\\\"")
}

/// Returns the LLVM-side function symbol for a Gossamer function
/// name. The user's `main` becomes `gos_main` so the C runtime can
/// own the real `main` (it sets up argv, calls into `gos_main`,
/// then forwards the i64 return through `gos_rt_main_exit_code`).
/// Every other name passes through unchanged.
///
/// Centralising this here lets both the `define` line in `lower`
/// and the declaration emitter in `emit` agree without a post-hoc
/// `out.replace("@\"main\"", ...)` pass that doubled the IR
/// string's peak heap on big programs.
pub(crate) fn mangle_fn_name(name: &str) -> &str {
    if name == "main" { "gos_main" } else { name }
}

/// Maps user-level math path names onto the LLVM intrinsic
/// that `llc` will lower to the host's SIMD/FP instruction.
/// Recognises both the bare (`sqrt`) and module-qualified
/// (`math::sqrt`) spellings so the match fires regardless of
/// whether the user writes `sqrt(x)` or `math::sqrt(x)`.
fn math_intrinsic(name: &str) -> Option<&'static str> {
    let tail = name.rsplit("::").next().unwrap_or(name);
    let llvm = match tail {
        "sqrt" => "llvm.sqrt.f64",
        "sin" => "llvm.sin.f64",
        "cos" => "llvm.cos.f64",
        "abs" | "fabs" => "llvm.fabs.f64",
        "floor" => "llvm.floor.f64",
        "ceil" => "llvm.ceil.f64",
        "exp" => "llvm.exp.f64",
        "ln" | "log" => "llvm.log.f64",
        _ => return None,
    };
    Some(llvm)
}

/// Maps a prelude / stdlib call name to the runtime symbol the
/// LLVM module should emit a `call` against. Each arm mirrors
/// the equivalent Cranelift intrinsic dispatch arm so the LLVM
/// backend covers the same surface area without per-program
/// patches. Names without a known mapping pass through verbatim
/// so user-defined functions still resolve.
fn map_prelude_symbol(name: &str) -> &str {
    match name {
        "println" | "print" | "eprintln" | "eprint" => "gos_rt_print_str",
        "panic" => "gos_rt_panic",
        "os::args" => "gos_rt_os_args",
        "os::exit" => "gos_rt_exit",
        "io::stdout" | "os::stdout" => "gos_rt_io_stdout",
        "io::stderr" | "os::stderr" => "gos_rt_io_stderr",
        "io::stdin" | "os::stdin" => "gos_rt_io_stdin",
        "time::now" => "gos_rt_time_now",
        "time::now_ms" => "gos_rt_time_now_ms",
        "time::now_ns" => "gos_rt_time_now_ns",
        "time::sleep" => "gos_rt_time_sleep",
        "math::pow" => "gos_rt_math_pow",
        "math::abs" => "gos_rt_math_abs",
        "math::sqrt" => "gos_rt_math_sqrt",
        "math::sin" => "gos_rt_math_sin",
        "math::cos" => "gos_rt_math_cos",
        "math::ln" | "math::log" => "gos_rt_math_log",
        "math::exp" => "gos_rt_math_exp",
        "math::floor" => "gos_rt_math_floor",
        "math::ceil" => "gos_rt_math_ceil",
        "sync::yield_now" | "runtime::yield_now" => "gos_rt_yield_now",
        "Mutex::new" | "sync::Mutex::new" | "mutex::new" => "gos_rt_mutex_new",
        "WaitGroup::new" | "sync::WaitGroup::new" | "wg::new" => "gos_rt_wg_new",
        "I64Vec::new" | "heap_i64::new" => "gos_rt_heap_i64_new",
        "Atomic::new" | "sync::Atomic::new" | "atomic::new" => "gos_rt_atomic_i64_new",
        "lcg::jump" | "lcg_jump" => "gos_rt_lcg_jump",
        other => other,
    }
}

/// Writes the outgoing terminator branch for a Call/Math
/// instruction: a `br label %bbN` for the success target or an
/// `unreachable` when the call is `noreturn`.
fn emit_terminator_branch(out: &mut String, target: Option<&gossamer_mir::BlockId>) {
    match target {
        Some(t) => {
            writeln!(out, "  br label %bb{}", t.as_u32()).unwrap();
        }
        None => {
            writeln!(out, "  unreachable").unwrap();
        }
    }
}

/// LLVM `\HH` hex-escape for string constants. Any byte that
/// isn't a printable ASCII character gets rendered as `\HH`.
fn escape_c_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        match *b {
            0x20..=0x7E if *b != b'"' && *b != b'\\' => out.push(*b as char),
            _ => {
                let _ = write!(out, "\\{b:02X}");
            }
        }
    }
    out
}
