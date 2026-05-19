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
    clippy::missing_safety_doc,
    clippy::not_unsafe_ptr_arg_deref,
    clippy::cast_ptr_alignment,
    clippy::manual_assert,
    clippy::manual_string_new,
    clippy::match_bool,
    clippy::nonminimal_bool,
    clippy::redundant_pattern_matching,
    clippy::useless_let_if_seq
)]
//! Real Cranelift-backed native codegen.
//! Lowers a slice of MIR [`Body`]s into a `cranelift-object` module
//! and serialises the result as ELF (or the host's equivalent object
//! format). Supported today:
//! - `fn main() -> i64` with integer arithmetic (`+`, `-`, `*`, `/`,
//!   `%`, `&`, `|`, `^`, `<<`, `>>`, unary `-`, `!`),
//! - integer constants,
//! - direct calls between lowered functions,
//! - `return` of an `i64`.
//!
//! A C-ABI shim `main(argc, argv) -> i32` is emitted automatically:
//! it calls the Gossamer `main` and truncates the `i64` result into
//! the process exit code, so the object file links through a
//! standard `cc` invocation.
//! Aggregates (tuples/arrays/structs), strings, closures, and
//! anything that needs a GC heap are not yet lowered — those
//! constructs fall back to [`crate::emit::emit_module`] for
//! inspection.

// Allow patterns the Cranelift lowering deliberately uses:
//   - `similar_names` fires on `print_str`/`print_i64`/etc.
//     intrinsic-name shadowing within the same arm. The
//     parallel naming makes the dispatch table readable.
//   - `many_single_char_names` fires on hot inner-loop locals
//     (`a`, `b`, `n`, `m`, `k`) where longer names would
//     overflow the 100-col limit.
//   - `items_after_statements` flags inline `extern "C"` decls
//     localised to the one helper that uses them. Hoisting them
//     to module scope spreads the FFI surface; localised wins.
//   - `too_many_lines` / `cognitive_complexity` fire on the
//     intrinsic-dispatch arm and the `lower_intrinsic_call`
//     match. Splitting either hides the one-arm-per-symbol
//     structure that makes the table grep-able.
//   - `unnecessary_wraps` flags helpers whose `Result` exists
//     so call sites can still `?` them once a future lowering
//     can fail.
//   - `if_chain_can_be_rewritten_with_match` would flatten
//     short `if let Some(x) = .. else if let Some(y) = ..`
//     chains into match-on-tuple-of-options that's strictly
//     uglier here.
//   - `doc_markdown` flags identifiers like `i64`, `f64`,
//     etc. in plain-prose docs. Backticking every numeric
//     type name in every comment is noise.
//   - `manual_debug_impl` flags `JitModule`'s `Debug` impl
//     (which deliberately omits the JIT module pointer to keep
//     debug output stable across runs).
#![forbid(unsafe_code)]
#![allow(clippy::comparison_chain)]

use std::collections::HashMap;

use std::collections::HashSet;

use anyhow::{Result, anyhow, bail};
use cranelift_codegen::ir::{
    AbiParam, ExtFuncData, Function, GlobalValueData, InstBuilder, MemFlags, Signature,
    StackSlotData, StackSlotKind, UserExternalName, UserFuncName, condcodes::IntCC,
    immediates::Imm64, types,
};
use cranelift_codegen::isa::{CallConv, TargetFrontendConfig};
use cranelift_codegen::settings::{self, Configurable};
use cranelift_codegen::{Context, ir};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Variable};
use cranelift_module::{DataDescription, DataId, FuncId, Linkage, Module, ModuleDeclarations};
use cranelift_object::{ObjectBuilder, ObjectModule};
use gossamer_mir::{
    BinOp, Body, ConstValue, Local, Operand, Place, Projection, Rvalue, StatementKind, Terminator,
    UnOp,
};
use gossamer_types::{FloatTy, IntTy, Ty, TyCtxt, TyKind};
use rayon::prelude::*;

use super::*;

