//! Trampoline that dispatches into a JIT-compiled body.
//!
//! Every call into native code goes through `invoke_prepared`: it inspects
//! the [`JitFn`]'s parameter and return kinds, marshals the VM's
//! boxed `Value`s into raw scalars, transmutes the function pointer
//! to a typed `extern "C"` callable, and calls it.
//!
//! Confining the raw-pointer dispatch here keeps the surface where
//! we have to reason about ABI safety down to a single module.
//!
//! # Safety invariants
//!
//! Every transmute below relies on the following invariants:
//! - `jit.ptr` was produced by `JITModule::get_finalized_function`
//!   for a body whose Cranelift signature exactly matches the
//!   chosen `extern "C" fn` shape - that match is guaranteed by
//!   `JitArtifact::compile_to_jit`'s own type classification, which
//!   only registers a [`JitFn`] when `JitKind` for every slot lines
//!   up with the MIR-derived cranelift type.
//! - The owning `JitArtifact` is still alive: the VM holds it in
//!   `Vm::_jit` for the entire lifetime of the `Global::Jit`
//!   entries that hand `JitFn`s to this module.
//! - The Gossamer language is single-threaded at the VM layer; the
//!   trampoline is therefore not re-entered from a foreign thread
//!   while a `JITed` body is running.
//!
//! Shapes the trampoline does not cover (e.g. heterogeneous mixes of
//! `i64`/`f64` beyond the listed patterns) return [`Dispatch::Fallback`]
//! so the caller can retry through the bytecode interpreter.

#![allow(unsafe_code)]
// Trampoline expands one arity-shape stub per `JitKind` permutation;
// the macro-generated dispatch keeps each shape in one place.
#![allow(clippy::too_many_lines)]

use std::ffi::c_char;
use std::mem;
use std::sync::Arc;

use gossamer_codegen_cranelift::{JitFn, JitKind};
use gossamer_runtime::c_abi as rt;

use crate::value::{SmolStr, Value};

/// One trampoline-owned native object built for an aggregate parameter,
/// recorded so it can be written back (for `&mut` params) and freed once
/// the JIT body returns. `cell` is `Some` only for a `&mut` argument
/// whose mutations the caller must observe.
type NativeArg = (JitKind, i64, Option<Arc<parking_lot::Mutex<Value>>>);

/// Builds a fresh, trampoline-owned native object for an aggregate
/// parameter from the VM value, returning its heap pointer as `i64`.
/// `None` when `value` is not a shape this kind can marshal (the caller
/// then falls back to bytecode).
fn build_native_arg(kind: JitKind, value: &Value) -> Option<i64> {
    match kind {
        JitKind::NativeVecI64 => build_native_vec_i64(value),
        JitKind::NativeStr => build_native_str(value),
        _ => None,
    }
}

/// Builds an owned `*mut GosVec` of 8-byte `i64` slots from a VM integer
/// vector. Returns the pointer as `i64` (RC = 1, trampoline-owned).
fn build_native_vec_i64(value: &Value) -> Option<i64> {
    // SAFETY: `gos_rt_vec_new_typed` returns an owned header (RC = 1) or
    // null; `gos_rt_vec_push_i64` copies each value into the buffer. We
    // own the result until `free_native` reclaims it.
    unsafe {
        let v = rt::gos_rt_vec_new_typed(8, rt::vec::vec_elem_kind::PRIMITIVE);
        if v.is_null() {
            return None;
        }
        match value {
            Value::IntArray(arc) => {
                for &n in arc.iter() {
                    rt::gos_rt_vec_push_i64(v, n);
                }
            }
            Value::Array(arc) => {
                for elem in arc.iter() {
                    match elem {
                        Value::Int(n) => rt::gos_rt_vec_push_i64(v, *n),
                        Value::Bool(b) => rt::gos_rt_vec_push_i64(v, i64::from(*b)),
                        Value::Char(c) => rt::gos_rt_vec_push_i64(v, *c as i64),
                        _ => {
                            rt::gos_rt_vec_free(v);
                            return None;
                        }
                    }
                }
            }
            _ => {
                rt::gos_rt_vec_free(v);
                return None;
            }
        }
        Some(v as i64)
    }
}

/// Builds an owned `*mut c_char` cstring from a VM string. Returns the
/// pointer as `i64` (trampoline-owned).
fn build_native_str(value: &Value) -> Option<i64> {
    match value {
        Value::String(s) => Some(rt::alloc_cstring(s.as_str().as_bytes()) as i64),
        _ => None,
    }
}

/// Reads a native return pointer back into an owned VM value WITHOUT
/// freeing the native object (the caller frees it, deduped against the
/// params, so a body returning its own param frees exactly once).
fn native_ptr_to_value(kind: JitKind, ptr: i64) -> Value {
    match kind {
        JitKind::NativeVecI64 => {
            if ptr == 0 {
                return Value::IntArray(Arc::new(Vec::new()));
            }
            let v = ptr as *const rt::vec::GosVec;
            // SAFETY: `v` is a live `GosVec` (a param we built or the
            // body's owned return); `len`/`get` read initialised slots.
            let len = unsafe { rt::gos_rt_vec_len(v) }.max(0);
            let mut out = Vec::with_capacity(len as usize);
            for i in 0..len {
                out.push(unsafe { rt::gos_rt_vec_get_i64(v, i) });
            }
            Value::IntArray(Arc::new(out))
        }
        JitKind::NativeStr => {
            if ptr == 0 {
                return Value::String(SmolStr::default());
            }
            let s = ptr as *const c_char;
            // SAFETY: `s` is a live cstring; `gos_rt_str_len` reads its
            // length header and the bytes are valid for that length.
            let len = unsafe { rt::gos_rt_str_len(s) }.max(0) as usize;
            let bytes = unsafe { std::slice::from_raw_parts(s.cast::<u8>(), len) };
            Value::String(SmolStr::from_str(&String::from_utf8_lossy(bytes)))
        }
        _ => Value::Unit,
    }
}

/// Frees one trampoline-owned native object through its runtime
/// reference-counted reclaim entry (`gos_rt_vec_free` decrements the vec
/// header RC; `gos_rt_str_free` checks the allocator tag).
///
/// # Safety
/// `ptr` must be a live object built by `build_native_arg` (or returned
/// by the body for that kind) and not already freed.
unsafe fn free_native(kind: JitKind, ptr: i64) {
    match kind {
        JitKind::NativeVecI64 => unsafe { rt::gos_rt_vec_free(ptr as *mut rt::vec::GosVec) },
        JitKind::NativeStr => unsafe { rt::gos_rt_str_free(ptr as *mut c_char) },
        _ => {}
    }
}

/// Reads each `&mut` parameter's mutated native object back into its VM
/// write-back cell so the caller observes in-place mutations.
fn writeback_natives(natives: &[NativeArg]) {
    for (kind, ptr, cell) in natives {
        if let Some(cell) = cell {
            *cell.lock() = native_ptr_to_value(*kind, *ptr);
        }
    }
}

/// Frees every trampoline-owned native object exactly once, deduped by
/// pointer. `ret` is the native aggregate return (if any); a body that
/// returns one of its own params yields `ret == param ptr`, so the dedup
/// frees that single allocation once - never a double free, never a leak.
fn free_natives(natives: &[NativeArg], ret: Option<(JitKind, i64)>) {
    let mut freed: Vec<i64> = Vec::with_capacity(natives.len() + 1);
    let mut free_once = |kind: JitKind, ptr: i64| {
        if ptr == 0 || freed.contains(&ptr) {
            return;
        }
        freed.push(ptr);
        // SAFETY: each pointer is a distinct live object we built (or the
        // body's owned return), freed at most once by the dedup above.
        unsafe { free_native(kind, ptr) };
    };
    for (kind, ptr, _) in natives {
        free_once(*kind, *ptr);
    }
    if let Some((kind, ptr)) = ret {
        free_once(kind, ptr);
    }
}

/// Result of attempting to dispatch through the JIT trampoline.
pub(crate) enum Dispatch {
    /// The JIT body ran and produced a value.
    Ok(Value),
    /// The JIT body cannot be invoked with these args (shape
    /// unsupported, or a runtime arg's type didn't match the JIT
    /// signature). The caller falls back to the bytecode chunk.
    Fallback,
}

const MAX_ARGS: usize = 12;

#[derive(Clone, Copy)]
pub(crate) enum Slot {
    I(i64),
    F(f64),
}

fn slot_i(s: Slot) -> i64 {
    match s {
        Slot::I(n) => n,
        Slot::F(_) => 0,
    }
}

fn slot_f(s: Slot) -> f64 {
    match s {
        Slot::F(x) => x,
        Slot::I(_) => 0.0,
    }
}

/// Calls the JIT body through a reified `extern "C"` signature
/// derived from the supplied parameter and return kinds. Used by
/// every per-arity-shape stub below so each only has to bind its
/// args; the four return-kind branches live in one place.
macro_rules! call_through {
    ($ptr:expr, $ret:expr, [$($a:ident: $t:ty),* $(,)?]) => {{
        match $ret {
            JitKind::I64 => {
                let f: extern "C" fn($($t),*) -> i64 = unsafe { mem::transmute($ptr) };
                Some(Value::Int(f($($a),*)))
            }
            JitKind::F64 => {
                let f: extern "C" fn($($t),*) -> f64 = unsafe { mem::transmute($ptr) };
                Some(Value::Float(f($($a),*)))
            }
            JitKind::Bool => {
                let f: extern "C" fn($($t),*) -> i8 = unsafe { mem::transmute($ptr) };
                Some(Value::Bool(f($($a),*) != 0))
            }
            JitKind::Unit => {
                let f: extern "C" fn($($t),*) = unsafe { mem::transmute($ptr) };
                f($($a),*);
                Some(Value::Unit)
            }
            // Aggregate (`String`, `Tuple`, `Adt`, channel, …):
            // the JIT body returns a `GossamerValue` u64 handle in
            // an integer register, which we decode back through
            // `Value::from_raw`. `GossamerValue` is a transparent
            // `u64` so the i64-shaped return register holds the
            // exact bit pattern.
            JitKind::Value => {
                let f: extern "C" fn($($t),*) -> i64 = unsafe { mem::transmute($ptr) };
                let raw = f($($a),*) as u64;
                Some(Value::from_raw(raw))
            }
            // Canonicalized to I64 before dispatch; never reaches a stub.
            JitKind::EnumPtr(_) => unreachable!("EnumPtr returns are canonicalized to I64"),
            // Native aggregate returns are canonicalized to I64 in `prepare`
            // and re-wrapped by `invoke_prepared_native`; the stub only ever
            // sees the I64 shape.
            JitKind::NativeStr | JitKind::NativeVecI64 => {
                unreachable!("native aggregate returns are canonicalized to I64")
            }
        }
    }};
}

/// Maps the per-slot shape token (`i` / `f`) to the matching
/// `slot_*` accessor.
macro_rules! slot_for {
    (i, $s:expr, $idx:expr) => {
        slot_i($s[$idx])
    };
    (f, $s:expr, $idx:expr) => {
        slot_f($s[$idx])
    };
}

/// Maps the per-slot shape token (`i` / `f`) to the corresponding
/// Rust ABI type. Used to spell out the `extern "C" fn(...)`
/// signature inside `call_through!`.
macro_rules! ty_for {
    (i) => {
        i64
    };
    (f) => {
        f64
    };
}

