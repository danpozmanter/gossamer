#![allow(clippy::missing_safety_doc)]
#![allow(missing_docs)]
#![allow(clippy::too_many_lines)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]
#![allow(clippy::must_use_candidate)]
#![allow(clippy::similar_names)]
#![allow(clippy::many_single_char_names)]
#![allow(clippy::items_after_statements)]
#![allow(clippy::cast_lossless)]
#![allow(clippy::cast_sign_loss)]
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::doc_markdown)]
#![allow(clippy::cast_ptr_alignment)]
#![allow(clippy::ptr_as_ptr)]
#![allow(unused_unsafe)]
#![allow(clippy::wildcard_imports)]

use parking_lot::{Condvar, Mutex, Once};

// ---------------------------------------------------------------
// sync::Barrier - fixed-participant rendezvous
// ---------------------------------------------------------------
//
// Mirrors `gossamer_std::sync::Barrier` (the VM/interp backing
// type) bit-for-bit: a `Mutex<BarrierState>` plus a `Condvar`,
// generation-counted so a barrier reused across rounds wakes only
// the waiters of the current generation. Every participant calls
// `wait()`; the first `n - 1` block on the condvar, the `n`th
// flips the generation and notifies all. No spinning, no sleeps -
// identical observable semantics to the interpreter so tier-parity
// output matches.
//
// Like the compiled `GosWaitGroup`, a waiter blocks its OS worker
// thread on the condvar. A barrier needs every participant alive
// at once, so a program must not enqueue more simultaneous
// participants than the scheduler has worker threads (the main
// goroutine runs on its own thread and never consumes a pool
// worker).

struct BarrierState {
    expected: usize,
    arrived: usize,
    generation: u64,
}

pub struct GosBarrier {
    state: Mutex<BarrierState>,
    cv: Condvar,
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_barrier_new(n: i64) -> *mut GosBarrier {
    ffi_entry!(std::ptr::null_mut(), {
        // A zero or negative count would never release; clamp to a
        // single participant so `wait()` returns immediately rather
        // than deadlocking, matching `Barrier::new(1)`.
        let expected = if n < 1 { 1 } else { n as usize };
        Box::into_raw(Box::new(GosBarrier {
            state: Mutex::new(BarrierState {
                expected,
                arrived: 0,
                generation: 0,
            }),
            cv: Condvar::new(),
        }))
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_barrier_wait(b: *mut GosBarrier) {
    ffi_entry!((), {
        if b.is_null() {
            return;
        }
        let b = unsafe { &*b };
        let mut state = b.state.lock();
        let captured_gen = state.generation;
        state.arrived += 1;
        if state.arrived >= state.expected {
            state.arrived = 0;
            state.generation = state.generation.wrapping_add(1);
            b.cv.notify_all();
            return;
        }
        while state.generation == captured_gen {
            b.cv.wait(&mut state);
        }
    });
}

// ---------------------------------------------------------------
// sync::Once - run-exactly-once guard with a closure callback
// ---------------------------------------------------------------
//
// Wraps `parking_lot::Once`. `call(env)` runs the supplied closure
// the first time it is reached and never again, returning `1` on
// the run that executed the body and `0` on every subsequent call.
//
// The closure crosses the C-ABI through the shared callable
// convention used by the `iter::*` / `option::*` combinators: `env`
// is a heap blob whose first word is the callable address; the body
// is invoked as `f(env)`. A nullary closure returning `()` lowers to
// the same `fn(env) -> i64` value-thunk shape as
// `option::default_with`'s callback.

type ThunkValFn = unsafe extern "C" fn(env: *const u8) -> i64;

/// Callable address stored at `env[0]`, or `None` for a null/zero env.
fn env_fn_addr(env: *const u8) -> Option<*const ()> {
    if env.is_null() {
        return None;
    }
    // SAFETY: `env` is a live closure blob whose first word is the
    // callable address (codegen invariant shared with the combinator
    // and `iter::*` families).
    let addr = unsafe { (env.cast::<usize>()).read() };
    if addr == 0 {
        None
    } else {
        // Recover the address's exposed provenance so the pointer is
        // sound to call under strict provenance; a bare integer
        // transmute at the call site would carry none.
        Some(std::ptr::with_exposed_provenance::<()>(addr))
    }
}

pub struct GosOnce {
    inner: Once,
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_once_new() -> *mut GosOnce {
    ffi_entry!(std::ptr::null_mut(), {
        Box::into_raw(Box::new(GosOnce { inner: Once::new() }))
    })
}

/// Runs `env`'s closure exactly once across all callers of this
/// handle. Returns `1` on the call that executed the body, `0`
/// otherwise. Mirrors the interp's `native_once_call`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_once_call(o: *mut GosOnce, env: *const u8) -> i64 {
    ffi_entry!(0, {
        if o.is_null() {
            return 0;
        }
        let o = unsafe { &*o };
        let mut ran = 0i64;
        o.inner.call_once(|| {
            ran = 1;
            if let Some(addr) = env_fn_addr(env) {
                // SAFETY: addr is the callable stored by the closure
                // lowering; the nullary-unit closure lowers to the
                // `fn(env) -> i64` value-thunk shape.
                let f: ThunkValFn = unsafe { std::mem::transmute(addr) };
                unsafe {
                    f(env);
                }
            }
        });
        ran
    })
}
