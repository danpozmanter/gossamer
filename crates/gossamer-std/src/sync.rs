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

use gossamer_runtime::race;
use parking_lot::{Condvar, Mutex as PMutex, Once as POnce, RwLock as PRwLock};

/// Mutual-exclusion lock.
///
/// On contention from a goroutine, parks the calling goroutine on
/// the lock's wait queue and lets the worker thread run other
/// goroutines. From a non-goroutine OS thread the lock falls back
/// to `parking_lot::Mutex::lock`. Acquisition is unfair — no
/// queue ordering guarantee.
#[derive(Debug, Default)]
pub struct Mutex<T: ?Sized> {
    last_unlocker: StdAtomicI64,
    /// Parked goroutine ids waiting to acquire. The releaser pops
    /// the head and unparks it.
    waiters: PMutex<std::collections::VecDeque<gossamer_runtime::sched::Gid>>,
    inner: PMutex<T>,
}

impl<T> Mutex<T> {
    /// Creates a new mutex protecting `value`.
    #[must_use]
    pub const fn new(value: T) -> Self {
        Self {
            last_unlocker: StdAtomicI64::new(-1),
            waiters: PMutex::new(std::collections::VecDeque::new()),
            inner: PMutex::new(value),
        }
    }

    /// Acquires the lock for the duration of `f`. Goroutines park
    /// on contention; OS threads fall back to `parking_lot`'s OS
    /// park. Never panics on poisoning.
    ///
    /// Bookends `f` with `race::record_sync` calls so the race
    /// detector observes the happens-before edge from the previous
    /// unlocker to the current acquirer; on exit it publishes the
    /// current goroutine as the new unlocker.
    pub fn with<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        loop {
            if let Some(mut guard) = self.inner.try_lock() {
                let from = self.last_unlocker.load(Ordering::Acquire);
                if from >= 0 {
                    race::record_sync(u32::try_from(from).unwrap_or(0), race::current_gid());
                }
                let result = f(&mut guard);
                self.last_unlocker
                    .store(i64::from(race::current_gid()), Ordering::Release);
                drop(guard);
                if let Some(gid) = self.waiters.lock().pop_front() {
                    gossamer_runtime::sched_global::scheduler().unpark(gid);
                }
                return result;
            }
            if !gossamer_coro::in_goroutine() {
                // OS-thread fallback. Block on the inner mutex —
                // no goroutine concurrency to preserve here.
                let mut guard = self.inner.lock();
                let from = self.last_unlocker.load(Ordering::Acquire);
                if from >= 0 {
                    race::record_sync(u32::try_from(from).unwrap_or(0), race::current_gid());
                }
                let result = f(&mut guard);
                self.last_unlocker
                    .store(i64::from(race::current_gid()), Ordering::Release);
                drop(guard);
                if let Some(gid) = self.waiters.lock().pop_front() {
                    gossamer_runtime::sched_global::scheduler().unpark(gid);
                }
                return result;
            }
            gossamer_runtime::sched_global::park(
                gossamer_runtime::sched::ParkReason::Sync,
                |parker| {
                    self.waiters.lock().push_back(parker.gid);
                    // Wake-before-park race close: re-check inside
                    // arm. If the lock is now free, self-unpark via
                    // pre_unpark.
                    if !self.inner.is_locked() {
                        gossamer_runtime::sched_global::scheduler().unpark(parker.gid);
                    }
                },
            );
            if let Some(gid) = gossamer_runtime::sched_global::current_gid() {
                self.waiters.lock().retain(|g| *g != gid);
            }
        }
    }

    /// Attempts to acquire the lock without blocking. Returns
    /// `Some(result)` when the lock was free, otherwise `None`.
    pub fn try_with<R>(&self, f: impl FnOnce(&mut T) -> R) -> Option<R> {
        let mut guard = self.inner.try_lock()?;
        let from = self.last_unlocker.load(Ordering::Acquire);
        if from >= 0 {
            race::record_sync(u32::try_from(from).unwrap_or(0), race::current_gid());
        }
        let result = f(&mut guard);
        self.last_unlocker
            .store(i64::from(race::current_gid()), Ordering::Release);
        Some(result)
    }
}

/// Backwards-compatible alias for [`Mutex<T>`].
///
/// Earlier releases shipped a separate "goroutine-aware" mutex that
/// spin-then-`yield_now`'d on contention. With true goroutines, the
/// regular [`Mutex<T>`] now parks the calling coroutine on
/// contention via the M:N scheduler — exactly the semantics
/// `GoMutex<T>` was approximating. The two types are identical.
pub type GoMutex<T> = Mutex<T>;

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