/// Generates a `call_<arity><shape>` function for one (arity, shape)
/// combination. Distinct binding names per slot (`a0`, `a1`, …) are
/// required so each `let` introduces a fresh local instead of
/// shadowing the previous one - `call_through!` then sees every
/// argument in scope simultaneously when it expands the
/// `extern "C"` call.
macro_rules! gen_call {
    ($name:ident, $c0:ident) => {
        unsafe fn $name(ptr: *const u8, s: &[Slot], ret: JitKind) -> Option<Value> {
            let a0 = slot_for!($c0, s, 0);
            call_through!(ptr, ret, [a0: ty_for!($c0)])
        }
    };
    ($name:ident, $c0:ident, $c1:ident) => {
        unsafe fn $name(ptr: *const u8, s: &[Slot], ret: JitKind) -> Option<Value> {
            let a0 = slot_for!($c0, s, 0);
            let a1 = slot_for!($c1, s, 1);
            call_through!(ptr, ret, [a0: ty_for!($c0), a1: ty_for!($c1)])
        }
    };
    ($name:ident, $c0:ident, $c1:ident, $c2:ident) => {
        unsafe fn $name(ptr: *const u8, s: &[Slot], ret: JitKind) -> Option<Value> {
            let a0 = slot_for!($c0, s, 0);
            let a1 = slot_for!($c1, s, 1);
            let a2 = slot_for!($c2, s, 2);
            call_through!(
                ptr, ret,
                [a0: ty_for!($c0), a1: ty_for!($c1), a2: ty_for!($c2)]
            )
        }
    };
    ($name:ident, $c0:ident, $c1:ident, $c2:ident, $c3:ident) => {
        unsafe fn $name(ptr: *const u8, s: &[Slot], ret: JitKind) -> Option<Value> {
            let a0 = slot_for!($c0, s, 0);
            let a1 = slot_for!($c1, s, 1);
            let a2 = slot_for!($c2, s, 2);
            let a3 = slot_for!($c3, s, 3);
            call_through!(
                ptr, ret,
                [a0: ty_for!($c0), a1: ty_for!($c1),
                 a2: ty_for!($c2), a3: ty_for!($c3)]
            )
        }
    };
    ($name:ident, $c0:ident, $c1:ident, $c2:ident, $c3:ident, $c4:ident) => {
        unsafe fn $name(ptr: *const u8, s: &[Slot], ret: JitKind) -> Option<Value> {
            let a0 = slot_for!($c0, s, 0);
            let a1 = slot_for!($c1, s, 1);
            let a2 = slot_for!($c2, s, 2);
            let a3 = slot_for!($c3, s, 3);
            let a4 = slot_for!($c4, s, 4);
            call_through!(
                ptr, ret,
                [a0: ty_for!($c0), a1: ty_for!($c1),
                 a2: ty_for!($c2), a3: ty_for!($c3),
                 a4: ty_for!($c4)]
            )
        }
    };
    ($name:ident, $c0:ident, $c1:ident, $c2:ident, $c3:ident, $c4:ident, $c5:ident) => {
        unsafe fn $name(ptr: *const u8, s: &[Slot], ret: JitKind) -> Option<Value> {
            let a0 = slot_for!($c0, s, 0);
            let a1 = slot_for!($c1, s, 1);
            let a2 = slot_for!($c2, s, 2);
            let a3 = slot_for!($c3, s, 3);
            let a4 = slot_for!($c4, s, 4);
            let a5 = slot_for!($c5, s, 5);
            call_through!(
                ptr, ret,
                [a0: ty_for!($c0), a1: ty_for!($c1),
                 a2: ty_for!($c2), a3: ty_for!($c3),
                 a4: ty_for!($c4), a5: ty_for!($c5)]
            )
        }
    };
    ($name:ident, $c0:ident, $c1:ident, $c2:ident, $c3:ident, $c4:ident, $c5:ident, $c6:ident) => {
        unsafe fn $name(ptr: *const u8, s: &[Slot], ret: JitKind) -> Option<Value> {
            let a0 = slot_for!($c0, s, 0);
            let a1 = slot_for!($c1, s, 1);
            let a2 = slot_for!($c2, s, 2);
            let a3 = slot_for!($c3, s, 3);
            let a4 = slot_for!($c4, s, 4);
            let a5 = slot_for!($c5, s, 5);
            let a6 = slot_for!($c6, s, 6);
            call_through!(
                ptr, ret,
                [a0: ty_for!($c0), a1: ty_for!($c1),
                 a2: ty_for!($c2), a3: ty_for!($c3),
                 a4: ty_for!($c4), a5: ty_for!($c5),
                 a6: ty_for!($c6)]
            )
        }
    };
    ($name:ident, $c0:ident, $c1:ident, $c2:ident, $c3:ident, $c4:ident, $c5:ident, $c6:ident, $c7:ident) => {
        unsafe fn $name(ptr: *const u8, s: &[Slot], ret: JitKind) -> Option<Value> {
            let a0 = slot_for!($c0, s, 0);
            let a1 = slot_for!($c1, s, 1);
            let a2 = slot_for!($c2, s, 2);
            let a3 = slot_for!($c3, s, 3);
            let a4 = slot_for!($c4, s, 4);
            let a5 = slot_for!($c5, s, 5);
            let a6 = slot_for!($c6, s, 6);
            let a7 = slot_for!($c7, s, 7);
            call_through!(
                ptr, ret,
                [a0: ty_for!($c0), a1: ty_for!($c1),
                 a2: ty_for!($c2), a3: ty_for!($c3),
                 a4: ty_for!($c4), a5: ty_for!($c5),
                 a6: ty_for!($c6), a7: ty_for!($c7)]
            )
        }
    };
    ($name:ident, $c0:ident, $c1:ident, $c2:ident, $c3:ident, $c4:ident,
     $c5:ident, $c6:ident, $c7:ident, $c8:ident) => {
        unsafe fn $name(ptr: *const u8, s: &[Slot], ret: JitKind) -> Option<Value> {
            let a0 = slot_for!($c0, s, 0);
            let a1 = slot_for!($c1, s, 1);
            let a2 = slot_for!($c2, s, 2);
            let a3 = slot_for!($c3, s, 3);
            let a4 = slot_for!($c4, s, 4);
            let a5 = slot_for!($c5, s, 5);
            let a6 = slot_for!($c6, s, 6);
            let a7 = slot_for!($c7, s, 7);
            let a8 = slot_for!($c8, s, 8);
            call_through!(
                ptr, ret,
                [a0: ty_for!($c0), a1: ty_for!($c1),
                 a2: ty_for!($c2), a3: ty_for!($c3),
                 a4: ty_for!($c4), a5: ty_for!($c5),
                 a6: ty_for!($c6), a7: ty_for!($c7),
                 a8: ty_for!($c8)]
            )
        }
    };
    ($name:ident, $c0:ident, $c1:ident, $c2:ident, $c3:ident, $c4:ident,
     $c5:ident, $c6:ident, $c7:ident, $c8:ident, $c9:ident) => {
        unsafe fn $name(ptr: *const u8, s: &[Slot], ret: JitKind) -> Option<Value> {
            let a0 = slot_for!($c0, s, 0);
            let a1 = slot_for!($c1, s, 1);
            let a2 = slot_for!($c2, s, 2);
            let a3 = slot_for!($c3, s, 3);
            let a4 = slot_for!($c4, s, 4);
            let a5 = slot_for!($c5, s, 5);
            let a6 = slot_for!($c6, s, 6);
            let a7 = slot_for!($c7, s, 7);
            let a8 = slot_for!($c8, s, 8);
            let a9 = slot_for!($c9, s, 9);
            call_through!(
                ptr, ret,
                [a0: ty_for!($c0), a1: ty_for!($c1),
                 a2: ty_for!($c2), a3: ty_for!($c3),
                 a4: ty_for!($c4), a5: ty_for!($c5),
                 a6: ty_for!($c6), a7: ty_for!($c7),
                 a8: ty_for!($c8), a9: ty_for!($c9)]
            )
        }
    };
    ($name:ident, $c0:ident, $c1:ident, $c2:ident, $c3:ident, $c4:ident,
     $c5:ident, $c6:ident, $c7:ident, $c8:ident, $c9:ident, $c10:ident) => {
        unsafe fn $name(ptr: *const u8, s: &[Slot], ret: JitKind) -> Option<Value> {
            let a0 = slot_for!($c0, s, 0);
            let a1 = slot_for!($c1, s, 1);
            let a2 = slot_for!($c2, s, 2);
            let a3 = slot_for!($c3, s, 3);
            let a4 = slot_for!($c4, s, 4);
            let a5 = slot_for!($c5, s, 5);
            let a6 = slot_for!($c6, s, 6);
            let a7 = slot_for!($c7, s, 7);
            let a8 = slot_for!($c8, s, 8);
            let a9 = slot_for!($c9, s, 9);
            let a10 = slot_for!($c10, s, 10);
            call_through!(
                ptr, ret,
                [a0: ty_for!($c0), a1: ty_for!($c1),
                 a2: ty_for!($c2), a3: ty_for!($c3),
                 a4: ty_for!($c4), a5: ty_for!($c5),
                 a6: ty_for!($c6), a7: ty_for!($c7),
                 a8: ty_for!($c8), a9: ty_for!($c9),
                 a10: ty_for!($c10)]
            )
        }
    };
    ($name:ident, $c0:ident, $c1:ident, $c2:ident, $c3:ident, $c4:ident,
     $c5:ident, $c6:ident, $c7:ident, $c8:ident, $c9:ident, $c10:ident, $c11:ident) => {
        unsafe fn $name(ptr: *const u8, s: &[Slot], ret: JitKind) -> Option<Value> {
            let a0 = slot_for!($c0, s, 0);
            let a1 = slot_for!($c1, s, 1);
            let a2 = slot_for!($c2, s, 2);
            let a3 = slot_for!($c3, s, 3);
            let a4 = slot_for!($c4, s, 4);
            let a5 = slot_for!($c5, s, 5);
            let a6 = slot_for!($c6, s, 6);
            let a7 = slot_for!($c7, s, 7);
            let a8 = slot_for!($c8, s, 8);
            let a9 = slot_for!($c9, s, 9);
            let a10 = slot_for!($c10, s, 10);
            let a11 = slot_for!($c11, s, 11);
            call_through!(
                ptr, ret,
                [a0: ty_for!($c0), a1: ty_for!($c1),
                 a2: ty_for!($c2), a3: ty_for!($c3),
                 a4: ty_for!($c4), a5: ty_for!($c5),
                 a6: ty_for!($c6), a7: ty_for!($c7),
                 a8: ty_for!($c8), a9: ty_for!($c9),
                 a10: ty_for!($c10), a11: ty_for!($c11)]
            )
        }
    };
}

