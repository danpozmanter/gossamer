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
// Scheduler — every `go fn(args)` lands on the M:N pool
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
            // an `extern "C" fn() -> i64` — the SysV-ABI convention
            // native codegen emits for every Gossamer function. The
            // typed registry verify above rejects mismatched kinds.
            type Fn0 = unsafe extern "C" fn() -> i64;
            let f: Fn0 = unsafe { std::mem::transmute(fn_addr) };
            let _ = unsafe { f() };
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
            type Fn1 = unsafe extern "C" fn(i64) -> i64;
            let f: Fn1 = unsafe { std::mem::transmute(fn_addr) };
            let _ = unsafe { f(arg0) };
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
            type Fn2 = unsafe extern "C" fn(i64, i64) -> i64;
            let f: Fn2 = unsafe { std::mem::transmute(fn_addr) };
            let _ = unsafe { f(arg0, arg1) };
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
            type Fn3 = unsafe extern "C" fn(i64, i64, i64) -> i64;
            let f: Fn3 = unsafe { std::mem::transmute(fn_addr) };
            let _ = unsafe { f(arg0, arg1, arg2) };
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
            type Fn4 = unsafe extern "C" fn(i64, i64, i64, i64) -> i64;
            let f: Fn4 = unsafe { std::mem::transmute(fn_addr) };
            let _ = unsafe { f(arg0, arg1, arg2, arg3) };
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
            type Fn5 = unsafe extern "C" fn(i64, i64, i64, i64, i64) -> i64;
            let f: Fn5 = unsafe { std::mem::transmute(fn_addr) };
            let _ = unsafe { f(arg0, arg1, arg2, arg3, arg4) };
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
            type Fn6 = unsafe extern "C" fn(i64, i64, i64, i64, i64, i64) -> i64;
            let f: Fn6 = unsafe { std::mem::transmute(fn_addr) };
            let _ = unsafe { f(arg0, arg1, arg2, arg3, arg4, arg5) };
        }));
    });
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_go_yield() {
    ffi_entry!((), {
        // Real coroutine yield — suspend this goroutine and let the
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

/// `time::sleep(ms: i64)` — the millisecond-units variant
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
        let ns = ms.max(0).saturating_mul(1_000_000);
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
