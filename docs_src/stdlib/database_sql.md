# `std::database::sql`

Status: shipped

Driver-pluggable SQL database access. No driver ships in the box; bring your own (Postgres, MySQL, SQLite, ...) by registering one at startup.

## Public items

| Name | Kind | Description |
|---|---|---|
| `Driver` | trait | Host-side database driver contract. Rust bindings register implementations before the Gossamer program starts. |
| `register_native` | fn | Registers a Gossamer-native driver under its canonical name. The driver must provide the SQL dispatch surface used by the runtime. |
| `drivers` | fn | Lists every currently-registered driver name. |
| `open` | fn | Opens a database connection by driver name + URL. |
| `Conn` | type | Open database connection. `prepare`, `execute`, `query`, `query_each`, `begin`, `begin_with`, `ping`, `execute_many`, `execute_ctx`, `query_ctx`, `interrupt`, `close` (closing sweeps any cursors still open on the connection). |
| `Tx` | type | Active transaction. `commit`, `rollback`, `savepoint`, `release_savepoint`, `rollback_to_savepoint`, `execute`. |
| `Stmt` | type | Prepared statement. |
| `Rows` | type | Result-set cursor. `next_row`, `columns`, `close` (idempotent). Advancing frees the previous Row; a full drain reclaims everything. For early exits, `defer rows.close()`. |
| `Row` | type | Current row inside a `Rows` walk; valid until the cursor advances or closes. Typed `get_i64`, `get_f64`, `get_bool`, `get_text`, `get_blob` plus `get_opt_*` and `is_null`. |
| `Value` | type | Bound or fetched value. Null / Bool / Int / Float / Text / Blob. |
| `IsolationLevel` | type | Default / ReadUncommitted / ReadCommitted / RepeatableRead / Serializable. Passed to `Conn::begin_with`. |
| `Error` | type | Driver error. `Error::driver(driver, msg)` builds one; `Error::PoolExhausted` and `Error::Cancelled` are variants. |
| `Pool` | type | Connection pool. `new`, `fill`, `get` (blocks up to `acquire_timeout`), `len`. Cheap to clone. |
| `PoolConfig` | type | Pool tuning: `min`, `max`, `idle_timeout`, `max_lifetime`, `acquire_timeout`, `statement_cache`. Fluent `with_*` builders. |
| `PooledConn` | type | Connection checked out from a `Pool`; returned on drop. |
| `Select` | type | Fluent SELECT builder. `Select::new(table).columns(&[..]).where_eq(col, sql::Value::Int(...))...render() -> String`; `.params()` returns the bound parameters. Emits Postgres-style `$N` placeholders. |
| `migrate_up` | fn | Applies pending forward-only schema migrations from a directory of `<version>_<slug>.sql` files. `migrate::up(&mut conn, dir)` is an equivalent namespaced spelling. |