// Arity 1.
gen_call!(call_1i, i);
gen_call!(call_1f, f);
// Arity 2.
gen_call!(call_2ii, i, i);
gen_call!(call_2if, i, f);
gen_call!(call_2fi, f, i);
gen_call!(call_2ff, f, f);
// Arity 3.
gen_call!(call_3iii, i, i, i);
gen_call!(call_3fii, f, i, i);
gen_call!(call_3ifi, i, f, i);
gen_call!(call_3ffi, f, f, i);
gen_call!(call_3iif, i, i, f);
gen_call!(call_3fif, f, i, f);
gen_call!(call_3iff, i, f, f);
gen_call!(call_3fff, f, f, f);
// Arity 4.
gen_call!(call_4iiii, i, i, i, i);
gen_call!(call_4fiii, f, i, i, i);
gen_call!(call_4ifii, i, f, i, i);
gen_call!(call_4ffii, f, f, i, i);
gen_call!(call_4iifi, i, i, f, i);
gen_call!(call_4fifi, f, i, f, i);
gen_call!(call_4iffi, i, f, f, i);
gen_call!(call_4fffi, f, f, f, i);
gen_call!(call_4iiif, i, i, i, f);
gen_call!(call_4fiif, f, i, i, f);
gen_call!(call_4ifif, i, f, i, f);
gen_call!(call_4ffif, f, f, i, f);
gen_call!(call_4iiff, i, i, f, f);
gen_call!(call_4fiff, f, i, f, f);
gen_call!(call_4ifff, i, f, f, f);
gen_call!(call_4ffff, f, f, f, f);
// Arity 5-8: every int/float shape permutation.
gen_call!(call_5iiiii, i, i, i, i, i);
gen_call!(call_5fiiii, f, i, i, i, i);
gen_call!(call_5ifiii, i, f, i, i, i);
gen_call!(call_5ffiii, f, f, i, i, i);
gen_call!(call_5iifii, i, i, f, i, i);
gen_call!(call_5fifii, f, i, f, i, i);
gen_call!(call_5iffii, i, f, f, i, i);
gen_call!(call_5fffii, f, f, f, i, i);
gen_call!(call_5iiifi, i, i, i, f, i);
gen_call!(call_5fiifi, f, i, i, f, i);
gen_call!(call_5ififi, i, f, i, f, i);
gen_call!(call_5ffifi, f, f, i, f, i);
gen_call!(call_5iiffi, i, i, f, f, i);
gen_call!(call_5fiffi, f, i, f, f, i);
gen_call!(call_5ifffi, i, f, f, f, i);
gen_call!(call_5ffffi, f, f, f, f, i);
gen_call!(call_5iiiif, i, i, i, i, f);
gen_call!(call_5fiiif, f, i, i, i, f);
gen_call!(call_5ifiif, i, f, i, i, f);
gen_call!(call_5ffiif, f, f, i, i, f);
gen_call!(call_5iifif, i, i, f, i, f);
gen_call!(call_5fifif, f, i, f, i, f);
gen_call!(call_5iffif, i, f, f, i, f);
gen_call!(call_5fffif, f, f, f, i, f);
gen_call!(call_5iiiff, i, i, i, f, f);
gen_call!(call_5fiiff, f, i, i, f, f);
gen_call!(call_5ififf, i, f, i, f, f);
gen_call!(call_5ffiff, f, f, i, f, f);
gen_call!(call_5iifff, i, i, f, f, f);
gen_call!(call_5fifff, f, i, f, f, f);
gen_call!(call_5iffff, i, f, f, f, f);
gen_call!(call_5fffff, f, f, f, f, f);
gen_call!(call_6iiiiii, i, i, i, i, i, i);
gen_call!(call_6fiiiii, f, i, i, i, i, i);
gen_call!(call_6ifiiii, i, f, i, i, i, i);
gen_call!(call_6ffiiii, f, f, i, i, i, i);
gen_call!(call_6iifiii, i, i, f, i, i, i);
gen_call!(call_6fifiii, f, i, f, i, i, i);
gen_call!(call_6iffiii, i, f, f, i, i, i);
gen_call!(call_6fffiii, f, f, f, i, i, i);
gen_call!(call_6iiifii, i, i, i, f, i, i);
gen_call!(call_6fiifii, f, i, i, f, i, i);
gen_call!(call_6ififii, i, f, i, f, i, i);
gen_call!(call_6ffifii, f, f, i, f, i, i);
gen_call!(call_6iiffii, i, i, f, f, i, i);
gen_call!(call_6fiffii, f, i, f, f, i, i);
gen_call!(call_6ifffii, i, f, f, f, i, i);
gen_call!(call_6ffffii, f, f, f, f, i, i);
gen_call!(call_6iiiifi, i, i, i, i, f, i);
gen_call!(call_6fiiifi, f, i, i, i, f, i);
gen_call!(call_6ifiifi, i, f, i, i, f, i);
gen_call!(call_6ffiifi, f, f, i, i, f, i);
gen_call!(call_6iififi, i, i, f, i, f, i);
gen_call!(call_6fififi, f, i, f, i, f, i);
gen_call!(call_6iffifi, i, f, f, i, f, i);
gen_call!(call_6fffifi, f, f, f, i, f, i);
gen_call!(call_6iiiffi, i, i, i, f, f, i);
gen_call!(call_6fiiffi, f, i, i, f, f, i);
gen_call!(call_6ififfi, i, f, i, f, f, i);
gen_call!(call_6ffiffi, f, f, i, f, f, i);
gen_call!(call_6iifffi, i, i, f, f, f, i);
gen_call!(call_6fifffi, f, i, f, f, f, i);
gen_call!(call_6iffffi, i, f, f, f, f, i);
gen_call!(call_6fffffi, f, f, f, f, f, i);
gen_call!(call_6iiiiif, i, i, i, i, i, f);
gen_call!(call_6fiiiif, f, i, i, i, i, f);
gen_call!(call_6ifiiif, i, f, i, i, i, f);
gen_call!(call_6ffiiif, f, f, i, i, i, f);
gen_call!(call_6iifiif, i, i, f, i, i, f);
gen_call!(call_6fifiif, f, i, f, i, i, f);
gen_call!(call_6iffiif, i, f, f, i, i, f);
gen_call!(call_6fffiif, f, f, f, i, i, f);
gen_call!(call_6iiifif, i, i, i, f, i, f);
gen_call!(call_6fiifif, f, i, i, f, i, f);
gen_call!(call_6ififif, i, f, i, f, i, f);
gen_call!(call_6ffifif, f, f, i, f, i, f);
gen_call!(call_6iiffif, i, i, f, f, i, f);
gen_call!(call_6fiffif, f, i, f, f, i, f);
gen_call!(call_6ifffif, i, f, f, f, i, f);
gen_call!(call_6ffffif, f, f, f, f, i, f);
gen_call!(call_6iiiiff, i, i, i, i, f, f);
gen_call!(call_6fiiiff, f, i, i, i, f, f);
gen_call!(call_6ifiiff, i, f, i, i, f, f);
gen_call!(call_6ffiiff, f, f, i, i, f, f);
gen_call!(call_6iififf, i, i, f, i, f, f);
gen_call!(call_6fififf, f, i, f, i, f, f);
gen_call!(call_6iffiff, i, f, f, i, f, f);
gen_call!(call_6fffiff, f, f, f, i, f, f);
gen_call!(call_6iiifff, i, i, i, f, f, f);
gen_call!(call_6fiifff, f, i, i, f, f, f);
gen_call!(call_6ififff, i, f, i, f, f, f);
gen_call!(call_6ffifff, f, f, i, f, f, f);
gen_call!(call_6iiffff, i, i, f, f, f, f);
gen_call!(call_6fiffff, f, i, f, f, f, f);
gen_call!(call_6ifffff, i, f, f, f, f, f);
gen_call!(call_6ffffff, f, f, f, f, f, f);
gen_call!(call_7iiiiiii, i, i, i, i, i, i, i);
gen_call!(call_7fiiiiii, f, i, i, i, i, i, i);
gen_call!(call_7ifiiiii, i, f, i, i, i, i, i);
gen_call!(call_7ffiiiii, f, f, i, i, i, i, i);
gen_call!(call_7iifiiii, i, i, f, i, i, i, i);
gen_call!(call_7fifiiii, f, i, f, i, i, i, i);
gen_call!(call_7iffiiii, i, f, f, i, i, i, i);
gen_call!(call_7fffiiii, f, f, f, i, i, i, i);
gen_call!(call_7iiifiii, i, i, i, f, i, i, i);
gen_call!(call_7fiifiii, f, i, i, f, i, i, i);
gen_call!(call_7ififiii, i, f, i, f, i, i, i);
gen_call!(call_7ffifiii, f, f, i, f, i, i, i);
gen_call!(call_7iiffiii, i, i, f, f, i, i, i);
gen_call!(call_7fiffiii, f, i, f, f, i, i, i);
gen_call!(call_7ifffiii, i, f, f, f, i, i, i);
gen_call!(call_7ffffiii, f, f, f, f, i, i, i);
gen_call!(call_7iiiifii, i, i, i, i, f, i, i);
gen_call!(call_7fiiifii, f, i, i, i, f, i, i);
gen_call!(call_7ifiifii, i, f, i, i, f, i, i);
gen_call!(call_7ffiifii, f, f, i, i, f, i, i);
gen_call!(call_7iififii, i, i, f, i, f, i, i);
gen_call!(call_7fififii, f, i, f, i, f, i, i);
gen_call!(call_7iffifii, i, f, f, i, f, i, i);
gen_call!(call_7fffifii, f, f, f, i, f, i, i);
gen_call!(call_7iiiffii, i, i, i, f, f, i, i);
gen_call!(call_7fiiffii, f, i, i, f, f, i, i);
gen_call!(call_7ififfii, i, f, i, f, f, i, i);
gen_call!(call_7ffiffii, f, f, i, f, f, i, i);
gen_call!(call_7iifffii, i, i, f, f, f, i, i);
gen_call!(call_7fifffii, f, i, f, f, f, i, i);
gen_call!(call_7iffffii, i, f, f, f, f, i, i);
gen_call!(call_7fffffii, f, f, f, f, f, i, i);
gen_call!(call_7iiiiifi, i, i, i, i, i, f, i);
gen_call!(call_7fiiiifi, f, i, i, i, i, f, i);
gen_call!(call_7ifiiifi, i, f, i, i, i, f, i);
gen_call!(call_7ffiiifi, f, f, i, i, i, f, i);
gen_call!(call_7iifiifi, i, i, f, i, i, f, i);
gen_call!(call_7fifiifi, f, i, f, i, i, f, i);
gen_call!(call_7iffiifi, i, f, f, i, i, f, i);
gen_call!(call_7fffiifi, f, f, f, i, i, f, i);
gen_call!(call_7iiififi, i, i, i, f, i, f, i);
gen_call!(call_7fiififi, f, i, i, f, i, f, i);
gen_call!(call_7ifififi, i, f, i, f, i, f, i);
gen_call!(call_7ffififi, f, f, i, f, i, f, i);
gen_call!(call_7iiffifi, i, i, f, f, i, f, i);
gen_call!(call_7fiffifi, f, i, f, f, i, f, i);
gen_call!(call_7ifffifi, i, f, f, f, i, f, i);
gen_call!(call_7ffffifi, f, f, f, f, i, f, i);
gen_call!(call_7iiiiffi, i, i, i, i, f, f, i);
gen_call!(call_7fiiiffi, f, i, i, i, f, f, i);
gen_call!(call_7ifiiffi, i, f, i, i, f, f, i);
gen_call!(call_7ffiiffi, f, f, i, i, f, f, i);
gen_call!(call_7iififfi, i, i, f, i, f, f, i);
gen_call!(call_7fififfi, f, i, f, i, f, f, i);
gen_call!(call_7iffiffi, i, f, f, i, f, f, i);
gen_call!(call_7fffiffi, f, f, f, i, f, f, i);
gen_call!(call_7iiifffi, i, i, i, f, f, f, i);
gen_call!(call_7fiifffi, f, i, i, f, f, f, i);
gen_call!(call_7ififffi, i, f, i, f, f, f, i);
gen_call!(call_7ffifffi, f, f, i, f, f, f, i);
gen_call!(call_7iiffffi, i, i, f, f, f, f, i);
gen_call!(call_7fiffffi, f, i, f, f, f, f, i);
gen_call!(call_7ifffffi, i, f, f, f, f, f, i);
gen_call!(call_7ffffffi, f, f, f, f, f, f, i);
gen_call!(call_7iiiiiif, i, i, i, i, i, i, f);
gen_call!(call_7fiiiiif, f, i, i, i, i, i, f);
gen_call!(call_7ifiiiif, i, f, i, i, i, i, f);
gen_call!(call_7ffiiiif, f, f, i, i, i, i, f);
gen_call!(call_7iifiiif, i, i, f, i, i, i, f);
gen_call!(call_7fifiiif, f, i, f, i, i, i, f);
gen_call!(call_7iffiiif, i, f, f, i, i, i, f);
gen_call!(call_7fffiiif, f, f, f, i, i, i, f);
gen_call!(call_7iiifiif, i, i, i, f, i, i, f);
gen_call!(call_7fiifiif, f, i, i, f, i, i, f);
gen_call!(call_7ififiif, i, f, i, f, i, i, f);
gen_call!(call_7ffifiif, f, f, i, f, i, i, f);
gen_call!(call_7iiffiif, i, i, f, f, i, i, f);
gen_call!(call_7fiffiif, f, i, f, f, i, i, f);
gen_call!(call_7ifffiif, i, f, f, f, i, i, f);
gen_call!(call_7ffffiif, f, f, f, f, i, i, f);
gen_call!(call_7iiiifif, i, i, i, i, f, i, f);
gen_call!(call_7fiiifif, f, i, i, i, f, i, f);
gen_call!(call_7ifiifif, i, f, i, i, f, i, f);
gen_call!(call_7ffiifif, f, f, i, i, f, i, f);
gen_call!(call_7iififif, i, i, f, i, f, i, f);
gen_call!(call_7fififif, f, i, f, i, f, i, f);
gen_call!(call_7iffifif, i, f, f, i, f, i, f);
gen_call!(call_7fffifif, f, f, f, i, f, i, f);
gen_call!(call_7iiiffif, i, i, i, f, f, i, f);
gen_call!(call_7fiiffif, f, i, i, f, f, i, f);
gen_call!(call_7ififfif, i, f, i, f, f, i, f);
gen_call!(call_7ffiffif, f, f, i, f, f, i, f);
gen_call!(call_7iifffif, i, i, f, f, f, i, f);
gen_call!(call_7fifffif, f, i, f, f, f, i, f);
gen_call!(call_7iffffif, i, f, f, f, f, i, f);
gen_call!(call_7fffffif, f, f, f, f, f, i, f);
gen_call!(call_7iiiiiff, i, i, i, i, i, f, f);
gen_call!(call_7fiiiiff, f, i, i, i, i, f, f);
gen_call!(call_7ifiiiff, i, f, i, i, i, f, f);
gen_call!(call_7ffiiiff, f, f, i, i, i, f, f);
gen_call!(call_7iifiiff, i, i, f, i, i, f, f);
gen_call!(call_7fifiiff, f, i, f, i, i, f, f);
gen_call!(call_7iffiiff, i, f, f, i, i, f, f);
gen_call!(call_7fffiiff, f, f, f, i, i, f, f);
gen_call!(call_7iiififf, i, i, i, f, i, f, f);
gen_call!(call_7fiififf, f, i, i, f, i, f, f);
gen_call!(call_7ifififf, i, f, i, f, i, f, f);
gen_call!(call_7ffififf, f, f, i, f, i, f, f);
gen_call!(call_7iiffiff, i, i, f, f, i, f, f);
gen_call!(call_7fiffiff, f, i, f, f, i, f, f);
gen_call!(call_7ifffiff, i, f, f, f, i, f, f);
gen_call!(call_7ffffiff, f, f, f, f, i, f, f);
gen_call!(call_7iiiifff, i, i, i, i, f, f, f);
gen_call!(call_7fiiifff, f, i, i, i, f, f, f);
gen_call!(call_7ifiifff, i, f, i, i, f, f, f);
gen_call!(call_7ffiifff, f, f, i, i, f, f, f);
gen_call!(call_7iififff, i, i, f, i, f, f, f);
gen_call!(call_7fififff, f, i, f, i, f, f, f);
gen_call!(call_7iffifff, i, f, f, i, f, f, f);
gen_call!(call_7fffifff, f, f, f, i, f, f, f);
gen_call!(call_7iiiffff, i, i, i, f, f, f, f);
gen_call!(call_7fiiffff, f, i, i, f, f, f, f);
gen_call!(call_7ififfff, i, f, i, f, f, f, f);
gen_call!(call_7ffiffff, f, f, i, f, f, f, f);
gen_call!(call_7iifffff, i, i, f, f, f, f, f);
gen_call!(call_7fifffff, f, i, f, f, f, f, f);
gen_call!(call_7iffffff, i, f, f, f, f, f, f);
gen_call!(call_7fffffff, f, f, f, f, f, f, f);
gen_call!(call_8iiiiiiii, i, i, i, i, i, i, i, i);
gen_call!(call_8fiiiiiii, f, i, i, i, i, i, i, i);
gen_call!(call_8ifiiiiii, i, f, i, i, i, i, i, i);
gen_call!(call_8ffiiiiii, f, f, i, i, i, i, i, i);
gen_call!(call_8iifiiiii, i, i, f, i, i, i, i, i);
gen_call!(call_8fifiiiii, f, i, f, i, i, i, i, i);
gen_call!(call_8iffiiiii, i, f, f, i, i, i, i, i);
gen_call!(call_8fffiiiii, f, f, f, i, i, i, i, i);
gen_call!(call_8iiifiiii, i, i, i, f, i, i, i, i);
gen_call!(call_8fiifiiii, f, i, i, f, i, i, i, i);
gen_call!(call_8ififiiii, i, f, i, f, i, i, i, i);
gen_call!(call_8ffifiiii, f, f, i, f, i, i, i, i);
gen_call!(call_8iiffiiii, i, i, f, f, i, i, i, i);
gen_call!(call_8fiffiiii, f, i, f, f, i, i, i, i);
gen_call!(call_8ifffiiii, i, f, f, f, i, i, i, i);
gen_call!(call_8ffffiiii, f, f, f, f, i, i, i, i);
gen_call!(call_8iiiifiii, i, i, i, i, f, i, i, i);
gen_call!(call_8fiiifiii, f, i, i, i, f, i, i, i);
gen_call!(call_8ifiifiii, i, f, i, i, f, i, i, i);
gen_call!(call_8ffiifiii, f, f, i, i, f, i, i, i);
gen_call!(call_8iififiii, i, i, f, i, f, i, i, i);
gen_call!(call_8fififiii, f, i, f, i, f, i, i, i);
gen_call!(call_8iffifiii, i, f, f, i, f, i, i, i);
gen_call!(call_8fffifiii, f, f, f, i, f, i, i, i);
gen_call!(call_8iiiffiii, i, i, i, f, f, i, i, i);
gen_call!(call_8fiiffiii, f, i, i, f, f, i, i, i);
gen_call!(call_8ififfiii, i, f, i, f, f, i, i, i);
gen_call!(call_8ffiffiii, f, f, i, f, f, i, i, i);
gen_call!(call_8iifffiii, i, i, f, f, f, i, i, i);
gen_call!(call_8fifffiii, f, i, f, f, f, i, i, i);
gen_call!(call_8iffffiii, i, f, f, f, f, i, i, i);
gen_call!(call_8fffffiii, f, f, f, f, f, i, i, i);
gen_call!(call_8iiiiifii, i, i, i, i, i, f, i, i);
gen_call!(call_8fiiiifii, f, i, i, i, i, f, i, i);
gen_call!(call_8ifiiifii, i, f, i, i, i, f, i, i);
gen_call!(call_8ffiiifii, f, f, i, i, i, f, i, i);
gen_call!(call_8iifiifii, i, i, f, i, i, f, i, i);
gen_call!(call_8fifiifii, f, i, f, i, i, f, i, i);
gen_call!(call_8iffiifii, i, f, f, i, i, f, i, i);
gen_call!(call_8fffiifii, f, f, f, i, i, f, i, i);
gen_call!(call_8iiififii, i, i, i, f, i, f, i, i);
gen_call!(call_8fiififii, f, i, i, f, i, f, i, i);
gen_call!(call_8ifififii, i, f, i, f, i, f, i, i);
gen_call!(call_8ffififii, f, f, i, f, i, f, i, i);
gen_call!(call_8iiffifii, i, i, f, f, i, f, i, i);
gen_call!(call_8fiffifii, f, i, f, f, i, f, i, i);
gen_call!(call_8ifffifii, i, f, f, f, i, f, i, i);
gen_call!(call_8ffffifii, f, f, f, f, i, f, i, i);
gen_call!(call_8iiiiffii, i, i, i, i, f, f, i, i);
gen_call!(call_8fiiiffii, f, i, i, i, f, f, i, i);
gen_call!(call_8ifiiffii, i, f, i, i, f, f, i, i);
gen_call!(call_8ffiiffii, f, f, i, i, f, f, i, i);
gen_call!(call_8iififfii, i, i, f, i, f, f, i, i);
gen_call!(call_8fififfii, f, i, f, i, f, f, i, i);
gen_call!(call_8iffiffii, i, f, f, i, f, f, i, i);
gen_call!(call_8fffiffii, f, f, f, i, f, f, i, i);
gen_call!(call_8iiifffii, i, i, i, f, f, f, i, i);
gen_call!(call_8fiifffii, f, i, i, f, f, f, i, i);
gen_call!(call_8ififffii, i, f, i, f, f, f, i, i);
gen_call!(call_8ffifffii, f, f, i, f, f, f, i, i);
gen_call!(call_8iiffffii, i, i, f, f, f, f, i, i);
gen_call!(call_8fiffffii, f, i, f, f, f, f, i, i);
gen_call!(call_8ifffffii, i, f, f, f, f, f, i, i);
gen_call!(call_8ffffffii, f, f, f, f, f, f, i, i);
gen_call!(call_8iiiiiifi, i, i, i, i, i, i, f, i);
gen_call!(call_8fiiiiifi, f, i, i, i, i, i, f, i);
gen_call!(call_8ifiiiifi, i, f, i, i, i, i, f, i);
gen_call!(call_8ffiiiifi, f, f, i, i, i, i, f, i);
gen_call!(call_8iifiiifi, i, i, f, i, i, i, f, i);
gen_call!(call_8fifiiifi, f, i, f, i, i, i, f, i);
gen_call!(call_8iffiiifi, i, f, f, i, i, i, f, i);
gen_call!(call_8fffiiifi, f, f, f, i, i, i, f, i);
gen_call!(call_8iiifiifi, i, i, i, f, i, i, f, i);
gen_call!(call_8fiifiifi, f, i, i, f, i, i, f, i);
gen_call!(call_8ififiifi, i, f, i, f, i, i, f, i);
gen_call!(call_8ffifiifi, f, f, i, f, i, i, f, i);
gen_call!(call_8iiffiifi, i, i, f, f, i, i, f, i);
gen_call!(call_8fiffiifi, f, i, f, f, i, i, f, i);
gen_call!(call_8ifffiifi, i, f, f, f, i, i, f, i);
gen_call!(call_8ffffiifi, f, f, f, f, i, i, f, i);
gen_call!(call_8iiiififi, i, i, i, i, f, i, f, i);
gen_call!(call_8fiiififi, f, i, i, i, f, i, f, i);
gen_call!(call_8ifiififi, i, f, i, i, f, i, f, i);
gen_call!(call_8ffiififi, f, f, i, i, f, i, f, i);
gen_call!(call_8iifififi, i, i, f, i, f, i, f, i);
gen_call!(call_8fifififi, f, i, f, i, f, i, f, i);
gen_call!(call_8iffififi, i, f, f, i, f, i, f, i);
gen_call!(call_8fffififi, f, f, f, i, f, i, f, i);
gen_call!(call_8iiiffifi, i, i, i, f, f, i, f, i);
gen_call!(call_8fiiffifi, f, i, i, f, f, i, f, i);
gen_call!(call_8ififfifi, i, f, i, f, f, i, f, i);
gen_call!(call_8ffiffifi, f, f, i, f, f, i, f, i);
gen_call!(call_8iifffifi, i, i, f, f, f, i, f, i);
gen_call!(call_8fifffifi, f, i, f, f, f, i, f, i);
gen_call!(call_8iffffifi, i, f, f, f, f, i, f, i);
gen_call!(call_8fffffifi, f, f, f, f, f, i, f, i);
gen_call!(call_8iiiiiffi, i, i, i, i, i, f, f, i);
gen_call!(call_8fiiiiffi, f, i, i, i, i, f, f, i);
gen_call!(call_8ifiiiffi, i, f, i, i, i, f, f, i);
gen_call!(call_8ffiiiffi, f, f, i, i, i, f, f, i);
gen_call!(call_8iifiiffi, i, i, f, i, i, f, f, i);
gen_call!(call_8fifiiffi, f, i, f, i, i, f, f, i);
gen_call!(call_8iffiiffi, i, f, f, i, i, f, f, i);
gen_call!(call_8fffiiffi, f, f, f, i, i, f, f, i);
gen_call!(call_8iiififfi, i, i, i, f, i, f, f, i);
gen_call!(call_8fiififfi, f, i, i, f, i, f, f, i);
gen_call!(call_8ifififfi, i, f, i, f, i, f, f, i);
gen_call!(call_8ffififfi, f, f, i, f, i, f, f, i);
gen_call!(call_8iiffiffi, i, i, f, f, i, f, f, i);
gen_call!(call_8fiffiffi, f, i, f, f, i, f, f, i);
gen_call!(call_8ifffiffi, i, f, f, f, i, f, f, i);
gen_call!(call_8ffffiffi, f, f, f, f, i, f, f, i);
gen_call!(call_8iiiifffi, i, i, i, i, f, f, f, i);
gen_call!(call_8fiiifffi, f, i, i, i, f, f, f, i);
gen_call!(call_8ifiifffi, i, f, i, i, f, f, f, i);
gen_call!(call_8ffiifffi, f, f, i, i, f, f, f, i);
gen_call!(call_8iififffi, i, i, f, i, f, f, f, i);
gen_call!(call_8fififffi, f, i, f, i, f, f, f, i);
gen_call!(call_8iffifffi, i, f, f, i, f, f, f, i);
gen_call!(call_8fffifffi, f, f, f, i, f, f, f, i);
gen_call!(call_8iiiffffi, i, i, i, f, f, f, f, i);
gen_call!(call_8fiiffffi, f, i, i, f, f, f, f, i);
gen_call!(call_8ififfffi, i, f, i, f, f, f, f, i);
gen_call!(call_8ffiffffi, f, f, i, f, f, f, f, i);
gen_call!(call_8iifffffi, i, i, f, f, f, f, f, i);
gen_call!(call_8fifffffi, f, i, f, f, f, f, f, i);
gen_call!(call_8iffffffi, i, f, f, f, f, f, f, i);
gen_call!(call_8fffffffi, f, f, f, f, f, f, f, i);
gen_call!(call_8iiiiiiif, i, i, i, i, i, i, i, f);
gen_call!(call_8fiiiiiif, f, i, i, i, i, i, i, f);
gen_call!(call_8ifiiiiif, i, f, i, i, i, i, i, f);
gen_call!(call_8ffiiiiif, f, f, i, i, i, i, i, f);
gen_call!(call_8iifiiiif, i, i, f, i, i, i, i, f);
gen_call!(call_8fifiiiif, f, i, f, i, i, i, i, f);
gen_call!(call_8iffiiiif, i, f, f, i, i, i, i, f);
gen_call!(call_8fffiiiif, f, f, f, i, i, i, i, f);
gen_call!(call_8iiifiiif, i, i, i, f, i, i, i, f);
gen_call!(call_8fiifiiif, f, i, i, f, i, i, i, f);
gen_call!(call_8ififiiif, i, f, i, f, i, i, i, f);
gen_call!(call_8ffifiiif, f, f, i, f, i, i, i, f);
gen_call!(call_8iiffiiif, i, i, f, f, i, i, i, f);
gen_call!(call_8fiffiiif, f, i, f, f, i, i, i, f);
gen_call!(call_8ifffiiif, i, f, f, f, i, i, i, f);
gen_call!(call_8ffffiiif, f, f, f, f, i, i, i, f);
gen_call!(call_8iiiifiif, i, i, i, i, f, i, i, f);
gen_call!(call_8fiiifiif, f, i, i, i, f, i, i, f);
gen_call!(call_8ifiifiif, i, f, i, i, f, i, i, f);
gen_call!(call_8ffiifiif, f, f, i, i, f, i, i, f);
gen_call!(call_8iififiif, i, i, f, i, f, i, i, f);
gen_call!(call_8fififiif, f, i, f, i, f, i, i, f);
gen_call!(call_8iffifiif, i, f, f, i, f, i, i, f);
gen_call!(call_8fffifiif, f, f, f, i, f, i, i, f);
gen_call!(call_8iiiffiif, i, i, i, f, f, i, i, f);
gen_call!(call_8fiiffiif, f, i, i, f, f, i, i, f);
gen_call!(call_8ififfiif, i, f, i, f, f, i, i, f);
gen_call!(call_8ffiffiif, f, f, i, f, f, i, i, f);
gen_call!(call_8iifffiif, i, i, f, f, f, i, i, f);
gen_call!(call_8fifffiif, f, i, f, f, f, i, i, f);
gen_call!(call_8iffffiif, i, f, f, f, f, i, i, f);
gen_call!(call_8fffffiif, f, f, f, f, f, i, i, f);
gen_call!(call_8iiiiifif, i, i, i, i, i, f, i, f);
gen_call!(call_8fiiiifif, f, i, i, i, i, f, i, f);
gen_call!(call_8ifiiifif, i, f, i, i, i, f, i, f);
gen_call!(call_8ffiiifif, f, f, i, i, i, f, i, f);
gen_call!(call_8iifiifif, i, i, f, i, i, f, i, f);
gen_call!(call_8fifiifif, f, i, f, i, i, f, i, f);
gen_call!(call_8iffiifif, i, f, f, i, i, f, i, f);
gen_call!(call_8fffiifif, f, f, f, i, i, f, i, f);
gen_call!(call_8iiififif, i, i, i, f, i, f, i, f);
gen_call!(call_8fiififif, f, i, i, f, i, f, i, f);
gen_call!(call_8ifififif, i, f, i, f, i, f, i, f);
gen_call!(call_8ffififif, f, f, i, f, i, f, i, f);
gen_call!(call_8iiffifif, i, i, f, f, i, f, i, f);
gen_call!(call_8fiffifif, f, i, f, f, i, f, i, f);
gen_call!(call_8ifffifif, i, f, f, f, i, f, i, f);
gen_call!(call_8ffffifif, f, f, f, f, i, f, i, f);
gen_call!(call_8iiiiffif, i, i, i, i, f, f, i, f);
gen_call!(call_8fiiiffif, f, i, i, i, f, f, i, f);
gen_call!(call_8ifiiffif, i, f, i, i, f, f, i, f);
gen_call!(call_8ffiiffif, f, f, i, i, f, f, i, f);
gen_call!(call_8iififfif, i, i, f, i, f, f, i, f);
gen_call!(call_8fififfif, f, i, f, i, f, f, i, f);
gen_call!(call_8iffiffif, i, f, f, i, f, f, i, f);
gen_call!(call_8fffiffif, f, f, f, i, f, f, i, f);
gen_call!(call_8iiifffif, i, i, i, f, f, f, i, f);
gen_call!(call_8fiifffif, f, i, i, f, f, f, i, f);
gen_call!(call_8ififffif, i, f, i, f, f, f, i, f);
gen_call!(call_8ffifffif, f, f, i, f, f, f, i, f);
gen_call!(call_8iiffffif, i, i, f, f, f, f, i, f);
gen_call!(call_8fiffffif, f, i, f, f, f, f, i, f);
gen_call!(call_8ifffffif, i, f, f, f, f, f, i, f);
gen_call!(call_8ffffffif, f, f, f, f, f, f, i, f);
gen_call!(call_8iiiiiiff, i, i, i, i, i, i, f, f);
gen_call!(call_8fiiiiiff, f, i, i, i, i, i, f, f);
gen_call!(call_8ifiiiiff, i, f, i, i, i, i, f, f);
gen_call!(call_8ffiiiiff, f, f, i, i, i, i, f, f);
gen_call!(call_8iifiiiff, i, i, f, i, i, i, f, f);
gen_call!(call_8fifiiiff, f, i, f, i, i, i, f, f);
gen_call!(call_8iffiiiff, i, f, f, i, i, i, f, f);
gen_call!(call_8fffiiiff, f, f, f, i, i, i, f, f);
gen_call!(call_8iiifiiff, i, i, i, f, i, i, f, f);
gen_call!(call_8fiifiiff, f, i, i, f, i, i, f, f);
gen_call!(call_8ififiiff, i, f, i, f, i, i, f, f);
gen_call!(call_8ffifiiff, f, f, i, f, i, i, f, f);
gen_call!(call_8iiffiiff, i, i, f, f, i, i, f, f);
gen_call!(call_8fiffiiff, f, i, f, f, i, i, f, f);
gen_call!(call_8ifffiiff, i, f, f, f, i, i, f, f);
gen_call!(call_8ffffiiff, f, f, f, f, i, i, f, f);
gen_call!(call_8iiiififf, i, i, i, i, f, i, f, f);
gen_call!(call_8fiiififf, f, i, i, i, f, i, f, f);
gen_call!(call_8ifiififf, i, f, i, i, f, i, f, f);
gen_call!(call_8ffiififf, f, f, i, i, f, i, f, f);
gen_call!(call_8iifififf, i, i, f, i, f, i, f, f);
gen_call!(call_8fifififf, f, i, f, i, f, i, f, f);
gen_call!(call_8iffififf, i, f, f, i, f, i, f, f);
gen_call!(call_8fffififf, f, f, f, i, f, i, f, f);
gen_call!(call_8iiiffiff, i, i, i, f, f, i, f, f);
gen_call!(call_8fiiffiff, f, i, i, f, f, i, f, f);
gen_call!(call_8ififfiff, i, f, i, f, f, i, f, f);
gen_call!(call_8ffiffiff, f, f, i, f, f, i, f, f);
gen_call!(call_8iifffiff, i, i, f, f, f, i, f, f);
gen_call!(call_8fifffiff, f, i, f, f, f, i, f, f);
gen_call!(call_8iffffiff, i, f, f, f, f, i, f, f);
gen_call!(call_8fffffiff, f, f, f, f, f, i, f, f);
gen_call!(call_8iiiiifff, i, i, i, i, i, f, f, f);
gen_call!(call_8fiiiifff, f, i, i, i, i, f, f, f);
gen_call!(call_8ifiiifff, i, f, i, i, i, f, f, f);
gen_call!(call_8ffiiifff, f, f, i, i, i, f, f, f);
gen_call!(call_8iifiifff, i, i, f, i, i, f, f, f);
gen_call!(call_8fifiifff, f, i, f, i, i, f, f, f);
gen_call!(call_8iffiifff, i, f, f, i, i, f, f, f);
gen_call!(call_8fffiifff, f, f, f, i, i, f, f, f);
gen_call!(call_8iiififff, i, i, i, f, i, f, f, f);
gen_call!(call_8fiififff, f, i, i, f, i, f, f, f);
gen_call!(call_8ifififff, i, f, i, f, i, f, f, f);
gen_call!(call_8ffififff, f, f, i, f, i, f, f, f);
gen_call!(call_8iiffifff, i, i, f, f, i, f, f, f);
gen_call!(call_8fiffifff, f, i, f, f, i, f, f, f);
gen_call!(call_8ifffifff, i, f, f, f, i, f, f, f);
gen_call!(call_8ffffifff, f, f, f, f, i, f, f, f);
gen_call!(call_8iiiiffff, i, i, i, i, f, f, f, f);
gen_call!(call_8fiiiffff, f, i, i, i, f, f, f, f);
gen_call!(call_8ifiiffff, i, f, i, i, f, f, f, f);
gen_call!(call_8ffiiffff, f, f, i, i, f, f, f, f);
gen_call!(call_8iififfff, i, i, f, i, f, f, f, f);
gen_call!(call_8fififfff, f, i, f, i, f, f, f, f);
gen_call!(call_8iffiffff, i, f, f, i, f, f, f, f);
gen_call!(call_8fffiffff, f, f, f, i, f, f, f, f);
gen_call!(call_8iiifffff, i, i, i, f, f, f, f, f);
gen_call!(call_8fiifffff, f, i, i, f, f, f, f, f);
gen_call!(call_8ififffff, i, f, i, f, f, f, f, f);
gen_call!(call_8ffifffff, f, f, i, f, f, f, f, f);
gen_call!(call_8iiffffff, i, i, f, f, f, f, f, f);
gen_call!(call_8fiffffff, f, i, f, f, f, f, f, f);
gen_call!(call_8ifffffff, i, f, f, f, f, f, f, f);
gen_call!(call_8ffffffff, f, f, f, f, f, f, f, f);
gen_call!(call_9i, i, i, i, i, i, i, i, i, i);
gen_call!(call_9f, f, f, f, f, f, f, f, f, f);
gen_call!(call_10i, i, i, i, i, i, i, i, i, i, i);
gen_call!(call_10f, f, f, f, f, f, f, f, f, f, f);
gen_call!(call_11i, i, i, i, i, i, i, i, i, i, i, i);
gen_call!(call_11f, f, f, f, f, f, f, f, f, f, f, f);
gen_call!(call_12i, i, i, i, i, i, i, i, i, i, i, i, i);
gen_call!(call_12f, f, f, f, f, f, f, f, f, f, f, f, f);

