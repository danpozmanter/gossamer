//! In-process Cranelift JIT used by `gos run --vm`.
//!
//! Reuses the [`super::native::lower_program`] HIR → MIR → CLIF
//! pipeline that the AOT object backend drives, swapping the
//! `ObjectModule` for a `JITModule`. The resulting raw fn pointers
//! are returned in a [`JitArtifact`] that the bytecode VM reads at
//! every `Op::Call` so hot user functions execute as native code
//! instead of dispatching through the bytecode loop.
//!
//! The VM's register-based dispatch maps cleanly onto SSA, so the
//! same MIR form the AOT path consumes drops straight in. Functions
//! whose codegen path can't lower a feature (closures, dynamic
//! shapes, …) are simply skipped; the VM's existing bytecode
//! interpreter still handles them.

#![allow(unsafe_code)]

use std::collections::HashMap;

use anyhow::{Result, anyhow};
use cranelift_jit::{JITBuilder, JITModule};
use gossamer_mir::Body;
use gossamer_types::{Ty, TyCtxt, TyKind};

use crate::native::{build_native_isa, lower_program};

/// Cranelift register class for one parameter or return slot of a
/// JIT-compiled body. Used by the dispatch trampoline to pick the
/// right marshalling shape per slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JitKind {
    /// A 64-bit signed integer (`i64`, `i32` widened, `usize`, …).
    I64,
    /// A 64-bit IEEE-754 float.
    F64,
    /// A 1-bit boolean represented as `i8` in the cranelift ABI.
    Bool,
    /// The unit value (no representation; the body has no return).
    Unit,
    /// A runtime [`gossamer_runtime::GossamerValue`] - the u64-packed shape the
    /// codegen uses for any non-scalar type (String, Tuple, Array,
    /// Struct, Variant, Closure, Channel). Aggregate values cross
    /// the JIT boundary as `gossamer_runtime::GossamerValue`
    /// handles; the trampoline marshals via
    /// `Value::to_raw` / `Value::from_raw`.
    Value,
    /// A heap enum crossing the boundary as its NATIVE tagged pointer
    /// (the compiled-tier representation the JIT body works with
    /// directly, zero conversion). The payload is the VM-side
    /// shape-table index used to re-wrap returned pointers. Integer
    /// register class.
    EnumPtr(u32),
    /// A `String` crossing the boundary as the runtime's native
    /// `*mut c_char` cstring pointer (the flat-ABI shape the codegen
    /// uses). The trampoline builds a fresh owned cstring from the VM
    /// String for a param, and reads back + frees a returned cstring.
    /// Integer register class.
    NativeStr,
    /// A `Vec<i64>` crossing the boundary as the runtime's native
    /// `*mut GosVec` pointer (8-byte primitive slots). The trampoline
    /// builds a fresh owned `GosVec` from the VM vec for a param, and
    /// reads back + frees a returned `GosVec`. Integer register class.
    NativeVecI64,
}

/// Raw handle for a JIT-compiled function: a fn pointer plus the
/// per-slot kinds that tell the dispatch trampoline how to marshal
/// arguments and the return value.
#[derive(Debug, Clone)]
pub struct JitFn {
    /// The Gossamer source name of the function. Mainly for
    /// `GOS_JIT_TRACE` diagnostics.
    pub name: String,
    /// Raw pointer to the entry of the compiled function. Valid for
    /// the lifetime of the owning [`JitArtifact`].
    pub ptr: *const u8,
    /// One [`JitKind`] per parameter, in source order.
    pub params: Vec<JitKind>,
    /// The return slot's kind.
    pub returns: JitKind,
}

// SAFETY: `ptr` is read-only from any thread, but the VM is
// single-threaded today. We do not implement Send/Sync for `JitFn`
// - anyone who copies it must keep it on the owning thread.

/// Owns a finalised [`JITModule`] and a name → [`JitFn`] map.
/// Dropping the artifact frees every page that backs the function
/// pointers it has handed out, so the VM must hold the artifact
/// for as long as any compiled fn is reachable.
pub struct JitArtifact {
    /// `Option` so [`Drop`] can call `JITModule::free_memory(self)`,
    /// which takes the module by value.
    module: Option<JITModule>,
    /// Compiled functions keyed by their Gossamer source name.
    pub functions: HashMap<String, JitFn>,
}

impl std::fmt::Debug for JitArtifact {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The `module` field is intentionally omitted - its
        // pointer-shaped `Debug` output churns across runs and
        // adds no signal. `finish_non_exhaustive` documents the
        // skip in a clippy-blessed way.
        f.debug_struct("JitArtifact")
            .field("functions", &self.functions.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

impl Drop for JitArtifact {
    fn drop(&mut self) {
        if let Some(module) = self.module.take() {
            // SAFETY: we have unique ownership of the JITModule (the
            // `Option::take` above is single-threaded), and the VM
            // promises to drop the artifact only after every JitFn
            // copy in its globals table has been flushed.
            unsafe { module.free_memory() };
        }
    }
}

/// Returns the names of user-defined bodies called by `body`.
/// User-function callees of `body`: by-name calls into known bodies
/// plus `FnRef` calls resolved through the def -> body-name map. The
/// second tuple slot reports an UNRESOLVABLE `FnRef` (a def with no
/// MIR body - e.g. a prelude scalar): such a body cannot be compiled
/// (the lowering refuses zero-stubs) and must be excluded.
fn body_user_calls<'a>(
    body: &'a Body,
    all_names: &std::collections::HashSet<&'a str>,
    def_to_name: &HashMap<u32, &'a str>,
) -> (Vec<&'a str>, bool) {
    use gossamer_mir::{ConstValue, Operand, Terminator};
    let mut calls = Vec::new();
    let mut unresolved = false;
    for block in &body.blocks {
        let Terminator::Call { callee, .. } = &block.terminator else {
            continue;
        };
        match callee {
            Operand::Const(ConstValue::Str(name)) if all_names.contains(name.as_str()) => {
                calls.push(name.as_str());
            }
            Operand::FnRef { def, .. } => match def_to_name.get(&def.local) {
                Some(name) => calls.push(name),
                None => unresolved = true,
            },
            _ => {}
        }
    }
    (calls, unresolved)
}

/// Computes the minimal set of body names needed in the JIT module.
///
/// Starts from bodies whose param/return types support JIT promotion
/// (scalar scalars only), then BFS-expands to include every user body
/// they transitively call - those need to be compiled too so that
/// intra-module call references resolve at finalize time.
fn jit_compile_set<'a>(
    bodies: &'a [Body],
    tcx: &TyCtxt,
    enum_shapes: &HashMap<u32, u32>,
) -> std::collections::HashSet<&'a str> {
    let all_names: std::collections::HashSet<&str> =
        bodies.iter().map(|b| b.name.as_str()).collect();
    let body_map: HashMap<&str, &Body> = bodies.iter().map(|b| (b.name.as_str(), b)).collect();
    let def_to_name: HashMap<u32, &str> = bodies
        .iter()
        .filter_map(|b| b.def.map(|d| (d.local, b.name.as_str())))
        .collect();

    // Bodies whose LOCALS include the two-word inline Option/Result
    // representation (sentinel Adt) or raw i128 integers are declined:
    // the JIT lowering's i64 register model fails cranelift's verifier
    // on them. They stay on bytecode; callees they invoke can still
    // compile (the trampoline marshals at the boundary).
    let uses_i128_repr = |b: &Body| -> bool {
        b.locals.iter().any(|l| match tcx.kind_of(l.ty) {
            // `String` and `Vec<_>` locals are marshalled across the JIT
            // boundary by the native-pointer bridge (see `ty_to_kind`),
            // so they never force a body onto bytecode.
            TyKind::String | TyKind::Vec(_) | TyKind::Slice(_) => false,
            // Inline Option/Result sentinels are two-word i128 values.
            // Non-enum Adts (structs) are also declined: the JIT-side
            // struct lowering is unexercised - bodies holding struct
            // locals stay on bytecode until that lands.
            TyKind::Adt { def, .. } => {
                def.local == u32::MAX || def.local == u32::MAX - 1 || !tcx.is_rc_managed(l.ty)
            }
            TyKind::Int(it) => {
                matches!(
                    it,
                    gossamer_types::IntTy::I128 | gossamer_types::IntTy::U128
                )
            }
            // A by-value tuple carrying an RC-managed element needs per-field
            // retain/release teardown at the JIT boundary, where the aggregate
            // is a marshalled handle rather than a native stack layout. Like
            // struct locals, such bodies stay on bytecode; LLVM AOT lowers the
            // per-field accounting natively.
            TyKind::Tuple(elems) => elems.iter().any(|t| {
                tcx.is_rc_managed(*t)
                    && !matches!(tcx.kind_of(*t), TyKind::Vec(_) | TyKind::Slice(_))
            }),
            _ => false,
        })
    };
    let mut included: std::collections::HashSet<&str> = bodies
        .iter()
        .filter(|b| body_kinds(b, tcx, enum_shapes).is_some() && !uses_i128_repr(b))
        .map(|b| b.name.as_str())
        .collect();

    let mut worklist: Vec<&str> = included.iter().copied().collect();
    while let Some(name) = worklist.pop() {
        let Some(body) = body_map.get(name) else {
            continue;
        };
        let (calls, _) = body_user_calls(body, &all_names, &def_to_name);
        for callee in calls {
            let ok = body_map.get(callee).is_some_and(|b| !uses_i128_repr(b));
            if ok && included.insert(callee) {
                worklist.push(callee);
            }
        }
    }
    // Exclude bodies the lowering would hard-fail on (an FnRef to a
    // def with no MIR body), and propagate: a caller of an excluded
    // body is itself uncompilable. One bad body must not fail the
    // whole module.
    loop {
        let mut removed = false;
        let snapshot: Vec<&str> = included.iter().copied().collect();
        for name in snapshot {
            let Some(body) = body_map.get(name) else {
                continue;
            };
            let (calls, unresolved) = body_user_calls(body, &all_names, &def_to_name);
            let calls_excluded = calls.iter().any(|c| !included.contains(c));
            if unresolved || calls_excluded {
                included.remove(name);
                removed = true;
            }
        }
        if !removed {
            break;
        }
    }
    included
}

