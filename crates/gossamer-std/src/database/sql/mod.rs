//! Driver-pluggable SQL database access, modelled after Go's
//! `database/sql`.
//!
//! The trait surface (`Driver`, `ConnectionImpl`, `StatementImpl`,
//! `TransactionImpl`, `RowsImpl`, `Value`, `Error`, `IsolationLevel`,
//! `Kind`, `DriverError`, `register`, `open`, `drivers`) lives in
//! `gossamer-runtime::sql` so the C-ABI shims that compiled-tier
//! code calls into can dispatch through it without depending on
//! `gossamer-std`. We re-export the whole surface here so the
//! public path `gossamer_std::database::sql::*` is unchanged.
//!
//! The high-level user-facing wrappers (`Conn`, `Stmt`, `Rows`,
//! `Row`, `Tx`, `Pool`, `migrate`, `query`) stay in `gossamer-std`
//! - they're convenience layers on top of the relocated traits.

#![forbid(unsafe_code)]

pub mod migrate;
pub mod pool;
pub mod query;

pub use gossamer_runtime::sql::{
    ConnectionImpl, Driver, DriverError, DriverErrorKind, Error, IsolationLevel, Kind,
    Notification, RowsImpl, StatementImpl, TransactionImpl, Value, drivers, register,
};
pub use pool::{Pool, PoolConfig, PooledConn};
pub use query::Select;

/// Opens a SQL connection by driver name + URL. Wraps the runtime
/// trait object in a [`Conn`] so callers get the convenience API.
pub fn open(name: &str, url: &str) -> Result<Conn, Error> {
    let inner = gossamer_runtime::sql::open(name, url)?;
    Ok(Conn { inner })
}

// --- user-facing wrappers -----------------------------------------

/// Open SQL connection.
pub struct Conn {
    inner: Box<dyn ConnectionImpl>,
}

impl std::fmt::Debug for Conn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Conn").finish_non_exhaustive()
    }
}

impl Conn {
    /// Wraps a driver-supplied [`ConnectionImpl`].
    #[must_use]
    pub fn new(inner: Box<dyn ConnectionImpl>) -> Self {
        Self { inner }
    }

    /// Prepares a statement.
    pub fn prepare(&mut self, sql: &str) -> Result<Stmt, Error> {
        Ok(Stmt {
            inner: self.inner.prepare(sql)?,
        })
    }

    /// Convenience: prepares + executes a statement, returning rows
    /// affected.
    pub fn execute(&mut self, sql: &str, params: &[Value]) -> Result<u64, Error> {
        let mut stmt = self.prepare(sql)?;
        stmt.execute(params)
    }

    /// Convenience: prepares once, then executes the same statement
    /// against every parameter row, summing the row counts.
    pub fn execute_many(&mut self, sql: &str, rows: &[&[Value]]) -> Result<u64, Error> {
        let mut stmt = self.prepare(sql)?;
        let mut total: u64 = 0;
        for row in rows {
            total = total.saturating_add(stmt.execute(row)?);
        }
        Ok(total)
    }

    /// Convenience: prepares + queries a statement.
    pub fn query(&mut self, sql: &str, params: &[Value]) -> Result<Rows, Error> {
        let mut stmt = self.prepare(sql)?;
        stmt.query(params)
    }

    /// Begins a transaction.
    pub fn begin(&mut self) -> Result<Tx, Error> {
        Ok(Tx {
            inner: self.inner.begin()?,
        })
    }

    /// Begins a transaction at the requested isolation level.
    pub fn begin_with(&mut self, iso: IsolationLevel) -> Result<Tx, Error> {
        Ok(Tx {
            inner: self.inner.begin_with(iso)?,
        })
    }

    /// Round-trips a no-op statement against the connection.
    pub fn ping(&mut self) -> Result<(), Error> {
        self.inner.ping()
    }

    /// Sets the driver's busy-timeout in milliseconds.
    pub fn set_busy_timeout(&mut self, ms: i64) -> Result<(), Error> {
        self.inner.set_busy_timeout(ms)
    }

    /// Cancels any in-flight statement on this connection.
    pub fn interrupt(&self) {
        self.inner.interrupt();
    }