/// A monomorphised dispatch stub: marshals an arg-slot slice through a
/// reified `extern "C"` signature and returns the boxed result.
pub(crate) type StubFn = unsafe fn(*const u8, &[Slot], JitKind) -> Option<Value>;

/// Arity-0 stub (no `gen_call!` slot to bind).
unsafe fn call_0(ptr: *const u8, _s: &[Slot], ret: JitKind) -> Option<Value> {
    call_through!(ptr, ret, [])
}

/// Returns the stub for an `(arity, shape)` pair, or `None` for a shape
/// the trampoline doesn't cover. Mirrors the `match` in `invoke_prepared`.
pub(crate) fn resolve_stub(arity: usize, shape: u32) -> Option<StubFn> {
    let f: StubFn = match (arity, shape) {
        (0, _) => call_0,
        (1, 0b0) => call_1i,
        (1, 0b1) => call_1f,
        (2, 0b00) => call_2ii,
        (2, 0b01) => call_2fi,
        (2, 0b10) => call_2if,
        (2, 0b11) => call_2ff,
        (3, 0b000) => call_3iii,
        (3, 0b001) => call_3fii,
        (3, 0b010) => call_3ifi,
        (3, 0b011) => call_3ffi,
        (3, 0b100) => call_3iif,
        (3, 0b101) => call_3fif,
        (3, 0b110) => call_3iff,
        (3, 0b111) => call_3fff,
        (4, 0b0000) => call_4iiii,
        (4, 0b0001) => call_4fiii,
        (4, 0b0010) => call_4ifii,
        (4, 0b0011) => call_4ffii,
        (4, 0b0100) => call_4iifi,
        (4, 0b0101) => call_4fifi,
        (4, 0b0110) => call_4iffi,
        (4, 0b0111) => call_4fffi,
        (4, 0b1000) => call_4iiif,
        (4, 0b1001) => call_4fiif,
        (4, 0b1010) => call_4ifif,
        (4, 0b1011) => call_4ffif,
        (4, 0b1100) => call_4iiff,
        (4, 0b1101) => call_4fiff,
        (4, 0b1110) => call_4ifff,
        (4, 0b1111) => call_4ffff,
        (5, 0b00000) => call_5iiiii,
        (5, 0b00001) => call_5fiiii,
        (5, 0b00010) => call_5ifiii,
        (5, 0b00011) => call_5ffiii,
        (5, 0b00100) => call_5iifii,
        (5, 0b00101) => call_5fifii,
        (5, 0b00110) => call_5iffii,
        (5, 0b00111) => call_5fffii,
        (5, 0b01000) => call_5iiifi,
        (5, 0b01001) => call_5fiifi,
        (5, 0b01010) => call_5ififi,
        (5, 0b01011) => call_5ffifi,
        (5, 0b01100) => call_5iiffi,
        (5, 0b01101) => call_5fiffi,
        (5, 0b01110) => call_5ifffi,
        (5, 0b01111) => call_5ffffi,
        (5, 0b10000) => call_5iiiif,
        (5, 0b10001) => call_5fiiif,
        (5, 0b10010) => call_5ifiif,
        (5, 0b10011) => call_5ffiif,
        (5, 0b10100) => call_5iifif,
        (5, 0b10101) => call_5fifif,
        (5, 0b10110) => call_5iffif,
        (5, 0b10111) => call_5fffif,
        (5, 0b11000) => call_5iiiff,
        (5, 0b11001) => call_5fiiff,
        (5, 0b11010) => call_5ififf,
        (5, 0b11011) => call_5ffiff,
        (5, 0b11100) => call_5iifff,
        (5, 0b11101) => call_5fifff,
        (5, 0b11110) => call_5iffff,
        (5, 0b11111) => call_5fffff,
        (6, 0b000000) => call_6iiiiii,
        (6, 0b000001) => call_6fiiiii,
        (6, 0b000010) => call_6ifiiii,
        (6, 0b000011) => call_6ffiiii,
        (6, 0b000100) => call_6iifiii,
        (6, 0b000101) => call_6fifiii,
        (6, 0b000110) => call_6iffiii,
        (6, 0b000111) => call_6fffiii,
        (6, 0b001000) => call_6iiifii,
        (6, 0b001001) => call_6fiifii,
        (6, 0b001010) => call_6ififii,
        (6, 0b001011) => call_6ffifii,
        (6, 0b001100) => call_6iiffii,
        (6, 0b001101) => call_6fiffii,
        (6, 0b001110) => call_6ifffii,
        (6, 0b001111) => call_6ffffii,
        (6, 0b010000) => call_6iiiifi,
        (6, 0b010001) => call_6fiiifi,
        (6, 0b010010) => call_6ifiifi,
        (6, 0b010011) => call_6ffiifi,
        (6, 0b010100) => call_6iififi,
        (6, 0b010101) => call_6fififi,
        (6, 0b010110) => call_6iffifi,
        (6, 0b010111) => call_6fffifi,
        (6, 0b011000) => call_6iiiffi,
        (6, 0b011001) => call_6fiiffi,
        (6, 0b011010) => call_6ififfi,
        (6, 0b011011) => call_6ffiffi,
        (6, 0b011100) => call_6iifffi,
        (6, 0b011101) => call_6fifffi,
        (6, 0b011110) => call_6iffffi,
        (6, 0b011111) => call_6fffffi,
        (6, 0b100000) => call_6iiiiif,
        (6, 0b100001) => call_6fiiiif,
        (6, 0b100010) => call_6ifiiif,
        (6, 0b100011) => call_6ffiiif,
        (6, 0b100100) => call_6iifiif,
        (6, 0b100101) => call_6fifiif,
        (6, 0b100110) => call_6iffiif,
        (6, 0b100111) => call_6fffiif,
        (6, 0b101000) => call_6iiifif,
        (6, 0b101001) => call_6fiifif,
        (6, 0b101010) => call_6ififif,
        (6, 0b101011) => call_6ffifif,
        (6, 0b101100) => call_6iiffif,
        (6, 0b101101) => call_6fiffif,
        (6, 0b101110) => call_6ifffif,
        (6, 0b101111) => call_6ffffif,
        (6, 0b110000) => call_6iiiiff,
        (6, 0b110001) => call_6fiiiff,
        (6, 0b110010) => call_6ifiiff,
        (6, 0b110011) => call_6ffiiff,
        (6, 0b110100) => call_6iififf,
        (6, 0b110101) => call_6fififf,
        (6, 0b110110) => call_6iffiff,
        (6, 0b110111) => call_6fffiff,
        (6, 0b111000) => call_6iiifff,
        (6, 0b111001) => call_6fiifff,
        (6, 0b111010) => call_6ififff,
        (6, 0b111011) => call_6ffifff,
        (6, 0b111100) => call_6iiffff,
        (6, 0b111101) => call_6fiffff,
        (6, 0b111110) => call_6ifffff,
        (6, 0b111111) => call_6ffffff,
        (7, 0b0000000) => call_7iiiiiii,
        (7, 0b0000001) => call_7fiiiiii,
        (7, 0b0000010) => call_7ifiiiii,
        (7, 0b0000011) => call_7ffiiiii,
        (7, 0b0000100) => call_7iifiiii,
        (7, 0b0000101) => call_7fifiiii,
        (7, 0b0000110) => call_7iffiiii,
        (7, 0b0000111) => call_7fffiiii,
        (7, 0b0001000) => call_7iiifiii,
        (7, 0b0001001) => call_7fiifiii,
        (7, 0b0001010) => call_7ififiii,
        (7, 0b0001011) => call_7ffifiii,
        (7, 0b0001100) => call_7iiffiii,
        (7, 0b0001101) => call_7fiffiii,
        (7, 0b0001110) => call_7ifffiii,
        (7, 0b0001111) => call_7ffffiii,
        (7, 0b0010000) => call_7iiiifii,
        (7, 0b0010001) => call_7fiiifii,
        (7, 0b0010010) => call_7ifiifii,
        (7, 0b0010011) => call_7ffiifii,
        (7, 0b0010100) => call_7iififii,
        (7, 0b0010101) => call_7fififii,
        (7, 0b0010110) => call_7iffifii,
        (7, 0b0010111) => call_7fffifii,
        (7, 0b0011000) => call_7iiiffii,
        (7, 0b0011001) => call_7fiiffii,
        (7, 0b0011010) => call_7ififfii,
        (7, 0b0011011) => call_7ffiffii,
        (7, 0b0011100) => call_7iifffii,
        (7, 0b0011101) => call_7fifffii,
        (7, 0b0011110) => call_7iffffii,
        (7, 0b0011111) => call_7fffffii,
        (7, 0b0100000) => call_7iiiiifi,
        (7, 0b0100001) => call_7fiiiifi,
        (7, 0b0100010) => call_7ifiiifi,
        (7, 0b0100011) => call_7ffiiifi,
        (7, 0b0100100) => call_7iifiifi,
        (7, 0b0100101) => call_7fifiifi,
        (7, 0b0100110) => call_7iffiifi,
        (7, 0b0100111) => call_7fffiifi,
        (7, 0b0101000) => call_7iiififi,
        (7, 0b0101001) => call_7fiififi,
        (7, 0b0101010) => call_7ifififi,
        (7, 0b0101011) => call_7ffififi,
        (7, 0b0101100) => call_7iiffifi,
        (7, 0b0101101) => call_7fiffifi,
        (7, 0b0101110) => call_7ifffifi,
        (7, 0b0101111) => call_7ffffifi,
        (7, 0b0110000) => call_7iiiiffi,
        (7, 0b0110001) => call_7fiiiffi,
        (7, 0b0110010) => call_7ifiiffi,
        (7, 0b0110011) => call_7ffiiffi,
        (7, 0b0110100) => call_7iififfi,
        (7, 0b0110101) => call_7fififfi,
        (7, 0b0110110) => call_7iffiffi,
        (7, 0b0110111) => call_7fffiffi,
        (7, 0b0111000) => call_7iiifffi,
        (7, 0b0111001) => call_7fiifffi,
        (7, 0b0111010) => call_7ififffi,
        (7, 0b0111011) => call_7ffifffi,
        (7, 0b0111100) => call_7iiffffi,
        (7, 0b0111101) => call_7fiffffi,
        (7, 0b0111110) => call_7ifffffi,
        (7, 0b0111111) => call_7ffffffi,
        (7, 0b1000000) => call_7iiiiiif,
        (7, 0b1000001) => call_7fiiiiif,
        (7, 0b1000010) => call_7ifiiiif,
        (7, 0b1000011) => call_7ffiiiif,
        (7, 0b1000100) => call_7iifiiif,
        (7, 0b1000101) => call_7fifiiif,
        (7, 0b1000110) => call_7iffiiif,
        (7, 0b1000111) => call_7fffiiif,
        (7, 0b1001000) => call_7iiifiif,
        (7, 0b1001001) => call_7fiifiif,
        (7, 0b1001010) => call_7ififiif,
        (7, 0b1001011) => call_7ffifiif,
        (7, 0b1001100) => call_7iiffiif,
        (7, 0b1001101) => call_7fiffiif,
        (7, 0b1001110) => call_7ifffiif,
        (7, 0b1001111) => call_7ffffiif,
        (7, 0b1010000) => call_7iiiifif,
        (7, 0b1010001) => call_7fiiifif,
        (7, 0b1010010) => call_7ifiifif,
        (7, 0b1010011) => call_7ffiifif,
        (7, 0b1010100) => call_7iififif,
        (7, 0b1010101) => call_7fififif,
        (7, 0b1010110) => call_7iffifif,
        (7, 0b1010111) => call_7fffifif,
        (7, 0b1011000) => call_7iiiffif,
        (7, 0b1011001) => call_7fiiffif,
        (7, 0b1011010) => call_7ififfif,
        (7, 0b1011011) => call_7ffiffif,
        (7, 0b1011100) => call_7iifffif,
        (7, 0b1011101) => call_7fifffif,
        (7, 0b1011110) => call_7iffffif,
        (7, 0b1011111) => call_7fffffif,
        (7, 0b1100000) => call_7iiiiiff,
        (7, 0b1100001) => call_7fiiiiff,
        (7, 0b1100010) => call_7ifiiiff,
        (7, 0b1100011) => call_7ffiiiff,
        (7, 0b1100100) => call_7iifiiff,
        (7, 0b1100101) => call_7fifiiff,
        (7, 0b1100110) => call_7iffiiff,
        (7, 0b1100111) => call_7fffiiff,
        (7, 0b1101000) => call_7iiififf,
        (7, 0b1101001) => call_7fiififf,
        (7, 0b1101010) => call_7ifififf,
        (7, 0b1101011) => call_7ffififf,
        (7, 0b1101100) => call_7iiffiff,
        (7, 0b1101101) => call_7fiffiff,
        (7, 0b1101110) => call_7ifffiff,
        (7, 0b1101111) => call_7ffffiff,
        (7, 0b1110000) => call_7iiiifff,
        (7, 0b1110001) => call_7fiiifff,
        (7, 0b1110010) => call_7ifiifff,
        (7, 0b1110011) => call_7ffiifff,
        (7, 0b1110100) => call_7iififff,
        (7, 0b1110101) => call_7fififff,
        (7, 0b1110110) => call_7iffifff,
        (7, 0b1110111) => call_7fffifff,
        (7, 0b1111000) => call_7iiiffff,
        (7, 0b1111001) => call_7fiiffff,
        (7, 0b1111010) => call_7ififfff,
        (7, 0b1111011) => call_7ffiffff,
        (7, 0b1111100) => call_7iifffff,
        (7, 0b1111101) => call_7fifffff,
        (7, 0b1111110) => call_7iffffff,
        (7, 0b1111111) => call_7fffffff,
        (8, 0b00000000) => call_8iiiiiiii,
        (8, 0b00000001) => call_8fiiiiiii,
        (8, 0b00000010) => call_8ifiiiiii,
        (8, 0b00000011) => call_8ffiiiiii,
        (8, 0b00000100) => call_8iifiiiii,
        (8, 0b00000101) => call_8fifiiiii,
        (8, 0b00000110) => call_8iffiiiii,
        (8, 0b00000111) => call_8fffiiiii,
        (8, 0b00001000) => call_8iiifiiii,
        (8, 0b00001001) => call_8fiifiiii,
        (8, 0b00001010) => call_8ififiiii,
        (8, 0b00001011) => call_8ffifiiii,
        (8, 0b00001100) => call_8iiffiiii,
        (8, 0b00001101) => call_8fiffiiii,
        (8, 0b00001110) => call_8ifffiiii,
        (8, 0b00001111) => call_8ffffiiii,
        (8, 0b00010000) => call_8iiiifiii,
        (8, 0b00010001) => call_8fiiifiii,
        (8, 0b00010010) => call_8ifiifiii,
        (8, 0b00010011) => call_8ffiifiii,
        (8, 0b00010100) => call_8iififiii,
        (8, 0b00010101) => call_8fififiii,
        (8, 0b00010110) => call_8iffifiii,
        (8, 0b00010111) => call_8fffifiii,
        (8, 0b00011000) => call_8iiiffiii,
        (8, 0b00011001) => call_8fiiffiii,
        (8, 0b00011010) => call_8ififfiii,
        (8, 0b00011011) => call_8ffiffiii,
        (8, 0b00011100) => call_8iifffiii,
        (8, 0b00011101) => call_8fifffiii,
        (8, 0b00011110) => call_8iffffiii,
        (8, 0b00011111) => call_8fffffiii,
        (8, 0b00100000) => call_8iiiiifii,
        (8, 0b00100001) => call_8fiiiifii,
        (8, 0b00100010) => call_8ifiiifii,
        (8, 0b00100011) => call_8ffiiifii,
        (8, 0b00100100) => call_8iifiifii,
        (8, 0b00100101) => call_8fifiifii,
        (8, 0b00100110) => call_8iffiifii,
        (8, 0b00100111) => call_8fffiifii,
        (8, 0b00101000) => call_8iiififii,
        (8, 0b00101001) => call_8fiififii,
        (8, 0b00101010) => call_8ifififii,
        (8, 0b00101011) => call_8ffififii,
        (8, 0b00101100) => call_8iiffifii,
        (8, 0b00101101) => call_8fiffifii,
        (8, 0b00101110) => call_8ifffifii,
        (8, 0b00101111) => call_8ffffifii,
        (8, 0b00110000) => call_8iiiiffii,
        (8, 0b00110001) => call_8fiiiffii,
        (8, 0b00110010) => call_8ifiiffii,
        (8, 0b00110011) => call_8ffiiffii,
        (8, 0b00110100) => call_8iififfii,
        (8, 0b00110101) => call_8fififfii,
        (8, 0b00110110) => call_8iffiffii,
        (8, 0b00110111) => call_8fffiffii,
        (8, 0b00111000) => call_8iiifffii,
        (8, 0b00111001) => call_8fiifffii,
        (8, 0b00111010) => call_8ififffii,
        (8, 0b00111011) => call_8ffifffii,
        (8, 0b00111100) => call_8iiffffii,
        (8, 0b00111101) => call_8fiffffii,
        (8, 0b00111110) => call_8ifffffii,
        (8, 0b00111111) => call_8ffffffii,
        (8, 0b01000000) => call_8iiiiiifi,
        (8, 0b01000001) => call_8fiiiiifi,
        (8, 0b01000010) => call_8ifiiiifi,
        (8, 0b01000011) => call_8ffiiiifi,
        (8, 0b01000100) => call_8iifiiifi,
        (8, 0b01000101) => call_8fifiiifi,
        (8, 0b01000110) => call_8iffiiifi,
        (8, 0b01000111) => call_8fffiiifi,
        (8, 0b01001000) => call_8iiifiifi,
        (8, 0b01001001) => call_8fiifiifi,
        (8, 0b01001010) => call_8ififiifi,
        (8, 0b01001011) => call_8ffifiifi,
        (8, 0b01001100) => call_8iiffiifi,
        (8, 0b01001101) => call_8fiffiifi,
        (8, 0b01001110) => call_8ifffiifi,
        (8, 0b01001111) => call_8ffffiifi,
        (8, 0b01010000) => call_8iiiififi,
        (8, 0b01010001) => call_8fiiififi,
        (8, 0b01010010) => call_8ifiififi,
        (8, 0b01010011) => call_8ffiififi,
        (8, 0b01010100) => call_8iifififi,
        (8, 0b01010101) => call_8fifififi,
        (8, 0b01010110) => call_8iffififi,
        (8, 0b01010111) => call_8fffififi,
        (8, 0b01011000) => call_8iiiffifi,
        (8, 0b01011001) => call_8fiiffifi,
        (8, 0b01011010) => call_8ififfifi,
        (8, 0b01011011) => call_8ffiffifi,
        (8, 0b01011100) => call_8iifffifi,
        (8, 0b01011101) => call_8fifffifi,
        (8, 0b01011110) => call_8iffffifi,
        (8, 0b01011111) => call_8fffffifi,
        (8, 0b01100000) => call_8iiiiiffi,
        (8, 0b01100001) => call_8fiiiiffi,
        (8, 0b01100010) => call_8ifiiiffi,
        (8, 0b01100011) => call_8ffiiiffi,
        (8, 0b01100100) => call_8iifiiffi,
        (8, 0b01100101) => call_8fifiiffi,
        (8, 0b01100110) => call_8iffiiffi,
        (8, 0b01100111) => call_8fffiiffi,
        (8, 0b01101000) => call_8iiififfi,
        (8, 0b01101001) => call_8fiififfi,
        (8, 0b01101010) => call_8ifififfi,
        (8, 0b01101011) => call_8ffififfi,
        (8, 0b01101100) => call_8iiffiffi,
        (8, 0b01101101) => call_8fiffiffi,
        (8, 0b01101110) => call_8ifffiffi,
        (8, 0b01101111) => call_8ffffiffi,
        (8, 0b01110000) => call_8iiiifffi,
        (8, 0b01110001) => call_8fiiifffi,
        (8, 0b01110010) => call_8ifiifffi,
        (8, 0b01110011) => call_8ffiifffi,
        (8, 0b01110100) => call_8iififffi,
        (8, 0b01110101) => call_8fififffi,
        (8, 0b01110110) => call_8iffifffi,
        (8, 0b01110111) => call_8fffifffi,
        (8, 0b01111000) => call_8iiiffffi,
        (8, 0b01111001) => call_8fiiffffi,
        (8, 0b01111010) => call_8ififfffi,
        (8, 0b01111011) => call_8ffiffffi,
        (8, 0b01111100) => call_8iifffffi,
        (8, 0b01111101) => call_8fifffffi,
        (8, 0b01111110) => call_8iffffffi,
        (8, 0b01111111) => call_8fffffffi,
        (8, 0b10000000) => call_8iiiiiiif,
        (8, 0b10000001) => call_8fiiiiiif,
        (8, 0b10000010) => call_8ifiiiiif,
        (8, 0b10000011) => call_8ffiiiiif,
        (8, 0b10000100) => call_8iifiiiif,
        (8, 0b10000101) => call_8fifiiiif,
        (8, 0b10000110) => call_8iffiiiif,
        (8, 0b10000111) => call_8fffiiiif,
        (8, 0b10001000) => call_8iiifiiif,
        (8, 0b10001001) => call_8fiifiiif,
        (8, 0b10001010) => call_8ififiiif,
        (8, 0b10001011) => call_8ffifiiif,
        (8, 0b10001100) => call_8iiffiiif,
        (8, 0b10001101) => call_8fiffiiif,
        (8, 0b10001110) => call_8ifffiiif,
        (8, 0b10001111) => call_8ffffiiif,
        (8, 0b10010000) => call_8iiiifiif,
        (8, 0b10010001) => call_8fiiifiif,
        (8, 0b10010010) => call_8ifiifiif,
        (8, 0b10010011) => call_8ffiifiif,
        (8, 0b10010100) => call_8iififiif,
        (8, 0b10010101) => call_8fififiif,
        (8, 0b10010110) => call_8iffifiif,
        (8, 0b10010111) => call_8fffifiif,
        (8, 0b10011000) => call_8iiiffiif,
        (8, 0b10011001) => call_8fiiffiif,
        (8, 0b10011010) => call_8ififfiif,
        (8, 0b10011011) => call_8ffiffiif,
        (8, 0b10011100) => call_8iifffiif,
        (8, 0b10011101) => call_8fifffiif,
        (8, 0b10011110) => call_8iffffiif,
        (8, 0b10011111) => call_8fffffiif,
        (8, 0b10100000) => call_8iiiiifif,
        (8, 0b10100001) => call_8fiiiifif,
        (8, 0b10100010) => call_8ifiiifif,
        (8, 0b10100011) => call_8ffiiifif,
        (8, 0b10100100) => call_8iifiifif,
        (8, 0b10100101) => call_8fifiifif,
        (8, 0b10100110) => call_8iffiifif,
        (8, 0b10100111) => call_8fffiifif,
        (8, 0b10101000) => call_8iiififif,
        (8, 0b10101001) => call_8fiififif,
        (8, 0b10101010) => call_8ifififif,
        (8, 0b10101011) => call_8ffififif,
        (8, 0b10101100) => call_8iiffifif,
        (8, 0b10101101) => call_8fiffifif,
        (8, 0b10101110) => call_8ifffifif,
        (8, 0b10101111) => call_8ffffifif,
        (8, 0b10110000) => call_8iiiiffif,
        (8, 0b10110001) => call_8fiiiffif,
        (8, 0b10110010) => call_8ifiiffif,
        (8, 0b10110011) => call_8ffiiffif,
        (8, 0b10110100) => call_8iififfif,
        (8, 0b10110101) => call_8fififfif,
        (8, 0b10110110) => call_8iffiffif,
        (8, 0b10110111) => call_8fffiffif,
        (8, 0b10111000) => call_8iiifffif,
        (8, 0b10111001) => call_8fiifffif,
        (8, 0b10111010) => call_8ififffif,
        (8, 0b10111011) => call_8ffifffif,
        (8, 0b10111100) => call_8iiffffif,
        (8, 0b10111101) => call_8fiffffif,
        (8, 0b10111110) => call_8ifffffif,
        (8, 0b10111111) => call_8ffffffif,
        (8, 0b11000000) => call_8iiiiiiff,
        (8, 0b11000001) => call_8fiiiiiff,
        (8, 0b11000010) => call_8ifiiiiff,
        (8, 0b11000011) => call_8ffiiiiff,
        (8, 0b11000100) => call_8iifiiiff,
        (8, 0b11000101) => call_8fifiiiff,
        (8, 0b11000110) => call_8iffiiiff,
        (8, 0b11000111) => call_8fffiiiff,
        (8, 0b11001000) => call_8iiifiiff,
        (8, 0b11001001) => call_8fiifiiff,
        (8, 0b11001010) => call_8ififiiff,
        (8, 0b11001011) => call_8ffifiiff,
        (8, 0b11001100) => call_8iiffiiff,
        (8, 0b11001101) => call_8fiffiiff,
        (8, 0b11001110) => call_8ifffiiff,
        (8, 0b11001111) => call_8ffffiiff,
        (8, 0b11010000) => call_8iiiififf,
        (8, 0b11010001) => call_8fiiififf,
        (8, 0b11010010) => call_8ifiififf,
        (8, 0b11010011) => call_8ffiififf,
        (8, 0b11010100) => call_8iifififf,
        (8, 0b11010101) => call_8fifififf,
        (8, 0b11010110) => call_8iffififf,
        (8, 0b11010111) => call_8fffififf,
        (8, 0b11011000) => call_8iiiffiff,
        (8, 0b11011001) => call_8fiiffiff,
        (8, 0b11011010) => call_8ififfiff,
        (8, 0b11011011) => call_8ffiffiff,
        (8, 0b11011100) => call_8iifffiff,
        (8, 0b11011101) => call_8fifffiff,
        (8, 0b11011110) => call_8iffffiff,
        (8, 0b11011111) => call_8fffffiff,
        (8, 0b11100000) => call_8iiiiifff,
        (8, 0b11100001) => call_8fiiiifff,
        (8, 0b11100010) => call_8ifiiifff,
        (8, 0b11100011) => call_8ffiiifff,
        (8, 0b11100100) => call_8iifiifff,
        (8, 0b11100101) => call_8fifiifff,
        (8, 0b11100110) => call_8iffiifff,
        (8, 0b11100111) => call_8fffiifff,
        (8, 0b11101000) => call_8iiififff,
        (8, 0b11101001) => call_8fiififff,
        (8, 0b11101010) => call_8ifififff,
        (8, 0b11101011) => call_8ffififff,
        (8, 0b11101100) => call_8iiffifff,
        (8, 0b11101101) => call_8fiffifff,
        (8, 0b11101110) => call_8ifffifff,
        (8, 0b11101111) => call_8ffffifff,
        (8, 0b11110000) => call_8iiiiffff,
        (8, 0b11110001) => call_8fiiiffff,
        (8, 0b11110010) => call_8ifiiffff,
        (8, 0b11110011) => call_8ffiiffff,
        (8, 0b11110100) => call_8iififfff,
        (8, 0b11110101) => call_8fififfff,
        (8, 0b11110110) => call_8iffiffff,
        (8, 0b11110111) => call_8fffiffff,
        (8, 0b11111000) => call_8iiifffff,
        (8, 0b11111001) => call_8fiifffff,
        (8, 0b11111010) => call_8ififffff,
        (8, 0b11111011) => call_8ffifffff,
        (8, 0b11111100) => call_8iiffffff,
        (8, 0b11111101) => call_8fiffffff,
        (8, 0b11111110) => call_8ifffffff,
        (8, 0b11111111) => call_8ffffffff,
        (9, 0b000000000) => call_9i,
        (9, 0b111111111) => call_9f,
        (10, 0b0000000000) => call_10i,
        (10, 0b1111111111) => call_10f,
        (11, 0b00000000000) => call_11i,
        (11, 0b11111111111) => call_11f,
        (12, 0b000000000000) => call_12i,
        (12, 0b111111111111) => call_12f,
        _ => return None,
    };
    Some(f)
}

