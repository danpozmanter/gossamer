//! SQLite driver. Wraps `rusqlite` (which links to the bundled
//! `libsqlite3` C library — the FFI seam this driver demonstrates).
//! Registered automatically when the `ffi-sqlite` feature is on.

#![forbid(unsafe_code)]
#![allow(
    clippy::doc_markdown,
    clippy::must_use_candidate,
    clippy::elidable_lifetime_names,
    clippy::needless_pass_by_value,
    clippy::let_underscore_untyped,
    clippy::ref_option,
    clippy::trivially_copy_pass_by_ref
)]

use std::sync::Arc;

use parking_lot::Mutex;
use rusqlite::types::{Value as SqlValue, ValueRef};
use rusqlite::{Connection, Statement, ToSql, Transaction};

use super::{ConnectionImpl, Driver, Error, RowsImpl, StatementImpl, TransactionImpl, Value};

/// SQLite driver entry. Pass `:memory:` for an in-memory database, or
/// a filesystem path for a persistent one.
#[derive(Debug, Default)]
pub struct SqliteDriver;

impl SqliteDriver {
    /// Convenience constructor.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Driver for SqliteDriver {
    fn name(&self) -> &'static str {
        "sqlite"
    }

    fn open(&self, url: &str) -> Result<super::Conn, Error> {
        let conn = if url == ":memory:" {
            Connection::open_in_memory()
        } else {
            Connection::open(url)
        }
        .map_err(|e| Error::Driver {
            driver: "sqlite".into(),
            message: e.to_string(),
        })?;
        Ok(super::Conn::new(Box::new(SqliteConn {
            inner: Arc::new(Mutex::new(conn)),
        })))
    }
}

/// Registers [`SqliteDriver`] in the global driver registry. Idempotent.
pub fn register() {
    super::register(Arc::new(SqliteDriver::new()));
}

struct SqliteConn {
    inner: Arc<Mutex<Connection>>,
}

impl ConnectionImpl for SqliteConn {
    fn prepare(&mut self, sql: &str) -> Result<Box<dyn StatementImpl>, Error> {
        let conn = Arc::clone(&self.inner);
        let sql = sql.to_string();
        Ok(Box::new(SqliteStmt {
            conn,
            sql,
            columns_cache: Vec::new(),
        }))
    }

    fn begin(&mut self) -> Result<Box<dyn TransactionImpl>, Error> {
        let conn = Arc::clone(&self.inner);
        conn.lock().execute_batch("BEGIN").map_err(map_err)?;
        Ok(Box::new(SqliteTx {
            conn,
            finished: false,
        }))
    }

    fn close(&mut self) -> Result<(), Error> {
        Ok(())
    }
}

struct SqliteStmt {
    conn: Arc<Mutex<Connection>>,
    sql: String,
    columns_cache: Vec<String>,
}

impl StatementImpl for SqliteStmt {
    fn execute(&mut self, params: &[Value]) -> Result<u64, Error> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(&self.sql).map_err(map_err)?;
        let bindings: Vec<rusqlite::types::Value> = params.iter().map(value_to_sql).collect();
        let count = stmt
            .execute(rusqlite::params_from_iter(bindings.iter()))
            .map_err(map_err)?;
        Ok(count as u64)
    }

    fn query(&mut self, params: &[Value]) -> Result<Box<dyn RowsImpl>, Error> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(&self.sql).map_err(map_err)?;
        let names: Vec<String> = stmt
            .column_names()
            .into_iter()
            .map(str::to_string)
            .collect();
        self.columns_cache.clone_from(&names);
        let bindings: Vec<rusqlite::types::Value> = params.iter().map(value_to_sql).collect();
        let mut rows = stmt
            .query(rusqlite::params_from_iter(bindings.iter()))
            .map_err(map_err)?;
        let mut materialised = Vec::new();
        while let Some(row) = rows.next().map_err(map_err)? {
            let mut tuple = Vec::with_capacity(names.len());
            for i in 0..names.len() {
                let v: ValueRef = row.get_ref(i).map_err(map_err)?;
                tuple.push(value_from_ref(v));
            }
            materialised.push(tuple);
        }
        Ok(Box::new(SqliteRows {
            cursor: 0,
            rows: materialised,
            columns: names,
        }))
    }
}

struct SqliteRows {
    cursor: usize,
    rows: Vec<Vec<Value>>,
    columns: Vec<String>,
}