/// Compiles every body in `bodies` through cranelift-jit and returns
/// the resulting handle table. Functions whose codegen path errors,
/// or whose ABI shape is not supported by the dispatch trampoline,
/// are silently skipped - the VM's existing bytecode dispatch picks
/// them up.
#[allow(
    clippy::implicit_hasher,
    reason = "single internal caller; generalizing the hasher adds a type parameter for nothing"
)]
pub fn compile_to_jit(
    bodies: &[Body],
    tcx: &TyCtxt,
    enum_shapes: &HashMap<u32, u32>,
) -> Result<JitArtifact> {
    let isa = build_native_isa(false)?;
    let mut builder = JITBuilder::with_isa(isa, cranelift_module::default_libcall_names());
    let mut runtime_symbol_set = register_runtime_symbols(&mut builder);
    // Register every `gos_binding_<...>` C-ABI thunk advertised by
    // the binding crates. Without these, `JITModule::finalize_definitions`
    // panics with `can't resolve symbol gos_binding_<...>` because
    // the default libloading-based resolver only sees the dynamic
    // symbol table, and Cargo binaries don't expose statically-linked
    // `pub extern "C"` symbols there.
    let leaked_binding_names = register_binding_symbols(&mut builder);
    runtime_symbol_set.extend(leaked_binding_names);
    let mut module = JITModule::new(builder);

    // Pre-filter: only compile bodies reachable from JIT-promotable roots.
    // Bodies whose param/return types can't be marshalled through the
    // trampoline (aggregates, closures) will never be promoted - compiling
    // them wastes Cranelift IR capacity and inflates peak RSS.  The BFS
    // below finds the transitive closure of user-function calls from the
    // promotable roots so inter-body calls inside the compiled set resolve.
    let compile_set = jit_compile_set(bodies, tcx, enum_shapes);
    // Clone only the bodies we'll actually compile. Skipping bodies
    // that can never be promoted (aggregate params/returns) saves
    // tens of megabytes of peak RSS without affecting correctness -
    // the bytecode VM handles any body the JIT doesn't cover.
    let filtered: Vec<Body> = bodies
        .iter()
        .filter(|b| compile_set.contains(b.name.as_str()))
        .cloned()
        .collect();

    // Rename the user's `main` to `gos_main` in the JIT's symbol
    // table. The host binary already exports `main` (the Rust
    // runtime's entry point); declaring a second `Linkage::Local`
    // `main` produced flaky SIGILLs on bring-up. The lookup map
    // we hand back to the VM keeps the original Gossamer name as
    // the key, so dispatch is unaffected.
    let lowered = lower_program(&mut module, &filtered, tcx, Some("gos_main"))?;

    module
        .finalize_definitions()
        .map_err(|e| anyhow!("jit finalize: {e}"))?;

    let body_name_set: std::collections::HashSet<&str> =
        filtered.iter().map(|b| b.name.as_str()).collect();
    let mut functions = HashMap::new();
    for body in &filtered {
        let Some(id) = lowered.function_ids_by_name.get(&body.name).copied() else {
            continue;
        };
        let Some((params, returns)) = body_kinds(body, tcx, enum_shapes) else {
            // Some param/return type isn't a primitive scalar - the
            // dispatch trampoline can't marshal it, so the VM will
            // fall back to bytecode for this fn.
            continue;
        };
        if body_calls_jit_unsafe(body, &runtime_symbol_set, &body_name_set) {
            // Body invokes something cranelift would lower as the
            // "soft-zero stub" at native.rs (~line 2099): unknown
            // by-name calls that aren't in the runtime symbol table
            // and aren't user-defined bodies. The stub silently
            // zeroes the destination, scrambling the program state.
            // Skip the JIT entry so the bytecode VM keeps semantics
            // intact for this function. The most common offenders
            // are closure-callback methods (`sort_by`, `sort_by_key`,
            // `map`, `filter`) plus any other user-facing helper
            // wired in the interpreter but not yet in the codegen
            // dispatch table.
            continue;
        }
        let ptr = module.get_finalized_function(id);
        functions.insert(
            body.name.clone(),
            JitFn {
                name: body.name.clone(),
                ptr,
                params,
                returns,
            },
        );
    }

    Ok(JitArtifact {
        module: Some(module),
        functions,
    })
}

/// Returns `true` when `body` contains a `Call(Const(Str(name)))`
/// whose `name` cranelift would lower as the "soft-zero stub"
/// (native.rs ~line 2099) - i.e. neither a registered runtime
/// symbol nor a user-defined body name nor a recognised
/// variant-constructor / qualified-path shape. The stub silently
/// zeroes the destination, so JIT-promoting such a body would
/// corrupt every program that exercises that call.
///
/// Variant constructors (`Ok`, `Err`, `Some`, `None`, user-defined
/// uppercase-starting names) and qualified paths (`std::…`,
/// `fmt::…`) get a dedicated shape in the cranelift backend at
/// native.rs ~line 2061 and are tolerated; only true unknowns
/// disqualify a body.
fn body_calls_jit_unsafe(
    body: &Body,
    runtime_symbols: &std::collections::HashSet<&'static str>,
    body_names: &std::collections::HashSet<&str>,
) -> bool {
    use gossamer_mir::{ConstValue, Operand, Terminator};
    for block in &body.blocks {
        let Terminator::Call { callee, .. } = &block.terminator else {
            continue;
        };
        let Operand::Const(ConstValue::Str(name)) = callee else {
            continue;
        };
        let n = name.as_str();
        if runtime_symbols.contains(n) {
            continue;
        }
        if body_names.contains(n) {
            continue;
        }
        // Variant-constructor / qualified-path shape: the cranelift
        // backend handles these explicitly (Ok/Err/Some pass-through;
        // anything qualified or capitalised falls through to a sound
        // zero-default semantics - uncommon but well-formed).
        let starts_uppercase = n.chars().next().is_some_and(char::is_uppercase);
        // Compiler-internal intrinsics (double-underscore prefix, e.g.
        // `__concat` for `format!`) always have a dedicated cranelift
        // lowering arm, so a body calling one is JIT-safe.
        if n.starts_with("__") || n.contains("::") || starts_uppercase {
            continue;
        }
        return true;
    }
    false
}

fn body_kinds(
    body: &Body,
    tcx: &TyCtxt,
    enum_shapes: &HashMap<u32, u32>,
) -> Option<(Vec<JitKind>, JitKind)> {
    let mut params = Vec::with_capacity(body.arity as usize);
    for pidx in 1..=body.arity {
        let local = gossamer_mir::Local(pidx);
        let kind = ty_to_kind(tcx, body.local_ty(local), enum_shapes)?;
        params.push(kind);
    }
    let returns = ty_to_kind(tcx, body.local_ty(gossamer_mir::Local::RETURN), enum_shapes)?;
    Some((params, returns))
}

fn ty_to_kind(tcx: &TyCtxt, ty: Ty, enum_shapes: &HashMap<u32, u32>) -> Option<JitKind> {
    // References to heap enums are the same native pointer at the ABI
    // (compiled convention) - peel before classifying.
    let mut ty = ty;
    while let TyKind::Ref { inner, .. } = tcx.kind_of(ty) {
        ty = *inner;
    }
    match tcx.kind_of(ty) {
        TyKind::Bool => Some(JitKind::Bool),
        TyKind::Int(_) => Some(JitKind::I64),
        TyKind::Float(_) => Some(JitKind::F64),
        TyKind::Unit => Some(JitKind::Unit),
        // Heap enums with a registered VM-side shape cross as native
        // tagged pointers; the body works on the compiled-tier
        // representation directly (zero conversion).
        TyKind::Adt { def, .. } if tcx.is_rc_managed(ty) => enum_shapes
            .get(&def.local)
            .map(|idx| JitKind::EnumPtr(*idx)),
        // `String` and `Vec<i64>` cross as the runtime's native pointer
        // (the flat-ABI shape `mir_ty_to_cabi` emits: a single pointer
        // slot). The trampoline builds a fresh runtime object from the
        // VM value at the call boundary and reclaims it after the call,
        // so the body sees the same `*mut c_char` / `*mut GosVec` shape
        // the AOT tier passes (zero in-body conversion).
        TyKind::String => Some(JitKind::NativeStr),
        TyKind::Vec(elem)
            if matches!(tcx.kind_of(*elem), TyKind::Int(gossamer_types::IntTy::I64)) =>
        {
            Some(JitKind::NativeVecI64)
        }
        // Remaining aggregates (`Tuple`, struct `Adt`, `Vec` of
        // non-`i64`, channels …) stay on bytecode: the trampoline has
        // no marshalling shape for them yet, and JIT-promoting them
        // risks segfaults at the boundary.
        _ => None,
    }
}

