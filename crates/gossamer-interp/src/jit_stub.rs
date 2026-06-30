//! No-op JIT backend for wasm32-unknown-unknown.
//!
//! Cranelift does not target wasm32-unknown-unknown, so the browser
//! playground links this stub in place of `gossamer-codegen-cranelift`.
//! It mirrors the public types and entry points the VM references
//! (`JitKind`, `JitFn`, `JitArtifact`, [`has_worthy_jit_body`],
//! [`compile_to_jit`]) but never compiles anything: `has_worthy_jit_body`
//! always returns `false` and `compile_to_jit` yields an empty artifact,
//! so every Gossamer function runs on the bytecode VM. The JIT is purely
//! a speed optimisation, so this is a clean functional equivalent.

use std::collections::HashMap;

/// ABI classification of a JIT slot. Mirrors the cranelift enum so the
/// VM's trampoline (`jit_call`) type-checks; no instance is ever
/// constructed on wasm because no body is JIT-compiled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JitKind {
    /// 64-bit signed integer.
    I64,
    /// 64-bit IEEE-754 float.
    F64,
    /// Boolean.
    Bool,
    /// Unit (no representation).
    Unit,
    /// Packed runtime `GossamerValue`.
    Value,
    /// Heap enum as its native tagged pointer; payload is the VM
    /// shape-table index.
    EnumPtr(u32),
    /// `Result<Enum, _>` return as the by-value two-word `i128`; payload is
    /// the `Ok` enum's VM shape-table index.
    ResultEnumPtr(u32),
    /// All-scalar user struct as a pointer to its flat field-slot block;
    /// payload is the VM struct-shape-table index.
    StructPtr(u32),
    /// `String` as a native cstring pointer.
    NativeStr,
    /// `Vec<i64>` as a native `GosVec` pointer.
    NativeVecI64,
    /// `Vec<f64>` as a native `GosVec` pointer.
    NativeVecF64,
    /// `Vec<(i64, f64)>` as a native `GosVec` of 16-byte primitive slots.
    NativeVecTupleIF,
    /// `Vec<Vec<i64>>` as a native outer `GosVec` of inner `GosVec` pointers.
    NativeVecVecI64,
    /// `U8Vec` byte-buffer handle, marshalled by copy-in / copy-back.
    U8VecHandle,
}

/// A compiled function handle. On wasm none is ever produced.
#[derive(Clone)]
pub struct JitFn {
    /// Gossamer source name.
    pub name: String,
    /// Entry pointer (never valid on wasm - no instance is created).
    pub ptr: *const u8,
    /// Parameter kinds in source order.
    pub params: Vec<JitKind>,
    /// Return slot kind.
    pub returns: JitKind,
    /// Mirrors `gossamer_codegen_cranelift::JitFn::returns_fresh` so the
    /// shared dispatch code in `jit_call` compiles against either handle.
    /// The wasm stub never promotes a body, so the value is irrelevant.
    pub returns_fresh: bool,
}

/// A set of compiled functions. Always empty on wasm.
#[derive(Default)]
pub struct JitArtifact {
    /// Compiled functions keyed by Gossamer source name.
    pub functions: HashMap<String, JitFn>,
}

/// wasm never promotes a body to native code.
#[must_use]
pub fn has_worthy_jit_body(
    _bodies: &[gossamer_mir::Body],
    _tcx: &gossamer_types::TyCtxt,
    _enum_shapes: &HashMap<u32, u32>,
    _struct_shapes: &HashMap<u32, u32>,
) -> bool {
    false
}

/// wasm promotes nothing, so there are no eager-compile candidates.
#[must_use]
pub fn jit_eager_loop_bodies(
    _bodies: &[gossamer_mir::Body],
    _tcx: &gossamer_types::TyCtxt,
    _enum_shapes: &HashMap<u32, u32>,
    _struct_shapes: &HashMap<u32, u32>,
) -> Vec<String> {
    Vec::new()
}

/// wasm yields an empty artifact - the VM installs no native overrides
/// and runs everything on the bytecode interpreter.
#[allow(clippy::missing_errors_doc)]
pub fn compile_to_jit(
    _bodies: &[gossamer_mir::Body],
    _tcx: &gossamer_types::TyCtxt,
    _enum_shapes: &HashMap<u32, u32>,
    _struct_shapes: &HashMap<u32, u32>,
) -> Result<JitArtifact, String> {
    Ok(JitArtifact::default())
}