/// Pre-resolved dispatch data for one [`JitFn`], computed once at
/// override-resolution time so the hot path skips the per-call bitmask
/// and the `(arity, shape)` match.
pub(crate) struct Prepared {
    /// The native entry plus its slot kinds.
    pub(crate) jit: std::sync::Arc<JitFn>,
    /// The stub resolved for this body's `(arity, shape)`.
    stub: StubFn,
    /// Return kind with `EnumPtr` and native aggregates canonicalised to
    /// `I64` (the stub reads a raw integer register; the post-call step
    /// re-wraps it).
    ret_kind: JitKind,
    /// `Some(shape_idx)` when the real return was `EnumPtr`.
    enum_return: Option<u32>,
    /// `Some(kind)` when the real return was a native aggregate
    /// (`NativeStr` / `NativeVecI64`); the raw pointer is read back and
    /// freed after the call.
    native_return: Option<JitKind>,
    /// `true` when any param or the return is a native aggregate, routing
    /// dispatch through `invoke_prepared_native`. The scalar/enum fast
    /// path (no native marshalling, no per-call allocation) stays
    /// untouched when this is `false`.
    has_native: bool,
    /// Set after the first full-shape marshal succeeds. Subsequent
    /// calls trust the proven scalar shapes (the type checker fixed the
    /// call site) and skip the per-kind `match`, still re-checking
    /// `EnumPtr` shape indices and falling back on any surprise.
    verified: std::cell::Cell<bool>,
}

