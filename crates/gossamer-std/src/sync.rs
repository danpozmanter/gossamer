//! Runtime support for `std::sync`.
//! Thin wrappers around the host stdlib primitives, giving them
//! Gossamer-facing names (`Mutex`, `RwLock`, `Once`, `WaitGroup`,
//! `Barrier`, plus atomic integer/boolean types). Future phases will
//! swap these for scheduler-aware variants that park goroutines
//! instead of OS threads; the observable API stays the same.

#![forbid(unsafe_code)]

use std::sync::atomic::{
    AtomicBool as StdAtomicBool, AtomicI64 as StdAtomicI64, AtomicU64 as StdAtomicU64, Ordering,
};
use std::sync::{Mutex as StdMutex, Once as StdOnce, RwLock as StdRwLock};

/// Mutual-exclusion lock.
#[derive(Debug, Default)]
pub struct Mutex<T: ?Sized> {
    inner: StdMutex<T>,
}

impl<T> Mutex<T> {
    /// Creates a new mutex protecting `value`.
    #[must_use]
    pub const fn new(value: T) -> Self {
        Self {
            inner: StdMutex::new(value),
        }
    }

    /// Acquires the lock, panicking if another holder has poisoned it.
    pub fn with<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        let mut guard = self.inner.lock().expect("mutex poisoned");
        f(&mut guard)
    }
}

/// Reader-writer lock.
#[derive(Debug, Default)]
pub struct RwLock<T: ?Sized> {
    inner: StdRwLock<T>,
}

impl<T> RwLock<T> {
    /// Creates a new lock protecting `value`.
    #[must_use]
    pub const fn new(value: T) -> Self {
        Self {
            inner: StdRwLock::new(value),
        }
    }

    /// Runs `f` with shared read access.
    pub fn with_read<R>(&self, f: impl FnOnce(&T) -> R) -> R {
        let guard = self.inner.read().expect("rwlock poisoned");
        f(&guard)
    }

    /// Runs `f` with exclusive write access.
    pub fn with_write<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        let mut guard = self.inner.write().expect("rwlock poisoned");
        f(&mut guard)
    }
}

/// One-shot initialisation latch.
#[derive(Debug)]
pub struct Once {
    inner: StdOnce,
}

impl Once {
    /// Fresh uninitialised latch.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            inner: StdOnce::new(),
        }
    }

    /// Runs `f` exactly once across every caller.
    pub fn call_once(&self, f: impl FnOnce()) {
        self.inner.call_once(f);
    }
}

impl Default for Once {
    fn default() -> Self {
        Self::new()
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

/// Waits for a group of goroutines to complete.
///
/// Safe-Rust stand-in for the Go-style `WaitGroup`. The real
/// scheduler-aware implementation arrives integration.
#[derive(Debug, Default)]
pub struct WaitGroup {
    count: AtomicI64,
}

impl WaitGroup {
    /// New wait group with zero pending goroutines.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            count: AtomicI64::new(0),
        }
    }
    /// Increments the pending count by `n`.
    pub fn add(&self, n: i64) {
        self.count.fetch_add(n);
    }
    /// Decrements the pending count by one.
    pub fn done(&self) {
        self.count.fetch_add(-1);
    }
    /// Spin-waits until the pending count reaches zero.
    pub fn wait(&self) {
        while self.count.load() > 0 {
            std::thread::yield_now();
        }
    }
    /// Snapshots the pending count.
    #[must_use]
    pub fn pending(&self) -> i64 {
        self.count.load()
    }
}

/// Synchronisation barrier across goroutines.
#[derive(Debug)]
pub struct Barrier {
    inner: std::sync::Barrier,
}

impl Barrier {
    /// Creates a new barrier that waits for `n` participants.
    #[must_use]
    pub fn new(n: usize) -> Self {
        Self {
            inner: std::sync::Barrier::new(n),
        }
    }
    /// Blocks until every participant has called `wait`.
    pub fn wait(&self) {
        let _ = self.inner.wait();
    }
}
