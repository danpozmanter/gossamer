#![allow(
    unused_imports,
    dead_code,
    unreachable_pub,
    missing_docs,
    clippy::wildcard_imports,
    clippy::too_many_lines,
    clippy::too_many_arguments,
    clippy::similar_names,
    clippy::many_single_char_names,
    clippy::items_after_statements,
    clippy::cast_lossless,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::doc_markdown,
    clippy::option_if_let_else,
    clippy::match_same_arms,
    clippy::if_not_else,
    clippy::single_match_else,
    clippy::needless_pass_by_value,
    clippy::manual_let_else,
    clippy::redundant_else,
    clippy::collapsible_if,
    clippy::collapsible_else_if,
    clippy::map_unwrap_or,
    clippy::struct_excessive_bools,
    clippy::module_name_repetitions,
    clippy::unnecessary_wraps,
    clippy::large_enum_variant,
    clippy::if_same_then_else,
    clippy::single_match,
    clippy::useless_conversion,
    clippy::needless_borrows_for_generic_args,
    clippy::let_and_return,
    unsafe_op_in_unsafe_fn,
    unsafe_code,
    clippy::missing_safety_doc,
    clippy::undocumented_unsafe_blocks,
    clippy::needless_collect,
    clippy::elidable_lifetime_names,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::must_use_candidate,
    clippy::missing_const_for_fn,
    clippy::needless_range_loop,
    clippy::cognitive_complexity,
    clippy::unused_io_amount,
    clippy::ptr_arg,
    clippy::ptr_as_ptr,
    clippy::redundant_closure,
    clippy::redundant_closure_for_method_calls,
    clippy::semicolon_if_nothing_returned,
    clippy::single_call_fn,
    clippy::unused_self,
    clippy::range_plus_one,
    clippy::not_unsafe_ptr_arg_deref,
    clippy::cast_ptr_alignment,
    clippy::manual_assert,
    clippy::manual_string_new,
    clippy::match_bool,
    clippy::nonminimal_bool,
    clippy::redundant_pattern_matching,
    clippy::useless_let_if_seq
)]
//! HIR → bytecode compiler.

#![forbid(unsafe_code)]
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

/// Returns `true` when `ty` is `&mut Vec<T>` / `&mut [T]` — the
/// parameter / argument shape that rides the write-back cell
/// protocol (`Op::CellNew` / `Op::CellTake` /
/// `FnChunk::mut_ref_params`). Fixed `[T; N]` arrays are excluded:
/// the compiled tiers copy them at the call boundary, so cell
/// write-back there would *create* a divergence.
pub(crate) fn is_mut_ref_vec(tcx: &TyCtxt, ty: Ty) -> bool {
    let Some(TyKind::Ref {
        mutability: gossamer_types::Mutbl::Mut,
        inner,
    }) = tcx.kind(ty)
    else {
        return false;
    };
    matches!(tcx.kind(*inner), Some(TyKind::Vec(_) | TyKind::Slice(_)))
}

/// Returns `true` when `ty` is a `&mut T` whose mutation through the
/// reference must be visible to the caller on every tier: `&mut Vec<T>`
/// / `&mut [T]` (the cell-protocol shapes above) plus `&mut <scalar
/// primitive>` (`i*` / `u*` / `f*` / `bool` / `char`) and `&mut String`
/// (a flat `*mut c_char` whose pointer IS the value). The compiled
/// tiers pass each of these by pointer, so `*p = v` writes back; the VM
/// must match. Used to decide that a call carrying such an argument
/// participates in the `MutCell` write-back + `*p = …` deref-assign
/// protocol. Aggregates (`struct` / `enum` /
/// fixed `[T; N]`) are deliberately excluded: their by-value vs
/// by-pointer treatment varies and a blanket write-back would diverge.
pub(crate) fn is_mut_ref_writeback(tcx: &TyCtxt, ty: Ty) -> bool {
    if is_mut_ref_vec(tcx, ty) {
        return true;
    }
    let Some(TyKind::Ref {
        mutability: gossamer_types::Mutbl::Mut,
        inner,
    }) = tcx.kind(ty)
    else {
        return false;
    };
    matches!(
        tcx.kind(*inner),
        Some(TyKind::Int(_) | TyKind::Float(_) | TyKind::Bool | TyKind::Char | TyKind::String)
    )
}