    /// Bulk-loads `data` through the dialect's copy mechanism
    /// (`COPY … FROM STDIN` on `PostgreSQL`); returns rows written.
    pub fn copy_in(&mut self, sql: &str, data: &[u8]) -> Result<u64, Error> {
        self.inner.copy_in(sql, data)
    }

    /// Bulk-extracts rows through the dialect's copy mechanism
    /// (`COPY … TO STDOUT` on `PostgreSQL`); returns the raw bytes.
    pub fn copy_out(&mut self, sql: &str) -> Result<Vec<u8>, Error> {
        self.inner.copy_out(sql)
    }

    /// Subscribes this connection to notifications on `channel`.
    pub fn listen(&mut self, channel: &str) -> Result<(), Error> {
        self.inner.listen(channel)
    }

    /// Unsubscribes this connection from `channel`.
    pub fn unlisten(&mut self, channel: &str) -> Result<(), Error> {
        self.inner.unlisten(channel)
    }

    /// Returns the next pending notification, waiting up to
    /// `timeout_ms` (0 = poll without waiting).
    pub fn poll_notification(&mut self, timeout_ms: i64) -> Result<Option<Notification>, Error> {
        self.inner.poll_notification(timeout_ms)
    }

    /// Runs `execute` while honouring `ctx`. If `ctx` is already
    /// cancelled, returns [`Error::Cancelled`] immediately. While the
    /// statement runs, a watchdog goroutine listens on `ctx.done()`
    /// and calls [`Self::interrupt`] if the context is cancelled
    /// before the call returns.
    pub fn execute_ctx(
        &mut self,
        ctx: &crate::context::Context,
        sql: &str,
        params: &[Value],
    ) -> Result<u64, Error> {
        if ctx.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let watchdog = InterruptWatchdog::install(ctx, &mut self.inner);
        let result = self.execute(sql, params);
        watchdog.disarm();
        if ctx.is_cancelled() {
            return Err(Error::Cancelled);
        }
        result
    }

    /// Same as [`Self::execute_ctx`] for queries.
    pub fn query_ctx(
        &mut self,
        ctx: &crate::context::Context,
        sql: &str,
        params: &[Value],
    ) -> Result<Rows, Error> {
        if ctx.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let watchdog = InterruptWatchdog::install(ctx, &mut self.inner);
        let result = self.query(sql, params);
        watchdog.disarm();
        if ctx.is_cancelled() {
            return Err(Error::Cancelled);
        }
        result
    }

    /// Closes the connection.
    pub fn close(mut self) -> Result<(), Error> {
        self.inner.close()
    }

    /// The raw driver connection, for the runtime-level helpers
    /// (`sql_migrate`, the C-ABI shims).
    pub fn as_impl_mut(&mut self) -> &mut dyn ConnectionImpl {
        self.inner.as_mut()
    }
}

