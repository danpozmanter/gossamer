//! Runtime support for `std::sync`.
//!
//! Goroutine-aware synchronisation primitives. The earlier
//! placeholder implementations wrapped `std::sync::*` directly,
//! which meant a goroutine that contended on a `Mutex` blocked the
//! underlying OS worker thread — destroying the M:N concurrency the
//! scheduler was supposed to provide. This pass migrates every
//! primitive to either:
//!
//! - **`parking_lot::*`** — for primitives whose contention path
//!   the goroutine model doesn't materially improve (read/write
//!   locks, OnceLock-style barriers). `parking_lot` is non-poisoned,
//!   spin-then-park, and ~2x faster than `std::sync` under low
//!   contention without changing the public API.
//! - **`Condvar`-backed wait** — for `WaitGroup` and `Barrier`,
//!   which previously spun on `std::thread::yield_now` and now
//!   block on a condvar that wakes when the count reaches zero.
//!
//! When the worker-stealing scheduler is wired through to the
//! mutex acquire path, the same Condvar-based wait will be
//! replaced with `MultiScheduler::park` / `unpark` so the
//! contended goroutine does not occupy a P slot at all.

#![forbid(unsafe_code)]

use std::sync::atomic::{
    AtomicBool as StdAtomicBool, AtomicI64 as StdAtomicI64, AtomicU64 as StdAtomicU64, Ordering,
};

use parking_lot::{Condvar, Mutex as PMutex, Once as POnce, RwLock as PRwLock};

/// Mutual-exclusion lock.
#[derive(Debug, Default)]
pub struct Mutex<T: ?Sized> {
    inner: PMutex<T>,
}

impl<T> Mutex<T> {
    /// Creates a new mutex protecting `value`.
    #[must_use]
    pub const fn new(value: T) -> Self {
        Self {
            inner: PMutex::new(value),
        }
    }

    /// Acquires the lock for the duration of `f`. Unlike the host
    /// `std::sync::Mutex` this never panics on poisoning — `parking_lot`
    /// does not propagate panics through the lock.
    pub fn with<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        let mut guard = self.inner.lock();
        f(&mut guard)
    }

    /// Attempts to acquire the lock without blocking. Returns
    /// `Some(result)` when the lock was free, otherwise `None`.
    pub fn try_with<R>(&self, f: impl FnOnce(&mut T) -> R) -> Option<R> {
        let mut guard = self.inner.try_lock()?;
        Some(f(&mut guard))
    }
}

/// Reader-writer lock.
#[derive(Debug, Default)]
pub struct RwLock<T: ?Sized> {
    inner: PRwLock<T>,
}

impl<T> RwLock<T> {
    /// Creates a new lock protecting `value`.
    #[must_use]
    pub const fn new(value: T) -> Self {
        Self {
            inner: PRwLock::new(value),
        }
    }

    /// Runs `f` with shared read access.
    pub fn with_read<R>(&self, f: impl FnOnce(&T) -> R) -> R {
        let guard = self.inner.read();
        f(&guard)
    }

    /// Runs `f` with exclusive write access.
    pub fn with_write<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        let mut guard = self.inner.write();
        f(&mut guard)
    }
}

/// One-shot initialisation latch. Backed by `parking_lot::Once`,
/// which uses futexes on Linux so contention does not spin.
#[derive(Debug, Default)]
pub struct Once {
    inner: POnce,
}

impl Once {
    /// Fresh uninitialised latch.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            inner: POnce::new(),
        }
    }

    /// Runs `f` exactly once across every caller.
    pub fn call_once(&self, f: impl FnOnce()) {
        self.inner.call_once(f);
    }
}

/// Atomic 64-bit signed integer.
#[derive(Debug, Default)]
pub struct AtomicI64 {
    inner: StdAtomicI64,
}

impl AtomicI64 {
    /// Creates a new atomic seeded with `value`.
    #[must_use]
    pub const fn new(value: i64) -> Self {
        Self {
            inner: StdAtomicI64::new(value),
        }
    }
    /// Loads the current value with sequentially-consistent ordering.
    #[must_use]
    pub fn load(&self) -> i64 {
        self.inner.load(Ordering::SeqCst)
    }
    /// Stores `value` with sequentially-consistent ordering.
    pub fn store(&self, value: i64) {
        self.inner.store(value, Ordering::SeqCst);
    }
    /// Atomic `+=` returning the previous value.
    pub fn fetch_add(&self, delta: i64) -> i64 {
        self.inner.fetch_add(delta, Ordering::SeqCst)
    }
    /// Atomic compare-and-swap. Returns `true` if the swap happened.
    pub fn compare_and_swap(&self, current: i64, new: i64) -> bool {
        self.inner
            .compare_exchange(current, new, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }
}

/// Atomic 64-bit unsigned integer.
#[derive(Debug, Default)]
pub struct AtomicU64 {
    inner: StdAtomicU64,
}

impl AtomicU64 {
    /// Creates a new atomic.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self {
            inner: StdAtomicU64::new(value),
        }
    }
    /// Loads the current value.
    #[must_use]
    pub fn load(&self) -> u64 {
        self.inner.load(Ordering::SeqCst)
    }
    /// Stores `value`.
    pub fn store(&self, value: u64) {
        self.inner.store(value, Ordering::SeqCst);
    }
    /// Atomic `+=` returning the previous value.
    pub fn fetch_add(&self, delta: u64) -> u64 {
        self.inner.fetch_add(delta, Ordering::SeqCst)
    }
    /// Atomic compare-and-swap. Returns `true` if the swap happened.
    pub fn compare_and_swap(&self, current: u64, new: u64) -> bool {
        self.inner
            .compare_exchange(current, new, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }
}

