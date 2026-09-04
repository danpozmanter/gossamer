//! Connection pool for `std::database::sql`, shared by every tier.
//!
//! The pool operates on the raw [`ConnectionImpl`] trait objects so
//! the C-ABI shims in [`crate::c_abi::sql`] (compiled tiers) and the
//! interpreter builtins can drive it directly; `gossamer-std`
//! re-wraps checkouts in its `Conn` convenience type for the Rust
//! façade.
//!
//! A bounded semaphore (capacity = `max`), a
//! `parking_lot::Mutex`-protected `VecDeque` of idle connections, and
//! idle/lifetime eviction. Checkouts implement [`ConnectionImpl`] by
//! delegation and return themselves to the pool on drop.

#![forbid(unsafe_code)]

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::{Condvar, Mutex};

use crate::platform::Instant;
use crate::sql::{
    ConnectionImpl, Error, IsolationLevel, Notification, StatementImpl, TransactionImpl,
};

/// Tunable configuration for [`Pool`].
#[derive(Debug, Clone)]
pub struct PoolConfig {
    /// Minimum idle connections to keep warm. Default 0 (lazy).
    pub min: usize,
    /// Maximum total connections. Default 8.
    pub max: usize,
    /// Drop an idle connection if it has been unused for this long.
    /// `None` disables idle eviction. Default 5 minutes.
    pub idle_timeout: Option<Duration>,
    /// Drop a connection after this total lifetime regardless of
    /// activity. `None` disables. Default 30 minutes.
    pub max_lifetime: Option<Duration>,
    /// How long [`Pool::get`] waits for a free connection before
    /// returning [`Error::PoolExhausted`]. Default 30 seconds.
    pub acquire_timeout: Duration,
    /// Capacity of the per-connection prepared-statement LRU cache
    /// maintained by the `gossamer-std` façade. `0` disables caching.
    /// Default 64.
    pub statement_cache: usize,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            min: 0,
            max: 8,
            #[allow(
                clippy::duration_suboptimal_units,
                reason = "Duration::from_mins is unstable in 1.95"
            )]
            idle_timeout: Some(Duration::from_secs(60 * 5)),
            #[allow(
                clippy::duration_suboptimal_units,
                reason = "Duration::from_mins is unstable in 1.95"
            )]
            max_lifetime: Some(Duration::from_secs(60 * 30)),
            acquire_timeout: Duration::from_secs(30),
            statement_cache: 64,
        }
    }
}

impl PoolConfig {
    /// Builder: max connections.
    #[must_use]
    pub fn with_max(mut self, max: usize) -> Self {
        self.max = max;
        self
    }
    /// Builder: min idle connections.
    #[must_use]
    pub fn with_min(mut self, min: usize) -> Self {
        self.min = min;
        self
    }
    /// Builder: idle timeout.
    #[must_use]
    pub fn with_idle_timeout(mut self, t: Option<Duration>) -> Self {
        self.idle_timeout = t;
        self
    }
    /// Builder: max lifetime.
    #[must_use]
    pub fn with_max_lifetime(mut self, t: Option<Duration>) -> Self {
        self.max_lifetime = t;
        self
    }
    /// Builder: acquire timeout.
    #[must_use]
    pub fn with_acquire_timeout(mut self, t: Duration) -> Self {
        self.acquire_timeout = t;
        self
    }
    /// Builder: per-connection statement cache size.
    #[must_use]
    pub fn with_statement_cache(mut self, n: usize) -> Self {
        self.statement_cache = n;
        self
    }
}

struct IdleConn {
    conn: Box<dyn ConnectionImpl>,
    created: Instant,
    last_used: Instant,
}

struct PoolState {
    driver: String,
    url: String,
    config: PoolConfig,
    idle: VecDeque<IdleConn>,
    /// Total live connections (idle + in-flight).
    live: usize,
}

/// Connection pool. Cheap to clone - the inner state is reference
/// counted, so multiple goroutines / threads can share the same pool.
#[derive(Clone)]
pub struct Pool {
    state: Arc<Mutex<PoolState>>,
    cv: Arc<Condvar>,
}