/// Memory ordering selector for the relaxed-ordering API. The
/// SeqCst-only methods (`load`, `store`, `fetch_add`,
/// `compare_and_swap`) remain the safe default; the `*_ordered`
/// methods accept this enum so lock-free code can opt into the
/// cheaper Acquire/Release/Relaxed orderings on architectures
/// where `SeqCst` is materially more expensive (ARM64, RISC-V).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AtomicOrdering {
    /// Lowest cost. No happens-before guarantees beyond atomicity.
    Relaxed,
    /// Pair with [`AtomicOrdering::Release`] on the producer side.
    Acquire,
    /// Pair with [`AtomicOrdering::Acquire`] on the consumer side.
    Release,
    /// Both Acquire and Release semantics on RMW operations.
    AcqRel,
    /// Strongest. The default for the parameter-less methods.
    SeqCst,
}

impl AtomicOrdering {
    fn to_std(self) -> Ordering {
        match self {
            Self::Relaxed => Ordering::Relaxed,
            Self::Acquire => Ordering::Acquire,
            Self::Release => Ordering::Release,
            Self::AcqRel => Ordering::AcqRel,
            Self::SeqCst => Ordering::SeqCst,
        }
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

    /// Load with caller-supplied ordering. `Release` and `AcqRel`
    /// are illegal on a load and are silently promoted to `SeqCst`.
    #[must_use]
    pub fn load_ordered(&self, order: AtomicOrdering) -> i64 {
        let std_order = match order {
            AtomicOrdering::Release | AtomicOrdering::AcqRel => Ordering::SeqCst,
            other => other.to_std(),
        };
        self.inner.load(std_order)
    }

    /// Store with caller-supplied ordering. `Acquire` and `AcqRel`
    /// are illegal on a store and are silently promoted to `SeqCst`.
    pub fn store_ordered(&self, value: i64, order: AtomicOrdering) {
        let std_order = match order {
            AtomicOrdering::Acquire | AtomicOrdering::AcqRel => Ordering::SeqCst,
            other => other.to_std(),
        };
        self.inner.store(value, std_order);
    }

    /// `fetch_add` with caller-supplied ordering.
    pub fn fetch_add_ordered(&self, delta: i64, order: AtomicOrdering) -> i64 {
        self.inner.fetch_add(delta, order.to_std())
    }

    /// `compare_exchange` with caller-supplied success / failure
    /// orderings. Returns `Ok(prev)` on success, `Err(actual)` when
    /// the observed value did not match `current`. Failure ordering
    /// is automatically downgraded if it would be illegal.
    pub fn compare_exchange(
        &self,
        current: i64,
        new: i64,
        success: AtomicOrdering,
        failure: AtomicOrdering,
    ) -> Result<i64, i64> {
        let s = success.to_std();
        let f = match failure {
            AtomicOrdering::Release | AtomicOrdering::AcqRel => Ordering::Acquire,
            other => other.to_std(),
        };
        self.inner.compare_exchange(current, new, s, f)
    }

    /// Atomic exchange. Returns the previous value.
    pub fn swap_ordered(&self, value: i64, order: AtomicOrdering) -> i64 {
        self.inner.swap(value, order.to_std())
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
///
/// Misuse (decrement below zero, increment above [`i64::MAX`]) is
/// surfaced through [`Self::try_add`] / [`Self::try_done`] as a
/// [`WgError`] rather than a panic-in-lock, so callers can recover
/// without deadlocking the program. The legacy [`Self::add`] /
/// [`Self::done`] entry points keep their panicking shape but
/// release the lock before unwinding.
#[derive(Debug)]
pub struct WaitGroup {
    state: PMutex<i64>,
    cv: Condvar,
    /// Goroutines parked on `wait()` for the counter to reach zero.
    /// `add(n)` unparks every gid in this list when the counter
    /// transitions to zero; OS-thread waiters use the condvar.
    waiters: PMutex<Vec<gossamer_runtime::sched::Gid>>,
}

/// Misuse outcome reported by [`WaitGroup::try_add`] /
/// [`WaitGroup::try_done`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WgError {
    /// `done()` was called more times than `add()` granted.
    Underflow,
    /// `add(n)` would push the counter past [`i64::MAX`].
    Overflow,
}

impl std::fmt::Display for WgError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WgError::Underflow => write!(f, "WaitGroup counter went negative"),
            WgError::Overflow => write!(f, "WaitGroup counter overflow"),
        }
    }
}