#[derive(Clone)]
pub(super) struct IntrinsicContext {
    /// Interned map from string contents to the `DataId` of the
    /// null-terminated rodata slot holding them. Deduped so the same
    /// literal used in twenty calls still occupies one slot.
    pub(super) strings: HashMap<String, DataId>,
    /// Cached `FuncId` for each C-ABI runtime function we link.
    pub(super) externs: HashMap<&'static str, FuncId>,
    /// Monotonic counter for freshly-generated rodata symbol names.
    pub(super) next_str_id: u32,
    /// Mirror of `function_ids_by_name` from [`compile_to_object`].
    /// Populated up-front so intrinsics like `gos_fn_addr` can look
    /// up the target function without threading the parent map
    /// through every call.
    pub(super) functions: HashMap<String, FuncId>,
    /// Mirror of `function_ids_by_def` so `Operand::FnRef { def }`
    /// operands in non-call position (`let f = fib; f(5)`) can be
    /// materialised as function-pointer values.
    pub(super) functions_by_def: HashMap<u32, FuncId>,
    /// Per-function: the cranelift element type of stack-allocated
    /// aggregates rooted at each local. Populated when lowering
    /// `Rvalue::Aggregate` / `Rvalue::Repeat`, consumed by
    /// projected reads / writes when the MIR element type is still
    /// an unresolved inference variable. Cleared between bodies.
    pub(super) elem_cl_ty: HashMap<Local, ir::Type>,
    /// Per-function: size in 8-byte slots of each element in an
    /// aggregate rooted at the local. `1` for scalar arrays,
    /// `N` for `[Struct; _]` where `Struct` has `N` fields.
    /// Projected address computation uses this as the per-index
    /// stride. Cleared between bodies.
    pub(super) elem_slots: HashMap<Local, u32>,
    /// Per-function: total size in 8-byte slots of the aggregate
    /// rooted at the local. Used so that nested `[T; N]` → `[S;
    /// N]` aggregates produce correct per-element strides.
    /// Cleared between bodies.
    pub(super) local_slots: HashMap<Local, u32>,
    /// Per-function: the cranelift type each local's Variable was
    /// declared with. Populated by `define_var_to` on first
    /// declaration; consulted by `operand_print_kind` so print
    /// dispatch uses the concrete width even when the MIR local's
    /// type is still an unresolved inference variable. Cleared
    /// between bodies.
    pub(super) local_declared_ty: HashMap<Local, ir::Type>,
    /// Per-function: pre-computed cranelift type for every local,
    /// indexed by `local.0`. Populated once via `infer_body_cl_types`
    /// before the body's lowering begins; `define_var_to` and
    /// `ensure_var` read from here instead of re-running the full
    /// body scan on every assignment. Cleared between bodies.
    pub(crate) body_cl_types: Vec<Option<ir::Type>>,
}

impl IntrinsicContext {
    pub(super) fn new() -> Self {
        Self {
            strings: HashMap::new(),
            externs: HashMap::new(),
            next_str_id: 0,
            functions: HashMap::new(),
            functions_by_def: HashMap::new(),
            elem_cl_ty: HashMap::new(),
            elem_slots: HashMap::new(),
            local_slots: HashMap::new(),
            local_declared_ty: HashMap::new(),
            body_cl_types: Vec::new(),
        }
    }

    /// Returns the `DataId` for `text`, defining a new null-
    /// terminated rodata slot on first use.
    pub(super) fn intern_string(&mut self, module: &mut dyn Module, text: &str) -> Result<DataId> {
        if let Some(id) = self.strings.get(text).copied() {
            return Ok(id);
        }
        let symbol = format!(".Lstr{}", self.next_str_id);
        self.next_str_id += 1;
        let id = module
            .declare_data(&symbol, Linkage::Local, false, false)
            .map_err(|e| anyhow!("declare {symbol}: {e}"))?;
        let mut bytes = text.as_bytes().to_vec();
        bytes.push(0);
        let mut description = DataDescription::new();
        description.define(bytes.into_boxed_slice());
        module
            .define_data(id, &description)
            .map_err(|e| anyhow!("define {symbol}: {e}"))?;
        self.strings.insert(text.to_string(), id);
        Ok(id)
    }