/// Registers every `gos_rt_*` C-ABI symbol the codegen may emit
/// against the JIT builder so that compiled bodies can call into the
/// runtime in-process. Kept in lock-step with the symbol set the
/// AOT object backend imports - anything the codegen knows how to
/// emit must resolve here.
///
/// Returns the set of registered symbol names so the JIT-eligibility
/// check can identify bodies that call something cranelift can't
/// resolve (and would silently lower to a typed-zero stub).
#[allow(
    clippy::too_many_lines,
    reason = "flat-shape dispatch / lowering - splitting hides the per-arm intent"
)]
fn register_runtime_symbols(builder: &mut JITBuilder) -> std::collections::HashSet<&'static str> {
    use gossamer_runtime::c_abi as rt;
    use gossamer_runtime::preempt;
    let mut names: std::collections::HashSet<&'static str> = std::collections::HashSet::new();
    macro_rules! reg {
        ($($name:literal => $f:path),* $(,)?) => {
            $(
                builder.symbol($name, $f as *const u8);
                names.insert($name);
            )*
        };
    }
    reg! {
        "gos_rt_set_args"            => rt::gos_rt_set_args,
        "gos_rt_os_args"             => rt::gos_rt_os_args,
        "gos_rt_arr_len"             => rt::gos_rt_arr_len,
        "gos_rt_len"                 => rt::gos_rt_len,
        "gos_rt_str_len"             => rt::gos_rt_str_len,
        "gos_rt_str_byte_at"         => rt::gos_rt_str_byte_at,
        "gos_rt_str_substring"       => rt::gos_rt_str_substring,
        "gos_rt_os_read_dir"         => rt::gos_rt_os_read_dir,
        "gos_rt_str_concat"          => rt::gos_rt_str_concat,
        "gos_rt_str_concat_drop_a"     => rt::gos_rt_str_concat_drop_a,
        "gos_rt_str_append_bytes"      => rt::gos_rt_str_append_bytes,
        "gos_rt_str_append_i64"        => rt::gos_rt_str_append_i64,
        "gos_rt_str_append_f64"        => rt::gos_rt_str_append_f64,
        "gos_rt_str_trim"            => rt::gos_rt_str_trim,
        "gos_rt_str_to_upper"        => rt::gos_rt_str_to_upper,
        "gos_rt_str_to_lower"        => rt::gos_rt_str_to_lower,
        "gos_rt_str_contains"        => rt::gos_rt_str_contains,
        "gos_rt_str_starts_with"     => rt::gos_rt_str_starts_with,
        "gos_rt_str_ends_with"       => rt::gos_rt_str_ends_with,
        "gos_rt_str_find"            => rt::gos_rt_str_find,
        "gos_rt_str_replace"         => rt::gos_rt_str_replace,
        "gos_rt_str_split"           => rt::gos_rt_str_split,
        "gos_rt_str_split_once"      => rt::gos_rt_str_split_once,
        "gos_rt_str_rsplit_once"     => rt::gos_rt_str_rsplit_once,
        "gos_rt_str_count"           => rt::gos_rt_str_count,
        "gos_rt_str_chars"           => rt::gos_rt_str_chars,
        "gos_rt_str_strip_chars"     => rt::gos_rt_str_strip_chars,
        "gos_rt_str_lstrip_chars"    => rt::gos_rt_str_lstrip_chars,
        "gos_rt_str_rstrip_chars"    => rt::gos_rt_str_rstrip_chars,
        "gos_rt_str_zfill"           => rt::gos_rt_str_zfill,
        "gos_rt_str_center"          => rt::gos_rt_str_center,
        "gos_rt_str_slice"           => rt::gos_rt_str_slice,
        "gos_rt_str_rfind_opt"       => rt::gos_rt_str_rfind_opt,
        "gos_rt_str_lines"           => rt::gos_rt_str_lines,
        "gos_rt_str_push_char"       => rt::gos_rt_str_push_char,
        "gos_rt_str_push_byte"       => rt::gos_rt_str_push_byte,
        "gos_rt_deque_new"           => rt::gos_rt_deque_new,
        "gos_rt_deque_push_back"     => rt::gos_rt_deque_push_back,
        "gos_rt_deque_pop_front"     => rt::gos_rt_deque_pop_front,
        "gos_rt_deque_len"           => rt::gos_rt_deque_len,
        "gos_rt_deque_is_empty"      => rt::gos_rt_deque_is_empty,
        "gos_rt_deque_free"          => rt::gos_rt_deque_free,
        "gos_rt_strings_join"        => rt::gos_rt_strings_join,
        "gos_rt_path_base"           => rt::gos_rt_path_base,
        "gos_rt_path_dir"            => rt::gos_rt_path_dir,
        "gos_rt_path_ext"            => rt::gos_rt_path_ext,
        "gos_rt_vec_first"           => rt::gos_rt_vec_first,
        "gos_rt_vec_last"            => rt::gos_rt_vec_last,
        "gos_rt_vec_reversed"        => rt::gos_rt_vec_reversed,
        "gos_rt_vec_index_of_i64"    => rt::gos_rt_vec_index_of_i64,
        "gos_rt_vec_index_of_str"    => rt::gos_rt_vec_index_of_str,
        "gos_rt_vec_count_of_i64"    => rt::gos_rt_vec_count_of_i64,
        "gos_rt_vec_count_of_str"    => rt::gos_rt_vec_count_of_str,
        "gos_rt_vec_contains_i64"    => rt::gos_rt_vec_contains_i64,
        "gos_rt_vec_contains_str"    => rt::gos_rt_vec_contains_str,
        "gos_rt_vec_slice_result"    => rt::gos_rt_vec_slice_result,
        "gos_rt_intarr_slice_result" => rt::gos_rt_intarr_slice_result,
        "gos_rt_floatarr_slice_result" => rt::gos_rt_floatarr_slice_result,
        "gos_rt_vec_insert_safe"     => rt::gos_rt_vec_insert_safe,
        "gos_rt_vec_remove_safe"     => rt::gos_rt_vec_remove_safe,
        "gos_rt_map_keys_vec"        => rt::gos_rt_map_keys_vec,
        "gos_rt_map_values_vec"      => rt::gos_rt_map_values_vec,
        "gos_rt_map_pop_i64"         => rt::gos_rt_map_pop_i64,
        "gos_rt_map_pop_str"         => rt::gos_rt_map_pop_str,
        "gos_rt_min_i64"             => rt::gos_rt_min_i64,
        "gos_rt_max_i64"             => rt::gos_rt_max_i64,
        "gos_rt_clamp_i64"           => rt::gos_rt_clamp_i64,
        "gos_rt_min_f64"             => rt::gos_rt_min_f64,
        "gos_rt_max_f64"             => rt::gos_rt_max_f64,
        "gos_rt_clamp_f64"           => rt::gos_rt_clamp_f64,
        "gos_rt_str_repeat"          => rt::gos_rt_str_repeat,
        "gos_rt_str_eq"              => rt::gos_rt_str_eq,
        "gos_rt_str_compare"         => rt::gos_rt_str_compare,
        "gos_rt_str_is_empty"        => rt::gos_rt_str_is_empty,
        "gos_rt_str_free"            => rt::gos_rt_str_free,
        "gos_rt_len_is_zero"         => rt::gos_rt_len_is_zero,
        "gos_rt_error_new"           => rt::gos_rt_error_new,
        "gos_rt_error_from"          => rt::gos_rt_error_from,
        "gos_rt_error_wrap"          => rt::gos_rt_error_wrap,
        "gos_rt_error_message"       => rt::gos_rt_error_message,
        "gos_rt_error_display"       => rt::gos_rt_error_display,
        "gos_rt_error_cause"         => rt::gos_rt_error_cause,
        "gos_rt_error_is"            => rt::gos_rt_error_is,
        "gos_rt_regex_compile"       => rt::gos_rt_regex_compile,
        "gos_rt_regex_is_match"      => rt::gos_rt_regex_is_match,
        "gos_rt_regex_find"          => rt::gos_rt_regex_find,
        "gos_rt_regex_find_opt"      => rt::gos_rt_regex_find_opt,
        "gos_rt_regex_captures"      => rt::gos_rt_regex_captures,
        "gos_rt_regex_find_all"      => rt::gos_rt_regex_find_all,
        "gos_rt_regex_replace_all"   => rt::gos_rt_regex_replace_all,
        "gos_rt_regex_split"         => rt::gos_rt_regex_split,
        "gos_rt_fs_read_to_string"   => rt::gos_rt_fs_read_to_string,
        "gos_rt_fs_write"            => rt::gos_rt_fs_write,
        "gos_rt_fs_create_dir_all"   => rt::gos_rt_fs_create_dir_all,
        "gos_rt_path_join"           => rt::gos_rt_path_join,
        "gos_rt_flag_set_new"        => rt::gos_rt_flag_set_new,
        "gos_rt_flag_set_string"     => rt::gos_rt_flag_set_string,
        "gos_rt_flag_set_int"        => rt::gos_rt_flag_set_int,
        "gos_rt_flag_set_uint"       => rt::gos_rt_flag_set_uint,
        "gos_rt_flag_set_float"      => rt::gos_rt_flag_set_float,
        "gos_rt_flag_set_bool"       => rt::gos_rt_flag_set_bool,
        "gos_rt_flag_set_duration"   => rt::gos_rt_flag_set_duration,
        "gos_rt_flag_set_string_list" => rt::gos_rt_flag_set_string_list,
        "gos_rt_flag_set_short"      => rt::gos_rt_flag_set_short,
        "gos_rt_flag_set_usage"      => rt::gos_rt_flag_set_usage,
        "gos_rt_flag_set_parse"      => rt::gos_rt_flag_set_parse,
        "gos_rt_duration_from_secs"  => rt::gos_rt_duration_from_secs,
        "gos_rt_duration_from_millis" => rt::gos_rt_duration_from_millis,
        "gos_rt_time_format_rfc3339" => rt::gos_rt_time_format_rfc3339,
        "gos_rt_flag_parse"          => rt::gos_rt_flag_parse,
        "gos_rt_flag_map_get"        => rt::gos_rt_flag_map_get,
        "gos_rt_os_env"              => rt::gos_rt_os_env,
        "gos_rt_os_cwd"              => rt::gos_rt_os_cwd,
        "gos_rt_fs_list_dir"         => rt::gos_rt_fs_list_dir,
        "gos_rt_fs_walk_dir"         => rt::gos_rt_fs_walk_dir,
        "gos_rt_exec_run"            => rt::gos_rt_exec_run,
        "gos_rt_exec_spawn"          => rt::gos_rt_exec_spawn,
        "gos_rt_exec_kill"           => rt::gos_rt_exec_kill,
        "gos_rt_exec_signal"         => rt::gos_rt_exec_signal,
        "gos_rt_exec_kill_group"     => rt::gos_rt_exec_kill_group,
        "gos_rt_exec_wait_timeout"   => rt::gos_rt_exec_wait_timeout,
        "gos_rt_exec_pipeline_run"   => rt::gos_rt_exec_pipeline_run,
        "gos_rt_signal_on"           => rt::gos_rt_signal_on,
        "gos_rt_signal_wait"         => rt::gos_rt_signal_wait,
        "gos_rt_signal_try_wait"     => rt::gos_rt_signal_try_wait,
        "gos_rt_os_set_env"          => rt::gos_rt_os_set_env,
        "gos_rt_os_unset_env"        => rt::gos_rt_os_unset_env,
        "gos_rt_os_user_current_name" => rt::gos_rt_os_user_current_name,
        "gos_rt_os_user_current_uid" => rt::gos_rt_os_user_current_uid,
        "gos_rt_os_user_current_gid" => rt::gos_rt_os_user_current_gid,
        "gos_rt_os_user_current_home" => rt::gos_rt_os_user_current_home,
        "gos_rt_os_user_lookup_uid"  => rt::gos_rt_os_user_lookup_uid,
        "gos_rt_os_user_lookup_name" => rt::gos_rt_os_user_lookup_name,
        "gos_rt_netip_is_valid"      => rt::gos_rt_netip_is_valid,
        "gos_rt_netip_is_v4"         => rt::gos_rt_netip_is_v4,
        "gos_rt_netip_is_v6"         => rt::gos_rt_netip_is_v6,
        "gos_rt_netip_is_loopback"   => rt::gos_rt_netip_is_loopback,
        "gos_rt_netip_is_unspecified" => rt::gos_rt_netip_is_unspecified,
        "gos_rt_netip_is_multicast"  => rt::gos_rt_netip_is_multicast,
        "gos_rt_netip_is_private"    => rt::gos_rt_netip_is_private,
        "gos_rt_netip_normalize"     => rt::gos_rt_netip_normalize,
        "gos_rt_netip_host_of"       => rt::gos_rt_netip_host_of,
        "gos_rt_netip_port_of"       => rt::gos_rt_netip_port_of,
        "gos_rt_netip_join_addr_port" => rt::gos_rt_netip_join_addr_port,
        "gos_rt_mime_parse"          => rt::gos_rt_mime_parse,
        "gos_rt_mime_top"            => rt::gos_rt_mime_top,
        "gos_rt_mime_sub"            => rt::gos_rt_mime_sub,
        "gos_rt_mime_charset"        => rt::gos_rt_mime_charset,
        "gos_rt_mime_boundary"       => rt::gos_rt_mime_boundary,
        "gos_rt_mime_param"          => rt::gos_rt_mime_param,
        "gos_rt_mime_type_by_extension" => rt::gos_rt_mime_type_by_extension,
        "gos_rt_mime_extension_by_type" => rt::gos_rt_mime_extension_by_type,
        "gos_rt_mime_is_valid"       => rt::gos_rt_mime_is_valid,
        "gos_rt_toml_to_json"        => rt::gos_rt_toml_to_json,
        "gos_rt_toml_from_json"      => rt::gos_rt_toml_from_json,
        "gos_rt_toml_is_valid"       => rt::gos_rt_toml_is_valid,
        "gos_rt_toml_pretty"         => rt::gos_rt_toml_pretty,
        "gos_rt_yaml_to_json"        => rt::gos_rt_yaml_to_json,
        "gos_rt_yaml_from_json"      => rt::gos_rt_yaml_from_json,
        "gos_rt_yaml_is_valid"       => rt::gos_rt_yaml_is_valid,
        "gos_rt_sync_map_new"        => rt::gos_rt_sync_map_new,
        "gos_rt_sync_map_set"        => rt::gos_rt_sync_map_set,
        "gos_rt_sync_map_get"        => rt::gos_rt_sync_map_get,
        "gos_rt_sync_map_delete"     => rt::gos_rt_sync_map_delete,
        "gos_rt_sync_map_len"        => rt::gos_rt_sync_map_len,
        "gos_rt_sync_map_contains"   => rt::gos_rt_sync_map_contains,
        "gos_rt_sync_map_keys"       => rt::gos_rt_sync_map_keys,
        "gos_rt_barrier_new"         => rt::gos_rt_barrier_new,
        "gos_rt_barrier_wait"        => rt::gos_rt_barrier_wait,
        "gos_rt_once_new"            => rt::gos_rt_once_new,
        "gos_rt_once_call"           => rt::gos_rt_once_call,
        "gos_rt_math_rng_new"        => rt::gos_rt_math_rng_new,
        "gos_rt_math_rng_next_f64"   => rt::gos_rt_math_rng_next_f64,
        "gos_rt_math_rng_next_u32"   => rt::gos_rt_math_rng_next_u32,
        "gos_rt_math_rng_next_u64"   => rt::gos_rt_math_rng_next_u64,
        "gos_rt_math_rng_range_u64"  => rt::gos_rt_math_rng_range_u64,
        "gos_rt_bytes_builder_new"   => rt::gos_rt_bytes_builder_new,
        "gos_rt_bytes_builder_with_capacity" => rt::gos_rt_bytes_builder_with_capacity,
        "gos_rt_bytes_builder_write" => rt::gos_rt_bytes_builder_write,
        "gos_rt_bytes_builder_write_char" => rt::gos_rt_bytes_builder_write_char,
        "gos_rt_bytes_builder_build" => rt::gos_rt_bytes_builder_build,
        "gos_rt_bytes_builder_as_str" => rt::gos_rt_bytes_builder_as_str,
        "gos_rt_bytes_builder_len"   => rt::gos_rt_bytes_builder_len,
        "gos_rt_bytes_buffer_new"    => rt::gos_rt_bytes_buffer_new,
        "gos_rt_bytes_buffer_with_capacity" => rt::gos_rt_bytes_buffer_with_capacity,
        "gos_rt_bytes_buffer_write_str" => rt::gos_rt_bytes_buffer_write_str,
        "gos_rt_bytes_buffer_push"   => rt::gos_rt_bytes_buffer_push,
        "gos_rt_bytes_buffer_len"    => rt::gos_rt_bytes_buffer_len,
        "gos_rt_bytes_buffer_is_empty" => rt::gos_rt_bytes_buffer_is_empty,
        "gos_rt_bytes_buffer_clear"  => rt::gos_rt_bytes_buffer_clear,
        "gos_rt_bytes_buffer_to_string" => rt::gos_rt_bytes_buffer_to_string,
        "gos_rt_bytes_split"         => rt::gos_rt_bytes_split,
        "gos_rt_bytes_replace"       => rt::gos_rt_bytes_replace,
        "gos_rt_net_ip_octets"       => rt::gos_rt_net_ip_octets,
        "gos_rt_tcp_listener_close"  => rt::gos_rt_tcp_listener_close,
        "gos_rt_tcp_stream_close"    => rt::gos_rt_tcp_stream_close,
        "gos_rt_udp_close"           => rt::gos_rt_udp_close,
        "gos_rt_field_error_new" => rt::gos_rt_field_error_new,
        "gos_rt_field_error_path" => rt::gos_rt_field_error_path,
        "gos_rt_field_error_message" => rt::gos_rt_field_error_message,
        "gos_rt_field_error_code" => rt::gos_rt_field_error_code,
        "gos_rt_validate_errors_new" => rt::gos_rt_validate_errors_new,
        "gos_rt_validate_errors_add" => rt::gos_rt_validate_errors_add,
        "gos_rt_validate_errors_is_empty" => rt::gos_rt_validate_errors_is_empty,
        "gos_rt_validate_errors_len" => rt::gos_rt_validate_errors_len,
        "gos_rt_validate_errors_count" => rt::gos_rt_validate_errors_count,
        "gos_rt_validate_errors_get" => rt::gos_rt_validate_errors_get,
        "gos_rt_validate_errors_collect" => rt::gos_rt_validate_errors_collect,
        "gos_rt_rwlock_new" => rt::gos_rt_rwlock_new,
        "gos_rt_rwlock_get" => rt::gos_rt_rwlock_get,
        "gos_rt_rwlock_set" => rt::gos_rt_rwlock_set,
        "gos_rt_rwlock_with_read" => rt::gos_rt_rwlock_with_read,
        "gos_rt_rwlock_with_write" => rt::gos_rt_rwlock_with_write,
        "gos_rt_ctx_background" => rt::gos_rt_ctx_background,
        "gos_rt_ctx_with_cancel" => rt::gos_rt_ctx_with_cancel,
        "gos_rt_ctx_with_timeout" => rt::gos_rt_ctx_with_timeout,
        "gos_rt_ctx_cancel" => rt::gos_rt_ctx_cancel,
        "gos_rt_ctx_cancelled" => rt::gos_rt_ctx_cancelled,
        "gos_rt_ctx_is_cancelled" => rt::gos_rt_ctx_is_cancelled,
        "gos_rt_ctx_done" => rt::gos_rt_ctx_done,
        "gos_rt_metrics_counter_new" => rt::gos_rt_metrics_counter_new,
        "gos_rt_metrics_counter_inc" => rt::gos_rt_metrics_counter_inc,
        "gos_rt_metrics_counter_value" => rt::gos_rt_metrics_counter_value,
        "gos_rt_metrics_gauge_new" => rt::gos_rt_metrics_gauge_new,
        "gos_rt_metrics_gauge_set" => rt::gos_rt_metrics_gauge_set,
        "gos_rt_metrics_gauge_inc" => rt::gos_rt_metrics_gauge_inc,
        "gos_rt_metrics_gauge_dec" => rt::gos_rt_metrics_gauge_dec,
        "gos_rt_metrics_gauge_value" => rt::gos_rt_metrics_gauge_value,
        "gos_rt_metrics_histogram_new" => rt::gos_rt_metrics_histogram_new,
        "gos_rt_metrics_histogram_observe" => rt::gos_rt_metrics_histogram_observe,
        "gos_rt_metrics_histogram_sum" => rt::gos_rt_metrics_histogram_sum,
        "gos_rt_metrics_histogram_count" => rt::gos_rt_metrics_histogram_count,
        "gos_rt_metrics_registry_new" => rt::gos_rt_metrics_registry_new,
        "gos_rt_metrics_registry_register" => rt::gos_rt_metrics_registry_register,
        "gos_rt_metrics_registry_render" => rt::gos_rt_metrics_registry_render,
        "gos_rt_middleware_new"      => rt::gos_rt_middleware_new,
        "gos_rt_middleware_serve"    => rt::gos_rt_middleware_serve,
        "gos_rt_trace_tracer_new" => rt::gos_rt_trace_tracer_new,
        "gos_rt_trace_tracer_start_span" => rt::gos_rt_trace_tracer_start_span,
        "gos_rt_trace_span_set_attribute" => rt::gos_rt_trace_span_set_attribute,
        "gos_rt_trace_span_set_status" => rt::gos_rt_trace_span_set_status,
        "gos_rt_trace_span_end" => rt::gos_rt_trace_span_end,
        "gos_rt_trace_ended_to_otlp_json" => rt::gos_rt_trace_ended_to_otlp_json,
        "gos_rt_bheap_push_i64"      => rt::gos_rt_bheap_push_i64,
        "gos_rt_bheap_pop_i64"       => rt::gos_rt_bheap_pop_i64,
        "gos_rt_bheap_peek_i64"      => rt::gos_rt_bheap_peek_i64,
        "gos_rt_bheap_len"           => rt::gos_rt_bheap_len,
        "gos_rt_vec_first_i64"       => rt::gos_rt_vec_first_i64,
        "gos_rt_vec_last_i64"        => rt::gos_rt_vec_last_i64,
        "gos_rt_vec_pop_front_i64"   => rt::gos_rt_vec_pop_front_i64,
        "gos_rt_vec_pop_back_i64"    => rt::gos_rt_vec_pop_back_i64,
        "gos_rt_vec_push_front_i64"  => rt::gos_rt_vec_push_front_i64,
        "gos_rt_vec_push_back_i64"   => rt::gos_rt_vec_push_back_i64,
        "gos_rt_ovec_insert_i64"     => rt::gos_rt_ovec_insert_i64,
        "gos_rt_ovec_remove_at_i64"  => rt::gos_rt_ovec_remove_at_i64,
        "gos_rt_ovec_contains_i64"   => rt::gos_rt_ovec_contains_i64,
        "gos_rt_ovec_index_of_i64"   => rt::gos_rt_ovec_index_of_i64,
        "gos_rt_oset_insert_i64"     => rt::gos_rt_oset_insert_i64,
        "gos_rt_oset_remove_i64"     => rt::gos_rt_oset_remove_i64,
        "gos_rt_oset_contains_i64"   => rt::gos_rt_oset_contains_i64,
        "gos_rt_omap_insert_i64"     => rt::gos_rt_omap_insert_i64,
        "gos_rt_omap_remove_i64"     => rt::gos_rt_omap_remove_i64,
        "gos_rt_omap_get_i64"        => rt::gos_rt_omap_get_i64,
        "gos_rt_omap_contains_key_i64" => rt::gos_rt_omap_contains_key_i64,
        "gos_rt_omap_len"            => rt::gos_rt_omap_len,
        "gos_rt_url_query_escape"    => rt::gos_rt_url_query_escape,
        "gos_rt_url_path_escape"     => rt::gos_rt_url_path_escape,
        "gos_rt_url_query_unescape"  => rt::gos_rt_url_query_unescape,
        "gos_rt_url_path_unescape"   => rt::gos_rt_url_path_unescape,
        "gos_rt_os_exists"           => rt::gos_rt_os_exists,
        "gos_rt_os_is_file"          => rt::gos_rt_os_is_file,
        "gos_rt_os_is_dir"           => rt::gos_rt_os_is_dir,
        "gos_rt_os_is_symlink"       => rt::gos_rt_os_is_symlink,
        "gos_rt_os_file_size"        => rt::gos_rt_os_file_size,
        "gos_rt_os_remove_file"      => rt::gos_rt_os_remove_file,
        "gos_rt_result_map_bare"     => rt::gos_rt_result_map_bare,
        "gos_rt_result_map_err_bare" => rt::gos_rt_result_map_err_bare,
        "gos_rt_os_write_file_result" => rt::gos_rt_os_write_file_result,
        "gos_rt_os_write_file_bytes_result" => rt::gos_rt_os_write_file_bytes_result,
        "gos_rt_fs_read_bytes_result" => rt::gos_rt_fs_read_bytes_result,
        "gos_rt_os_mkdir_all_result"  => rt::gos_rt_os_mkdir_all_result,
        "gos_rt_os_remove_file_result" => rt::gos_rt_os_remove_file_result,
        "gos_rt_os_remove_dir_all_result" => rt::gos_rt_os_remove_dir_all_result,
        "gos_rt_http_stream"         => rt::gos_rt_http_stream,
        "gos_rt_http_get"            => rt::gos_rt_http_get,
        "gos_rt_http_head"           => rt::gos_rt_http_head,
        "gos_rt_http_options"        => rt::gos_rt_http_options,
        "gos_rt_http_post"           => rt::gos_rt_http_post,
        "gos_rt_http_put"            => rt::gos_rt_http_put,
        "gos_rt_http_delete"         => rt::gos_rt_http_delete,
        "gos_rt_http_request"        => rt::gos_rt_http_request,
        "gos_rt_http_request_bytes"  => rt::gos_rt_http_request_bytes,
        "gos_rt_http_stream_next_line" => rt::gos_rt_http_stream_next_line,
        "gos_rt_http_stream_next_chunk" => rt::gos_rt_http_stream_next_chunk,
        "gos_rt_bufio_scanner_new"   => rt::gos_rt_bufio_scanner_new,
        "gos_rt_bufio_scanner_scan"  => rt::gos_rt_bufio_scanner_scan,
        "gos_rt_bufio_scanner_text"  => rt::gos_rt_bufio_scanner_text,
        "gos_rt_http_client_new"     => rt::gos_rt_http_client_new,
        "gos_rt_http_client_get"     => rt::gos_rt_http_client_get,
        "gos_rt_http_client_post"    => rt::gos_rt_http_client_post,
        "gos_rt_http_client_put"     => rt::gos_rt_http_client_put,
        "gos_rt_http_client_options" => rt::gos_rt_http_client_options,
        "gos_rt_http_client_delete"  => rt::gos_rt_http_client_delete,
        "gos_rt_http_client_head"    => rt::gos_rt_http_client_head,
        "gos_rt_http_client_builder_new" => rt::gos_rt_http_client_builder_new,
        "gos_rt_http_client_builder_max_redirects" => rt::gos_rt_http_client_builder_max_redirects,
        "gos_rt_http_client_builder_timeout_ms" => rt::gos_rt_http_client_builder_timeout_ms,
        "gos_rt_http_client_builder_cookie_jar" => rt::gos_rt_http_client_builder_cookie_jar,
        "gos_rt_http_client_builder_proxy" => rt::gos_rt_http_client_builder_proxy,
        "gos_rt_http_client_builder_build" => rt::gos_rt_http_client_builder_build,
        "gos_rt_http_client_request" => rt::gos_rt_http_client_request,
        "gos_rt_http_client_request_bytes" => rt::gos_rt_http_client_request_bytes,
        "gos_rt_http_request_header" => rt::gos_rt_http_request_header,
        "gos_rt_http_request_body"   => rt::gos_rt_http_request_body,
        "gos_rt_http_request_send"   => rt::gos_rt_http_request_send,
        "gos_rt_http_response_status" => rt::gos_rt_http_response_status,
        "gos_rt_http_response_body"  => rt::gos_rt_http_response_body,
        "gos_rt_http_response_raw_bytes" => rt::gos_rt_http_response_raw_bytes,
        "gos_rt_http_response_headers" => rt::gos_rt_http_response_headers,
        "gos_rt_http_response_content_type" => rt::gos_rt_http_response_content_type,
        "gos_rt_http_response_location" => rt::gos_rt_http_response_location,
        "gos_rt_vec_get_i64"         => rt::gos_rt_vec_get_i64,
        "gos_rt_vec_set_i64"         => rt::gos_rt_vec_set_i64,
        "gos_rt_vec_format_i64"      => rt::gos_rt_vec_format_i64,
        "gos_rt_vec_format_f64"      => rt::gos_rt_vec_format_f64,
        "gos_rt_vec_format_bool"     => rt::gos_rt_vec_format_bool,
        "gos_rt_vec_format_string"   => rt::gos_rt_vec_format_string,
        "gos_rt_vec_format_vec_i64"  => rt::gos_rt_vec_format_vec_i64,
        "gos_rt_vec_format_vec_string" => rt::gos_rt_vec_format_vec_string,
        "gos_rt_tuple_format"        => rt::gos_rt_tuple_format,
        "gos_rt_concat_init"         => rt::gos_rt_concat_init,
        "gos_rt_concat_str"          => rt::gos_rt_concat_str,
        "gos_rt_concat_i64"          => rt::gos_rt_concat_i64,
        "gos_rt_concat_u64"          => rt::gos_rt_concat_u64,
        "gos_rt_concat_f64"          => rt::gos_rt_concat_f64,
        "gos_rt_concat_f64_prec"     => rt::gos_rt_concat_f64_prec,
        "gos_rt_concat_bool"         => rt::gos_rt_concat_bool,
        "gos_rt_concat_char"         => rt::gos_rt_concat_char,
        "gos_rt_concat_finish"       => rt::gos_rt_concat_finish,
        "gos_rt_main_exit_code"      => rt::gos_rt_main_exit_code,
        "gos_rt_result_new"          => rt::gos_rt_result_new,
        "gos_rt_result_disc"         => rt::gos_rt_result_disc,
        "gos_rt_result_payload"      => rt::gos_rt_result_payload,
        "gos_rt_result_unwrap"       => rt::gos_rt_result_unwrap,
        "gos_rt_result_unwrap_or"    => rt::gos_rt_result_unwrap_or,
        "gos_rt_result_ok"           => rt::gos_rt_result_ok,
        "gos_rt_result_err"          => rt::gos_rt_result_err,
        "gos_rt_result_ok_or"        => rt::gos_rt_result_ok_or,
        "gos_rt_result_is_ok"        => rt::gos_rt_result_is_ok,
        "gos_rt_result_is_err"       => rt::gos_rt_result_is_err,
        "gos_rt_set_new"             => rt::gos_rt_set_new,
        "gos_rt_set_insert"          => rt::gos_rt_set_insert,
        "gos_rt_set_contains"        => rt::gos_rt_set_contains,
        "gos_rt_set_remove"          => rt::gos_rt_set_remove,
        "gos_rt_set_len"             => rt::gos_rt_set_len,
        "gos_rt_set_union"           => rt::gos_rt_set_union,
        "gos_rt_set_intersection"    => rt::gos_rt_set_intersection,
        "gos_rt_set_difference"      => rt::gos_rt_set_difference,
        "gos_rt_set_symmetric_difference" => rt::gos_rt_set_symmetric_difference,
        "gos_rt_set_is_subset"       => rt::gos_rt_set_is_subset,
        "gos_rt_set_is_superset"     => rt::gos_rt_set_is_superset,
        "gos_rt_set_is_disjoint"     => rt::gos_rt_set_is_disjoint,
        "gos_rt_btmap_new"           => rt::gos_rt_btmap_new,
        "gos_rt_btmap_insert"        => rt::gos_rt_btmap_insert,
        "gos_rt_btmap_get_or"        => rt::gos_rt_btmap_get_or,
        "gos_rt_btmap_len"           => rt::gos_rt_btmap_len,
        "gos_rt_btmap_keys"          => rt::gos_rt_btmap_keys,
        "gos_rt_str_as_bytes"        => rt::gos_rt_str_as_bytes,
        "gos_rt_regex_captures_all"  => rt::gos_rt_regex_captures_all,
        "gos_rt_vec_clone"           => rt::gos_rt_vec_clone,
        "gos_rt_map_inc_str_i64"        => rt::gos_rt_map_inc_str_i64,
        "gos_rt_map_or_insert_str_i64"  => rt::gos_rt_map_or_insert_str_i64,
        "gos_rt_map_or_insert_i64_i64"  => rt::gos_rt_map_or_insert_i64_i64,
        "gos_rt_errors_join"            => rt::gos_rt_errors_join,
        "gos_rt_errors_join_vec"        => rt::gos_rt_errors_join_vec,
        "gos_rt_json_value_object_n"    => rt::gos_rt_json_value_object_n,
        "gos_rt_http_response_set_header" => rt::gos_rt_http_response_set_header,
        "gos_rt_http_response_set_content_type" => rt::gos_rt_http_response_set_content_type,
        "gos_rt_http_response_set_body_bytes" => rt::gos_rt_http_response_set_body_bytes,
        "gos_rt_http_response_with_header" => rt::gos_rt_http_response_with_header,
        "gos_rt_http_response_get_header" => rt::gos_rt_http_response_get_header,
        "gos_rt_http_request_set_header" => rt::gos_rt_http_request_set_header,
        "gos_rt_http_request_get_header" => rt::gos_rt_http_request_get_header,
        "gos_rt_http_request_path"   => rt::gos_rt_http_request_path,
        "gos_rt_http_request_method" => rt::gos_rt_http_request_method,
        "gos_rt_http_request_query"  => rt::gos_rt_http_request_query,
        "gos_rt_http_request_headers" => rt::gos_rt_http_request_headers,
        "gos_rt_http_request_body_str" => rt::gos_rt_http_request_body_str,
        "gos_rt_http_request_raw_body" => rt::gos_rt_http_request_raw_body,
        "gos_rt_http_response_text_new" => rt::gos_rt_http_response_text_new,
        "gos_rt_http_response_json_new" => rt::gos_rt_http_response_json_new,
        "gos_rt_http_response_stream_new" => rt::gos_rt_http_response_stream_new,
        "gos_rt_gzip_encode"         => rt::gos_rt_gzip_encode,
        "gos_rt_gzip_decode"         => rt::gos_rt_gzip_decode,
        "gos_rt_sha256_hex"          => rt::gos_rt_sha256_hex,
        "gos_rt_sha512_hex"          => rt::gos_rt_sha512_hex,
        "gos_rt_blake3_hex"          => rt::gos_rt_blake3_hex,
        "gos_rt_hmac_sha256_hex"     => rt::gos_rt_hmac_sha256_hex,
        "gos_rt_slog_info"           => rt::gos_rt_slog_info,
        "gos_rt_slog_warn"           => rt::gos_rt_slog_warn,
        "gos_rt_slog_error"          => rt::gos_rt_slog_error,
        "gos_rt_slog_debug"          => rt::gos_rt_slog_debug,
        // std::database::sql C-ABI shims.
        "gos_rt_sql_value_null"             => rt::sql::gos_rt_sql_value_null,
        "gos_rt_sql_value_bool"             => rt::sql::gos_rt_sql_value_bool,
        "gos_rt_sql_value_int"              => rt::sql::gos_rt_sql_value_int,
        "gos_rt_sql_value_float"            => rt::sql::gos_rt_sql_value_float,
        "gos_rt_sql_value_text"             => rt::sql::gos_rt_sql_value_text,
        "gos_rt_sql_open"                   => rt::sql::gos_rt_sql_open,
        "gos_rt_sql_drivers"                => rt::sql::gos_rt_sql_drivers,
        "gos_rt_sql_conn_execute"           => rt::sql::gos_rt_sql_conn_execute,
        "gos_rt_sql_conn_query"             => rt::sql::gos_rt_sql_conn_query,
        "gos_rt_sql_conn_begin"             => rt::sql::gos_rt_sql_conn_begin,
        "gos_rt_sql_conn_begin_with"        => rt::sql::gos_rt_sql_conn_begin_with,
        "gos_rt_sql_conn_ping"              => rt::sql::gos_rt_sql_conn_ping,
        "gos_rt_sql_conn_set_busy_timeout"  => rt::sql::gos_rt_sql_conn_set_busy_timeout,
        "gos_rt_sql_conn_interrupt"         => rt::sql::gos_rt_sql_conn_interrupt,
        "gos_rt_sql_rows_next_row"          => rt::sql::gos_rt_sql_rows_next_row,
        "gos_rt_sql_rows_close"             => rt::sql::gos_rt_sql_rows_close,
        "gos_rt_sql_rows_columns"           => rt::sql::gos_rt_sql_rows_columns,
        "gos_rt_sql_row_get_i64"            => rt::sql::gos_rt_sql_row_get_i64,
        "gos_rt_sql_row_get_f64"            => rt::sql::gos_rt_sql_row_get_f64,
        "gos_rt_sql_row_get_bool"           => rt::sql::gos_rt_sql_row_get_bool,
        "gos_rt_sql_row_get_text"           => rt::sql::gos_rt_sql_row_get_text,
        "gos_rt_sql_row_get_blob"           => rt::sql::gos_rt_sql_row_get_blob,
        "gos_rt_sql_row_get_opt_i64"        => rt::sql::gos_rt_sql_row_get_opt_i64,
        "gos_rt_sql_row_get_opt_f64"        => rt::sql::gos_rt_sql_row_get_opt_f64,
        "gos_rt_sql_row_get_opt_bool"       => rt::sql::gos_rt_sql_row_get_opt_bool,
        "gos_rt_sql_row_get_opt_text"       => rt::sql::gos_rt_sql_row_get_opt_text,
        "gos_rt_sql_row_is_null"            => rt::sql::gos_rt_sql_row_is_null,
        "gos_rt_sql_row_width"              => rt::sql::gos_rt_sql_row_width,
        "gos_rt_sql_tx_commit"              => rt::sql::gos_rt_sql_tx_commit,
        "gos_rt_sql_tx_rollback"            => rt::sql::gos_rt_sql_tx_rollback,
        "gos_rt_sql_tx_execute"             => rt::sql::gos_rt_sql_tx_execute,
        "gos_rt_sql_tx_savepoint"           => rt::sql::gos_rt_sql_tx_savepoint,
        "gos_rt_sql_tx_release_savepoint"   => rt::sql::gos_rt_sql_tx_release_savepoint,
        "gos_rt_sql_tx_rollback_to_savepoint" => rt::sql::gos_rt_sql_tx_rollback_to_savepoint,
        // Closure-callback combinators (Vec::sort_by / iter::map / ...).
        // Adds these to the JIT dispatch table so user bodies that
        // call them stop falling through to the bytecode VM.
        "gos_rt_arr_sort_by_i64"     => rt::gos_rt_arr_sort_by_i64,
        "gos_rt_vec_sort_by_i64"     => rt::gos_rt_vec_sort_by_i64,
        "gos_rt_vec_sort_i64"        => rt::gos_rt_vec_sort_i64,
        "gos_rt_vec_sort_str"        => rt::gos_rt_vec_sort_str,
        "gos_rt_arr_sort_by_aggr"    => rt::gos_rt_arr_sort_by_aggr,
        "gos_rt_vec_sort_by_aggr"    => rt::gos_rt_vec_sort_by_aggr,
        "gos_rt_callback_invoke"     => rt::gos_rt_callback_invoke,
        "gos_rt_iter_map_i64"        => rt::gos_rt_iter_map_i64,
        "gos_rt_testing_check"       => rt::gos_rt_testing_check,
        "gos_rt_testing_check_eq_i64" => rt::gos_rt_testing_check_eq_i64,
        "gos_rt_parse_i64"           => rt::gos_rt_parse_i64,
        "gos_rt_parse_i64_result"    => rt::gos_rt_parse_i64_result,
        "gos_rt_iter_count_by_i64" => rt::gos_rt_iter_count_by_i64,
        "gos_rt_iter_filter_map_i64" => rt::gos_rt_iter_filter_map_i64,
        "gos_rt_iter_find_map_i64" => rt::gos_rt_iter_find_map_i64,
        "gos_rt_iter_flat_map_i64" => rt::gos_rt_iter_flat_map_i64,
        "gos_rt_iter_flat_map_arr_i64" => rt::gos_rt_iter_flat_map_arr_i64,
        "gos_rt_iter_group_by_i64" => rt::gos_rt_iter_group_by_i64,
        "gos_rt_iter_max_by_i64" => rt::gos_rt_iter_max_by_i64,
        "gos_rt_iter_max_by_key_i64" => rt::gos_rt_iter_max_by_key_i64,
        "gos_rt_iter_min_by_i64" => rt::gos_rt_iter_min_by_i64,
        "gos_rt_iter_min_by_key_i64" => rt::gos_rt_iter_min_by_key_i64,
        "gos_rt_iter_partition_i64" => rt::gos_rt_iter_partition_i64,
        "gos_rt_iter_chunk_by_size_i64" => rt::gos_rt_iter_chunk_by_size_i64,
        "gos_rt_iter_dedup_i64" => rt::gos_rt_iter_dedup_i64,
        "gos_rt_iter_enumerate_i64" => rt::gos_rt_iter_enumerate_i64,
        "gos_rt_iter_flatten_i64" => rt::gos_rt_iter_flatten_i64,
        "gos_rt_iter_pairwise_i64" => rt::gos_rt_iter_pairwise_i64,
        "gos_rt_iter_unzip_i64" => rt::gos_rt_iter_unzip_i64,
        "gos_rt_iter_windowed_i64" => rt::gos_rt_iter_windowed_i64,
        "gos_rt_iter_zip_i64" => rt::gos_rt_iter_zip_i64,
        "gos_rt_iter_position_i64" => rt::gos_rt_iter_position_i64,
        "gos_rt_iter_product_by_i64" => rt::gos_rt_iter_product_by_i64,
        "gos_rt_iter_reduce_i64" => rt::gos_rt_iter_reduce_i64,
        "gos_rt_iter_scan_i64" => rt::gos_rt_iter_scan_i64,
        "gos_rt_iter_skip_while_i64" => rt::gos_rt_iter_skip_while_i64,
        "gos_rt_iter_sorted_by_i64" => rt::gos_rt_iter_sorted_by_i64,
        "gos_rt_iter_sorted_by_key_i64" => rt::gos_rt_iter_sorted_by_key_i64,
        "gos_rt_iter_take_while_i64" => rt::gos_rt_iter_take_while_i64,
        "gos_rt_option_and_then" => rt::gos_rt_option_and_then,
        "gos_rt_option_default_with" => rt::gos_rt_option_default_with,
        "gos_rt_option_filter" => rt::gos_rt_option_filter,
        "gos_rt_option_flatten" => rt::gos_rt_option_flatten,
        "gos_rt_option_iter" => rt::gos_rt_option_iter,
        "gos_rt_option_or" => rt::gos_rt_option_or,
        "gos_rt_option_or_else" => rt::gos_rt_option_or_else,
        "gos_rt_option_zip" => rt::gos_rt_option_zip,
        "gos_rt_result_and_then" => rt::gos_rt_result_and_then,
        "gos_rt_result_or_else" => rt::gos_rt_result_or_else,
        "gos_rt_result_to_opt_err" => rt::gos_rt_result_to_opt_err,
        "gos_rt_result_to_opt_ok" => rt::gos_rt_result_to_opt_ok,
        "gos_rt_result_map_err"      => rt::gos_rt_result_map_err,
        "gos_rt_result_map"          => rt::gos_rt_result_map,
        "gos_rt_flag_cell_load_str"  => rt::gos_rt_flag_cell_load_str,
        "gos_rt_flag_cell_load_i64"  => rt::gos_rt_flag_cell_load_i64,
        "gos_rt_flag_cell_load_bool" => rt::gos_rt_flag_cell_load_bool,
        "gos_rt_flag_cell_load_f64"  => rt::gos_rt_flag_cell_load_f64,
        "gos_rt_flag_cell_load_vec"  => rt::gos_rt_flag_cell_load_vec,
        "gos_rt_json_value_string"   => rt::gos_rt_json_value_string,
        "gos_rt_json_value_int"      => rt::gos_rt_json_value_int,
        "gos_rt_json_value_float"    => rt::gos_rt_json_value_float,
        "gos_rt_json_value_bool"     => rt::gos_rt_json_value_bool,
        "gos_rt_json_value_null"     => rt::gos_rt_json_value_null,
        "gos_rt_json_value_array"    => rt::gos_rt_json_value_array,
        "gos_rt_json_value_object"   => rt::gos_rt_json_value_object,
        "gos_rt_parse_f64"           => rt::gos_rt_parse_f64,
        "gos_rt_i64_to_str"          => rt::gos_rt_i64_to_str,
        "gos_rt_u64_to_str"          => rt::gos_rt_u64_to_str,
        "gos_rt_uuid_v4"             => rt::gos_rt_uuid_v4,
        "gos_rt_uuid_v7"             => rt::gos_rt_uuid_v7,
        "gos_rt_uuid_is_valid"       => rt::gos_rt_uuid_is_valid,
        "gos_rt_uuid_normalize"      => rt::gos_rt_uuid_normalize,
        "gos_rt_uuid_simple"         => rt::gos_rt_uuid_simple,
        "gos_rt_f64_to_str"          => rt::gos_rt_f64_to_str,
        "gos_rt_f64_prec_to_str"     => rt::gos_rt_f64_prec_to_str,
        "gos_rt_flush_stdout"        => rt::gos_rt_flush_stdout,
        "gos_rt_print_str"           => rt::gos_rt_print_str,
        "gos_rt_print_i64"           => rt::gos_rt_print_i64,
        "gos_rt_print_u64"           => rt::gos_rt_print_u64,
        "gos_rt_print_f64"           => rt::gos_rt_print_f64,
        "gos_rt_print_bool"          => rt::gos_rt_print_bool,
        "gos_rt_print_char"          => rt::gos_rt_print_char,
        "gos_rt_eprint_str"          => rt::gos_rt_eprint_str,
        "gos_rt_eprintln"            => rt::gos_rt_eprintln,
        "gos_rt_io_copy"             => rt::gos_rt_io_copy,
        "gos_rt_io_read_all"         => rt::gos_rt_io_read_all,
        "gos_rt_io_stdin"            => rt::gos_rt_io_stdin,
        "gos_rt_io_stdout"           => rt::gos_rt_io_stdout,
        "gos_rt_io_stderr"           => rt::gos_rt_io_stderr,
        "gos_rt_stream_write_byte"   => rt::gos_rt_stream_write_byte,
        "gos_rt_stream_write_str"    => rt::gos_rt_stream_write_str,
        "gos_rt_stream_flush"        => rt::gos_rt_stream_flush,
        "gos_rt_stream_read_line"    => rt::gos_rt_stream_read_line,
        "gos_rt_stream_read_to_string" => rt::gos_rt_stream_read_to_string,
        "gos_rt_println"             => rt::gos_rt_println,
        "gos_rt_stdout_acquire"      => rt::gos_rt_stdout_acquire,
        "gos_rt_stdout_release"      => rt::gos_rt_stdout_release,
        "gos_rt_vec_new"             => rt::gos_rt_vec_new,
        "gos_rt_vec_with_capacity"   => rt::gos_rt_vec_with_capacity,
        "gos_rt_vec_from_arr"        => rt::gos_rt_vec_from_arr,
        "gos_rt_vec_borrow_arr"      => rt::gos_rt_vec_borrow_arr,
        "gos_rt_nested_arr_to_vec"   => rt::gos_rt_nested_arr_to_vec,
        "gos_rt_vec_len"             => rt::gos_rt_vec_len,
        "gos_rt_vec_push"            => rt::gos_rt_vec_push,
        "gos_rt_vec_push_i64"        => rt::gos_rt_vec_push_i64,
        "gos_rt_vec_get_ptr"         => rt::gos_rt_vec_get_ptr,
        "gos_rt_vec_pop"             => rt::gos_rt_vec_pop,
        "gos_rt_vec_pop_opt"         => rt::gos_rt_vec_pop_opt,
        "gos_rt_vec_slice"           => rt::gos_rt_vec_slice,
        "gos_rt_map_new"             => rt::gos_rt_map_new,
        "gos_rt_map_len"             => rt::gos_rt_map_len,
        "gos_rt_map_insert"          => rt::gos_rt_map_insert,
        "gos_rt_map_get"             => rt::gos_rt_map_get,
        "gos_rt_map_get_or_i64"      => rt::gos_rt_map_get_or_i64,
        "gos_rt_map_inc_i64"         => rt::gos_rt_map_inc_i64,
        "gos_rt_map_or_insert_str_i64" => rt::gos_rt_map_or_insert_str_i64,
        "gos_rt_map_or_insert_i64_i64" => rt::gos_rt_map_or_insert_i64_i64,
        "gos_rt_map_remove"          => rt::gos_rt_map_remove,
        "gos_rt_map_insert_i64_i64"  => rt::gos_rt_map_insert_i64_i64,
        "gos_rt_map_insert_skey"     => rt::gos_rt_map_insert_skey,
        "gos_rt_map_get_skey_opt"    => rt::gos_rt_map_get_skey_opt,
        "gos_rt_map_contains_skey"   => rt::gos_rt_map_contains_skey,
        "gos_rt_map_get_i64"         => rt::gos_rt_map_get_i64,
        "gos_rt_map_remove_i64"      => rt::gos_rt_map_remove_i64,
        "gos_rt_map_contains_key_i64" => rt::gos_rt_map_contains_key_i64,
        "gos_rt_map_insert_str_i64"  => rt::gos_rt_map_insert_str_i64,
        "gos_rt_map_get_str_i64"     => rt::gos_rt_map_get_str_i64,
        "gos_rt_map_insert_str_str"  => rt::gos_rt_map_insert_str_str,
        "gos_rt_map_get_str_str"     => rt::gos_rt_map_get_str_str,
        "gos_rt_map_contains_key_str" => rt::gos_rt_map_contains_key_str,
        "gos_rt_map_remove_str"      => rt::gos_rt_map_remove_str,
        "gos_rt_map_clear"           => rt::gos_rt_map_clear,
        "gos_rt_map_inc_at_str_i64"  => rt::gos_rt_map_inc_at_str_i64,
        "gos_rt_map_free"            => rt::gos_rt_map_free,
        "gos_rt_vec_free"            => rt::gos_rt_vec_free,
        "gos_rt_set_free"            => rt::gos_rt_set_free,
        "gos_rt_btmap_free"          => rt::gos_rt_btmap_free,
        "gos_rt_map_keys_i64"        => rt::gos_rt_map_keys_i64,
        "gos_rt_map_values_i64"      => rt::gos_rt_map_values_i64,
        "gos_rt_map_keys_str"        => rt::gos_rt_map_keys_str,
        "gos_rt_map_values_str"      => rt::gos_rt_map_values_str,
        "gos_rt_map_get_or_str_i64"  => rt::gos_rt_map_get_or_str_i64,
        "gos_rt_map_get_or_str_str"  => rt::gos_rt_map_get_or_str_str,
        "gos_rt_map_get_or_i64_str"  => rt::gos_rt_map_get_or_i64_str,
        "gos_rt_map_insert_i64_str"  => rt::gos_rt_map_insert_i64_str,
        "gos_rt_map_get_i64_str"     => rt::gos_rt_map_get_i64_str,
        "gos_rt_map_format"          => rt::gos_rt_map_format,
        "gos_rt_json_parse"          => rt::gos_rt_json_parse,
        "gos_rt_json_render"         => rt::gos_rt_json_render,
        "gos_rt_json_display"        => rt::gos_rt_json_display,
        "gos_rt_json_get"            => rt::gos_rt_json_get,
        "gos_rt_json_get_opt"        => rt::gos_rt_json_get_opt,
        "gos_rt_json_keys_opt"       => rt::gos_rt_json_keys_opt,
        "gos_rt_json_as_array_opt"   => rt::gos_rt_json_as_array_opt,
        "gos_rt_json_at"             => rt::gos_rt_json_at,
        "gos_rt_json_len"            => rt::gos_rt_json_len,
        "gos_rt_json_is_null"        => rt::gos_rt_json_is_null,
        "gos_rt_json_as_i64"         => rt::gos_rt_json_as_i64,
        "gos_rt_json_as_f64"         => rt::gos_rt_json_as_f64,
        "gos_rt_json_as_str"         => rt::gos_rt_json_as_str,
        "gos_rt_json_as_bool"        => rt::gos_rt_json_as_bool,
        "gos_rt_json_identity"       => rt::gos_rt_json_identity,
        "gos_rt_chan_new"            => rt::gos_rt_chan_new,
        "gos_rt_chan_send"               => rt::gos_rt_chan_send,
        "gos_rt_chan_try_send"           => rt::gos_rt_chan_try_send,
        "gos_rt_chan_recv"               => rt::gos_rt_chan_recv,
        "gos_rt_chan_recv_option"        => rt::gos_rt_chan_recv_option,
        "gos_rt_chan_try_recv"           => rt::gos_rt_chan_try_recv,
        "gos_rt_chan_try_recv_option"    => rt::gos_rt_chan_try_recv_option,
        "gos_rt_chan_close"              => rt::gos_rt_chan_close,
        "gos_rt_go_spawn"            => rt::gos_rt_go_spawn,
        "gos_rt_go_spawn_call_0"     => rt::gos_rt_go_spawn_call_0,
        "gos_rt_go_spawn_call_1"     => rt::gos_rt_go_spawn_call_1,
        "gos_rt_go_spawn_call_2"     => rt::gos_rt_go_spawn_call_2,
        "gos_rt_go_yield"            => rt::gos_rt_go_yield,
        "gos_rt_spawn"               => rt::gos_rt_spawn,
        "gos_rt_join"                => rt::gos_rt_join,
        "gos_rt_sleep_ns"            => rt::gos_rt_sleep_ns,
        "gos_rt_sleep_ms"            => rt::gos_rt_sleep_ms,
        "gos_rt_now_ns"              => rt::gos_rt_now_ns,
        "gos_rt_gc_alloc"            => rt::gos_rt_gc_alloc,
        "gos_rt_aggr_alloc"          => rt::gos_rt_aggr_alloc,
        "gos_rt_aggr_free"           => rt::gos_rt_aggr_free,
        "gos_rt_rc_alloc"            => rt::gos_rt_rc_alloc,
        "gos_rt_rc_retain"           => rt::gos_rt_rc_retain,
        "gos_rt_rc_release"          => rt::gos_rt_rc_release,
        "gos_rt_rc_downgrade"        => rt::gos_rt_rc_downgrade,
        "gos_rt_rc_weak_retain"      => rt::gos_rt_rc_weak_retain,
        "gos_rt_rc_weak_release"     => rt::gos_rt_rc_weak_release,
        "gos_rt_rc_weak_upgrade"     => rt::gos_rt_rc_weak_upgrade,
        "gos_rt_rc_weak_upgrade_opt" => rt::gos_rt_rc_weak_upgrade_opt,
        "gos_rt_arena_push"         => rt::gos_rt_arena_push,
        "gos_rt_arena_pop"          => rt::gos_rt_arena_pop,
        "gos_rt_collect_cycles"      => rt::gos_rt_collect_cycles,
        "gos_rt_goroutine_panicked"  => rt::gos_rt_goroutine_panicked,
        "gos_rt_callback_invoke"     => rt::gos_rt_callback_invoke,
        "gos_rt_callback_register"   => rt::gos_rt_callback_register,
        "gos_rt_callback_unregister" => rt::gos_rt_callback_unregister,
        "gos_rt_env_home_dir"        => rt::gos_rt_env_home_dir,
        "gos_rt_env_temp_dir"        => rt::gos_rt_env_temp_dir,
        "gos_rt_vec_new_typed"       => rt::gos_rt_vec_new_typed,
        "gos_rt_vec_with_capacity_typed" => rt::gos_rt_vec_with_capacity_typed,
        "gos_rt_binding_map_free"    => rt::gos_rt_binding_map_free,
        "gos_rt_panic_oob"           => rt::gos_rt_panic_oob,
        "gos_rt_gc_deregister"       => rt::gos_rt_gc_deregister,
        "gos_rt_gc_collect"          => rt::gos_rt_gc_collect,
        "gos_rt_gc_alloc_count"      => rt::gos_rt_gc_alloc_count,
        "gos_rt_gc_reset"            => rt::gos_rt_gc_reset,
        "gos_rt_arena_save"          => rt::gos_rt_arena_save,
        "gos_rt_arena_restore"       => rt::gos_rt_arena_restore,
        "gos_rt_http_serve"          => rt::gos_rt_http_serve,
        "gos_rt_http2_bind_and_run_h2c" => rt::gos_rt_http2_bind_and_run_h2c,
        "gos_rt_chunked_encode"      => rt::gos_rt_chunked_encode,
        "gos_rt_chunked_decode"      => rt::gos_rt_chunked_decode,
        "gos_rt_sse_encode_event"    => rt::gos_rt_sse_encode_event,
        "gos_rt_sse_encode_comment"  => rt::gos_rt_sse_encode_comment,
        "gos_rt_sse_encode_retry"    => rt::gos_rt_sse_encode_retry,
        "gos_rt_mw_new_request_id"   => rt::gos_rt_mw_new_request_id,
        "gos_rt_mw_accepts_gzip"     => rt::gos_rt_mw_accepts_gzip,
        "gos_rt_mw_decode_basic_auth" => rt::gos_rt_mw_decode_basic_auth,
        "gos_rt_ws_accept"           => rt::gos_rt_ws_accept,
        "gos_rt_ws_is_upgrade"       => rt::gos_rt_ws_is_upgrade,
        "gos_rt_ws_accept_key"       => rt::gos_rt_ws_accept_key,
        "gos_rt_static_mime_for_path" => rt::gos_rt_static_mime_for_path,
        "gos_rt_static_serve_file"   => rt::gos_rt_static_serve_file,
        "gos_rt_router_new"           => rt::gos_rt_router_new,
        "gos_rt_router_add"           => rt::gos_rt_router_add,
        "gos_rt_router_get"           => rt::gos_rt_router_get,
        "gos_rt_router_post"          => rt::gos_rt_router_post,
        "gos_rt_router_put"           => rt::gos_rt_router_put,
        "gos_rt_router_delete"        => rt::gos_rt_router_delete,
        "gos_rt_router_patch"         => rt::gos_rt_router_patch,
        "gos_rt_router_head"          => rt::gos_rt_router_head,
        "gos_rt_router_options"       => rt::gos_rt_router_options,
        "gos_rt_router_add_fn"        => rt::gos_rt_router_add_fn,
        "gos_rt_router_add_pattern"   => rt::gos_rt_router_add_pattern,
        "gos_rt_router_lookup"        => rt::gos_rt_router_lookup,
        "gos_rt_router_get_fn"        => rt::gos_rt_router_get_fn,
        "gos_rt_router_post_fn"       => rt::gos_rt_router_post_fn,
        "gos_rt_router_put_fn"        => rt::gos_rt_router_put_fn,
        "gos_rt_router_delete_fn"     => rt::gos_rt_router_delete_fn,
        "gos_rt_router_patch_fn"      => rt::gos_rt_router_patch_fn,
        "gos_rt_router_head_fn"       => rt::gos_rt_router_head_fn,
        "gos_rt_router_options_fn"    => rt::gos_rt_router_options_fn,
        "gos_rt_router_serve"         => rt::gos_rt_router_serve,
        "gos_rt_file_server_new"      => rt::gos_rt_file_server_new,
        "gos_rt_file_server_serve"    => rt::gos_rt_file_server_serve,
        "gos_rt_native_client_new"    => rt::gos_rt_native_client_new,
        "gos_rt_native_client_get"    => rt::gos_rt_native_client_get,
        "gos_rt_nc_get"               => rt::gos_rt_nc_get,
        "gos_rt_nc_delete"            => rt::gos_rt_nc_delete,
        "gos_rt_nc_post"              => rt::gos_rt_nc_post,
        "gos_rt_nc_put"               => rt::gos_rt_nc_put,
        "gos_rt_proxy_new"            => rt::gos_rt_proxy_new,
        "gos_rt_proxy_forward"        => rt::gos_rt_proxy_forward,
        "gos_rt_proxy_forward_url"    => rt::gos_rt_proxy_forward_url,
        "gos_rt_ws_frame_text"        => rt::gos_rt_ws_frame_text,
        "gos_rt_panic"               => rt::gos_rt_panic,
        "gos_rt_panic_oob"           => rt::gos_rt_panic_oob,
        "gos_rt_stack_push"          => rt::gos_rt_stack_push,
        "gos_rt_stack_pop"           => rt::gos_rt_stack_pop,
        "gos_rt_stack_set_line"      => rt::gos_rt_stack_set_line,
        "gos_rt_cov_record"          => rt::gos_rt_cov_record,
        "gos_rt_cov_bump"            => rt::gos_rt_cov_bump,
        "gos_rt_cov_reset"           => rt::gos_rt_cov_reset,
        "gos_rt_cov_set_enabled"     => rt::gos_rt_cov_set_enabled,
        "gos_rt_exit"                => rt::gos_rt_exit,
        "gos_rt_process_id"          => rt::gos_rt_process_id,
        "gos_rt_process_abort"       => rt::gos_rt_process_abort,
        "gos_rt_time_now"            => rt::gos_rt_time_now,
        "gos_rt_math_sqrt"           => rt::gos_rt_math_sqrt,
        "gos_rt_math_pow"            => rt::gos_rt_math_pow,
        "gos_rt_math_sin"            => rt::gos_rt_math_sin,
        "gos_rt_math_cos"            => rt::gos_rt_math_cos,
        "gos_rt_math_log"            => rt::gos_rt_math_log,
        "gos_rt_math_exp"            => rt::gos_rt_math_exp,
        "gos_rt_math_abs"            => rt::gos_rt_math_abs,
        "gos_rt_math_floor"          => rt::gos_rt_math_floor,
        "gos_rt_math_ceil"           => rt::gos_rt_math_ceil,
        "gos_rt_time_now_ms"         => rt::gos_rt_time_now_ms,
        // Fn-trait coercion trampolines (closure_fn_trait_plan.md).
        // Emitted by the cranelift codegen when a bare `fn`/`fn item`
        // value is wrapped into a `Fn(args) -> ret` slot - the env
        // blob's offset 0 holds one of these, offset 8 holds the
        // real fn ptr.
        "gos_rt_fn_tramp_0"          => rt::gos_rt_fn_tramp_0,
        "gos_rt_fn_tramp_1"          => rt::gos_rt_fn_tramp_1,
        "gos_rt_fn_tramp_2"          => rt::gos_rt_fn_tramp_2,
        "gos_rt_fn_tramp_3"          => rt::gos_rt_fn_tramp_3,
        "gos_rt_fn_tramp_4"          => rt::gos_rt_fn_tramp_4,
        "gos_rt_fn_tramp_5"          => rt::gos_rt_fn_tramp_5,
        "gos_rt_fn_tramp_6"          => rt::gos_rt_fn_tramp_6,
        "gos_rt_fn_tramp_7"          => rt::gos_rt_fn_tramp_7,
        "gos_rt_fn_tramp_8"          => rt::gos_rt_fn_tramp_8,
        // Stringification helpers for compound `println!` /
        // `format!`. The codegen emits these whenever an arg's
        // print-kind is bool or char.
        "gos_rt_bool_to_str"         => rt::gos_rt_bool_to_str,
        "gos_rt_char_to_str"         => rt::gos_rt_char_to_str,
        // Block-write helpers used by `Stream::write_byte_array`
        // for bulk per-line byte dumps.
        "gos_rt_stream_write_byte_array" => rt::gos_rt_stream_write_byte_array,
        // Heap-allocated i64 vector - `I64Vec` in source.
        // Used as a shared scratch buffer by goroutine workers.
        "gos_rt_heap_i64_new"        => rt::gos_rt_heap_i64_new,
        "gos_rt_heap_i64_free"       => rt::gos_rt_heap_i64_free,
        "gos_rt_heap_i64_get"        => rt::gos_rt_heap_i64_get,
        "gos_rt_heap_i64_set"        => rt::gos_rt_heap_i64_set,
        "gos_rt_heap_i64_len"        => rt::gos_rt_heap_i64_len,
        "gos_rt_heap_i64_write_lines_to_stdout"
                                     => rt::gos_rt_heap_i64_write_lines_to_stdout,
        "gos_rt_heap_i64_write_bytes_to_stdout"
                                     => rt::gos_rt_heap_i64_write_bytes_to_stdout,
        // U8Vec - 1-byte-per-element heap vec for byte-oriented
        // scratch buffers. Same shape as the i64 family but
        // with byte-aligned storage.
        "gos_rt_heap_u8_new"         => rt::gos_rt_heap_u8_new,
        "gos_rt_heap_u8_free"        => rt::gos_rt_heap_u8_free,
        "gos_rt_heap_u8_get"         => rt::gos_rt_heap_u8_get,
        "gos_rt_heap_u8_set"         => rt::gos_rt_heap_u8_set,
        "gos_rt_heap_u8_len"         => rt::gos_rt_heap_u8_len,
        "gos_rt_heap_u8_to_string"   => rt::gos_rt_heap_u8_to_string,
        "gos_rt_heap_u8_write_lines_to_stdout"
                                     => rt::gos_rt_heap_u8_write_lines_to_stdout,
        "gos_rt_heap_u8_write_bytes_to_stdout"
                                     => rt::gos_rt_heap_u8_write_bytes_to_stdout,
        // Sync primitives + LCG jump used by goroutine
        // worker patterns.
        "gos_rt_mutex_new"           => rt::gos_rt_mutex_new,
        "gos_rt_mutex_lock"          => rt::gos_rt_mutex_lock,
        "gos_rt_mutex_unlock"        => rt::gos_rt_mutex_unlock,
        "gos_rt_wg_new"              => rt::gos_rt_wg_new,
        "gos_rt_wg_add"              => rt::gos_rt_wg_add,
        "gos_rt_wg_done"             => rt::gos_rt_wg_done,
        "gos_rt_wg_wait"             => rt::gos_rt_wg_wait,
        "gos_rt_wg_error"            => rt::gos_rt_wg_error,
        "gos_rt_wg_error_clear"      => rt::gos_rt_wg_error_clear,
        "gos_rt_atomic_i64_new"      => rt::gos_rt_atomic_i64_new,
        "gos_rt_atomic_bool_new"     => rt::gos_rt_atomic_bool_new,
        "gos_rt_atomic_bool_load"    => rt::gos_rt_atomic_bool_load,
        "gos_rt_atomic_bool_store"   => rt::gos_rt_atomic_bool_store,
        "gos_rt_atomic_i64_load"     => rt::gos_rt_atomic_i64_load,
        "gos_rt_atomic_i64_store"    => rt::gos_rt_atomic_i64_store,
        "gos_rt_atomic_i64_fetch_add"=> rt::gos_rt_atomic_i64_fetch_add,
        "gos_rt_atomic_i64_load_acquire"
                                     => rt::gos_rt_atomic_i64_load_acquire,
        "gos_rt_atomic_i64_store_release"
                                     => rt::gos_rt_atomic_i64_store_release,
        "gos_rt_atomic_i64_load_relaxed"
                                     => rt::gos_rt_atomic_i64_load_relaxed,
        "gos_rt_atomic_i64_store_relaxed"
                                     => rt::gos_rt_atomic_i64_store_relaxed,
        "gos_rt_atomic_i64_fetch_add_acqrel"
                                     => rt::gos_rt_atomic_i64_fetch_add_acqrel,
        "gos_rt_atomic_i64_cas"      => rt::gos_rt_atomic_i64_cas,
        "gos_rt_atomic_i64_cas_acq_rel"
                                     => rt::gos_rt_atomic_i64_cas_acq_rel,
        "gos_rt_atomic_i64_swap"     => rt::gos_rt_atomic_i64_swap,
        "gos_rt_preempt_check"       => preempt::gos_rt_preempt_check,
        "gos_rt_preempt_check_and_yield"
                                     => preempt::gos_rt_preempt_check_and_yield,
        "gos_rt_stdout_acquire"      => rt::gos_rt_stdout_acquire,
        "gos_rt_stdout_release"      => rt::gos_rt_stdout_release,
        "gos_rt_sync_i64_new"        => rt::gos_rt_sync_i64_new,
        "gos_rt_sync_i64_drop"       => rt::gos_rt_sync_i64_drop,
        "gos_rt_sync_i64_len"        => rt::gos_rt_sync_i64_len,
        "gos_rt_sync_i64_get"        => rt::gos_rt_sync_i64_get,
        "gos_rt_sync_i64_set"        => rt::gos_rt_sync_i64_set,
        "gos_rt_sync_i64_push"       => rt::gos_rt_sync_i64_push,
        "gos_rt_sync_i64_add"        => rt::gos_rt_sync_i64_add,
        "gos_rt_sync_u8_new"         => rt::gos_rt_sync_u8_new,
        "gos_rt_sync_u8_drop"        => rt::gos_rt_sync_u8_drop,
        "gos_rt_sync_u8_len"         => rt::gos_rt_sync_u8_len,
        "gos_rt_sync_u8_get"         => rt::gos_rt_sync_u8_get,
        "gos_rt_sync_u8_set"         => rt::gos_rt_sync_u8_set,
        "gos_rt_sync_u8_push"        => rt::gos_rt_sync_u8_push,
        "gos_rt_lcg_jump"            => rt::gos_rt_lcg_jump,
        "gos_rt_go_spawn_call_3"     => rt::gos_rt_go_spawn_call_3,
        "gos_rt_go_spawn_call_4"     => rt::gos_rt_go_spawn_call_4,
        "gos_rt_go_spawn_call_5"     => rt::gos_rt_go_spawn_call_5,
        "gos_rt_go_spawn_call_6"     => rt::gos_rt_go_spawn_call_6,
    }
    // Register every remaining `gos_rt_*` symbol from the ABI registry's
    // address table. The explicit `reg!` block above covers the common
    // symbols with their bespoke spellings; this pass closes the gap so
    // that a body calling any registered runtime helper (math, strconv,
    // unicode, encoding, …) resolves at JIT-finalize time instead of
    // failing the whole module and collapsing the program to the
    // interpreter. A runtime test asserts the table is registry-complete.
    for (name, addr) in rt::runtime_symbol_addrs() {
        if names.insert(name) {
            builder.symbol(name, addr);
        }
    }
    names
}

