//! Stackful coroutines for Gossamer goroutines.
//!
//! Wraps [`corosensei`] in just enough surface area to make every
//! Gossamer `go fn(args)` a real stackful coroutine: an OS-thread
//! worker (the M) resumes a coroutine; the coroutine runs user code;
//! when user code calls [`suspend`], control returns to the worker,
//! which can pick up another goroutine. The coroutine's stack is
//! preserved between resumes so the function can pick up exactly
//! where it left off.
//!
//! The crate is deliberately thin — it does not own scheduling,
//! parking semantics, or wakeup wiring. Those live in
//! `gossamer-runtime::sched` / `sched_global`. This crate only
//! exposes:
//!
//! - [`Goroutine`] — owns a `corosensei::Coroutine` plus a stable
//!   pointer to its [`corosensei::Yielder`].
//! - [`suspend`] — yields the currently running goroutine via a
//!   thread-local pointer to its yielder. The scheduler's worker
//!   loop sets this pointer before each resume.
//!
//! ## Send / Sync
//!
//! `corosensei::Coroutine` is `!Send` by default to guard against
//! TLS-binding accidents. Gossamer's M:N scheduler explicitly
//! migrates goroutines across worker threads, so [`Goroutine`]
//! provides an `unsafe impl Send`. The contract is:
//!
//! - User code inside a goroutine **must not assume** any TLS slot
//!   is preserved across [`suspend`] calls. If user code stashes
//!   thread-local state, suspend, and the goroutine resumes on a
//!   different worker, the TLS read returns the new worker's slot.
//!   This is the same constraint Go imposes on goroutines.
//! - The coroutine's saved register state and stack are
//!   pure-data and trivially `Send`.

#![forbid(unsafe_op_in_unsafe_fn)]

use std::cell::Cell;
use std::sync::Arc;
use std::sync::atomic::{AtomicPtr, Ordering};

use corosensei::stack::DefaultStack;
use corosensei::{Coroutine, CoroutineResult, Yielder};

/// Default goroutine stack size in bytes (1 MiB). Override via the
/// `GOSSAMER_GOROUTINE_STACK` environment variable, parsed at the
/// first [`Goroutine::new`] call after process start.
///
/// 1 MiB is generous compared to Go's 8 KiB starting size, but Go
/// grows stacks on demand via segmented + relocating allocation;
/// our stacks are fixed. On 64-bit hosts the cost is virtual address
/// space (cheap) — `mmap`'s on-demand committing keeps RSS
/// proportional to *actual* depth used. 10 000 goroutines eat
/// ~10 GiB of address space and typically tens of MiB of committed
/// RAM. Compiled-tier code (HTTP handlers, JSON parsing, regex
/// captures, format!) routinely uses tens of KiB of stack frames,
/// so the previous 16 KiB default overflowed under real workloads
/// and corrupted adjacent heap mappings.
pub const DEFAULT_STACK_BYTES: usize = 1024 * 1024;

/// Minimum allowed stack size in bytes (32 KiB). Overrides smaller
/// than this are clamped up — anything less risks overflowing into
/// the guard page from a single function prologue.
pub const MIN_STACK_BYTES: usize = 32 * 1024;

/// Reads the configured goroutine stack size. Honours
/// `GOSSAMER_GOROUTINE_STACK` (parsed once and cached). Values
/// below [`MIN_STACK_BYTES`] are clamped up so a stray override
/// can't reintroduce the heap-corruption bug.
#[must_use]
pub fn stack_size() -> usize {
    use std::sync::OnceLock;
    static SIZE: OnceLock<usize> = OnceLock::new();
    *SIZE.get_or_init(|| {
        std::env::var("GOSSAMER_GOROUTINE_STACK")
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
            .map_or(DEFAULT_STACK_BYTES, |n| n.max(MIN_STACK_BYTES))
    })
}