impl std::error::Error for WgError {}

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
            waiters: PMutex::new(Vec::new()),
        }
    }

    /// Fallible counter adjust. Returns the new counter value on
    /// success, or [`WgError`] on misuse — never panics, never
    /// holds the lock past the unwind.
    pub fn try_add(&self, n: i64) -> Result<i64, WgError> {
        let mut count = self.state.lock();
        let next = count.checked_add(n).ok_or(WgError::Overflow)?;
        if next < 0 {
            return Err(WgError::Underflow);
        }
        *count = next;
        let reached_zero = next == 0;
        drop(count);
        if reached_zero {
            // Unpark every parked goroutine waiter, then notify
            // any condvar-blocked OS thread waiter.
            let parked: Vec<_> = self.waiters.lock().drain(..).collect();
            if !parked.is_empty() {
                let sched = gossamer_runtime::sched_global::scheduler();
                for gid in parked {
                    sched.unpark(gid);
                }
            }
            self.cv.notify_all();
        }
        Ok(next)
    }

    /// Fallible decrement. Returns the new counter value on success.
    pub fn try_done(&self) -> Result<i64, WgError> {
        self.try_add(-1)
    }

    /// Increments the pending count by `n`. `n` may be negative —
    /// matching Go's semantics — but bringing the count below zero
    /// panics, since that signals a programming error. The lock is
    /// released before the panic unwinds.
    pub fn add(&self, n: i64) {
        match self.try_add(n) {
            Ok(_) => {}
            Err(WgError::Underflow) => panic!("WaitGroup counter went negative"),
            Err(WgError::Overflow) => panic!("WaitGroup counter overflow"),
        }
    }

    /// Decrements the pending count by one. Equivalent to `add(-1)`.
    pub fn done(&self) {
        self.add(-1);
    }

    /// Blocks until the pending count reaches zero. Goroutines park
    /// (their worker thread is freed for other goroutines); OS-thread
    /// callers fall back to a condvar wait. No spinning.
    pub fn wait(&self) {
        loop {
            {
                let count = self.state.lock();
                if *count == 0 {
                    return;
                }
            }
            if gossamer_coro::in_goroutine() {
                gossamer_runtime::sched_global::park(
                    gossamer_runtime::sched::ParkReason::Sync,
                    |parker| {
                        // Push our gid first, then re-check the
                        // counter under the same lock to close the
                        // wake-before-park race window.
                        self.waiters.lock().push(parker.gid);
                        let count = self.state.lock();
                        if *count == 0 {
                            // The notifier ran in the gap between
                            // our last check and waiter
                            // registration. Self-unpark via
                            // pre_unpark so the suspend exits
                            // immediately.
                            gossamer_runtime::sched_global::scheduler().unpark(parker.gid);
                        }
                    },
                );
                if let Some(gid) = gossamer_runtime::sched_global::current_gid() {
                    self.waiters.lock().retain(|g| *g != gid);
                }
            } else {
                let mut count = self.state.lock();
                while *count > 0 {
                    self.cv.wait(&mut count);
                }
                return;
            }
        }
    }

    /// Snapshots the pending count.
    #[must_use]
    pub fn pending(&self) -> i64 {
        *self.state.lock()
    }
}

/// Cross-goroutine-safe vector of `i64` slots. Mirrors Go's
/// `sync.Map` for the narrow case of a numeric slot table that
/// many goroutines push to or update concurrently. Every
/// operation acquires an internal `parking_lot::Mutex<Vec<i64>>`
/// briefly. Use this in place of bare `Vec<i64>` whenever the
/// vec is captured into a `go` closure or sent through a channel.
#[derive(Debug, Default)]
pub struct SyncIntVec {
    inner: PMutex<Vec<i64>>,
}

impl SyncIntVec {
    /// Empty vec.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: PMutex::new(Vec::new()),
        }
    }

    /// Vec of `len` zero slots.
    #[must_use]
    pub fn with_len(len: usize) -> Self {
        Self {
            inner: PMutex::new(vec![0i64; len]),
        }
    }

    /// Current length.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.lock().len()
    }

    /// `true` when the vec has zero elements.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.lock().is_empty()
    }

    /// Reads slot `idx`. Returns `0` for out-of-bounds, matching
    /// the runtime helper's saturating read.
    #[must_use]
    pub fn get(&self, idx: usize) -> i64 {
        self.inner.lock().get(idx).copied().unwrap_or(0)
    }

    /// Writes slot `idx`. No-op for out-of-bounds.
    pub fn set(&self, idx: usize, value: i64) {
        let mut g = self.inner.lock();
        if let Some(slot) = g.get_mut(idx) {
            *slot = value;
        }
    }

    /// Appends a new slot.
    pub fn push(&self, value: i64) {
        self.inner.lock().push(value);
    }

    /// Atomic increment of slot `idx` by `delta`. Returns the new
    /// value. Equivalent to a brief lock; `0` on out-of-bounds.
    pub fn add(&self, idx: usize, delta: i64) -> i64 {
        let mut g = self.inner.lock();
        if let Some(slot) = g.get_mut(idx) {
            *slot = slot.wrapping_add(delta);
            *slot
        } else {
            0
        }
    }

    /// Snapshots the current contents.
    #[must_use]
    pub fn snapshot(&self) -> Vec<i64> {
        self.inner.lock().clone()
    }
}