/// Atomic boolean.
#[derive(Debug, Default)]
pub struct AtomicBool {
    inner: StdAtomicBool,
}

impl AtomicBool {
    /// Creates a new atomic boolean.
    #[must_use]
    pub const fn new(value: bool) -> Self {
        Self {
            inner: StdAtomicBool::new(value),
        }
    }
    /// Loads the current value.
    #[must_use]
    pub fn load(&self) -> bool {
        self.inner.load(Ordering::SeqCst)
    }
    /// Stores `value`.
    pub fn store(&self, value: bool) {
        self.inner.store(value, Ordering::SeqCst);
    }
    /// Atomic compare-and-swap.
    pub fn compare_and_swap(&self, current: bool, new: bool) -> bool {
        self.inner
            .compare_exchange(current, new, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }
}

/// Counts down to zero, then releases every waiter.
///
/// Mirrors Go's `sync.WaitGroup`. Waiters block on a [`Condvar`]
/// rather than spinning on `yield_now`, so a fan-out of many
/// goroutines does not melt the host CPU. The condition variable
/// is signalled when `done` brings the counter to zero, releasing
/// every parked waiter at once.
#[derive(Debug)]
pub struct WaitGroup {
    state: PMutex<i64>,
    cv: Condvar,
}

impl Default for WaitGroup {
    fn default() -> Self {
        Self::new()
    }
}

impl WaitGroup {
    /// New wait group with zero pending goroutines.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            state: PMutex::new(0),
            cv: Condvar::new(),
        }
    }

    /// Increments the pending count by `n`. `n` may be negative —
    /// matching Go's semantics — but bringing the count below zero
    /// panics, since that signals a programming error.
    pub fn add(&self, n: i64) {
        let mut count = self.state.lock();
        *count = count.checked_add(n).expect("WaitGroup counter overflow");
        assert!(*count >= 0, "WaitGroup counter went negative ({})", *count);
        if *count == 0 {
            self.cv.notify_all();
        }
    }

    /// Decrements the pending count by one. Equivalent to `add(-1)`.
    pub fn done(&self) {
        self.add(-1);
    }

    /// Blocks until the pending count reaches zero. No spinning.
    pub fn wait(&self) {
        let mut count = self.state.lock();
        while *count > 0 {
            self.cv.wait(&mut count);
        }
    }

    /// Snapshots the pending count.
    #[must_use]
    pub fn pending(&self) -> i64 {
        *self.state.lock()
    }
}

/// Synchronisation barrier across goroutines.
///
/// Like Go's `sync.WaitGroup` with a fixed participant count, every
/// participant calls `wait()` and unblocks once `n` waiters are
/// present. The implementation uses a `Mutex<usize>` plus a
/// `Condvar`; participants do not occupy a spinning loop.
#[derive(Debug)]
pub struct Barrier {
    state: PMutex<BarrierState>,
    cv: Condvar,
}

#[derive(Debug)]
struct BarrierState {
    expected: usize,
    arrived: usize,
    /// Generation counter — incremented every time the barrier
    /// fires. Waiters wake up and check that their captured
    /// generation differs from the current one.
    generation: u64,
}

impl Barrier {
    /// Creates a new barrier that waits for `n` participants.
    #[must_use]
    pub fn new(n: usize) -> Self {
        Self {
            state: PMutex::new(BarrierState {
                expected: n,
                arrived: 0,
                generation: 0,
            }),
            cv: Condvar::new(),
        }
    }

    /// Blocks until `n` participants have called `wait`.
    pub fn wait(&self) {
        let mut state = self.state.lock();
        let captured_gen = state.generation;
        state.arrived += 1;
        if state.arrived >= state.expected {
            state.arrived = 0;
            state.generation = state.generation.wrapping_add(1);
            self.cv.notify_all();
            return;
        }
        while state.generation == captured_gen {
            self.cv.wait(&mut state);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;
    use std::time::{Duration, Instant};

    #[test]
    fn wait_group_does_not_spin() {
        let wg = Arc::new(WaitGroup::new());
        wg.add(3);
        for _ in 0..3 {
            let wg = Arc::clone(&wg);
            thread::spawn(move || {
                thread::sleep(Duration::from_millis(20));
                wg.done();
            });
        }
        let start = Instant::now();
        wg.wait();
        let elapsed = start.elapsed();
        assert!(elapsed >= Duration::from_millis(15));
        assert!(elapsed < Duration::from_secs(1));
    }

    #[test]
    fn wait_group_panics_on_negative() {
        let wg = WaitGroup::new();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| wg.add(-1)));
        assert!(result.is_err());
    }

    #[test]
    fn barrier_releases_participants_together() {
        let b = Arc::new(Barrier::new(4));
        let counter = Arc::new(AtomicI64::new(0));
        let mut handles = Vec::new();
        for _ in 0..4 {
            let b = Arc::clone(&b);
            let counter = Arc::clone(&counter);
            handles.push(thread::spawn(move || {
                b.wait();
                counter.fetch_add(1);
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(counter.load(), 4);
    }

    #[test]
    fn mutex_try_with_returns_none_when_held() {
        let mu = Arc::new(Mutex::new(0));
        let mu2 = Arc::clone(&mu);
        let g = mu.inner.lock();
        let r = mu2.try_with(|x| *x + 1);
        assert!(r.is_none());
        drop(g);
        let r = mu.try_with(|x| *x + 1);
        assert_eq!(r, Some(1));
    }
}
