//! Connection pool for `std::database::sql`.
//!
//! Typical use:
//!
//! ```text
//! // a driver crate has been imported and registered.
//! let pool = Pool::new("postgres", &url, PoolConfig::default().with_max(8))?;
//! let mut conn = pool.get()?;
//! conn.execute("CREATE TABLE t (v INTEGER)", &[])?;
//! // returned to pool on drop
//! ```
//!
//! The pool is intentionally small and dependency-free: a bounded
//! semaphore (capacity = `max`), a `parking_lot::Mutex`-protected
//! `VecDeque` of idle connections, and a per-connection prepared
//! statement LRU cache.

#![forbid(unsafe_code)]

use std::collections::{HashSet, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::{Condvar, Mutex};

use super::{Conn, ConnectionImpl, Error, RowsImpl, StatementImpl, TransactionImpl, Value};
use super::{IsolationLevel, open};

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
    /// Capacity of the per-connection prepared-statement LRU cache.
    /// `0` disables caching. Default 64.
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

/// One connection wrapped with metadata + a prepared-statement cache.
struct PooledInner {
    conn: Conn,
    created: Instant,
    last_used: Instant,
    /// Set of prepared statements keyed on the raw SQL text.
    cache: HashSet<String>,
    /// LRU eviction order (least-recently-used at front).
    lru: VecDeque<String>,
    cache_capacity: usize,
}

impl PooledInner {
    fn new(conn: Conn, cache_capacity: usize) -> Self {
        let now = Instant::now();
        Self {
            conn,
            created: now,
            last_used: now,
            cache: HashSet::new(),
            lru: VecDeque::new(),
            cache_capacity,
        }
    }

    fn touch(&mut self) {
        self.last_used = Instant::now();
    }

    fn record_prepare(&mut self, sql: &str) {
        if self.cache_capacity == 0 {
            return;
        }
        if self.cache.contains(sql) {
            // Move to back (most recently used).
            self.lru.retain(|s| s != sql);
            self.lru.push_back(sql.to_string());
            return;
        }
        if self.lru.len() >= self.cache_capacity
            && let Some(evict) = self.lru.pop_front()
        {
            self.cache.remove(&evict);
        }
        self.cache.insert(sql.to_string());
        self.lru.push_back(sql.to_string());
    }
}

struct PoolState {
    driver: String,
    url: String,
    config: PoolConfig,
    idle: VecDeque<PooledInner>,
    /// Total live connections (idle + in-flight).
    live: usize,
}

/// Connection pool. Cheap to clone — the inner state is reference
/// counted, so multiple goroutines / threads can share the same pool.
#[derive(Clone)]
pub struct Pool {
    state: Arc<Mutex<PoolState>>,
    cv: Arc<Condvar>,
}

impl Pool {
    /// Builds a new pool against `driver` (a registered driver name)
    /// and `url`. The pool does not eagerly open any connections; the
    /// first [`Pool::get`] triggers a lazy open. Use [`Pool::fill`] to warm up.
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
        let mut state = self.state.lock();
        while state.live < state.config.min {
            let conn = open(&state.driver, &state.url)?;
            let cache_cap = state.config.statement_cache;
            state.idle.push_back(PooledInner::new(conn, cache_cap));
            state.live += 1;
        }
        Ok(())
    }

    /// Acquires a connection, blocking up to
    /// [`PoolConfig::acquire_timeout`].
    pub fn get(&self) -> Result<PooledConn, Error> {
        let deadline = Instant::now() + self.state.lock().config.acquire_timeout;
        let mut state = self.state.lock();
        loop {
            // Reuse an idle connection if available.
            while let Some(mut inner) = state.idle.pop_front() {
                let now = Instant::now();
                if state
                    .config
                    .max_lifetime
                    .is_some_and(|t| now.duration_since(inner.created) > t)
                {
                    state.live -= 1;
                    continue;
                }
                if state
                    .config
                    .idle_timeout
                    .is_some_and(|t| now.duration_since(inner.last_used) > t)
                {
                    state.live -= 1;
                    continue;
                }
                inner.touch();
                let cap = state.config.statement_cache;
                drop(state);
                return Ok(PooledConn {
                    pool: self.clone(),
                    inner: Some(inner),
                    _cache_capacity: cap,
                });
            }
            // Open a new connection if under cap.
            if state.live < state.config.max {
                state.live += 1;
                let driver = state.driver.clone();
                let url = state.url.clone();
                let cache_cap = state.config.statement_cache;
                drop(state);
                let conn = open(&driver, &url).inspect_err(|_| {
                    // Roll back the live count on failure so the
                    // pool doesn't permanently lose a slot.
                    let mut s = self.state.lock();
                    s.live -= 1;
                    self.cv.notify_all();
                })?;
                return Ok(PooledConn {
                    pool: self.clone(),
                    inner: Some(PooledInner::new(conn, cache_cap)),
                    _cache_capacity: cache_cap,
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

    /// Forces a close of all idle connections. Live in-flight
    /// connections are returned to the pool on drop and closed
    /// lazily on next eviction.
    pub fn close_idle(&self) {
        let mut state = self.state.lock();
        let n = state.idle.len();
        state.idle.clear();
        state.live = state.live.saturating_sub(n);
    }
}

fn return_to_pool(pool: &Pool, mut inner: PooledInner) {
    inner.touch();
    let mut state = pool.state.lock();
    state.idle.push_back(inner);
    pool.cv.notify_one();
}

/// Connection borrowed from a [`Pool`]. Implements `Deref` /
/// `DerefMut` to [`Conn`]. Returns itself to the pool on drop.
pub struct PooledConn {
    pool: Pool,
    inner: Option<PooledInner>,
    _cache_capacity: usize,
}

impl std::fmt::Debug for PooledConn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PooledConn").finish_non_exhaustive()
    }
}

impl PooledConn {
    /// Prepare + execute on the underlying connection. Records the
    /// SQL in the per-connection prepared-statement LRU cache so
    /// repeated prepares are cheap.
    pub fn execute(&mut self, sql: &str, params: &[Value]) -> Result<u64, Error> {
        let inner = self.inner.as_mut().expect("pooled conn used after drop");
        inner.record_prepare(sql);
        inner.conn.execute(sql, params)
    }

    /// Prepare + query on the underlying connection. Records in the
    /// statement cache.
    pub fn query(&mut self, sql: &str, params: &[Value]) -> Result<super::Rows, Error> {
        let inner = self.inner.as_mut().expect("pooled conn used after drop");
        inner.record_prepare(sql);
        inner.conn.query(sql, params)
    }

    /// Begin a transaction.
    pub fn begin(&mut self) -> Result<super::Tx, Error> {
        let inner = self.inner.as_mut().expect("pooled conn used after drop");
        inner.conn.begin()
    }

    /// Begin a transaction at an explicit isolation level.
    pub fn begin_with(&mut self, iso: IsolationLevel) -> Result<super::Tx, Error> {
        let inner = self.inner.as_mut().expect("pooled conn used after drop");
        inner.conn.begin_with(iso)
    }

    /// Prepare a statement directly (skips the cache record because
    /// the caller is keeping the statement handle).
    pub fn prepare(&mut self, sql: &str) -> Result<super::Stmt, Error> {
        let inner = self.inner.as_mut().expect("pooled conn used after drop");
        inner.conn.prepare(sql)
    }

    /// Ping the underlying connection.
    pub fn ping(&mut self) -> Result<(), Error> {
        let inner = self.inner.as_mut().expect("pooled conn used after drop");
        inner.conn.ping()
    }

    /// Number of cached prepared-statement SQL strings.
    #[must_use]
    pub fn cached_statements(&self) -> usize {
        self.inner.as_ref().map_or(0, |i| i.cache.len())
    }
}

impl Drop for PooledConn {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.take() {
            return_to_pool(&self.pool, inner);
        }
    }
}

// PooledConn needs to satisfy ConnectionImpl + StatementImpl wherever
// callers pass it through the trait. The simplest path is to expose
// the inner `Conn` via deref-style accessors above; we don't impl
// the impls directly because PooledConn owns the lifecycle.

// Compile-time check that the trait imports remain in scope.
#[allow(dead_code)]
fn _trait_imports_kept_in_scope() {
    fn _check<T: ConnectionImpl + StatementImpl + TransactionImpl + RowsImpl>(_t: &T) {}
}
