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
// Scheduler - every `go fn(args)` lands on the M:N pool
// ---------------------------------------------------------------

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_go_spawn(
    func: Option<unsafe extern "C" fn(*mut u8)>,
    env: *mut u8,
) {
    ffi_entry!((), {
        let Some(f) = func else { return };
        let env_addr = env as usize;
        spawn_task(Box::new(move || {
            let env = env_addr as *mut u8;
            unsafe { f(env) };
        }));
    });
}

fn spawn_task(task: Box<dyn FnOnce() + Send + 'static>) {
    crate::sched_global::spawn(task);
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_go_spawn_call_0(fn_addr: usize) {
    ffi_entry!((), {
        if fn_addr == 0 {
            return;
        }
        super::fn_registry::verify(
            fn_addr,
            super::fn_registry::FnKind::GoSpawnEntry { arity: 0 },
        );
        spawn_task(Box::new(move || {
            // SAFETY: the caller promises `fn_addr` is the address of
            // a goroutine entry function emitted by native codegen. `go`
            // statements discard the callee result, and codegen emits these
            // entries as `extern "C" fn(...)` returning void. The typed
            // registry verify above rejects mismatched arities.
            type Fn0 = unsafe extern "C" fn();
            let f: Fn0 = unsafe { std::mem::transmute(fn_addr) };
            unsafe { f() };
        }));
    });
}

/// Spawns a goroutine on the work-stealing scheduler (or, if no
/// scheduler is installed, an OS thread) that calls a one-argument
/// function with a single i64 payload. All Gossamer scalar types
/// fit in an i64 slot; floats are passed by bitcast.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_go_spawn_call_1(fn_addr: usize, arg0: i64) {
    ffi_entry!((), {
        if fn_addr == 0 {
            return;
        }
        super::fn_registry::verify(
            fn_addr,
            super::fn_registry::FnKind::GoSpawnEntry { arity: 1 },
        );
        spawn_task(Box::new(move || {
            type Fn1 = unsafe extern "C" fn(i64);
            let f: Fn1 = unsafe { std::mem::transmute(fn_addr) };
            unsafe { f(arg0) };
        }));
    });
}

/// Two-arg version.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_go_spawn_call_2(fn_addr: usize, arg0: i64, arg1: i64) {
    ffi_entry!((), {
        if fn_addr == 0 {
            return;
        }
        super::fn_registry::verify(
            fn_addr,
            super::fn_registry::FnKind::GoSpawnEntry { arity: 2 },
        );
        spawn_task(Box::new(move || {
            type Fn2 = unsafe extern "C" fn(i64, i64);
            let f: Fn2 = unsafe { std::mem::transmute(fn_addr) };
            unsafe { f(arg0, arg1) };
        }));
    });
}

/// Three-arg version. Required for fan-out patterns (buf, idx, wg).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_go_spawn_call_3(fn_addr: usize, arg0: i64, arg1: i64, arg2: i64) {
    ffi_entry!((), {
        if fn_addr == 0 {
            return;
        }
        super::fn_registry::verify(
            fn_addr,
            super::fn_registry::FnKind::GoSpawnEntry { arity: 3 },
        );
        spawn_task(Box::new(move || {
            type Fn3 = unsafe extern "C" fn(i64, i64, i64);
            let f: Fn3 = unsafe { std::mem::transmute(fn_addr) };
            unsafe { f(arg0, arg1, arg2) };
        }));
    });
}

/// Four-arg version. Common fasta worker shape (buf, start, count, wg).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_go_spawn_call_4(
    fn_addr: usize,
    arg0: i64,
    arg1: i64,
    arg2: i64,
    arg3: i64,
) {
    ffi_entry!((), {
        if fn_addr == 0 {
            return;
        }
        super::fn_registry::verify(
            fn_addr,
            super::fn_registry::FnKind::GoSpawnEntry { arity: 4 },
        );
        spawn_task(Box::new(move || {
            type Fn4 = unsafe extern "C" fn(i64, i64, i64, i64);
            let f: Fn4 = unsafe { std::mem::transmute(fn_addr) };
            unsafe { f(arg0, arg1, arg2, arg3) };
        }));
    });
}

/// Five-arg version. Used by fasta_mt's IUB worker.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_go_spawn_call_5(
    fn_addr: usize,
    arg0: i64,
    arg1: i64,
    arg2: i64,
    arg3: i64,
    arg4: i64,
) {
    ffi_entry!((), {
        if fn_addr == 0 {
            return;
        }
        super::fn_registry::verify(
            fn_addr,
            super::fn_registry::FnKind::GoSpawnEntry { arity: 5 },
        );
        spawn_task(Box::new(move || {
            type Fn5 = unsafe extern "C" fn(i64, i64, i64, i64, i64);
            let f: Fn5 = unsafe { std::mem::transmute(fn_addr) };
            unsafe { f(arg0, arg1, arg2, arg3, arg4) };
        }));
    });
}

