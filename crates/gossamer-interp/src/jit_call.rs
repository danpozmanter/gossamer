//! Trampoline that dispatches into a JIT-compiled body.
//!
//! Every call into native code goes through [`invoke`]: it inspects
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
//!   chosen `extern "C" fn` shape — that match is guaranteed by
//!   `JitArtifact::compile_to_jit`'s own type classification, which
//!   only registers a [`JitFn`] when `JitKind` for every slot lines
//!   up with the MIR-derived cranelift type.
//! - The owning `JitArtifact` is still alive: the VM holds it in
//!   `Vm::_jit` for the entire lifetime of the [`Global::Jit`]
//!   entries that hand `JitFn`s to this module.
//! - The Gossamer language is single-threaded at the VM layer; the
//!   trampoline is therefore not re-entered from a foreign thread
//!   while a `JITed` body is running.
//!
//! Shapes the trampoline does not cover (e.g. heterogeneous mixes of
//! `i64`/`f64` beyond the listed patterns) return [`Dispatch::Fallback`]
//! so the caller can retry through the bytecode interpreter.

#![allow(unsafe_code)]
#![allow(clippy::too_many_lines)]

use std::mem;

use gossamer_codegen_cranelift::{JitFn, JitKind};

use crate::value::{RuntimeError, Value};

/// Result of attempting to dispatch through the JIT trampoline.
pub(crate) enum Dispatch {
    /// The JIT body ran and produced a value.
    Ok(Value),
    /// The JIT body cannot be invoked with these args (shape
    /// unsupported, or a runtime arg's type didn't match the JIT
    /// signature). The caller falls back to the bytecode chunk.
    Fallback,
    /// The JIT body called the runtime in a way that surfaced an
    /// error the VM should propagate as a `RuntimeError`. Reserved
    /// for runtime-side panics; the trampoline doesn't construct
    /// this variant today but the call site is structured to honour
    /// it as soon as the JIT learns to surface `gos_rt_panic` as a
    /// recoverable Rust error rather than a process abort.
    #[allow(dead_code)]
    Err(RuntimeError),
}

/// Calls into a JIT-compiled body. Returns `Dispatch::Fallback` if
/// the trampoline doesn't cover the body's shape or an arg's
/// concrete type doesn't match the slot kind.
pub(crate) fn invoke(jit: &JitFn, args: &[Value]) -> Dispatch {
    if jit.params.len() != args.len() {
        return Dispatch::Fallback;
    }

    // Slot-by-slot marshal. Any `Value` whose concrete shape doesn't
    // match the expected `JitKind` aborts to bytecode — the VM keeps
    // type-erased `Value`s and the JIT only handles primitive
    // scalars today.
    let mut slots: [Slot; MAX_ARGS] = [Slot::I(0); MAX_ARGS];
    if jit.params.len() > MAX_ARGS {
        return Dispatch::Fallback;
    }
    for (i, (kind, value)) in jit.params.iter().zip(args.iter()).enumerate() {
        let slot = match kind {
            JitKind::I64 => match value {
                Value::Int(n) => Slot::I(*n),
                _ => return Dispatch::Fallback,
            },
            JitKind::F64 => match value {
                Value::Float(x) => Slot::F(*x),
                _ => return Dispatch::Fallback,
            },
            JitKind::Bool => match value {
                Value::Bool(b) => Slot::I(i64::from(*b)),
                _ => return Dispatch::Fallback,
            },
            JitKind::Unit => match value {
                Value::Unit => Slot::I(0),
                _ => return Dispatch::Fallback,
            },
            // Aggregate slot: encode through the runtime's u64 packed
            // shape and stuff into an integer-class slot. Cranelift
            // passes Value-typed args via integer registers, so the
            // i64 ABI (`Slot::I`) is the right home for the bit
            // pattern. `Value::to_raw` allocates a heap-handle in the
            // shared registry for aggregate variants — the JIT body
            // sees the same handle that bytecode-VM aggregates use.
            JitKind::Value => Slot::I(value.to_raw() as i64),
        };
        slots[i] = slot;
    }

    // SAFETY: the `JitFn` was registered by `compile_to_jit` only
    // after its parameter and return types were classified into
    // `JitKind`s, so each fn-pointer cast below pairs with a slot
    // shape we know cranelift produced.
    let outcome = unsafe { dispatch(jit.ptr, &jit.params, &slots[..jit.params.len()], jit.returns) };
    match outcome {
        Some(value) => Dispatch::Ok(value),
        None => Dispatch::Fallback,
    }
}

