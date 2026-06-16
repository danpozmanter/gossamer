//! Application-state container for HTTP handlers.
//!
//! A `TypeMap` (see `AppState`) keyed by `TypeId` that stores at most one value
//! per Rust type. Handlers reach shared dependencies (database pools,
//! configuration, template caches, metrics sinks, etc.) by looking them up
//! through this container rather than threading every dependency through
//! closure captures.
//!
//! # Design
//!
//! - Values are stored as `Arc<dyn Any + Send + Sync>`. Retrieval downcasts
//!   the trait object back to the concrete type and clones the `Arc`, so
//!   callers receive `Arc<T>` and can hold a reference for as long as they
//!   need without blocking writers.
//! - The map itself lives behind `Arc<RwLock<...>>`. Cloning `AppState`
//!   is O(1) and every clone observes the same set of values.
//! - All operations take a read or write lock, perform the lookup, and
//!   release the guard before returning - the lock is never held across a
//!   call into user code.
//!
//! # Thread safety
//!
//! `AppState` is `Send + Sync`. Goroutines (and OS threads) may share a
//! single instance freely; the entries themselves must be `Send + Sync +
//! 'static` so they can also cross those boundaries.

#![forbid(unsafe_code)]

use std::any::{Any, TypeId, type_name};
use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::RwLock;

/// Type-erased application-state container.
///
/// Stores at most one value per Rust type. Cloning is O(1) - the underlying
/// map is shared via `Arc<RwLock<...>>`, so all clones observe the same set
/// of values. Entries must be `Send + Sync + 'static` so they can cross
/// goroutine boundaries.
#[derive(Clone, Default)]
pub struct AppState {
    inner: Arc<RwLock<HashMap<TypeId, Arc<dyn Any + Send + Sync>>>>,
}

impl AppState {
    /// Constructs an empty container.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Inserts (or replaces) the value of type `T`. Returns `self` for
    /// builder chaining.
    #[must_use]
    pub fn insert<T: Any + Send + Sync + 'static>(self, value: T) -> Self {
        self.put(value);
        self
    }

    /// Inserts (or replaces) the value of type `T` through a shared
    /// reference. Useful when the container is already held behind a clone
    /// rather than being built up fluently.
    pub fn put<T: Any + Send + Sync + 'static>(&self, value: T) {
        let mut guard = self.inner.write();
        guard.insert(TypeId::of::<T>(), Arc::new(value));
    }

    /// Returns the stored value of type `T`, if any. The returned `Arc`
    /// is cheap to clone and may be held independently by many goroutines.
    #[must_use]
    pub fn get<T: Any + Send + Sync + 'static>(&self) -> Option<Arc<T>> {
        let guard = self.inner.read();
        let entry = guard.get(&TypeId::of::<T>())?.clone();
        drop(guard);
        // Downcast cannot fail: the TypeId key uniquely identifies T.
        entry.downcast::<T>().ok()
    }

    /// Like [`get`](Self::get) but returns an error naming the missing
    /// type instead of `None`.
    pub fn require<T: Any + Send + Sync + 'static>(&self) -> Result<Arc<T>, crate::errors::Error> {
        self.get::<T>().ok_or_else(|| {
            crate::errors::Error::new(format!(
                "app state missing value of type `{}`",
                type_name::<T>()
            ))
        })
    }

    /// Returns `true` if a value of type `T` has been inserted.
    #[must_use]
    pub fn contains_t<T: Any + Send + Sync + 'static>(&self) -> bool {
        let guard = self.inner.read();
        guard.contains_key(&TypeId::of::<T>())
    }

    /// Removes the value of type `T`, returning it if present.
    #[must_use]
    pub fn remove<T: Any + Send + Sync + 'static>(&self) -> Option<Arc<T>> {
        let mut guard = self.inner.write();
        let entry = guard.remove(&TypeId::of::<T>())?;
        drop(guard);
        entry.downcast::<T>().ok()
    }

    /// Returns the number of distinct types currently stored.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.read().len()
    }

    /// Returns `true` if the container holds no values.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.read().is_empty()
    }

    /// Removes every entry.
    pub fn clear(&self) {
        self.inner.write().clear();
    }
}

/// Wires `state` into `router`'s middleware chain so every handler
/// registered against the router sees the same shared [`AppState`].
///
/// Handlers retrieve typed values through `State::from_request` -
/// the request object carries the `AppState` reference under a
/// stable extension slot.
///
/// ```text
/// let mut router = http_router::Router::new();
/// let mut state = AppState::new();
/// state.insert(Arc::new(MyDb::open()?));
/// http_state::attach_to_router(&mut router, state);
/// router.get("/users/:id", get_user);
///
/// fn get_user(req: http::Request) -> Result<http::Response, http::Error> {
///     let db: State<MyDb> = State::from_request(&req)?;
///     // ... use `db.query(...)`
///     Ok(http::Response::text(200, "ok"))
/// }
/// ```
pub fn attach_to_router(router: &mut crate::http_router::Router, state: AppState) {
    router.set_state(state);
}

/// Typed extractor wrapper.
///
/// Handlers receive their state values through this wrapper. The
/// underlying value lives behind an `Arc<T>` so the extractor never
/// blocks writers and the handler can hold the reference for as
/// long as it needs.
#[derive(Debug)]
pub struct State<T>(pub Arc<T>);