/// Six-arg version, headroom for future fan-out shapes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_go_spawn_call_6(
    fn_addr: usize,
    arg0: i64,
    arg1: i64,
    arg2: i64,
    arg3: i64,
    arg4: i64,
    arg5: i64,
) {
    ffi_entry!((), {
        if fn_addr == 0 {
            return;
        }
        super::fn_registry::verify(
            fn_addr,
            super::fn_registry::FnKind::GoSpawnEntry { arity: 6 },
        );
        spawn_task(Box::new(move || {
            type Fn6 = unsafe extern "C" fn(i64, i64, i64, i64, i64, i64);
            let f: Fn6 = unsafe { std::mem::transmute(fn_addr) };
            unsafe { f(arg0, arg1, arg2, arg3, arg4, arg5) };
        }));
    });
}

// ---------------------------------------------------------------
// Join handles - `spawn(f)` captures the goroutine's outcome
// ---------------------------------------------------------------

/// Heap-boxed result of a spawned goroutine, carried over the
/// one-shot handle channel as a single 8-byte pointer.
/// `disc` 0 = Ok (`payload` is the returned i64); `disc` 1 = Err
/// (`payload` is a c-string pointer to the panic message).
#[repr(C)]
struct SpawnOutcome {
    disc: i64,
    payload: i64,
}

/// Sends a boxed `SpawnOutcome` over the one-shot handle channel.
fn deliver_outcome(ch_addr: usize, disc: i64, payload: i64) {
    let boxed = Box::new(SpawnOutcome { disc, payload });
    let outcome_ptr = Box::into_raw(boxed) as i64;
    let bytes = outcome_ptr.to_ne_bytes();
    let ch = ch_addr as *mut super::chan::GosChan;
    // SAFETY: `ch` is the live one-shot channel; `bytes` is an 8-byte
    // buffer matching the channel's element width.
    unsafe {
        super::chan::gos_rt_chan_send(ch, bytes.as_ptr());
    }
}

/// Delivers `Err(message)` to the join handle if the spawned body is
/// unwinding (a panic). On the normal path it is disarmed after the
/// `Ok` outcome is sent.
struct SpawnOutcomeGuard {
    ch_addr: usize,
    armed: bool,
}

impl Drop for SpawnOutcomeGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        // Unwinding: the goroutine panicked. `gos_rt_panic` stashed the
        // message before raising; recover it (falling back to a generic
        // note for a non-`gos_rt_panic` unwind) and hand the joiner an
        // Err. The panic itself continues up to the coroutine wrapper,
        // which isolates the goroutine.
        let msg = super::panic::take_last_goroutine_panic()
            .unwrap_or_else(|| "spawned goroutine panicked".to_string());
        let cstr = super::string::alloc_cstring(msg.as_bytes());
        deliver_outcome(self.ch_addr, 1, cstr as i64);
    }
}

/// `spawn(f) -> handle` - runs the callable `code`/`env` pair on the
/// goroutine pool and returns a one-shot channel handle. The outcome
/// (returned value, or panic message) is delivered to `gos_rt_join`.
/// `code` is the per-shape thunk / lifted-closure address; `env` is
/// the closure environment blob (passed as the implicit first arg).
///
/// A panic is NOT caught here: catching across the runtime-call
/// boundary trips the nounwind contract on the `gos_rt_panic` frame
/// and aborts. Instead the panic propagates to the coroutine wrapper
/// (the same path `go` uses to isolate goroutine panics), and a
/// Drop-guard delivers `Err(message)` to the handle as the stack
/// unwinds past it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_spawn(code: usize, env: usize) -> *mut super::chan::GosChan {
    ffi_entry!(std::ptr::null_mut(), {
        if code == 0 {
            return std::ptr::null_mut();
        }
        // One-shot, capacity-1 channel carrying a single SpawnOutcome
        // pointer. Capacity 1 lets the worker deposit the outcome
        // without waiting for the joiner to arrive.
        let ch = unsafe { super::chan::gos_rt_chan_new(8, 1) };
        let ch_addr = ch as usize;
        spawn_task(Box::new(move || {
            // This body is joinable: a panic here is observed through `join()`,
            // so `gos_rt_panic` suppresses its eager report (the guard delivers
            // `Err` instead). The scope restores the flag on the unwind too.
            let _joinable = gossamer_coro::JoinableScope::enter(true);
            let mut guard = SpawnOutcomeGuard {
                ch_addr,
                armed: true,
            };
            // SAFETY: `code` is the callable's entry address; the
            // closure ABI calls it as `fn(env) -> i64` with the
            // environment blob as the implicit argument. The
            // `C-unwind` ABI lets a goroutine panic propagate across
            // this call into the Drop-guard above.
            type Fn1 = unsafe extern "C-unwind" fn(usize) -> i64;
            let f: Fn1 = unsafe { std::mem::transmute(code) };
            let value = unsafe { f(env) };
            // Normal completion: disarm the guard and deliver Ok.
            guard.armed = false;
            deliver_outcome(ch_addr, 0, value);
        }));
        ch
    })
}

