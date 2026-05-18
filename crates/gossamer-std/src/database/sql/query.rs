//! Fluent `SELECT` query builder.
//!
//! The builder accumulates clauses through chained methods and
//! renders the final `(sql, params)` pair. Param values always
//! flow through the returned `Vec<Value>` — they never appear
//! inline in the SQL string, so the builder is safe against SQL
//! injection on values. Identifiers (table name, column names,
//! `ORDER BY` column) are concatenated verbatim; callers must
//! validate them before passing in.
//!
//! Placeholders use the `PostgreSQL` `$N` style. `SQLite` accepts
//! both `?N` and `$N`, so the same rendered SQL works against
//! either driver.

#![forbid(unsafe_code)]

use super::Value;

/// Fluent builder for a parameterised `SELECT` statement.
///
/// ```
/// use gossamer_std::database::sql::{Select, Value};
///
/// let (sql, params) = Select::new("users")
///     .columns(&["id", "name"])
///     .where_eq("active", Value::Bool(true))
///     .order_by("id", true)
///     .limit(10)
///     .render();
/// assert_eq!(sql, "SELECT id, name FROM users WHERE active = $1 ORDER BY id ASC LIMIT 10");
/// assert_eq!(params, vec![Value::Bool(true)]);
/// ```
#[derive(Debug, Clone, Default)]
pub struct Select {
    table: String,
    columns: Vec<String>,
    wheres: Vec<(String, Value)>,
    order: Vec<(String, bool)>,
    limit: Option<i64>,
    offset: Option<i64>,
}

impl Select {
    /// Starts a new `SELECT` against `table`. The table name is
    /// concatenated verbatim into the rendered SQL — validate it
    /// before calling.
    #[must_use]
    pub fn new(table: &str) -> Self {
        Self {
            table: table.to_string(),
            columns: Vec::new(),
            wheres: Vec::new(),
            order: Vec::new(),
            limit: None,
            offset: None,
        }
    }

    /// Sets the projection list. Empty list renders as `SELECT *`.
    /// Column names are concatenated verbatim; validate before use.
    #[must_use]
    pub fn columns(mut self, cols: &[&str]) -> Self {
        self.columns = cols.iter().map(|c| (*c).to_string()).collect();
        self
    }

    /// Adds a `col = $N` predicate. Multiple calls compose with
    /// `AND` in the order they were added. The column name is
    /// inlined; the value is bound through the param list.
    #[must_use]
    pub fn where_eq(mut self, col: &str, value: Value) -> Self {
        self.wheres.push((col.to_string(), value));
        self
    }

    /// Adds an `ORDER BY col ASC|DESC` clause. Multiple calls
    /// append additional sort keys in declaration order. `asc =
    /// true` renders `ASC`, `false` renders `DESC`.
    #[must_use]
    pub fn order_by(mut self, col: &str, asc: bool) -> Self {
        self.order.push((col.to_string(), asc));
        self
    }

    /// Adds a `LIMIT N` clause. Last call wins.
    #[must_use]
    pub fn limit(mut self, n: i64) -> Self {
        self.limit = Some(n);
        self
    }

    /// Adds an `OFFSET N` clause. Last call wins.
    #[must_use]
    pub fn offset(mut self, n: i64) -> Self {
        self.offset = Some(n);
        self
    }