impl Pool {
    /// Builds a new pool against `driver` (a registered driver name)
    /// and `url`, eagerly opening [`PoolConfig::min`] connections.
    pub fn new(driver: &str, url: &str, config: PoolConfig) -> Result<Self, Error> {
        if config.max == 0 {
            return Err(Error::driver("pool", "max must be > 0"));
        }
        if config.min > config.max {
            return Err(Error::driver(
                "pool",
                format!("min ({}) cannot exceed max ({})", config.min, config.max),
            ));
        }
        let pool = Self {
            state: Arc::new(Mutex::new(PoolState {
                driver: driver.to_string(),
                url: url.to_string(),
                config,
                idle: VecDeque::new(),
                live: 0,
            })),
            cv: Arc::new(Condvar::new()),
        };
        pool.fill()?;
        Ok(pool)
    }

    /// Eagerly opens connections up to [`PoolConfig::min`].
    pub fn fill(&self) -> Result<(), Error> {
        loop {
            // Reserve a slot under the lock, but open the driver connection
            // after releasing it. A networked driver's `open` may block for
            // DNS or a handshake, neither of which should stall pool state
            // inspection or a returning checkout.
            let (driver, url) = {
                let mut state = self.state.lock();
                if state.live >= state.config.min {
                    return Ok(());
                }
                state.live += 1;
                (state.driver.clone(), state.url.clone())
            };
            let conn = match crate::sql::open(&driver, &url) {
                Ok(conn) => conn,
                Err(error) => {
                    let mut state = self.state.lock();
                    state.live -= 1;
                    self.cv.notify_all();
                    return Err(error);
                }
            };
            let now = Instant::now();
            let mut state = self.state.lock();
            state.idle.push_back(IdleConn {
                conn,
                created: now,
                last_used: now,
            });
        }
    }

    /// Acquires a connection, blocking up to
    /// [`PoolConfig::acquire_timeout`].
    pub fn get(&self) -> Result<PooledConn, Error> {
        let deadline = Instant::now() + self.state.lock().config.acquire_timeout;
        let mut state = self.state.lock();
        loop {
            // Reuse an idle connection if available. Evicted
            // connections drop outside the lock so a blocking driver
            // Drop cannot stall unrelated checkouts.
            let mut evicted: Vec<IdleConn> = Vec::new();
            let reused = loop {
                let Some(inner) = state.idle.pop_front() else {
                    break None;
                };
                let now = Instant::now();
                let expired = state
                    .config
                    .max_lifetime
                    .is_some_and(|t| now.duration_since(inner.created) > t)
                    || state
                        .config
                        .idle_timeout
                        .is_some_and(|t| now.duration_since(inner.last_used) > t);
                if expired {
                    state.live -= 1;
                    evicted.push(inner);
                    continue;
                }
                break Some(inner);
            };
            if let Some(inner) = reused {
                drop(state);
                drop(evicted);
                return Ok(PooledConn {
                    pool: self.clone(),
                    conn: Some(inner.conn),
                    created: inner.created,
                });
            }
            if !evicted.is_empty() {
                // Capacity opened up; wake waiters after dropping the
                // evicted connections outside the lock.
                drop(state);
                drop(evicted);
                self.cv.notify_all();
                state = self.state.lock();
                continue;
            }
            // Open a new connection if under cap.
            if state.live < state.config.max {
                state.live += 1;
                let driver = state.driver.clone();
                let url = state.url.clone();
                drop(state);
                let conn = crate::sql::open(&driver, &url).inspect_err(|_| {
                    // Roll back the live count on failure so the
                    // pool doesn't permanently lose a slot.
                    let mut s = self.state.lock();
                    s.live -= 1;
                    self.cv.notify_all();
                })?;
                return Ok(PooledConn {
                    pool: self.clone(),
                    conn: Some(conn),
                    created: Instant::now(),
                });
            }
            // Wait for a return.
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                let cap = state.config.max;
                return Err(Error::PoolExhausted { capacity: cap });
            }
            if self.cv.wait_for(&mut state, remaining).timed_out() {
                let cap = state.config.max;
                return Err(Error::PoolExhausted { capacity: cap });
            }
        }
    }

    /// Number of currently live connections (idle + in-flight).
    #[must_use]
    pub fn live(&self) -> usize {
        self.state.lock().live
    }

    /// Number of idle (returned) connections.
    #[must_use]
    pub fn idle(&self) -> usize {
        self.state.lock().idle.len()
    }

    /// Configured statement-cache capacity (consumed by the
    /// `gossamer-std` façade's per-connection LRU bookkeeping).
    #[must_use]
    pub fn statement_cache_capacity(&self) -> usize {
        self.state.lock().config.statement_cache
    }

    /// Forces a close of all idle connections. Live in-flight
    /// connections are returned to the pool on drop and closed
    /// lazily on next eviction.
    pub fn close_idle(&self) {
        let drained: Vec<IdleConn> = {
            let mut state = self.state.lock();
            let n = state.idle.len();
            state.live = state.live.saturating_sub(n);
            state.idle.drain(..).collect()
        };
        // Driver drops run outside the lock.
        drop(drained);
        self.cv.notify_all();
    }
}