/// `handle.join() -> Result<T, String>` - blocks (parking the caller
/// cooperatively in a goroutine, or condvar-blocking on an OS thread)
/// until the spawned goroutine deposits its outcome, then unpacks it
/// into the 2-word Result aggregate.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_join(ch: *mut super::chan::GosChan) -> i128 {
    ffi_entry!(super::vec::pack_result(1, 0), {
        if ch.is_null() {
            return super::vec::pack_result(1, 0);
        }
        let mut buf = [0u8; 8];
        // SAFETY: `buf` is an 8-byte sink matching the channel width.
        let ok = unsafe { super::chan::gos_rt_chan_recv(ch, buf.as_mut_ptr()) };
        if ok == 0 {
            return super::vec::pack_result(1, 0);
        }
        let outcome_ptr = i64::from_ne_bytes(buf) as *mut SpawnOutcome;
        if outcome_ptr.is_null() {
            return super::vec::pack_result(1, 0);
        }
        // SAFETY: the pointer was produced by `Box::into_raw` in the
        // spawn body; reclaim ownership and free it here.
        let outcome = unsafe { Box::from_raw(outcome_ptr) };
        super::vec::pack_result(outcome.disc, outcome.payload)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_go_yield() {
    ffi_entry!((), {
        // Real coroutine yield - suspend this goroutine and let the
        // worker M run another. The scheduler immediately re-enqueues
        // the suspended goroutine because we don't set the
        // pending-park flag, so this is a "give up the slice"
        // primitive (Go's `runtime.Gosched`). Falls back to an OS
        // yield if called outside a goroutine context.
        if gossamer_coro::in_goroutine() {
            gossamer_coro::suspend();
        } else {
            std::thread::yield_now();
        }
    });
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sleep_ns(ns: i64) {
    ffi_entry!((), {
        if ns <= 0 {
            return;
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_nanos(ns as u64);
        // Park on the netpoller's timer wheel so a sleeping goroutine
        // does not consume a worker slot for the full duration. The
        // worker thread is still parked on a Condvar, but the
        // scheduler's pool grows transparently if multiple goroutines
        // sleep concurrently.
        crate::sched_global::sleep_until(deadline);
    });
}

/// `time::sleep_ctx(ctx, ms)` - sleeps for `ms` milliseconds unless the
/// context fires first. Returns `1` when the full duration elapsed and `0`
/// when the context cancelled the wait. A cancelled context returns `0`
/// without sleeping.
///
/// # Safety
/// `ctx_handle` is an opaque context handle, or null for an uncancellable
/// sleep.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sleep_ms_ctx(ctx_handle: *const u8, ms: i64) -> i64 {
    ffi_entry!(0, {
        if ms < 0 {
            unsafe {
                super::gos_rt_panic(c"time::sleep_ctx: duration_ms must be non-negative".as_ptr());
            };
        }
        let addr = ctx_handle as usize;
        let cancelled = || super::context::addr_is_cancelled(addr);
        if cancelled() {
            return 0;
        }
        if addr == 0 {
            unsafe { gos_rt_sleep_ms(ms) };
            return 1;
        }
        let deadline =
            std::time::Instant::now() + std::time::Duration::from_millis(ms.max(0) as u64);
        // Cancelling unparks every goroutine registered on the node, so the
        // sleep resumes on whichever of the two arrives first and re-parks
        // for the remainder when it was neither.
        while std::time::Instant::now() < deadline {
            if cancelled() {
                return 0;
            }
            if let Some(gid) = crate::sched_global::current_gid() {
                super::context::register_waiter(addr, gid);
            }
            crate::sched_global::sleep_until(deadline);
            if let Some(gid) = crate::sched_global::current_gid() {
                super::context::deregister_waiter(addr, gid);
            }
        }
        i64::from(!cancelled())
    })
}

/// `time::sleep(ms: i64)` - the millisecond-units variant
/// surfaced to Gossamer code. The bytecode VM uses
/// `Duration::from_millis(ms)`; this helper gives the cranelift
/// / LLVM dispatch the same units so `time::sleep(1000)` waits
/// one second across all three tiers. Without it the compiled
/// tier called `gos_rt_sleep_ns(ms)` directly and slept for
/// nanoseconds, busy-spinning every poll loop under
/// `gos build` / `gos build --release` builds.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sleep_ms(ms: i64) {
    ffi_entry!((), {
        if ms < 0 {
            unsafe {
                super::gos_rt_panic(c"time::sleep: duration_ms must be non-negative".as_ptr());
            };
        }
        let ns = ms.saturating_mul(1_000_000);
        unsafe { gos_rt_sleep_ns(ns) }
    });
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_now_ns() -> i64 {
    ffi_entry!(-1, {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos() as i64)
    })
}