/// Stackful coroutine that runs a single Gossamer goroutine to
/// completion across one or more `resume()` calls.
pub struct Goroutine {
    coro: Coroutine<(), (), (), DefaultStack>,
    yielder_slot: Arc<AtomicPtr<()>>,
}

// SAFETY: the coroutine's stack and saved register state are
// plain data. Migration across OS threads is deliberate: the
// Gossamer M:N scheduler's worker pool steals goroutines off
// peer deques. User code is documented to not rely on
// TLS-stable-across-yield semantics.
unsafe impl Send for Goroutine {}

impl Goroutine {
    /// Constructs a new goroutine whose entry point is `main`. The
    /// goroutine does not start running until [`Self::resume`] is
    /// called.
    ///
    /// # Panics
    ///
    /// Panics if `corosensei` cannot allocate the coroutine stack
    /// (typically `mmap` failure on a near-OOM host).
    #[must_use]
    pub fn new(main: Box<dyn FnOnce() + Send + 'static>) -> Self {
        let yielder_slot: Arc<AtomicPtr<()>> = Arc::new(AtomicPtr::new(std::ptr::null_mut()));
        let yielder_slot_clone = Arc::clone(&yielder_slot);
        let stack = DefaultStack::new(stack_size()).expect("alloc goroutine stack");
        let coro = Coroutine::with_stack(stack, move |yielder: &Yielder<(), ()>, ()| {
            // The yielder is a stack value with an address that is
            // stable for the lifetime of the coroutine. Two writes
            // happen on first entry:
            //
            // 1. `yielder_slot` — published so subsequent resumes
            //    (which the scheduler initiates from a worker M
            //    that may differ from this one) can read the
            //    pointer and re-arm the worker's TLS_YIELDER.
            // 2. `set_current_yielder` — bootstrap value for *this*
            //    first resume, so `suspend()` can find the yielder
            //    before the worker had a chance to set TLS itself.
            let ptr = std::ptr::from_ref::<Yielder<(), ()>>(yielder)
                .cast::<()>()
                .cast_mut();
            yielder_slot_clone.store(ptr, Ordering::Release);
            set_current_yielder(ptr);
            main();
        });
        Self { coro, yielder_slot }
    }

    /// Returns the yielder pointer for this goroutine, or null if
    /// the coroutine has not yet been resumed for the first time.
    /// The scheduler's worker loop reads this and pushes it into
    /// thread-local state before calling [`Self::resume`].
    #[must_use]
    pub fn yielder_ptr(&self) -> *mut () {
        self.yielder_slot.load(Ordering::Acquire)
    }

    /// Resumes execution of the goroutine. Returns `true` when the
    /// goroutine has completed (its entry function returned).
    /// Returns `false` when the goroutine called [`suspend`] —
    /// the caller should re-resume later when the wakeup event
    /// fires.
    ///
    /// # Panics
    ///
    /// Panics if the goroutine itself panics; the panic is propagated
    /// to the caller. (Suspended-then-resumed coroutines do not panic
    /// from the resume site itself.)
    pub fn resume(&mut self) -> bool {
        match self.coro.resume(()) {
            CoroutineResult::Yield(()) => false,
            CoroutineResult::Return(()) => true,
        }
    }

    /// Returns whether the goroutine has finished.
    #[must_use]
    pub fn done(&self) -> bool {
        self.coro.done()
    }
}

thread_local! {
    /// Pointer to the [`Yielder`] of the goroutine currently
    /// running on this OS thread. Set by the scheduler's worker
    /// loop immediately before each `resume()` call; cleared after.
    /// Code paths that want to suspend the calling goroutine read
    /// this and call `suspend()` on the yielder.
    static CURRENT_YIELDER: Cell<*mut ()> = const { Cell::new(std::ptr::null_mut()) };
}

/// Sets the thread-local current-yielder pointer. Called by the
/// scheduler's worker loop before resuming a goroutine.
pub fn set_current_yielder(ptr: *mut ()) {
    CURRENT_YIELDER.with(|c| c.set(ptr));
}