/// Walks both binding-symbol registration paths and registers each
/// `(name, addr)` with the JIT builder.
///
/// - [`crate::native_symbols::NATIVE_SYMBOLS`] - the link-time
///   `linkme::distributed_slice` populated by
///   `gossamer_binding::register_module!` for every binding item.
/// - [`crate::native_symbols::native_symbols_snapshot`] - the
///   runtime `Mutex<Vec>` registry populated by the legacy
///   `force_link()` path. Kept for backward compatibility with
///   bindings that publish from a runtime hook.
///
/// Returns the leaked `&'static str` names so the caller can fold
/// them into the runtime-symbol set used by [`body_calls_jit_unsafe`] -
/// that keeps bodies that call bindings eligible for JIT
/// promotion (the `body_kinds` primitive-only filter still vetoes
/// anything the dispatch trampoline can't marshal).
fn register_binding_symbols(builder: &mut JITBuilder) -> Vec<&'static str> {
    use std::collections::HashSet;

    let mut names: Vec<&'static str> = Vec::new();
    let mut seen: HashSet<&'static str> = HashSet::new();
    // `linkme::DistributedSlice` doesn't impl `IntoIterator` for
    // `&Self`; the explicit `.iter()` is the only way in.
    #[allow(
        clippy::explicit_iter_loop,
        reason = "DistributedSlice has no &Self IntoIterator impl"
    )]
    for entry in crate::native_symbols::NATIVE_SYMBOLS.iter() {
        if seen.insert(entry.name) {
            builder.symbol(entry.name, (entry.addr_fn)());
            names.push(entry.name);
        }
    }
    for sym in crate::native_symbols::native_symbols_snapshot() {
        if seen.insert(sym.name) {
            builder.symbol(sym.name, sym.addr);
            names.push(sym.name);
        }
    }
    names
}
