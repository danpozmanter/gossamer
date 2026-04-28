//! Driver-pluggable SQL database access, modelled after Go's
//! `database/sql`.
//!
//! Programs work against this façade and bring in the appropriate
//! driver crate; today the bundled driver is SQLite (gated on the
//! `ffi-sqlite` feature). The driver registers itself via [`register`]
//! at start-up; user code calls [`open`] with a name + url and gets
//! back a [`Conn`].

#![forbid(unsafe_code)]
#![allow(clippy::doc_markdown, clippy::missing_errors_doc)]

use std::sync::Arc;

use parking_lot::Mutex;
use thiserror::Error;

#[cfg(feature = "ffi-sqlite")]
pub mod sqlite;

/// Statically typed value passed to / returned from the database.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// `NULL` literal.
    Null,
    /// Boolean (sqlite stores 0/1).
    Bool(bool),
    /// 64-bit signed integer.
    Int(i64),
    /// 64-bit floating-point.
    Float(f64),
    /// UTF-8 text.
    Text(String),
    /// Binary blob.
    Blob(Vec<u8>),
}

/// Errors raised by drivers and the façade.
#[derive(Debug, Clone, Error)]
pub enum Error {
    /// Driver name was not registered.
    #[error("sql: no driver registered as {0:?}")]
    UnknownDriver(String),
    /// Driver-specific failure.
    #[error("sql: driver {driver}: {message}")]
    Driver {
        /// Driver identifier (e.g. `"sqlite"`).
        driver: String,
        /// Lower-level message.
        message: String,
    },
    /// Caller asked for the wrong column type.
    #[error("sql: column type mismatch: {0}")]
    Type(String),
    /// Connection has been closed.
    #[error("sql: connection closed")]
    Closed,
}

/// Driver trait — concrete drivers implement [`open`] and return a
/// [`Conn`] backed by their own state. Drivers are registered with
/// [`register`].
pub trait Driver: Send + Sync {
    /// Driver name (for [`open`]).
    fn name(&self) -> &str;
    /// Opens a connection to the database identified by `url`.
    fn open(&self, url: &str) -> Result<Conn, Error>;
}

/// Connection trait. The wrapped trait object is what user code
/// drives.
pub trait ConnectionImpl: Send {
    /// Prepares a statement for repeated execution.
    fn prepare(&mut self, sql: &str) -> Result<Box<dyn StatementImpl>, Error>;
    /// Begins a transaction.
    fn begin(&mut self) -> Result<Box<dyn TransactionImpl>, Error>;
    /// Closes the connection. Subsequent calls return [`Error::Closed`].
    fn close(&mut self) -> Result<(), Error>;
}

/// Prepared statement trait.
pub trait StatementImpl: Send {
    /// Executes the statement with positional bindings; returns the
    /// number of rows affected.
    fn execute(&mut self, params: &[Value]) -> Result<u64, Error>;
    /// Runs the statement and returns rows.
    fn query(&mut self, params: &[Value]) -> Result<Box<dyn RowsImpl>, Error>;
}

/// Transaction trait.
pub trait TransactionImpl: Send {
    /// Commits the transaction.
    fn commit(&mut self) -> Result<(), Error>;
    /// Rolls back.
    fn rollback(&mut self) -> Result<(), Error>;
    /// Executes raw SQL inside the transaction (no parameters).
    fn execute(&mut self, sql: &str) -> Result<u64, Error>;
}

/// Rows trait — iterate result sets.
pub trait RowsImpl: Send {
    /// Pulls the next row, or `None` on end-of-set.
    fn next_row(&mut self) -> Result<Option<Vec<Value>>, Error>;
    /// Column names in the result set.
    fn columns(&self) -> &[String];
}

// --- registry -----------------------------------------------------

static REGISTRY: Mutex<Vec<Arc<dyn Driver>>> = Mutex::new(Vec::new());

/// Registers a driver so [`open`] can find it. Idempotent on driver
/// name — re-registering replaces the previous handle.
pub fn register(driver: Arc<dyn Driver>) {
    let mut reg = REGISTRY.lock();
    let name = driver.name().to_string();
    reg.retain(|d| d.name() != name);
    reg.push(driver);
}

/// Looks up a driver and opens a connection.
pub fn open(name: &str, url: &str) -> Result<Conn, Error> {
    let reg = REGISTRY.lock();
    for driver in reg.iter() {
        if driver.name() == name {
            let driver = Arc::clone(driver);
            drop(reg);
            return driver.open(url);
        }
    }
    Err(Error::UnknownDriver(name.to_string()))
}

// --- user-facing wrappers -----------------------------------------

/// Open SQL connection.
pub struct Conn {
    inner: Box<dyn ConnectionImpl>,
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

    /// Closes the connection.
    pub fn close(mut self) -> Result<(), Error> {
        self.inner.close()
    }
}

/// Active transaction.
pub struct Tx {
    inner: Box<dyn TransactionImpl>,
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

    /// Number of columns in the row. (Useful for round-tripping
    /// row width across the dyn-trait boundary.)
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