impl RowsImpl for SqliteRows {
    fn next_row(&mut self) -> Result<Option<Vec<Value>>, Error> {
        if self.cursor >= self.rows.len() {
            return Ok(None);
        }
        let row = self.rows[self.cursor].clone();
        self.cursor += 1;
        Ok(Some(row))
    }
    fn columns(&self) -> &[String] {
        &self.columns
    }
}

struct SqliteTx {
    conn: Arc<Mutex<Connection>>,
    finished: bool,
}

impl TransactionImpl for SqliteTx {
    fn commit(&mut self) -> Result<(), Error> {
        if self.finished {
            return Ok(());
        }
        self.finished = true;
        self.conn.lock().execute_batch("COMMIT").map_err(map_err)
    }
    fn rollback(&mut self) -> Result<(), Error> {
        if self.finished {
            return Ok(());
        }
        self.finished = true;
        self.conn.lock().execute_batch("ROLLBACK").map_err(map_err)
    }
    fn execute(&mut self, sql: &str) -> Result<u64, Error> {
        let conn = self.conn.lock();
        let count = conn.execute(sql, []).map_err(map_err)?;
        Ok(count as u64)
    }
}

impl Drop for SqliteTx {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.conn.lock().execute_batch("ROLLBACK");
        }
    }
}

fn map_err(e: rusqlite::Error) -> Error {
    Error::Driver {
        driver: "sqlite".into(),
        message: e.to_string(),
    }
}

fn value_to_sql(value: &Value) -> SqlValue {
    match value {
        Value::Null => SqlValue::Null,
        Value::Bool(b) => SqlValue::Integer(i64::from(*b)),
        Value::Int(n) => SqlValue::Integer(*n),
        Value::Float(n) => SqlValue::Real(*n),
        Value::Text(s) => SqlValue::Text(s.clone()),
        Value::Blob(bytes) => SqlValue::Blob(bytes.clone()),
    }
}

fn value_from_ref(value: ValueRef<'_>) -> Value {
    match value {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(n) => Value::Int(n),
        ValueRef::Real(f) => Value::Float(f),
        ValueRef::Text(bytes) => Value::Text(String::from_utf8_lossy(bytes).into_owned()),
        ValueRef::Blob(bytes) => Value::Blob(bytes.to_vec()),
    }
}

#[allow(dead_code)]
fn _to_sql_marker<T: ToSql>(_: T) {}

#[allow(dead_code)]
fn _statement_marker(_: &mut Statement<'_>, _: Option<&Transaction<'_>>) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn select_one_returns_one() {
        register();
        let mut conn = super::super::open("sqlite", ":memory:").unwrap();
        let mut rows = conn.query("SELECT 1 AS n", &[]).unwrap();
        let row = rows.next_row().unwrap().expect("one row");
        assert_eq!(row.get_i64("n").unwrap(), 1);
        assert!(rows.next_row().unwrap().is_none());
    }

    #[test]
    fn crud_round_trip() {
        register();
        let mut conn = super::super::open("sqlite", ":memory:").unwrap();
        conn.execute(
            "CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT NOT NULL)",
            &[],
        )
        .unwrap();
        let inserted = conn
            .execute(
                "INSERT INTO notes (body) VALUES (?1)",
                &[Value::Text("hello".into())],
            )
            .unwrap();
        assert_eq!(inserted, 1);
        let mut rows = conn
            .query("SELECT id, body FROM notes ORDER BY id", &[])
            .unwrap();
        let row = rows.next_row().unwrap().expect("row");
        assert_eq!(row.get_text("body").unwrap(), "hello");
        assert!(row.get_i64("id").unwrap() >= 1);
    }

    #[test]
    fn transactions_roll_back_on_error() {
        register();
        let mut conn = super::super::open("sqlite", ":memory:").unwrap();
        conn.execute("CREATE TABLE k (v INTEGER)", &[]).unwrap();
        let mut tx = conn.begin().unwrap();
        tx.execute("INSERT INTO k VALUES (1)").unwrap();
        tx.rollback().unwrap();
        let mut rows = conn.query("SELECT count(*) AS c FROM k", &[]).unwrap();
        let row = rows.next_row().unwrap().expect("count row");
        assert_eq!(row.get_i64("c").unwrap(), 0);
    }
}
