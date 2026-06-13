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
//! The pooling core (bounded capacity, idle/lifetime eviction,
//! acquire timeout) lives in `gossamer_runtime::sql_pool` so the
//! compiled tiers' C-ABI shims and the interpreter share one
//! implementation; this module adds the `Conn` convenience wrapper
//! and the per-connection prepared-statement LRU bookkeeping.

#![forbid(unsafe_code)]

use std::collections::{HashSet, VecDeque};

pub use gossamer_runtime::sql_pool::PoolConfig;

use super::{Conn, Error, IsolationLevel, Value};

/// Connection pool. Cheap to clone — the inner state is reference
/// counted, so multiple goroutines / threads can share the same pool.
#[derive(Clone)]
pub struct Pool {
    inner: gossamer_runtime::sql_pool::Pool,
}

impl Pool {
    /// Builds a new pool against `driver` (a registered driver name)
    /// and `url`, eagerly opening [`PoolConfig::min`] connections.
    pub fn new(driver: &str, url: &str, config: PoolConfig) -> Result<Self, Error> {
        Ok(Self {
            inner: gossamer_runtime::sql_pool::Pool::new(driver, url, config)?,
        })
    }

    /// Eagerly opens connections up to [`PoolConfig::min`].
    pub fn fill(&self) -> Result<(), Error> {
        self.inner.fill()
    }

    /// Acquires a connection, blocking up to
    /// [`PoolConfig::acquire_timeout`].
    pub fn get(&self) -> Result<PooledConn, Error> {
        let checkout = self.inner.get()?;
        Ok(PooledConn {
            conn: Conn::new(Box::new(checkout)),
            cache: HashSet::new(),
            lru: VecDeque::new(),
            cache_capacity: self.inner.statement_cache_capacity(),
        })
    }

    /// Number of currently live connections (idle + in-flight).
    #[must_use]
    pub fn live(&self) -> usize {
        self.inner.live()
    }

    /// Number of idle (returned) connections.
    #[must_use]
    pub fn idle(&self) -> usize {
        self.inner.idle()
    }

    /// Forces a close of all idle connections. Live in-flight
    /// connections are returned to the pool on drop and closed
    /// lazily on next eviction.
    pub fn close_idle(&self) {
        self.inner.close_idle();
    }
}

/// Connection borrowed from a [`Pool`]. Wraps the checkout in the
/// [`Conn`] convenience API; dropping it returns the underlying
/// connection to the pool.
pub struct PooledConn {
    conn: Conn,
    /// Set of prepared statements keyed on the raw SQL text.
    cache: HashSet<String>,
    /// LRU eviction order (least-recently-used at front).
    lru: VecDeque<String>,
    cache_capacity: usize,
}

impl std::fmt::Debug for PooledConn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PooledConn").finish_non_exhaustive()
    }
}

impl PooledConn {
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

    /// Prepare + execute on the underlying connection. Records the
    /// SQL in the per-connection prepared-statement LRU cache so
    /// repeated prepares are cheap.
    pub fn execute(&mut self, sql: &str, params: &[Value]) -> Result<u64, Error> {
        self.record_prepare(sql);
        self.conn.execute(sql, params)
    }

    /// Prepare + query on the underlying connection. Records in the
    /// statement cache.
    pub fn query(&mut self, sql: &str, params: &[Value]) -> Result<super::Rows, Error> {
        self.record_prepare(sql);
        self.conn.query(sql, params)
    }

    /// Begin a transaction.
    pub fn begin(&mut self) -> Result<super::Tx, Error> {
        self.conn.begin()
    }

    /// Begin a transaction at an explicit isolation level.
    pub fn begin_with(&mut self, iso: IsolationLevel) -> Result<super::Tx, Error> {
        self.conn.begin_with(iso)
    }

    /// Prepare a statement directly (skips the cache record because
    /// the caller is keeping the statement handle).
    pub fn prepare(&mut self, sql: &str) -> Result<super::Stmt, Error> {
        self.conn.prepare(sql)
    }

    /// Ping the underlying connection.
    pub fn ping(&mut self) -> Result<(), Error> {
        self.conn.ping()
    }

    /// Number of cached prepared-statement SQL strings.
    #[must_use]
    pub fn cached_statements(&self) -> usize {
        self.cache.len()
    }
}