/// Helper that arms a context watchdog: spawns a worker thread that
/// polls `ctx.is_cancelled()` and calls the connection's
/// `interrupt()` if cancellation fires before [`Self::disarm`] is
/// called.
///
/// `Conn::interrupt` requires only a `&ConnectionImpl` (not `&mut`),
/// which is why we can capture an `Arc<*const dyn ConnectionImpl>`
/// in the watchdog without conflicting with the running statement's
/// `&mut` borrow. `SQLite`'s `sqlite3_interrupt` is thread-safe with
/// respect to the connection.
struct InterruptWatchdog {
    armed: std::sync::Arc<std::sync::atomic::AtomicBool>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl InterruptWatchdog {
    fn install(ctx: &crate::context::Context, conn: &mut Box<dyn ConnectionImpl>) -> Self {
        let armed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let armed_clone = std::sync::Arc::clone(&armed);
        let ctx = ctx.clone();
        // Spawn the watchdog through the runtime's safe helper so we
        // do not need an unsafe block inside gossamer-std (which
        // carries `#![forbid(unsafe_code)]`). The runtime owns the
        // pointer reconstruction; gossamer-std only passes the
        // cancellation signal.
        let interrupt_fn: Box<dyn Fn() + Send + 'static> = {
            let conn_addr =
                std::ptr::from_mut::<dyn ConnectionImpl>(conn.as_mut()).cast::<()>() as usize;
            Box::new(move || gossamer_runtime::sql::interrupt_connection_by_addr(conn_addr))
        };
        let join = std::thread::Builder::new()
            .name("gos-sql-interrupt-watchdog".into())
            .spawn(move || {
                while armed_clone.load(std::sync::atomic::Ordering::Acquire) {
                    if ctx.is_cancelled() {
                        interrupt_fn();
                        return;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
            })
            .ok();
        InterruptWatchdog { armed, join }
    }

    fn disarm(mut self) {
        self.armed
            .store(false, std::sync::atomic::Ordering::Release);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

/// Active transaction.
pub struct Tx {
    inner: Box<dyn TransactionImpl>,
}

impl std::fmt::Debug for Tx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tx").finish_non_exhaustive()
    }
}

impl Tx {
    /// Commits the transaction.
    pub fn commit(mut self) -> Result<(), Error> {
        self.inner.commit()
    }
    /// Rolls back the transaction.
    pub fn rollback(mut self) -> Result<(), Error> {
        self.inner.rollback()
    }
    /// Executes a parameterless statement inside the tx.
    pub fn execute(&mut self, sql: &str) -> Result<u64, Error> {
        self.inner.execute(sql)
    }
    /// Executes a statement with positional bindings inside the tx.
    pub fn execute_params(&mut self, sql: &str, params: &[Value]) -> Result<u64, Error> {
        self.inner.execute_params(sql, params)
    }
    /// Runs a query with positional bindings inside the tx.
    pub fn query_params(&mut self, sql: &str, params: &[Value]) -> Result<Rows, Error> {
        Ok(Rows {
            inner: self.inner.query_params(sql, params)?,
        })
    }
    /// Establishes a savepoint named `name` inside this transaction.
    pub fn savepoint(&mut self, name: &str) -> Result<(), Error> {
        self.inner.savepoint(name)
    }
    /// Releases the savepoint named `name` (commits it).
    pub fn release_savepoint(&mut self, name: &str) -> Result<(), Error> {
        self.inner.release_savepoint(name)
    }
    /// Rolls back to the savepoint named `name`.
    pub fn rollback_to_savepoint(&mut self, name: &str) -> Result<(), Error> {
        self.inner.rollback_to_savepoint(name)
    }
}

/// Prepared statement handle.
pub struct Stmt {
    inner: Box<dyn StatementImpl>,
}

impl Stmt {
    /// Executes the statement, returning rows affected.
    pub fn execute(&mut self, params: &[Value]) -> Result<u64, Error> {
        self.inner.execute(params)
    }

    /// Runs the statement, yielding rows.
    pub fn query(&mut self, params: &[Value]) -> Result<Rows, Error> {
        Ok(Rows {
            inner: self.inner.query(params)?,
        })
    }
}

/// Result-set iterator.
pub struct Rows {
    inner: Box<dyn RowsImpl>,
}

impl Rows {
    /// Pulls the next row.
    pub fn next_row(&mut self) -> Result<Option<Row>, Error> {
        Ok(self.inner.next_row()?.map(|values| Row {
            values,
            columns: self.inner.columns().to_vec(),
        }))
    }

    /// Column names in declaration order.
    #[must_use]
    pub fn columns(&self) -> &[String] {
        self.inner.columns()
    }
}

/// One result row.
#[derive(Debug, Clone)]
pub struct Row {
    /// Values in column order.
    pub values: Vec<Value>,
    /// Column names.
    pub columns: Vec<String>,
}

impl Row {
    /// Looks up a value by column name.
    #[must_use]
    pub fn get(&self, column: &str) -> Option<&Value> {
        self.columns
            .iter()
            .position(|c| c == column)
            .map(|i| &self.values[i])
    }

    /// Number of columns in the row.
    #[must_use]
    pub fn width(&self) -> usize {
        self.values.len()
    }

    /// Casts a column to `i64`.
    pub fn get_i64(&self, column: &str) -> Result<i64, Error> {
        match self.get(column) {
            Some(Value::Int(n)) => Ok(*n),
            Some(other) => Err(Error::Type(format!("column {column}: {other:?}"))),
            None => Err(Error::Type(format!("column {column}: missing"))),
        }
    }

    /// Casts a column to `&str`.
    pub fn get_text(&self, column: &str) -> Result<&str, Error> {
        match self.get(column) {
            Some(Value::Text(s)) => Ok(s),
            Some(other) => Err(Error::Type(format!("column {column}: {other:?}"))),
            None => Err(Error::Type(format!("column {column}: missing"))),
        }
    }
}