/// Cross-goroutine-safe vector of `u8` bytes. Same shape as
/// [`SyncIntVec`] but with byte slots — for shared output
/// buffers, ring buffers, etc. Mutating the underlying vec
/// concurrently across goroutines via a bare `Vec<u8>` is
/// undefined; use this wrapper instead.
#[derive(Debug, Default)]
pub struct SyncByteVec {
    inner: PMutex<Vec<u8>>,
}

impl SyncByteVec {
    /// Empty byte vec.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: PMutex::new(Vec::new()),
        }
    }

    /// Vec of `len` zero bytes.
    #[must_use]
    pub fn with_len(len: usize) -> Self {
        Self {
            inner: PMutex::new(vec![0u8; len]),
        }
    }

    /// Current length in bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.lock().len()
    }

    /// `true` when the vec has no bytes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.lock().is_empty()
    }

    /// Reads byte `idx`. Returns `0` for out-of-bounds.
    #[must_use]
    pub fn get(&self, idx: usize) -> u8 {
        self.inner.lock().get(idx).copied().unwrap_or(0)
    }

    /// Writes byte `idx`. No-op for out-of-bounds.
    pub fn set(&self, idx: usize, value: u8) {
        let mut g = self.inner.lock();
        if let Some(slot) = g.get_mut(idx) {
            *slot = value;
        }
    }

    /// Appends a single byte.
    pub fn push(&self, value: u8) {
        self.inner.lock().push(value);
    }

    /// Append the given byte slice in one locked operation.
    pub fn extend_from_slice(&self, bytes: &[u8]) {
        self.inner.lock().extend_from_slice(bytes);
    }

    /// Snapshots the current contents.
    #[must_use]
    pub fn snapshot(&self) -> Vec<u8> {
        self.inner.lock().clone()
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
    fn wait_group_try_done_returns_underflow_without_panic() {
        let wg = WaitGroup::new();
        assert!(matches!(wg.try_done(), Err(WgError::Underflow)));
        // Lock must still be reusable — proves the failure released
        // it cleanly.
        wg.add(1);
        assert_eq!(wg.try_done(), Ok(0));
    }

    #[test]
    fn wait_group_try_add_returns_overflow_without_panic() {
        let wg = WaitGroup::new();
        wg.add(i64::MAX);
        assert!(matches!(wg.try_add(1), Err(WgError::Overflow)));
        assert_eq!(wg.pending(), i64::MAX);
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
    fn atomic_compare_exchange_round_trips() {
        let a = AtomicI64::new(7);
        assert_eq!(
            a.compare_exchange(7, 9, AtomicOrdering::SeqCst, AtomicOrdering::Relaxed),
            Ok(7)
        );
        assert_eq!(a.load(), 9);
        assert_eq!(
            a.compare_exchange(7, 11, AtomicOrdering::AcqRel, AtomicOrdering::Acquire),
            Err(9)
        );
        assert_eq!(a.load(), 9);
    }

    #[test]
    fn go_mutex_serialises_concurrent_increments() {
        let m = Arc::new(GoMutex::new(0i64));
        let mut handles = Vec::new();
        for _ in 0..16 {
            let m = Arc::clone(&m);
            handles.push(thread::spawn(move || {
                for _ in 0..1000 {
                    m.with(|v| *v += 1);
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        m.with(|v| assert_eq!(*v, 16 * 1000));
    }

    #[test]
    fn go_mutex_try_with_returns_none_when_held() {
        let m = Arc::new(GoMutex::new(0));
        let m2 = Arc::clone(&m);
        let g = m.inner.lock();
        assert!(m2.try_with(|x| *x + 1).is_none());
        drop(g);
        assert_eq!(m.try_with(|x| *x + 1), Some(1));
    }

    #[test]
    fn atomic_swap_returns_previous() {
        let a = AtomicI64::new(0);
        assert_eq!(a.swap_ordered(42, AtomicOrdering::AcqRel), 0);
        assert_eq!(a.load_ordered(AtomicOrdering::Acquire), 42);
    }

    #[test]
    fn sync_int_vec_handles_concurrent_pushes() {
        let v = Arc::new(SyncIntVec::with_len(0));
        let mut handles = Vec::new();
        for t in 0..8 {
            let v = Arc::clone(&v);
            handles.push(thread::spawn(move || {
                for i in 0..1000 {
                    v.push(t * 1000 + i);
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(v.len(), 8 * 1000);
    }

    #[test]
    fn sync_int_vec_add_is_atomic_under_contention() {
        let v = Arc::new(SyncIntVec::with_len(1));
        let mut handles = Vec::new();
        for _ in 0..16 {
            let v = Arc::clone(&v);
            handles.push(thread::spawn(move || {
                for _ in 0..1000 {
                    v.add(0, 1);
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(v.get(0), 16 * 1000);
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
