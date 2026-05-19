#![allow(clippy::missing_safety_doc)]
#![allow(missing_docs)]
#![allow(clippy::too_many_lines)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]
#![allow(clippy::must_use_candidate)]
#![allow(clippy::similar_names)]
#![allow(clippy::many_single_char_names)]
#![allow(clippy::items_after_statements)]
#![allow(clippy::same_length_and_capacity)]
#![allow(clippy::cast_lossless)]
#![allow(clippy::doc_markdown)]
#![allow(clippy::cast_ptr_alignment)]
#![allow(clippy::ptr_as_ptr)]
#![allow(static_mut_refs)]
#![allow(unused_unsafe)]
#![allow(clippy::wildcard_imports)]

// ---------------------------------------------------------------
// LCG jump-ahead helper
// ---------------------------------------------------------------
//
// fasta-style benchmarks use a Lehmer / Park-Miller LCG of the
// form `state' = (state * IA + IC) mod IM`. Multi-threaded
// fasta needs each worker to start at a different point in the
// stream so the streams interleave correctly. This helper
// computes `LCG^n(state)` in O(log n) time using fast modular
// exponentiation.

/// Compute `LCG^n(state)` where the LCG is
/// `s' = (s * ia + ic) mod im`. Returns the state after `n`
/// applications. `n` is clamped to non-negative.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_lcg_jump(state: i64, ia: i64, ic: i64, im: i64, n: i64) -> i64 {
    ffi_entry!(-1, {
        if n <= 0 || im <= 0 {
            return state;
        }
        // Apply the recurrence n times via doubling on the
        // affine transform `s -> a*s + b mod m`.
        //
        // Composition: (a1 * (a2 * s + b2) + b1) = a1*a2*s + a1*b2 + b1.
        // So composing two transforms (a, b) is (a1*a2, a1*b2 + b1).
        // Doubling: (a, b) -> (a*a, a*b + b).
        let mut a = ia.rem_euclid(im);
        let mut b = ic.rem_euclid(im);
        let mut result_a: i64 = 1; // identity affine: 1*s + 0
        let mut result_b: i64 = 0;
        let m = im;
        let mut k = n;
        while k > 0 {
            if k & 1 == 1 {
                // result <- a * result_a, a * result_b + b
                // i.e. composition: (result_a, result_b) ∘ (a, b)
                // applied as `(result_a, result_b) := compose((a, b), (result_a, result_b))`
                let new_a = mul_mod(a, result_a, m);
                let new_b = (mul_mod(a, result_b, m) + b).rem_euclid(m);
                result_a = new_a;
                result_b = new_b;
            }
            // Double the (a, b) transform.
            let next_a = mul_mod(a, a, m);
            let next_b = (mul_mod(a, b, m) + b).rem_euclid(m);
            a = next_a;
            b = next_b;
            k >>= 1;
        }
        (mul_mod(result_a, state.rem_euclid(m), m) + result_b).rem_euclid(m)
    })
}

/// `(a * b) mod m` without i128 overflow on i64-sized
/// operands. fasta's IM is 139968, well within i32 range, so
/// this is fine on x86_64; the i128 widening keeps it correct
/// for any callers that pick larger moduli.
fn mul_mod(a: i64, b: i64, m: i64) -> i64 {
    let prod = (a as i128) * (b as i128);
    (prod.rem_euclid(m as i128)) as i64
}