/// Resolves dispatch data for `jit`, or `None` if the trampoline can't
/// cover its shape (the caller then keeps the body on bytecode).
pub(crate) fn prepare(jit: std::sync::Arc<JitFn>) -> Option<Prepared> {
    if jit.params.len() > MAX_ARGS {
        return None;
    }
    let mut shape: u32 = 0;
    for (i, k) in jit.params.iter().enumerate() {
        if matches!(k, JitKind::F64) {
            shape |= 1 << i;
        }
    }
    let stub = resolve_stub(jit.params.len(), shape)?;
    let (ret_kind, enum_return, native_return) = match jit.returns {
        JitKind::EnumPtr(idx) => (JitKind::I64, Some(idx), None),
        k @ (JitKind::NativeStr | JitKind::NativeVecI64) => (JitKind::I64, None, Some(k)),
        other => (other, None, None),
    };
    let has_native = native_return.is_some()
        || jit
            .params
            .iter()
            .any(|k| matches!(k, JitKind::NativeStr | JitKind::NativeVecI64));
    Some(Prepared {
        jit,
        stub,
        ret_kind,
        enum_return,
        native_return,
        has_native,
        verified: std::cell::Cell::new(false),
    })
}

/// Hot-path dispatch through a [`Prepared`]. Marshals args, calls the
/// cached stub under `catch_unwind`, then re-wraps an `EnumPtr` return.
pub(crate) fn invoke_prepared(p: &Prepared, args: &[Value]) -> Dispatch {
    let jit = &p.jit;
    if jit.params.len() != args.len() {
        return Dispatch::Fallback;
    }
    // Bodies with a native aggregate param or return marshal through a
    // separate path that owns the runtime objects it builds; the scalar
    // fast path below stays allocation-free.
    if p.has_native {
        return invoke_prepared_native(p, args);
    }
    let mut slots: [Slot; MAX_ARGS] = [Slot::I(0); MAX_ARGS];
    // Fast marshal once shapes are proven for this resolved entry.
    // `EnumPtr` is always re-checked (the shape index guards a real
    // mismatch); other scalar kinds trust the prior verification, with
    // a debug assertion catching any divergence in debug builds.
    let fast = p.verified.get();
    for (i, (kind, value)) in jit.params.iter().zip(args.iter()).enumerate() {
        let slot = match (kind, fast) {
            (JitKind::EnumPtr(idx), _) => match value {
                Value::NativeEnum(h) if h.shape.index == *idx => Slot::I(h.ptr as i64),
                _ => return Dispatch::Fallback,
            },
            (JitKind::Value, _) => Slot::I(value.to_raw() as i64),
            (JitKind::F64, true) => {
                debug_assert!(matches!(value, Value::Float(_)));
                match value {
                    Value::Float(x) => Slot::F(*x),
                    _ => return Dispatch::Fallback,
                }
            }
            // Native aggregate kinds are dispatched by `invoke_prepared_native`
            // above; the `has_native` guard makes this unreachable here.
            (JitKind::NativeStr | JitKind::NativeVecI64, _) => return Dispatch::Fallback,
            (_, true) => match value {
                Value::Int(n) => Slot::I(*n),
                Value::Bool(b) => Slot::I(i64::from(*b)),
                Value::Unit => Slot::I(0),
                _ => return Dispatch::Fallback,
            },
            // Slow (unverified) path: full per-kind check.
            (JitKind::I64, false) => match value {
                Value::Int(n) => Slot::I(*n),
                _ => return Dispatch::Fallback,
            },
            (JitKind::F64, false) => match value {
                Value::Float(x) => Slot::F(*x),
                _ => return Dispatch::Fallback,
            },
            (JitKind::Bool, false) => match value {
                Value::Bool(b) => Slot::I(i64::from(*b)),
                _ => return Dispatch::Fallback,
            },
            (JitKind::Unit, false) => match value {
                Value::Unit => Slot::I(0),
                _ => return Dispatch::Fallback,
            },
        };
        slots[i] = slot;
    }
    p.verified.set(true);
    let n = jit.params.len();
    // SAFETY: `prepare` resolved `stub` for exactly this body's
    // `(arity, shape, ret)` triple, so the reified `extern "C"`
    // signature matches the cranelift-emitted entry. `catch_unwind`
    // demotes a panic unwound through the boundary to a `Fallback`.
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
        (p.stub)(jit.ptr, &slots[..n], p.ret_kind)
    }));
    match outcome {
        Ok(Some(value)) => {
            if let Some(shape_idx) = p.enum_return {
                let Value::Int(raw) = value else {
                    return Dispatch::Fallback;
                };
                let Some(shape) = crate::value::native_shape(shape_idx) else {
                    return Dispatch::Fallback;
                };
                return Dispatch::Ok(Value::NativeEnum(std::sync::Arc::new(
                    crate::value::NativeEnumOwner {
                        ptr: raw as usize,
                        shape,
                    },
                )));
            }
            Dispatch::Ok(value)
        }
        Ok(None) => Dispatch::Fallback,
        Err(_) => {
            eprintln!("jit: panic inside JIT-compiled body; falling back to bytecode");
            Dispatch::Fallback
        }
    }
}