/// Clears the thread-local current-yielder pointer. Called by the
/// scheduler's worker loop after a resume returns.
pub fn clear_current_yielder() {
    CURRENT_YIELDER.with(|c| c.set(std::ptr::null_mut()));
}

/// Returns whether the calling thread is currently executing inside
/// a goroutine. Equivalent to `current_yielder().is_some()`.
#[must_use]
pub fn in_goroutine() -> bool {
    CURRENT_YIELDER.with(|c| !c.get().is_null())
}

/// Suspends the goroutine currently running on this OS thread.
/// Control returns to the scheduler at the resume site; the
/// goroutine becomes runnable again only when the scheduler's
/// `unpark(gid)` is called by whatever the goroutine was waiting
/// on (a channel, a poller readiness, a mutex release, ...).
///
/// # Panics
///
/// Panics if called from outside a goroutine (i.e. when the
/// thread-local current-yielder pointer is null). This is a
/// programming error: stdlib code that may suspend the calling
/// goroutine must check [`in_goroutine`] first if it can be
/// invoked from a non-goroutine thread.
pub fn suspend() {
    let ptr = CURRENT_YIELDER.with(Cell::get);
    assert!(
        !ptr.is_null(),
        "gossamer_coro::suspend() called outside a goroutine context",
    );
    // SAFETY: the scheduler's worker loop sets this pointer to the
    // yielder of the goroutine currently executing on this OS
    // thread, and clears it after the resume returns. `suspend()`
    // is therefore only ever called between matching set/clear
    // calls, while the pointed-to yielder's coroutine is alive on
    // the worker's stack.
    let yielder: &Yielder<(), ()> = unsafe { &*ptr.cast::<Yielder<(), ()>>() };
    yielder.suspend(());
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn coroutine_runs_to_completion() {
        let trace: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
        let trace_for_main = Arc::clone(&trace);
        let mut g = Goroutine::new(Box::new(move || {
            trace_for_main.lock().unwrap().push("a");
        }));
        // Worker shim: stash yielder, resume, clear.
        set_current_yielder(g.yielder_ptr());
        let done = g.resume();
        clear_current_yielder();
        assert!(done);
        assert_eq!(*trace.lock().unwrap(), vec!["a"]);
    }

    #[test]
    fn coroutine_suspends_and_resumes() {
        let trace: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
        let trace_for_main = Arc::clone(&trace);
        let mut g = Goroutine::new(Box::new(move || {
            trace_for_main.lock().unwrap().push("a");
            suspend();
            trace_for_main.lock().unwrap().push("b");
            suspend();
            trace_for_main.lock().unwrap().push("c");
        }));
        // First resume: runs until first suspend.
        set_current_yielder(g.yielder_ptr());
        assert!(!g.resume());
        clear_current_yielder();
        assert_eq!(*trace.lock().unwrap(), vec!["a"]);
        // Second resume: runs until second suspend.
        set_current_yielder(g.yielder_ptr());
        assert!(!g.resume());
        clear_current_yielder();
        assert_eq!(*trace.lock().unwrap(), vec!["a", "b"]);
        // Third resume: runs to completion.
        set_current_yielder(g.yielder_ptr());
        assert!(g.resume());
        clear_current_yielder();
        assert_eq!(*trace.lock().unwrap(), vec!["a", "b", "c"]);
    }

    #[test]
    fn in_goroutine_returns_true_inside_running_coroutine() {
        let observation: Arc<Mutex<Option<bool>>> = Arc::new(Mutex::new(None));
        let observation_for_main = Arc::clone(&observation);
        let mut g = Goroutine::new(Box::new(move || {
            *observation_for_main.lock().unwrap() = Some(in_goroutine());
        }));
        assert!(!in_goroutine());
        set_current_yielder(g.yielder_ptr());
        let _ = g.resume();
        clear_current_yielder();
        assert_eq!(*observation.lock().unwrap(), Some(true));
        assert!(!in_goroutine());
    }
}