const MAX_ARGS: usize = 8;

#[derive(Clone, Copy)]
enum Slot {
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
        }
    }};
}

/// Maps the per-slot shape token (`i` / `f`) to the matching
/// `slot_*` accessor.
macro_rules! slot_for {
    (i, $s:expr, $idx:expr) => { slot_i($s[$idx]) };
    (f, $s:expr, $idx:expr) => { slot_f($s[$idx]) };
}

/// Maps the per-slot shape token (`i` / `f`) to the corresponding
/// Rust ABI type. Used to spell out the `extern "C" fn(...)`
/// signature inside `call_through!`.
macro_rules! ty_for {
    (i) => { i64 };
    (f) => { f64 };
}

/// Generates a `call_<arity><shape>` function for one (arity, shape)
/// combination. Distinct binding names per slot (`a0`, `a1`, …) are
/// required so each `let` introduces a fresh local instead of
/// shadowing the previous one — `call_through!` then sees every
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
}

unsafe fn dispatch(
    ptr: *const u8,
    kinds: &[JitKind],
    slots: &[Slot],
    ret: JitKind,
) -> Option<Value> {
    // Build a compact "shape" descriptor: bit `i` is `1` iff
    // param `i` is a float-class slot (F64). Every other kind
    // (`I64` / `Bool` / `Unit` / `Value`) uses the integer
    // register ABI and marshals through `slot_i`. This collapses
    // every (per-param-kind, per-return-kind) combination into a
    // single (arity, shape, ret) match key.
    let mut shape: u32 = 0;
    for (i, k) in kinds.iter().enumerate() {
        if matches!(k, JitKind::F64) {
            shape |= 1 << i;
        }
    }
    let arity = kinds.len();
    match (arity, shape) {
        // Arity 0 — return-only.
        (0, _) => match ret {
            JitKind::I64 => {
                let f: extern "C" fn() -> i64 = unsafe { mem::transmute(ptr) };
                Some(Value::Int(f()))
            }
            JitKind::F64 => {
                let f: extern "C" fn() -> f64 = unsafe { mem::transmute(ptr) };
                Some(Value::Float(f()))
            }
            JitKind::Bool => {
                let f: extern "C" fn() -> i8 = unsafe { mem::transmute(ptr) };
                Some(Value::Bool(f() != 0))
            }
            JitKind::Unit => {
                let f: extern "C" fn() = unsafe { mem::transmute(ptr) };
                f();
                Some(Value::Unit)
            }
            JitKind::Value => {
                let f: extern "C" fn() -> i64 = unsafe { mem::transmute(ptr) };
                let raw = f() as u64;
                Some(Value::from_raw(raw))
            }
        },
        (1, 0b0) => unsafe { call_1i(ptr, slots, ret) },
        (1, 0b1) => unsafe { call_1f(ptr, slots, ret) },
        (2, 0b00) => unsafe { call_2ii(ptr, slots, ret) },
        (2, 0b01) => unsafe { call_2fi(ptr, slots, ret) },
        (2, 0b10) => unsafe { call_2if(ptr, slots, ret) },
        (2, 0b11) => unsafe { call_2ff(ptr, slots, ret) },
        // All eight arity-3 shapes.
        (3, 0b000) => unsafe { call_3iii(ptr, slots, ret) },
        (3, 0b001) => unsafe { call_3fii(ptr, slots, ret) },
        (3, 0b010) => unsafe { call_3ifi(ptr, slots, ret) },
        (3, 0b011) => unsafe { call_3ffi(ptr, slots, ret) },
        (3, 0b100) => unsafe { call_3iif(ptr, slots, ret) },
        (3, 0b101) => unsafe { call_3fif(ptr, slots, ret) },
        (3, 0b110) => unsafe { call_3iff(ptr, slots, ret) },
        (3, 0b111) => unsafe { call_3fff(ptr, slots, ret) },
        // All sixteen arity-4 shapes.
        (4, 0b0000) => unsafe { call_4iiii(ptr, slots, ret) },
        (4, 0b0001) => unsafe { call_4fiii(ptr, slots, ret) },
        (4, 0b0010) => unsafe { call_4ifii(ptr, slots, ret) },
        (4, 0b0011) => unsafe { call_4ffii(ptr, slots, ret) },
        (4, 0b0100) => unsafe { call_4iifi(ptr, slots, ret) },
        (4, 0b0101) => unsafe { call_4fifi(ptr, slots, ret) },
        (4, 0b0110) => unsafe { call_4iffi(ptr, slots, ret) },
        (4, 0b0111) => unsafe { call_4fffi(ptr, slots, ret) },
        (4, 0b1000) => unsafe { call_4iiif(ptr, slots, ret) },
        (4, 0b1001) => unsafe { call_4fiif(ptr, slots, ret) },
        (4, 0b1010) => unsafe { call_4ifif(ptr, slots, ret) },
        (4, 0b1011) => unsafe { call_4ffif(ptr, slots, ret) },
        (4, 0b1100) => unsafe { call_4iiff(ptr, slots, ret) },
        (4, 0b1101) => unsafe { call_4fiff(ptr, slots, ret) },
        (4, 0b1110) => unsafe { call_4ifff(ptr, slots, ret) },
        (4, 0b1111) => unsafe { call_4ffff(ptr, slots, ret) },
        // Arity 5-8: all-int and all-float only. Heterogeneous
        // mixes at these arities are rare in practice (the type
        // checker proves call sites first; mixed-scalar arities
        // ≥ 5 mostly come from monomorphised generics that
        // already lower to homogeneous shapes). Anything else
        // returns `None` and the caller falls back to bytecode.
        (5, 0b00000) => unsafe { call_5iiiii(ptr, slots, ret) },
        (5, 0b11111) => unsafe { call_5fffff(ptr, slots, ret) },
        (6, 0b000000) => unsafe { call_6iiiiii(ptr, slots, ret) },
        (6, 0b111111) => unsafe { call_6ffffff(ptr, slots, ret) },
        (7, 0b0000000) => unsafe { call_7iiiiiii(ptr, slots, ret) },
        (7, 0b1111111) => unsafe { call_7fffffff(ptr, slots, ret) },
        (8, 0b00000000) => unsafe { call_8iiiiiiii(ptr, slots, ret) },
        (8, 0b11111111) => unsafe { call_8ffffffff(ptr, slots, ret) },
        _ => None,
    }
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
// Arity 5-8: homogeneous only.
gen_call!(call_5iiiii, i, i, i, i, i);
gen_call!(call_5fffff, f, f, f, f, f);
gen_call!(call_6iiiiii, i, i, i, i, i, i);
gen_call!(call_6ffffff, f, f, f, f, f, f);
gen_call!(call_7iiiiiii, i, i, i, i, i, i, i);
gen_call!(call_7fffffff, f, f, f, f, f, f, f);
gen_call!(call_8iiiiiiii, i, i, i, i, i, i, i, i);
gen_call!(call_8ffffffff, f, f, f, f, f, f, f, f);

use std::sync::atomic::{AtomicBool, Ordering};

/// CLI override for the JIT default. `Vm::load` consults this
/// flag so `gos run --no-jit` can disable the JIT without mutating
/// the process environment. The JIT is on by default per Tier D
/// of the interp wow plan; this flag (or `GOS_JIT=0`) is the only
/// way to turn it back off.
static JIT_DISABLED: AtomicBool = AtomicBool::new(false);

/// CLI hook used by `gos run --no-jit` to suppress every JIT
/// compile attempt regardless of `GOS_JIT`. The flag is process-
/// wide and set once at startup; flipping it back on requires a
/// fresh process.
pub fn force_jit_disabled() {
    JIT_DISABLED.store(true, Ordering::Relaxed);
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
    !matches!(std::env::var("GOS_JIT").ok().as_deref(), Some("0" | "false"))
}

/// Returns `true` when `GOS_JIT_TRACE` is set, in which case the VM
/// emits per-function compile / dispatch diagnostics on stderr.
pub(crate) fn jit_trace() -> bool {
    matches!(std::env::var("GOS_JIT_TRACE").ok().as_deref(), Some(s) if !s.is_empty() && s != "0")
}