/// Dispatch for a body with a native aggregate (`String` / `Vec<i64>`)
/// param or return. Builds a fresh runtime object for each aggregate
/// param from the VM value, calls the stub, reads any aggregate return
/// back into a VM value, writes mutated `&mut` params back, then frees
/// every object it built. All native objects are trampoline-owned and
/// reclaimed through the runtime's RC reclaim entries, so the VM values
/// the caller passed are never aliased or freed by this path.
fn invoke_prepared_native(p: &Prepared, args: &[Value]) -> Dispatch {
    let jit = &p.jit;
    let mut slots: [Slot; MAX_ARGS] = [Slot::I(0); MAX_ARGS];
    let mut natives: Vec<NativeArg> = Vec::new();
    for (i, (kind, value)) in jit.params.iter().zip(args.iter()).enumerate() {
        let slot = match kind {
            JitKind::NativeStr | JitKind::NativeVecI64 => {
                // Unwrap a `&mut` write-back cell so we marshal its inner
                // aggregate; record the cell so mutations flow back.
                let (inner, cell) = match value {
                    Value::MutCell(c) => (c.lock().clone(), Some(c.clone())),
                    other => (other.clone(), None),
                };
                let Some(ptr) = build_native_arg(*kind, &inner) else {
                    free_natives(&natives, None);
                    return Dispatch::Fallback;
                };
                natives.push((*kind, ptr, cell));
                Slot::I(ptr)
            }
            JitKind::EnumPtr(idx) => match value {
                Value::NativeEnum(h) if h.shape.index == *idx => Slot::I(h.ptr as i64),
                _ => {
                    free_natives(&natives, None);
                    return Dispatch::Fallback;
                }
            },
            JitKind::Value => Slot::I(value.to_raw() as i64),
            JitKind::F64 => {
                if let Value::Float(x) = value {
                    Slot::F(*x)
                } else {
                    free_natives(&natives, None);
                    return Dispatch::Fallback;
                }
            }
            JitKind::I64 | JitKind::Bool | JitKind::Unit => match value {
                Value::Int(n) => Slot::I(*n),
                Value::Bool(b) => Slot::I(i64::from(*b)),
                Value::Unit => Slot::I(0),
                _ => {
                    free_natives(&natives, None);
                    return Dispatch::Fallback;
                }
            },
        };
        slots[i] = slot;
    }
    let n = jit.params.len();
    // SAFETY: `prepare` resolved `stub` for this body's `(arity, shape,
    // ret)` triple; native aggregate slots cross as pointer-sized i64
    // values matching the flat-ABI signature. `catch_unwind` demotes a
    // boundary panic to a `Fallback`.
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
        (p.stub)(jit.ptr, &slots[..n], p.ret_kind)
    }));
    let raw = match outcome {
        Ok(Some(v)) => v,
        Ok(None) => {
            free_natives(&natives, None);
            return Dispatch::Fallback;
        }
        Err(_) => {
            eprintln!("jit: panic inside JIT-compiled body; falling back to bytecode");
            free_natives(&natives, None);
            return Dispatch::Fallback;
        }
    };
    // Re-wrap the return, write back `&mut` params, then free every
    // native object exactly once (the return is deduped against params).
    if let Some(nret) = p.native_return {
        let Value::Int(ret_ptr) = raw else {
            free_natives(&natives, None);
            return Dispatch::Fallback;
        };
        let result = native_ptr_to_value(nret, ret_ptr);
        writeback_natives(&natives);
        free_natives(&natives, Some((nret, ret_ptr)));
        Dispatch::Ok(result)
    } else if let Some(shape_idx) = p.enum_return {
        let Value::Int(ret_ptr) = raw else {
            free_natives(&natives, None);
            return Dispatch::Fallback;
        };
        let Some(shape) = crate::value::native_shape(shape_idx) else {
            free_natives(&natives, None);
            return Dispatch::Fallback;
        };
        writeback_natives(&natives);
        free_natives(&natives, None);
        Dispatch::Ok(Value::NativeEnum(Arc::new(crate::value::NativeEnumOwner {
            ptr: ret_ptr as usize,
            shape,
        })))
    } else {
        writeback_natives(&natives);
        free_natives(&natives, None);
        Dispatch::Ok(raw)
    }
}