/// Trivial-wrapper inlining table: for each user function
/// whose body is `return intrinsic(param)` (a pattern common
/// enough in library code that its call overhead shows up on
/// profiles), we record the target intrinsic's path segments.
/// Calls to the wrapper are rewritten to direct intrinsic
/// calls at compile time — no `Op::Call`, no push/pop of a
/// frame, no boxing across the call boundary.
pub(crate) type InlinableWrappers = std::collections::HashMap<String, Vec<String>>;

/// An owned snapshot of a user function the bytecode compiler may inline
/// at its call sites. Holds clones of the function's parameter patterns
/// and its single tail expression, taken once at load time so the
/// inliner re-compiles the body without re-borrowing the `HirProgram`.
/// Built by [`inline::detect_inlinable_fn`] and consulted by
/// [`FnBuilder::try_inline_user_call`].
#[derive(Clone)]
pub(crate) struct InlinableFn {
    /// Parameter binding patterns in declaration order.
    pub(crate) params: Vec<HirPat>,
    /// The function's tail expression — its sole computation, re-compiled
    /// directly into the caller at each inlined call site.
    pub(crate) tail: HirExpr,
    /// Weighted node count of `tail`, charged against the caller's inline
    /// budget so transitive inlining stays bounded.
    pub(crate) cost: usize,
}

/// User-function inlining table, keyed by bare function name (mirroring
/// [`InlinableWrappers`]). A call whose callee is a single-segment path
/// naming an entry here may be inlined in place of an `Op::Call`. The
/// map is owned by the VM load frame and plumbed by `&` exactly like
/// `wrappers` / `module_consts`, so its lifetime never entangles with
/// the `HirProgram` borrow.
pub(crate) type InlinableFns = std::collections::HashMap<String, InlinableFn>;

/// Top-level `const` items, keyed by name, with their already-
/// evaluated `Value`. The bytecode compiler inlines a path that
/// resolves to one of these into a `LoadConst` (constant-pool
/// fetch — single index) instead of a `LoadGlobal` (string-keyed
/// `HashMap` lookup). The win shows up on hot loops that close
/// over constants — fasta's `(state*IA+IC) % IM` LCG step would
/// otherwise pay three name lookups per iteration.
pub(crate) type ConstValues = std::collections::HashMap<String, Value>;

/// Qualified names (`Type::method`) of every user `impl` method whose
/// receiver is `&mut self`. A method call on a writeback place
/// (`obj.bump()`, `(&mut __for_iter).next()`) lowers through the
/// write-back cell protocol so the method's mutation of `self`
/// persists in the caller's binding — matching the by-pointer receiver
/// the compiled tiers pass. The bytecode compiler reconstructs the
/// `Type::method` key from the receiver's resolved type at each call
/// site and consults this set to decide whether the writeback fires.
pub(crate) type MutSelfMethods = std::collections::HashSet<String>;

/// Bare names of every `static mut` item in the program. The bytecode
/// compiler consults this set to lower an assignment whose place is
/// rooted at a mutable static into an [`Op::StoreStatic`] against the
/// shared `Global::MutStatic` cell, rather than treating the path as a
/// local. Reads need no entry here: a mutable static is excluded from
/// the const-inlining snapshot, so its path already lowers to a
/// `LoadGlobal` that resolves the cell at runtime.
pub(crate) type MutStatics = std::collections::HashSet<String>;

/// Collects the [`MutStatics`] set: the name of every `static mut` item.
pub fn collect_mut_statics(program: &gossamer_hir::HirProgram) -> MutStatics {
    let mut out = MutStatics::new();
    for item in &program.items {
        if let gossamer_hir::HirItemKind::Static(decl) = &item.kind
            && decl.mutable
        {
            out.insert(decl.name.name.clone());
        }
    }
    out
}