impl<T: Any + Send + Sync + 'static> State<T> {
    /// Pulls the value of type `T` out of `state`, returning an error if it
    /// has not been inserted.
    pub fn from_app_state(state: &AppState) -> Result<Self, crate::errors::Error> {
        state.require::<T>().map(State)
    }

    /// Pulls the value of type `T` out of `router`'s attached
    /// [`AppState`]. Returns an error if no `AppState` has been
    /// wired into the router (see
    /// [`crate::http_state::attach_to_router`]) or if no value of
    /// type `T` has been inserted.
    pub fn from_router(router: &crate::http_router::Router) -> Result<Self, crate::errors::Error> {
        let state = router.state().ok_or_else(|| {
            crate::errors::Error::new("http_state: AppState not attached to router")
        })?;
        Self::from_app_state(state)
    }
}

impl<T> std::ops::Deref for State<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T> Clone for State<T> {
    fn clone(&self) -> Self {
        State(Arc::clone(&self.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq, Eq)]
    struct Config {
        host: String,
        port: u16,
    }

    #[derive(Debug, PartialEq, Eq)]
    struct Metrics {
        counter: u64,
    }

    #[test]
    fn insert_and_get_i64_round_trip() {
        let state = AppState::new().insert(42_i64);
        let value = state.get::<i64>().expect("i64 should be present");
        assert_eq!(*value, 42);
    }

    #[test]
    fn insert_and_get_custom_struct_round_trip() {
        let cfg = Config {
            host: "localhost".into(),
            port: 8080,
        };
        let state = AppState::new().insert(cfg);
        let got = state.get::<Config>().expect("Config should be present");
        assert_eq!(got.host, "localhost");
        assert_eq!(got.port, 8080);
    }

    #[test]
    fn get_missing_type_returns_none() {
        let state = AppState::new();
        assert!(state.get::<i64>().is_none());
    }

    #[test]
    fn require_missing_type_returns_err_with_type_name() {
        let state = AppState::new();
        let err = state.require::<Config>().unwrap_err();
        let msg = err.message();
        assert!(msg.contains("Config"), "error message was: {msg}");
        assert!(msg.contains("missing"), "error message was: {msg}");
    }

    #[test]
    fn multiple_types_coexist() {
        let state = AppState::new()
            .insert(7_i64)
            .insert(Config {
                host: "h".into(),
                port: 1,
            })
            .insert(Metrics { counter: 99 });

        assert_eq!(*state.get::<i64>().unwrap(), 7);
        assert_eq!(state.get::<Config>().unwrap().port, 1);
        assert_eq!(state.get::<Metrics>().unwrap().counter, 99);
        assert_eq!(state.len(), 3);
    }

    #[test]
    fn inserting_same_type_twice_replaces_previous() {
        let state = AppState::new().insert(1_i64);
        state.put(2_i64);
        assert_eq!(*state.get::<i64>().unwrap(), 2);
        assert_eq!(state.len(), 1);
    }

    #[test]
    fn remove_returns_value_and_clears_entry() {
        let state = AppState::new().insert(Metrics { counter: 5 });
        let removed = state.remove::<Metrics>().expect("entry should exist");
        assert_eq!(removed.counter, 5);
        assert!(state.get::<Metrics>().is_none());
        assert!(state.remove::<Metrics>().is_none());
    }

    #[test]
    fn contains_t_before_and_after_insert() {
        let state = AppState::new();
        assert!(!state.contains_t::<Config>());
        state.put(Config {
            host: "x".into(),
            port: 9,
        });
        assert!(state.contains_t::<Config>());
        let removed = state.remove::<Config>();
        assert!(removed.is_some());
        assert!(!state.contains_t::<Config>());
    }

    #[test]
    fn clone_shares_state() {
        let a = AppState::new();
        let b = a.clone();
        a.put(123_i64);
        assert_eq!(*b.get::<i64>().expect("clone should observe insert"), 123);
        b.put(456_i64);
        assert_eq!(*a.get::<i64>().unwrap(), 456);
    }

    #[test]
    fn clear_drops_all_entries() {
        let state = AppState::new().insert(1_i64).insert(Metrics { counter: 1 });
        assert_eq!(state.len(), 2);
        state.clear();
        assert!(state.is_empty());
        assert!(state.get::<i64>().is_none());
    }

    #[test]
    fn is_empty_reflects_contents() {
        let state = AppState::new();
        assert!(state.is_empty());
        state.put(1_i64);
        assert!(!state.is_empty());
    }

    #[test]
    fn state_extractor_deref_and_clone() {
        let app = AppState::new().insert(Metrics { counter: 17 });
        let extracted: State<Metrics> = State::from_app_state(&app).unwrap();
        assert_eq!(extracted.counter, 17);
        let clone = extracted.clone();
        assert_eq!(clone.counter, 17);
        assert!(Arc::ptr_eq(&extracted.0, &clone.0));
    }

    #[test]
    fn state_extractor_propagates_missing_error() {
        let app = AppState::new();
        let err = State::<Config>::from_app_state(&app).unwrap_err();
        assert!(err.message().contains("Config"));
    }

    #[test]
    fn concurrent_reads_from_many_threads() {
        let state = AppState::new().insert(Metrics { counter: 4242 });
        let mut handles = Vec::with_capacity(8);
        for _ in 0..8 {
            let s = state.clone();
            handles.push(std::thread::spawn(move || {
                for _ in 0..1000 {
                    let v = s.get::<Metrics>().expect("present");
                    assert_eq!(v.counter, 4242);
                }
            }));
        }
        for h in handles {
            h.join().expect("worker thread should not panic");
        }
    }

    #[test]
    fn concurrent_mixed_read_write_does_not_deadlock() {
        let state = AppState::new().insert(0_i64);
        let mut handles = Vec::with_capacity(4);
        for i in 0..4_i64 {
            let s = state.clone();
            handles.push(std::thread::spawn(move || {
                for _ in 0..200 {
                    s.put(i);
                    let _ = s.get::<i64>();
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert!(state.get::<i64>().is_some());
    }
}