    /// Declares (if needed) an imported C-ABI function and returns
    /// its `FuncId`.
    pub(super) fn extern_fn(
        &mut self,
        module: &mut dyn Module,
        name: &'static str,
        params: &[ir::Type],
        returns: &[ir::Type],
    ) -> Result<FuncId> {
        if let Some(id) = self.externs.get(name).copied() {
            return Ok(id);
        }
        let mut sig = module.make_signature();
        for p in params {
            sig.params.push(AbiParam::new(*p));
        }
        for r in returns {
            sig.returns.push(AbiParam::new(*r));
        }
        let id = module
            .declare_function(name, Linkage::Import, &sig)
            .map_err(|e| anyhow!("declare extern {name}: {e}"))?;
        self.externs.insert(name, id);
        Ok(id)
    }

    /// Declares an imported `gos_rt_*` C-ABI function using the
    /// typed signature from the ABI registry.
    ///
    /// Panics if `name` is not in the registry — this turns typos in
    /// symbol names into a build-time panic instead of a silent
    /// wrong-code or segfault at runtime.
    pub(super) fn extern_fn_by_name(
        &mut self,
        module: &mut dyn Module,
        name: &'static str,
    ) -> Result<FuncId> {
        if let Some(id) = self.externs.get(name).copied() {
            return Ok(id);
        }
        let entry = gossamer_abi::lookup(name).unwrap_or_else(|| {
            panic!("extern_fn_by_name: unknown runtime symbol {name:?} — add it to gossamer-abi/src/registry.rs")
        });
        let mut sig = module.make_signature();
        for abi_ty in entry.sig.params {
            let cl_ty = abi_type_to_cranelift(*abi_ty);
            if let Some(t) = cl_ty {
                sig.params.push(AbiParam::new(t));
            }
        }
        if let Some(t) = abi_type_to_cranelift(entry.sig.ret) {
            sig.returns.push(AbiParam::new(t));
        }
        let id = module
            .declare_function(name, Linkage::Import, &sig)
            .map_err(|e| anyhow!("declare extern {name}: {e}"))?;
        self.externs.insert(name, id);
        Ok(id)
    }
}

pub(super) struct IntrinsicOutcome {
    pub(super) handled: bool,
    pub(super) noreturn: bool,
}

pub(super) fn lower_intrinsic_outcome(
    name: &str,
    module: &mut dyn Module,
    builder: &mut FunctionBuilder<'_>,
    locals: &mut HashMap<Local, Variable>,
    body: &Body,
    tcx: &TyCtxt,
    args: &[Operand],
    destination: &gossamer_mir::Place,
    intrinsics: &mut IntrinsicContext,
) -> Result<IntrinsicOutcome> {
    let handled = lower_intrinsic_call(
        module,
        builder,
        locals,
        body,
        tcx,
        args,
        name,
        destination,
        intrinsics,
    )?;
    let noreturn = handled && matches!(name, "panic");
    Ok(IntrinsicOutcome { handled, noreturn })
}

pub(super) fn lower_intrinsic_call(
    module: &mut dyn Module,
    builder: &mut FunctionBuilder<'_>,
    locals: &mut HashMap<Local, Variable>,
    body: &Body,
    tcx: &TyCtxt,
    args: &[Operand],
    name: &str,
    destination: &gossamer_mir::Place,
    intrinsics: &mut IntrinsicContext,
) -> Result<bool> {
    if lower_intrinsic_call_io_math(
        module,
        builder,
        locals,
        body,
        tcx,
        args,
        name,
        destination,
        intrinsics,
    )? {
        return Ok(true);
    }
    if lower_intrinsic_call_collections(
        module,
        builder,
        locals,
        body,
        tcx,
        args,
        name,
        destination,
        intrinsics,
    )? {
        return Ok(true);
    }
    if lower_intrinsic_call_handles(
        module,
        builder,
        locals,
        body,
        tcx,
        args,
        name,
        destination,
        intrinsics,
    )? {
        return Ok(true);
    }
    if lower_intrinsic_call_string(
        module,
        builder,
        locals,
        body,
        tcx,
        args,
        name,
        destination,
        intrinsics,
    )? {
        return Ok(true);
    }
    // No arm matched; let the caller fall back to the generic Call lowering.
    let _ = (
        module,
        builder,
        locals,
        body,
        tcx,
        args,
        name,
        destination,
        intrinsics,
    );
    Ok(false)
}