/// Connection borrowed from a [`Pool`]. Implements
/// [`ConnectionImpl`] by delegation and returns itself to the pool
/// on drop.
pub struct PooledConn {
    pool: Pool,
    conn: Option<Box<dyn ConnectionImpl>>,
    created: Instant,
}

impl std::fmt::Debug for PooledConn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PooledConn").finish_non_exhaustive()
    }
}

impl PooledConn {
    fn inner(&mut self) -> Result<&mut dyn ConnectionImpl, Error> {
        match self.conn.as_mut() {
            Some(c) => Ok(c.as_mut()),
            None => Err(Error::Closed),
        }
    }
}

impl ConnectionImpl for PooledConn {
    fn prepare(&mut self, sql: &str) -> Result<Box<dyn StatementImpl>, Error> {
        self.inner()?.prepare(sql)
    }
    fn begin(&mut self) -> Result<Box<dyn TransactionImpl>, Error> {
        self.inner()?.begin()
    }
    fn begin_with(&mut self, iso: IsolationLevel) -> Result<Box<dyn TransactionImpl>, Error> {
        self.inner()?.begin_with(iso)
    }
    fn ping(&mut self) -> Result<(), Error> {
        self.inner()?.ping()
    }
    fn set_busy_timeout(&mut self, ms: i64) -> Result<(), Error> {
        self.inner()?.set_busy_timeout(ms)
    }
    fn interrupt(&self) {
        if let Some(c) = self.conn.as_ref() {
            c.interrupt();
        }
    }
    fn copy_in(&mut self, sql: &str, data: &[u8]) -> Result<u64, Error> {
        self.inner()?.copy_in(sql, data)
    }
    fn copy_out(&mut self, sql: &str) -> Result<Vec<u8>, Error> {
        self.inner()?.copy_out(sql)
    }
    fn listen(&mut self, channel: &str) -> Result<(), Error> {
        self.inner()?.listen(channel)
    }
    fn unlisten(&mut self, channel: &str) -> Result<(), Error> {
        self.inner()?.unlisten(channel)
    }
    fn poll_notification(&mut self, timeout_ms: i64) -> Result<Option<Notification>, Error> {
        self.inner()?.poll_notification(timeout_ms)
    }
    /// "Closing" a pooled checkout returns it to the pool rather
    /// than tearing down the underlying connection.
    fn close(&mut self) -> Result<(), Error> {
        if let Some(conn) = self.conn.take() {
            return_to_pool(&self.pool, conn, self.created);
        }
        Ok(())
    }
}

fn return_to_pool(pool: &Pool, conn: Box<dyn ConnectionImpl>, created: Instant) {
    let mut state = pool.state.lock();
    state.idle.push_back(IdleConn {
        conn,
        created,
        last_used: Instant::now(),
    });
    pool.cv.notify_one();
}

impl Drop for PooledConn {
    fn drop(&mut self) {
        if let Some(conn) = self.conn.take() {
            return_to_pool(&self.pool, conn, self.created);
        }
    }
}