// ----- Fn-trait coercion trampolines -----
//
// When a bare `fn item` (or the address of a non-capturing lifted
// closure) is coerced to `Fn(args) -> ret`, MIR allocates a
// 16-byte env blob `[trampoline_addr, real_fn_addr]` and stores
// `gos_rt_fn_tramp_<arity>` at offset 0. The closure-call dispatch
// in the cranelift codegen then invokes that trampoline as
// `f(env, args…)`; the trampoline reads the real fn from `env+8`
// and forwards the args, dropping the env. Capturing closures
// don't need this — their env already carries the lifted body's
// (env, args) signature at offset 0.
//
// Arities 0..=8 cover every higher-order shape the stdlib uses
// today (most are arity ≤ 3); add more if a real call site needs
// it.

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_fn_tramp_0(env: *const u8) -> i64 {
    ffi_entry!(-1, {
        // SAFETY: `env` was constructed by the MIR coercion site as a
        // 16-byte blob whose word at offset 8 is the real fn ptr.
        // The typed registry vetoes a mismatched kind before transmute.
        let real_fn_addr = unsafe { core::ptr::read_unaligned(env.add(8).cast::<usize>()) };
        super::fn_registry::verify(
            real_fn_addr,
            super::fn_registry::FnKind::I64ArgsToI64 { arity: 0 },
        );
        let real_fn: extern "C" fn() -> i64 = unsafe { core::mem::transmute(real_fn_addr) };
        real_fn()
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_fn_tramp_1(env: *const u8, a0: i64) -> i64 {
    ffi_entry!(-1, {
        let real_fn_addr = unsafe { core::ptr::read_unaligned(env.add(8).cast::<usize>()) };
        super::fn_registry::verify(
            real_fn_addr,
            super::fn_registry::FnKind::I64ArgsToI64 { arity: 1 },
        );
        let real_fn: extern "C" fn(i64) -> i64 = unsafe { core::mem::transmute(real_fn_addr) };
        real_fn(a0)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_fn_tramp_2(env: *const u8, a0: i64, a1: i64) -> i64 {
    ffi_entry!(-1, {
        let real_fn_addr = unsafe { core::ptr::read_unaligned(env.add(8).cast::<usize>()) };
        super::fn_registry::verify(
            real_fn_addr,
            super::fn_registry::FnKind::I64ArgsToI64 { arity: 2 },
        );
        let real_fn: extern "C" fn(i64, i64) -> i64 = unsafe { core::mem::transmute(real_fn_addr) };
        real_fn(a0, a1)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_fn_tramp_3(env: *const u8, a0: i64, a1: i64, a2: i64) -> i64 {
    ffi_entry!(-1, {
        let real_fn_addr = unsafe { core::ptr::read_unaligned(env.add(8).cast::<usize>()) };
        super::fn_registry::verify(
            real_fn_addr,
            super::fn_registry::FnKind::I64ArgsToI64 { arity: 3 },
        );
        let real_fn: extern "C" fn(i64, i64, i64) -> i64 =
            unsafe { core::mem::transmute(real_fn_addr) };
        real_fn(a0, a1, a2)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_fn_tramp_4(
    env: *const u8,
    a0: i64,
    a1: i64,
    a2: i64,
    a3: i64,
) -> i64 {
    ffi_entry!(-1, {
        let real_fn_addr = unsafe { core::ptr::read_unaligned(env.add(8).cast::<usize>()) };
        super::fn_registry::verify(
            real_fn_addr,
            super::fn_registry::FnKind::I64ArgsToI64 { arity: 4 },
        );
        let real_fn: extern "C" fn(i64, i64, i64, i64) -> i64 =
            unsafe { core::mem::transmute(real_fn_addr) };
        real_fn(a0, a1, a2, a3)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_fn_tramp_5(
    env: *const u8,
    a0: i64,
    a1: i64,
    a2: i64,
    a3: i64,
    a4: i64,
) -> i64 {
    ffi_entry!(-1, {
        let real_fn_addr = unsafe { core::ptr::read_unaligned(env.add(8).cast::<usize>()) };
        super::fn_registry::verify(
            real_fn_addr,
            super::fn_registry::FnKind::I64ArgsToI64 { arity: 5 },
        );
        let real_fn: extern "C" fn(i64, i64, i64, i64, i64) -> i64 =
            unsafe { core::mem::transmute(real_fn_addr) };
        real_fn(a0, a1, a2, a3, a4)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_fn_tramp_6(
    env: *const u8,
    a0: i64,
    a1: i64,
    a2: i64,
    a3: i64,
    a4: i64,
    a5: i64,
) -> i64 {
    ffi_entry!(-1, {
        let real_fn_addr = unsafe { core::ptr::read_unaligned(env.add(8).cast::<usize>()) };
        super::fn_registry::verify(
            real_fn_addr,
            super::fn_registry::FnKind::I64ArgsToI64 { arity: 6 },
        );
        let real_fn: extern "C" fn(i64, i64, i64, i64, i64, i64) -> i64 =
            unsafe { core::mem::transmute(real_fn_addr) };
        real_fn(a0, a1, a2, a3, a4, a5)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_fn_tramp_7(
    env: *const u8,
    a0: i64,
    a1: i64,
    a2: i64,
    a3: i64,
    a4: i64,
    a5: i64,
    a6: i64,
) -> i64 {
    ffi_entry!(-1, {
        let real_fn_addr = unsafe { core::ptr::read_unaligned(env.add(8).cast::<usize>()) };
        super::fn_registry::verify(
            real_fn_addr,
            super::fn_registry::FnKind::I64ArgsToI64 { arity: 7 },
        );
        let real_fn: extern "C" fn(i64, i64, i64, i64, i64, i64, i64) -> i64 =
            unsafe { core::mem::transmute(real_fn_addr) };
        real_fn(a0, a1, a2, a3, a4, a5, a6)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_fn_tramp_8(
    env: *const u8,
    a0: i64,
    a1: i64,
    a2: i64,
    a3: i64,
    a4: i64,
    a5: i64,
    a6: i64,
    a7: i64,
) -> i64 {
    ffi_entry!(-1, {
        let real_fn_addr = unsafe { core::ptr::read_unaligned(env.add(8).cast::<usize>()) };
        super::fn_registry::verify(
            real_fn_addr,
            super::fn_registry::FnKind::I64ArgsToI64 { arity: 8 },
        );
        let real_fn: extern "C" fn(i64, i64, i64, i64, i64, i64, i64, i64) -> i64 =
            unsafe { core::mem::transmute(real_fn_addr) };
        real_fn(a0, a1, a2, a3, a4, a5, a6, a7)
    })
}