/// Collects the [`MutSelfMethods`] set from a program's `impl` blocks:
/// every method whose first parameter is a `&mut self` receiver. Built
/// once at load time and threaded into [`compile_fn`].
pub fn collect_mut_self_methods(program: &gossamer_hir::HirProgram) -> MutSelfMethods {
    let mut out = MutSelfMethods::new();
    for item in &program.items {
        let gossamer_hir::HirItemKind::Impl(decl) = &item.kind else {
            continue;
        };
        let Some(type_name) = &decl.self_name else {
            continue;
        };
        for method in &decl.methods {
            if method_has_mut_self(method) {
                out.insert(format!("{}::{}", type_name.name, method.name.name));
            }
        }
    }
    out
}

/// `true` when `decl`'s first parameter is a `&mut self` receiver — the
/// shape (`fn next(&mut self)`, `fn bump(&mut self)`) whose mutation
/// must flow back to the caller's binding.
fn method_has_mut_self(decl: &HirFn) -> bool {
    if !decl.has_self {
        return false;
    }
    matches!(
        decl.params.first().map(|p| &p.pattern.kind),
        Some(HirPatKind::Binding {
            name,
            mutable: true,
        }) if name.name == "self"
    )
}

/// Compiles an [`HirFn`] body into a [`FnChunk`]. The caller owns the
/// resulting chunk; the compiler itself has no shared state.
pub fn compile_fn(
    decl: &HirFn,
    tcx: &TyCtxt,
    layouts: &StructLayouts,
    wrappers: &InlinableWrappers,
    inline_fns: &InlinableFns,
    consts: &ConstValues,
    method_muts: &MutSelfMethods,
    mut_statics: &MutStatics,
    cov: Option<&gossamer_lex::SourceMap>,
) -> RuntimeResult<FnChunk> {
    let name = crate::value::intern_type_name(&decl.name.name);
    let Some(body) = decl.body.as_ref() else {
        return Ok(FnChunk {
            name,
            arity: u16::try_from(decl.params.len()).unwrap_or(u16::MAX),
            register_count: 0,
            float_count: 0,
            int_count: 0,
            instrs: Vec::new(),
            consts: Vec::new(),
            f64_consts: Vec::new(),
            i64_consts: Vec::new(),
            globals: Vec::new(),
            shape_names: Vec::new(),
            wide_ops: Vec::new(),
            call_cache_count: 0,
            arith_cache_count: 0,
            field_cache_count: 0,
            mut_ref_params: Vec::new(),
            closure_protos: Vec::new(),
            select_arms: Vec::new(),
        });
    };
    let mut builder = FnBuilder::new(
        name,
        tcx,
        layouts,
        wrappers,
        inline_fns,
        consts,
        method_muts,
        mut_statics,
        cov,
    );
    for (idx, param) in decl.params.iter().enumerate() {
        let reg = builder.alloc_reg();
        builder.bind_param(&param.pattern, reg);
        // `&mut Vec<T>` / `&mut [T]` / `&mut <scalar>` parameters
        // participate in the write-back cell protocol — the callee
        // unwraps an incoming `MutCell` into the param register and
        // publishes its final value back on return. See
        // `FnChunk::mut_ref_params`. A `&mut self` receiver (an
        // aggregate, which `is_mut_ref_writeback` deliberately excludes
        // for general args) rides the same protocol: the compiled tiers
        // pass `self` by pointer, so its mutation must reach the
        // caller's binding. Marking the receiver register here lets the
        // call-site cell wrapping in `compile_method_call` publish the
        // post-call `self` back to the receiver place.
        if is_mut_ref_writeback(tcx, param.ty)
            || (idx == 0 && method_has_mut_self(decl) && builder.is_adt_ref(param.ty))
        {
            builder.mut_ref_params.push(reg);
        }
        // Track typed-storage parameter shapes so callees can use
        // the same `IntArrayGetI64` / `FloatVecGetF64` fast paths
        // they would for a let-binding. The receiver invariant
        // holds whenever the caller built the argument via
        // `try_build_int_array` / `try_build_float_vec` — see
        // `Op::FloatVecGetF64` for the runtime gate.
        let elem_kind = builder.unwrap_ref(param.ty);
        if let Some(TyKind::Array { elem, .. } | TyKind::Vec(elem) | TyKind::Slice(elem)) =
            tcx.kind(elem_kind)
        {
            match tcx.kind(*elem) {
                Some(TyKind::Float(FloatTy::F64)) => {
                    builder.flat_float_locals.insert(reg);
                }
                Some(TyKind::Int(IntTy::I64 | IntTy::Isize | IntTy::Usize)) => {
                    builder.flat_int_locals.insert(reg);
                }
                _ => {}
            }
        }
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

/// Compiles a single `const`/`static` initializer expression into a
/// synthetic nullary [`FnChunk`]. Running the chunk on the VM yields the
/// item's value, so const/static evaluation reuses the ordinary
/// compile-and-run path — every initializer shape the VM can lower
/// (literals, arithmetic, aggregates, prelude/const-fn calls) is
/// evaluated by the same machinery, with no separate evaluator.
pub fn compile_initializer(
    expr: &HirExpr,
    tcx: &TyCtxt,
    layouts: &StructLayouts,
    wrappers: &InlinableWrappers,
    inline_fns: &InlinableFns,
    consts: &ConstValues,
    method_muts: &MutSelfMethods,
    mut_statics: &MutStatics,
    cov: Option<&gossamer_lex::SourceMap>,
) -> RuntimeResult<FnChunk> {
    let name = crate::value::intern_type_name("__init");
    let mut builder = FnBuilder::new(
        name,
        tcx,
        layouts,
        wrappers,
        inline_fns,
        consts,
        method_muts,
        mut_statics,
        cov,
    );
    let reg = builder.compile_expr(expr)?;
    builder.emit(Op::Return { value: reg });
    Ok(builder.finish(0))
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum BlockResult {
    Unit,
    ValueIn(Reg),
    Diverges,
}

pub(crate) struct FnBuilder<'tcx> {
    pub(crate) name: &'static str,
    pub(crate) tcx: &'tcx TyCtxt,
    pub(crate) layouts: &'tcx StructLayouts,
    pub(crate) wrappers: &'tcx InlinableWrappers,
    /// User functions eligible for call-site inlining (see
    /// [`InlinableFns`]). Borrowed from the VM load frame for the
    /// duration of the compile, like `wrappers`.
    pub(crate) inline_fns: &'tcx InlinableFns,
    /// Names of the functions whose bodies are currently being inlined
    /// into this builder, innermost last. Consulted before each inline to
    /// reject direct / mutual / transitive recursion; pushed before
    /// compiling a callee body and popped after.
    pub(crate) inlining: Vec<&'static str>,
    /// Running total of inlined tail nodes spliced into this builder.
    /// Once it would exceed the per-caller budget, further calls stay
    /// real `Op::Call`s, bounding code growth.
    pub(crate) inlined_nodes: usize,
    /// Pre-evaluated values for top-level `const` items. A path
    /// expression that resolves to one of these inlines as a
    /// `LoadConst` instead of a `LoadGlobal` so the bytecode VM
    /// fetches it via constant-pool index instead of a runtime
    /// name lookup.
    pub(crate) module_consts: &'tcx ConstValues,
    /// Value registers that are compile-time-proven to hold
    /// `Value::FloatArray` — populated by `BuildFloatArray`
    /// emission and cleared whenever the register is
    /// reassigned. When a read/write op's base is one of
    /// these, we emit `FlatGetF64` / `FlatSetF64` instead of
    /// the discriminant-checking `IndexedFieldGetF64ByOffset`.
    pub(crate) flat_locals: std::collections::HashMap<Reg, u16>,
    /// Value registers compile-time-proven to hold a
    /// `Value::IntArray` (a primitive `[i64; N]` literal). Reads
    /// against one of these registers route through
    /// [`Op::IntArrayGetI64`] into a typed `i64` register.
    pub(crate) flat_int_locals: std::collections::HashSet<Reg>,
    /// Mirror of [`Self::flat_int_locals`] for `Value::FloatVec` —
    /// `[f64; N]` literals built via [`Self::try_build_float_vec`].
    /// Lets indexed reads / writes route through the typed-`f64`
    /// fast path that skips the `Value::Float` round-trip.
    pub(crate) flat_float_locals: std::collections::HashSet<Reg>,
    /// Registers bound by a pattern to an array / vec / slice value
    /// (`Some(arr)`, `(head, tail)`, …). A pattern binding's `Path`
    /// reference carries the binding's *declared* type only when the
    /// frontend resolved it; for an inferred binding the HIR `ty` stays
    /// an unresolved var, so the for-loop fast path can't tell `for x in
    /// arr` iterates a collection. Tracking the binding register here
    /// lets `try_compile_for_loop_vec_iter` drive it by index instead of
    /// deferring. Populated from the pattern's resolved type at bind time.
    pub(crate) collection_locals: std::collections::HashSet<Reg>,
    /// Registers bound to a `flag::Set` handle (`flag::Set::new(...)`),
    /// so a chained `set.duration(...)` is recognised as constructing a
    /// duration-flag cell rather than calling a same-named user method.
    pub(crate) flag_set_locals: std::collections::HashSet<Reg>,
    /// Registers bound to a `flag::Set` duration cell
    /// (`set.duration(...)`). A duration cell's element type is the
    /// transparent `time::Duration` newtype, but the typechecker leaves
    /// the cell an inference var, so the method-form accessors
    /// (`cell.as_millis()`) dispatch on this tag instead of the receiver
    /// type, routing to the `time::Duration::<accessor>` global with the
    /// cell auto-deref'd to its backing `i64`-of-ms value.
    pub(crate) duration_cell_locals: std::collections::HashSet<Reg>,
    pub(crate) instrs: Vec<Op>,
    pub(crate) consts: Vec<Value>,
    pub(crate) const_cache: HashMap<ConstKey, ConstIdx>,
    pub(crate) f64_consts: Vec<f64>,
    pub(crate) f64_const_cache: HashMap<u64, ConstIdx>,
    pub(crate) i64_consts: Vec<i64>,
    pub(crate) i64_const_cache: HashMap<i64, ConstIdx>,
    pub(crate) globals: Vec<String>,
    pub(crate) shape_names: Vec<&'static str>,
    pub(crate) global_cache: HashMap<String, GlobalIdx>,
    pub(crate) next_reg: u16,
    pub(crate) next_float_reg: u16,
    pub(crate) next_int_reg: u16,
    pub(crate) scopes: Vec<Scope>,
    pub(crate) loop_stack: Vec<LoopCtx>,
    /// Per-block frames of `defer`red expressions, mirroring the MIR
    /// builder's `defer_stack`. `compile_block` pushes a frame on entry
    /// and emits it LIFO on a normal exit; `return` / `break` /
    /// `continue` emit the pending frames at their exit edge before the
    /// jump. The same block-scoped LIFO contract the compiled tiers use.
    pub(crate) defer_stack: Vec<Vec<HirExpr>>,
    pub(crate) closure_protos: Vec<crate::bytecode::ClosureProto>,
    pub(crate) select_arms: Vec<crate::bytecode::SelectArmMeta>,
    pub(crate) wide_ops: Vec<crate::bytecode::WideOp>,
    /// Counter incremented every time we emit a dispatch op
    /// (`Op::Call` / `Op::MethodCall`) so each call site gets a
    /// unique inline-cache slot index. The `FnChunk` allocates a
    /// `Vec<CacheSlot>` of this size at finish time.
    pub(crate) next_cache_idx: u16,
    /// Counter for `Op::FieldGet` IC slots (T2.5).
    pub(crate) next_field_cache_idx: u16,
    /// Counter incremented every time we emit a generic-`Value`
    /// arith op (`Op::AddInt` / `Op::SubInt` / `Op::MulInt` /
    /// `Op::DivInt` / `Op::RemInt`) so each site gets its own
    /// `arith_caches` slot. The `FnChunk` allocates the cache
    /// vector to this size at `finish` time. Tier C2.
    pub(crate) next_arith_cache_idx: u16,
    /// Parameter registers declared `&mut Vec<T>` / `&mut [T]`;
    /// copied into [`FnChunk::mut_ref_params`] at `finish` time.
    pub(crate) mut_ref_params: Vec<Reg>,
    /// Qualified names (`Type::method`) of user `&mut self` methods, used
    /// by `compile_method_call` to route a place-receiver call through
    /// the write-back cell protocol so the receiver's mutation persists.
    pub(crate) method_muts: &'tcx MutSelfMethods,
    /// Names of `static mut` items. Used to lower a static-rooted
    /// assignment into an [`Op::StoreStatic`] instead of deferring it.
    pub(crate) mut_statics: &'tcx MutStatics,
    /// Source map for `gos test --coverage`. `Some` only when the VM
    /// loaded the program with coverage active (see
    /// [`crate::vm::Vm::coverage_active`]); each `compile_stmt` then
    /// resolves the statement span to `(file, line)`, registers a
    /// counter slot, and emits [`Op::CovHit`]. `None` everywhere else,
    /// so non-coverage compiles pay nothing.
    pub(crate) cov: Option<&'tcx gossamer_lex::SourceMap>,
}

#[derive(Debug, Default)]
pub(crate) struct Scope {
    pub(crate) locals: HashMap<String, TypedReg>,
}

#[derive(Debug)]
pub(crate) struct LoopCtx {
    pub(crate) break_patches: Vec<InstrIdx>,
    /// Forward jumps emitted for `continue` inside this loop. Each
    /// entry is the index of an `Op::Jump { target: 0 }` waiting to
    /// be patched to the loop's per-iteration step op (or back to
    /// the re-entry header for shapes whose step happens at the
    /// top). The per-loop emitter resolves these once it has
    /// emitted its step / re-entry op so `continue` skips the
    /// rest of the body without bypassing the iteration counter.
    pub(crate) continue_patches: Vec<InstrIdx>,
    pub(crate) result_reg: Reg,
    /// `defer_stack` length at loop entry. `break` / `continue` emit the
    /// defer frames at indices `>= defer_depth` — the blocks nested
    /// inside the loop body — before jumping, so per-iteration cleanup
    /// runs on every exit edge while the loop's enclosing frames stay
    /// pending. Mirrors `gossamer-mir`'s `LoopCtx::defer_depth`.
    pub(crate) defer_depth: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum ConstKey {
    Unit,
    Bool(bool),
    Int(i64),
    Float(u64),
    Char(char),
    String(String),
}

mod block;
mod call_expr;
mod closure;
mod compile_expr;
mod control_flow;
mod emit_const;
mod fast_paths;
mod inline;
mod lifecycle;
mod op_expr;
mod reg_scope;
mod stmt;
mod type_helpers;

pub(crate) use inline::detect_inlinable_fn;

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

/// Strips leading module-relative prefix segments (`super`, `crate`,
/// `self`) from a path while more than one segment remains. The global
/// table is keyed by unqualified or module-joined names, so a
/// `super::foo` reference inside an inline `#[cfg(test)] mod tests`
/// resolves the flat parent-module item — matching the resolver's
/// flat-lookup strip (`resolver.rs`) and the walker's `eval_path`.
pub(crate) fn strip_module_relative(segments: &[Ident]) -> &[Ident] {
    let mut tail = segments;
    while tail.len() > 1 && matches!(tail[0].name.as_str(), "super" | "crate" | "self") {
        tail = &tail[1..];
    }
    tail
}

/// Maps a runtime [`Value`] back into the [`ConstKey`] used to
/// dedupe entries in the per-chunk constant pool. Falls back to a
/// disambiguating string key for shapes the pool doesn't model
/// (arrays, maps, etc.) — those still get pooled, but no two
/// inserts will ever collide because each call uses a fresh
/// formatter output.
fn const_key_for_value(value: &Value) -> ConstKey {
    match value {
        Value::Unit | Value::Void => ConstKey::Unit,
        Value::Bool(b) => ConstKey::Bool(*b),
        Value::Int(n) => ConstKey::Int(*n),
        Value::Float(f) => ConstKey::Float(f.to_bits()),
        Value::Char(c) => ConstKey::Char(*c),
        Value::String(s) => ConstKey::String(s.as_ref().to_string()),
        other => ConstKey::String(format!("__const_value_{other:?}")),
    }
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

/// `true` when `pat` introduces any name binding (a plain binding,
/// an `@`-binding, or a struct-field shorthand). The or-pattern
/// lowering uses this to decide whether its alternatives need a
/// shared destination-register set or are pure tests.
fn pattern_has_binding(pat: &HirPat) -> bool {
    match &pat.kind {
        HirPatKind::Wildcard
        | HirPatKind::Rest
        | HirPatKind::Literal(_)
        | HirPatKind::Range { .. } => false,
        HirPatKind::Binding { .. } | HirPatKind::At { .. } => true,
        HirPatKind::Tuple(ps) | HirPatKind::Variant { fields: ps, .. } => {
            ps.iter().any(pattern_has_binding)
        }
        HirPatKind::Struct { fields, .. } => fields
            .iter()
            .any(|f| f.pattern.as_ref().is_none_or(pattern_has_binding)),
        HirPatKind::Ref { inner, .. } => pattern_has_binding(inner),
        HirPatKind::Or(alts) => alts.iter().any(pattern_has_binding),
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

/// Returns `true` when the array-shaped `array_ty`'s element type
/// — or, when typeck left the array's elem as a still-bound
/// `TyKind::Var`, the optional `value_ty` of the literal element —
/// matches `pred`. Used by every typed-storage `try_build_*`
/// builder to gate the fast path. Without the value-side fallback,
/// `let mut perm: [i64; 16] = [0; 16]` and `let mut u: [f64; 6000] =
/// [1.0; 6000]` failed to specialise: typeck records the element
/// type on the binding annotation, but the array literal's
/// `Ty::Array { elem }` keeps a fresh inference var that
/// `default_unresolved_int_vars` resolves only inside the `InferCtxt`
/// — never substituted back into the HIR handle compile.rs reads.
fn is_array_elem_kind(
    tcx: &TyCtxt,
    array_ty: Ty,
    value_ty: Option<Ty>,
    pred: impl Fn(&TyKind) -> bool,
) -> bool {
    let array_elem = match tcx.kind(array_ty) {
        Some(TyKind::Array { elem, .. } | TyKind::Vec(elem) | TyKind::Slice(elem)) => Some(*elem),
        _ => None,
    };
    if let Some(t) = array_elem {
        if let Some(k) = tcx.kind(t) {
            if pred(k) {
                return true;
            }
        }
    }
    if let Some(t) = value_ty {
        if let Some(k) = tcx.kind(t) {
            if pred(k) {
                return true;
            }
        }
    }
    false
}

fn parse_int(text: &str) -> Option<i64> {
    let cleaned = strip_int_suffix(text).replace('_', "");
    if let Some(rest) = cleaned
        .strip_prefix("0x")
        .or_else(|| cleaned.strip_prefix("0X"))
    {
        // Try signed first; fall back to unsigned reinterpret for
        // bit patterns that overflow i64 (e.g. 0xFFFFFFFFFFFFFFFF).
        return i64::from_str_radix(rest, 16)
            .ok()
            .or_else(|| u64::from_str_radix(rest, 16).ok().map(|n| n as i64));
    }
    if let Some(rest) = cleaned
        .strip_prefix("0b")
        .or_else(|| cleaned.strip_prefix("0B"))
    {
        return i64::from_str_radix(rest, 2)
            .ok()
            .or_else(|| u64::from_str_radix(rest, 2).ok().map(|n| n as i64));
    }
    if let Some(rest) = cleaned
        .strip_prefix("0o")
        .or_else(|| cleaned.strip_prefix("0O"))
    {
        return i64::from_str_radix(rest, 8)
            .ok()
            .or_else(|| u64::from_str_radix(rest, 8).ok().map(|n| n as i64));
    }
    // For decimal, try signed parse first, then unsigned reinterpret
    // for values in [2^63, 2^64) such as u64::MAX or i64::MIN's
    // magnitude (9223372036854775808).
    cleaned
        .parse::<i64>()
        .ok()
        .or_else(|| cleaned.parse::<u64>().ok().map(|n| n as i64))
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