    /// Renders `(sql, params)`. Param values are pushed in
    /// `where_eq` declaration order, matching the `$1 .. $N`
    /// placeholders in the SQL.
    #[must_use]
    pub fn render(&self) -> (String, Vec<Value>) {
        let mut sql = String::from("SELECT ");
        if self.columns.is_empty() {
            sql.push('*');
        } else {
            sql.push_str(&self.columns.join(", "));
        }
        sql.push_str(" FROM ");
        sql.push_str(&self.table);

        let mut params = Vec::with_capacity(self.wheres.len());
        if !self.wheres.is_empty() {
            sql.push_str(" WHERE ");
            for (i, (col, value)) in self.wheres.iter().enumerate() {
                if i > 0 {
                    sql.push_str(" AND ");
                }
                sql.push_str(col);
                sql.push_str(" = $");
                sql.push_str(&(i + 1).to_string());
                params.push(value.clone());
            }
        }

        if !self.order.is_empty() {
            sql.push_str(" ORDER BY ");
            for (i, (col, asc)) in self.order.iter().enumerate() {
                if i > 0 {
                    sql.push_str(", ");
                }
                sql.push_str(col);
                sql.push_str(if *asc { " ASC" } else { " DESC" });
            }
        }

        if let Some(n) = self.limit {
            sql.push_str(" LIMIT ");
            sql.push_str(&n.to_string());
        }
        if let Some(n) = self.offset {
            sql.push_str(" OFFSET ");
            sql.push_str(&n.to_string());
        }

        (sql, params)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::sql::{Conn, ConnectionImpl, Error, RowsImpl, StatementImpl, TransactionImpl};

    #[test]
    fn select_empty_columns_defaults_to_star() {
        let (sql, params) = Select::new("users").render();
        assert_eq!(sql, "SELECT * FROM users");
        assert!(params.is_empty());
    }

    #[test]
    fn select_with_columns() {
        let (sql, params) = Select::new("users")
            .columns(&["id", "name", "email"])
            .render();
        assert_eq!(sql, "SELECT id, name, email FROM users");
        assert!(params.is_empty());
    }

    #[test]
    fn select_single_where_eq() {
        let (sql, params) = Select::new("users")
            .columns(&["id"])
            .where_eq("active", Value::Bool(true))
            .render();
        assert_eq!(sql, "SELECT id FROM users WHERE active = $1");
        assert_eq!(params, vec![Value::Bool(true)]);
    }

    #[test]
    fn select_multiple_where_eq_compose_with_and() {
        let (sql, params) = Select::new("users")
            .where_eq("active", Value::Bool(true))
            .where_eq("tenant", Value::Int(42))
            .where_eq("role", Value::Text("admin".to_string()))
            .render();
        assert_eq!(
            sql,
            "SELECT * FROM users WHERE active = $1 AND tenant = $2 AND role = $3"
        );
        assert_eq!(
            params,
            vec![
                Value::Bool(true),
                Value::Int(42),
                Value::Text("admin".to_string()),
            ]
        );
    }

    #[test]
    fn select_order_by_asc() {
        let (sql, _) = Select::new("users").order_by("id", true).render();
        assert_eq!(sql, "SELECT * FROM users ORDER BY id ASC");
    }

    #[test]
    fn select_order_by_desc() {
        let (sql, _) = Select::new("events").order_by("ts", false).render();
        assert_eq!(sql, "SELECT * FROM events ORDER BY ts DESC");
    }

    #[test]
    fn select_order_by_compound() {
        let (sql, _) = Select::new("events")
            .order_by("ts", false)
            .order_by("id", true)
            .render();
        assert_eq!(sql, "SELECT * FROM events ORDER BY ts DESC, id ASC");
    }

    #[test]
    fn select_limit() {
        let (sql, _) = Select::new("users").limit(25).render();
        assert_eq!(sql, "SELECT * FROM users LIMIT 25");
    }

    #[test]
    fn select_offset() {
        let (sql, _) = Select::new("users").offset(100).render();
        assert_eq!(sql, "SELECT * FROM users OFFSET 100");
    }

    #[test]
    fn select_limit_and_offset() {
        let (sql, _) = Select::new("users").limit(10).offset(20).render();
        assert_eq!(sql, "SELECT * FROM users LIMIT 10 OFFSET 20");
    }

    #[test]
    fn select_full_pipeline() {
        let (sql, params) = Select::new("orders")
            .columns(&["id", "total"])
            .where_eq("customer_id", Value::Int(7))
            .where_eq("status", Value::Text("paid".to_string()))
            .order_by("id", true)
            .limit(50)
            .offset(100)
            .render();
        assert_eq!(
            sql,
            "SELECT id, total FROM orders WHERE customer_id = $1 AND status = $2 \
             ORDER BY id ASC LIMIT 50 OFFSET 100"
        );
        assert_eq!(
            params,
            vec![Value::Int(7), Value::Text("paid".to_string())]
        );
    }

    #[test]
    fn select_last_limit_wins() {
        let (sql, _) = Select::new("t").limit(5).limit(99).render();
        assert_eq!(sql, "SELECT * FROM t LIMIT 99");
    }

    #[test]
    fn select_value_types_round_trip_through_params() {
        let (sql, params) = Select::new("mixed")
            .where_eq("a", Value::Null)
            .where_eq("b", Value::Float(1.5))
            .where_eq("c", Value::Blob(vec![0xde, 0xad]))
            .render();
        assert_eq!(
            sql,
            "SELECT * FROM mixed WHERE a = $1 AND b = $2 AND c = $3"
        );
        assert_eq!(
            params,
            vec![Value::Null, Value::Float(1.5), Value::Blob(vec![0xde, 0xad])]
        );
    }

    // ---- execute_many ------------------------------------------

    type ExecuteLog = std::sync::Arc<parking_lot::Mutex<Vec<(String, Vec<Value>)>>>;
    type PrepareCount = std::sync::Arc<parking_lot::Mutex<usize>>;

    struct CountingStmt {
        sql: String,
        log: ExecuteLog,
    }

    impl StatementImpl for CountingStmt {
        fn execute(&mut self, params: &[Value]) -> Result<u64, Error> {
            self.log.lock().push((self.sql.clone(), params.to_vec()));
            Ok(1)
        }
        fn query(&mut self, _params: &[Value]) -> Result<Box<dyn RowsImpl>, Error> {
            unreachable!("query not used in execute_many tests")
        }
    }

    struct CountingConn {
        prepares: PrepareCount,
        log: ExecuteLog,
    }

    impl ConnectionImpl for CountingConn {
        fn prepare(&mut self, sql: &str) -> Result<Box<dyn StatementImpl>, Error> {
            *self.prepares.lock() += 1;
            Ok(Box::new(CountingStmt {
                sql: sql.to_string(),
                log: self.log.clone(),
            }))
        }
        fn begin(&mut self) -> Result<Box<dyn TransactionImpl>, Error> {
            unreachable!("begin not used in execute_many tests")
        }
        fn close(&mut self) -> Result<(), Error> {
            Ok(())
        }
    }

    fn fixture() -> (Conn, PrepareCount, ExecuteLog) {
        let prepares = std::sync::Arc::new(parking_lot::Mutex::new(0));
        let log = std::sync::Arc::new(parking_lot::Mutex::new(Vec::new()));
        let conn = Conn::new(Box::new(CountingConn {
            prepares: prepares.clone(),
            log: log.clone(),
        }));
        (conn, prepares, log)
    }

    #[test]
    fn execute_many_zero_batches_prepares_once() {
        let (mut conn, prepares, log) = fixture();
        let n = conn
            .execute_many("INSERT INTO t(x) VALUES ($1)", &[])
            .unwrap();
        assert_eq!(n, 0);
        assert_eq!(*prepares.lock(), 1);
        assert!(log.lock().is_empty());
    }

    #[test]
    fn execute_many_single_batch() {
        let (mut conn, prepares, log) = fixture();
        let n = conn
            .execute_many("INSERT INTO t(x) VALUES ($1)", &[&[Value::Int(7)][..]])
            .unwrap();
        assert_eq!(n, 1);
        assert_eq!(*prepares.lock(), 1);
        assert_eq!(log.lock().len(), 1);
        assert_eq!(log.lock()[0].1, vec![Value::Int(7)]);
    }

    #[test]
    fn execute_many_n_batches_prepare_once_execute_n() {
        let (mut conn, prepares, log) = fixture();
        let batches: Vec<Vec<Value>> = (0..5).map(|i| vec![Value::Int(i)]).collect();
        let refs: Vec<&[Value]> = batches.iter().map(Vec::as_slice).collect();
        let n = conn
            .execute_many("INSERT INTO t(x) VALUES ($1)", &refs)
            .unwrap();
        assert_eq!(n, 5);
        assert_eq!(*prepares.lock(), 1);
        let log = log.lock();
        assert_eq!(log.len(), 5);
        for (i, (sql, params)) in log.iter().enumerate() {
            assert_eq!(sql, "INSERT INTO t(x) VALUES ($1)");
            assert_eq!(params, &vec![Value::Int(i as i64)]);
        }
    }
}