use std::sync::atomic::{AtomicBool, Ordering};

/// CLI override for the JIT default. `Vm::load` consults this
/// flag so `gos run --no-jit` can disable the JIT without mutating
/// the process environment. The JIT is on by default per Tier D
/// of the interp wow plan; this flag (or `GOS_JIT=0`) is the only
/// way to turn it back off.
static JIT_DISABLED: AtomicBool = AtomicBool::new(false);

/// CLI hook used by `gos run --no-jit` to suppress every JIT
/// compile attempt regardless of `GOS_JIT`. Pair with
/// [`force_jit_enable`] to scope the disable to a defined region in
/// long-lived processes (REPL, test runners, etc.) - see the
/// `set_stdout_writer` companion in `builtins.rs` for the canonical
/// scoped-disable shape.
pub fn force_jit_disabled() {
    JIT_DISABLED.store(true, Ordering::Relaxed);
}

/// Reverses a prior [`force_jit_disabled`] call. Long-lived processes
/// (REPLs, test harnesses that swap stdout writers between cases)
/// previously lost the JIT permanently once any caller flipped the
/// flag; this companion lets them restore it.
///
/// `gos run --no-jit` does not call this - the flag stays set for the
/// process lifetime in CLI mode. Test code that installs a custom
/// stdout writer should bracket the override with
/// `force_jit_disabled` / `force_jit_enable` if it wants to recover
/// the JIT path after the test exits.
pub fn force_jit_enable() {
    JIT_DISABLED.store(false, Ordering::Relaxed);
}

/// Returns `true` when [`force_jit_disabled`] has been called and not
/// yet reversed by [`force_jit_enable`]. Lets callers inspect the
/// flag without flipping it.
#[must_use]
pub fn jit_force_disabled_state() -> bool {
    JIT_DISABLED.load(Ordering::Relaxed)
}

/// Returns `true` when JIT compilation is permitted in this
/// process. Default is `true` (Tier D promoted JIT to the steady-
/// state execution path); the only ways to suppress it are
/// `gos run --no-jit` (which calls [`force_jit_disabled`]) or
/// setting `GOS_JIT=0` / `GOS_JIT=false` in the environment.
/// This is intentionally not memoised so tests can flip the env
/// between runs.
pub(crate) fn jit_enabled() -> bool {
    if JIT_DISABLED.load(Ordering::Relaxed) {
        return false;
    }
    !matches!(
        std::env::var("GOS_JIT").ok().as_deref(),
        Some("0" | "false")
    )
}

/// Returns `true` when `GOS_JIT_TRACE` is set, in which case the VM
/// emits per-function compile / dispatch diagnostics on stderr.
pub(crate) fn jit_trace() -> bool {
    matches!(std::env::var("GOS_JIT_TRACE").ok().as_deref(), Some(s) if !s.is_empty() && s != "0")
}
